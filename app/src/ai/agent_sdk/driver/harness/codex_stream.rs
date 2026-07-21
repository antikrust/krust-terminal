//! Parses Codex CLI's `codex exec --json` JSONL events into provider-agnostic
//! [`TurnEvent`]s.
//!
//! Schema confirmed against a real `codex exec --json` run (codex-cli
//! 0.144.x), not just documentation: each stdout line is one JSON object with
//! a top-level `type` of `thread.started` / `turn.started` / `item.started` /
//! `item.updated` / `item.completed` / `turn.completed` / `turn.failed` /
//! `error`. Items carry their own `type` (`agent_message`, `reasoning`,
//! `command_execution`, `file_change`, `mcp_tool_call`, `web_search`,
//! `todo_list`, `error`, ...). Only *completed* items are rendered —
//! `item.started`/`item.updated` would duplicate them — and stderr is plain
//! `tracing` log text (never JSON), so callers must only parse stdout.

use ai::agent::{ToolCallOutcome, TurnEvent, TurnUsage};
use serde::Deserialize;
use serde_json::json;

/// Parses one JSONL line into zero or more [`TurnEvent`]s.
///
/// Returns an empty vec for lines that are recognized-but-not-renderable
/// (`turn.started`, in-progress item updates, todo-list chatter) and for
/// lines that fail to parse (logged, not fatal — one malformed line must
/// not kill the run).
pub fn parse_codex_jsonl_line(line: &str) -> Vec<TurnEvent> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }

    let raw: RawEvent = match serde_json::from_str(line) {
        Ok(raw) => raw,
        Err(error) => {
            log::warn!("codex_stream: skipping unparseable line: {error}");
            return Vec::new();
        }
    };

    match raw {
        RawEvent::ThreadStarted { thread_id } => vec![TurnEvent::SessionInit {
            session_id: thread_id,
            model: None,
            cwd: None,
        }],
        RawEvent::ItemCompleted { item } => item_events(*item),
        RawEvent::TurnCompleted { usage } => vec![TurnEvent::TurnCompleted {
            usage: usage.map(turn_usage),
            is_error: false,
            error_message: None,
        }],
        RawEvent::TurnFailed { error } => vec![TurnEvent::TurnCompleted {
            usage: None,
            is_error: true,
            error_message: error.map(|error| error.message),
        }],
        RawEvent::Error { message } => vec![TurnEvent::Text {
            text: format!("**⚠ Codex error:** {message}"),
        }],
        RawEvent::Other => Vec::new(),
    }
}

fn turn_usage(usage: RawUsage) -> TurnUsage {
    TurnUsage {
        // Codex reports OpenAI-style counts where `input_tokens` *includes*
        // the cached prefix; `TurnUsage` keeps the fields disjoint
        // (Anthropic-style) so downstream context math can sum them.
        input_tokens: usage.input_tokens.saturating_sub(usage.cached_input_tokens),
        output_tokens: usage.output_tokens + usage.reasoning_output_tokens,
        cache_read_input_tokens: usage.cached_input_tokens,
        cache_creation_input_tokens: 0,
        // The stream never names the model; label usage rows by harness
        // until model selection is plumbed through the picker.
        model_id: Some("codex".to_string()),
        context_window: None,
        cost_usd: None,
        duration_ms: None,
    }
}

fn item_events(item: RawItem) -> Vec<TurnEvent> {
    match item.item_type.as_str() {
        "agent_message" => item
            .text
            .map(|text| TurnEvent::Text { text })
            .into_iter()
            .collect(),
        "reasoning" => item
            .text
            .map(|text| TurnEvent::Thinking { text })
            .into_iter()
            .collect(),
        "command_execution" => {
            let output = item.aggregated_output.clone().unwrap_or_default();
            let failed = item.status.as_deref() == Some("failed")
                || item.exit_code.is_some_and(|code| code != 0);
            let outcome = if failed {
                let exit_code = item
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "?".to_string());
                ToolCallOutcome::Error(format!("exit {exit_code}\n{output}"))
            } else {
                ToolCallOutcome::Success(json!({ "output": output }))
            };
            vec![
                TurnEvent::ToolCallStarted {
                    call_id: item.id.clone(),
                    name: "Shell".to_string(),
                    input: json!({ "command": item.command.unwrap_or_default() }),
                },
                TurnEvent::ToolCallResult {
                    call_id: item.id,
                    outcome,
                },
            ]
        }
        "file_change" => vec![
            TurnEvent::ToolCallStarted {
                call_id: item.id.clone(),
                name: "Edit".to_string(),
                input: json!({ "changes": item.changes.unwrap_or_default() }),
            },
            TurnEvent::ToolCallResult {
                call_id: item.id,
                outcome: ToolCallOutcome::Success(json!({
                    "status": item.status.unwrap_or_default(),
                })),
            },
        ],
        "mcp_tool_call" => {
            let name = format!(
                "{}.{}",
                item.server.unwrap_or_else(|| "mcp".to_string()),
                item.tool.unwrap_or_default()
            );
            let failed = item.status.as_deref() == Some("failed");
            let outcome = if failed {
                ToolCallOutcome::Error("failed".to_string())
            } else {
                ToolCallOutcome::Success(json!({
                    "status": item.status.unwrap_or_default(),
                }))
            };
            vec![
                TurnEvent::ToolCallStarted {
                    call_id: item.id.clone(),
                    name,
                    input: json!({}),
                },
                TurnEvent::ToolCallResult {
                    call_id: item.id,
                    outcome,
                },
            ]
        }
        "web_search" => vec![
            TurnEvent::ToolCallStarted {
                call_id: item.id.clone(),
                name: "WebSearch".to_string(),
                input: json!({ "query": item.query.unwrap_or_default() }),
            },
            TurnEvent::ToolCallResult {
                call_id: item.id,
                outcome: ToolCallOutcome::Success(json!({})),
            },
        ],
        "error" => item
            .message
            .or(item.text)
            .map(|message| TurnEvent::Text {
                text: format!("**⚠ Codex error:** {message}"),
            })
            .into_iter()
            .collect(),
        // todo_list and anything unrecognized: bookkeeping, not renderable.
        _ => Vec::new(),
    }
}

// --- Wire format (loosely typed: only fields we actually use) ---

#[derive(Deserialize)]
#[serde(tag = "type")]
enum RawEvent {
    #[serde(rename = "thread.started")]
    ThreadStarted { thread_id: String },
    #[serde(rename = "item.completed")]
    ItemCompleted { item: Box<RawItem> },
    #[serde(rename = "turn.completed")]
    TurnCompleted { usage: Option<RawUsage> },
    #[serde(rename = "turn.failed")]
    TurnFailed { error: Option<RawError> },
    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        message: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct RawItem {
    #[serde(default)]
    id: String,
    #[serde(rename = "type")]
    item_type: String,
    text: Option<String>,
    message: Option<String>,
    command: Option<String>,
    aggregated_output: Option<String>,
    exit_code: Option<i64>,
    status: Option<String>,
    changes: Option<serde_json::Value>,
    server: Option<String>,
    tool: Option<String>,
    query: Option<String>,
}

#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    reasoning_output_tokens: u64,
}

#[derive(Deserialize)]
struct RawError {
    #[serde(default)]
    message: String,
}

#[cfg(test)]
#[path = "codex_stream_tests.rs"]
mod tests;
