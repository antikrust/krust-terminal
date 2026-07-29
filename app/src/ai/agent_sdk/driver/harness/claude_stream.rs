//! Parses Claude Code CLI's `--output-format stream-json` ndjson lines into
//! provider-agnostic [`TurnEvent`]s.
//!
//! Schema confirmed against a real `claude -p ... --output-format
//! stream-json --verbose` run (not just documentation): each line is one
//! JSON object with a top-level `type`. `assistant`/`user` messages carry
//! an Anthropic Messages-API-shaped `content` array; tool calls and their
//! results correlate via `tool_use.id` / `tool_result.tool_use_id`. Some
//! `system` subtypes (`hook_started`, `hook_response`, `post_turn_summary`)
//! and the top-level `rate_limit_event` type are noise from Bork's own
//! notification-hook plugin and rate-limit telemetry — they carry no
//! renderable content and are skipped.

use std::collections::HashMap;

use ai::agent::{ToolCallOutcome, TurnEvent, TurnUsage};
use serde::Deserialize;

/// Parses one ndjson line into zero or more [`TurnEvent`]s.
///
/// Returns an empty vec for lines that are recognized-but-not-renderable
/// (hook noise, rate-limit events) and for lines that fail to parse at all
/// (logged, not fatal — a single malformed line shouldn't kill the run).
pub fn parse_stream_json_line(line: &str) -> Vec<TurnEvent> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }

    let raw: RawLine = match serde_json::from_str(line) {
        Ok(raw) => raw,
        Err(error) => {
            log::warn!("claude_stream: failed to parse stream-json line: {error}. line: {line}");
            return Vec::new();
        }
    };

    match raw {
        RawLine::System(system) => system_events(system),
        RawLine::Assistant(assistant) => content_block_events(assistant.message.content),
        RawLine::User(user) => tool_result_events(user.message.content, user.tool_use_result),
        RawLine::Result(result) => vec![result_event(result)],
        // Noise we deliberately don't render: rate-limit telemetry and any
        // future top-level type we don't recognize yet.
        RawLine::Other => Vec::new(),
    }
}

fn system_events(system: SystemLine) -> Vec<TurnEvent> {
    match system.subtype.as_deref() {
        Some("init") => vec![TurnEvent::SessionInit {
            session_id: system.session_id.unwrap_or_default(),
            model: system.model,
            cwd: system.cwd,
        }],
        // hook_started/hook_response/post_turn_summary/etc: bookkeeping
        // from Bork's own notification hook, not agent output.
        _ => Vec::new(),
    }
}

fn content_block_events(blocks: Vec<ContentBlock>) -> Vec<TurnEvent> {
    blocks
        .into_iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(TurnEvent::Text { text }),
            ContentBlock::Thinking { thinking } => Some(TurnEvent::Thinking { text: thinking }),
            ContentBlock::ToolUse { id, name, input } => Some(TurnEvent::ToolCallStarted {
                call_id: id,
                name,
                input,
            }),
            // Tool results only ever appear on `user` lines, never
            // `assistant` ones, but the content-block enum is shared.
            ContentBlock::ToolResult { .. } => None,
            ContentBlock::Unknown => None,
        })
        .collect()
}

fn tool_result_events(
    blocks: Vec<ContentBlock>,
    tool_use_result: Option<serde_json::Value>,
) -> Vec<TurnEvent> {
    blocks
        .into_iter()
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let outcome =
                    if is_error.unwrap_or(false) {
                        ToolCallOutcome::Error(tool_result_text(&content))
                    } else {
                        // Prefer the richer, tool-specific `tool_use_result`
                        // sibling field (e.g. `structuredPatch`/`filePath` for
                        // Write/Edit, `stdout`/`stderr` for Bash) when present;
                        // fall back to the plain content string otherwise.
                        ToolCallOutcome::Success(tool_use_result.clone().unwrap_or_else(|| {
                            serde_json::Value::String(tool_result_text(&content))
                        }))
                    };
                Some(TurnEvent::ToolCallResult {
                    call_id: tool_use_id,
                    outcome,
                })
            }
            _ => None,
        })
        .collect()
}

fn tool_result_text(content: &ToolResultContent) -> String {
    match content {
        ToolResultContent::_Empty => String::new(),
        ToolResultContent::Text(text) => text.clone(),
        ToolResultContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn result_event(result: ResultLine) -> TurnEvent {
    // The primary model is the one that saw the most tokens; subagent models
    // show up as additional (smaller) `modelUsage` entries.
    let primary_model = result
        .model_usage
        .iter()
        .max_by_key(|(_, usage)| usage.total_tokens());

    // Context occupancy after the run is what the *final* API call saw: its
    // prompt (fresh + cached) plus its output. The top-level counters sum
    // every call in the run, which would overcount; when the CLI provides
    // per-call `iterations`, prefer the last one.
    let usage = result.usage.as_ref();
    let final_call = usage.map(|u| u.iterations.last().unwrap_or(u));

    TurnEvent::TurnCompleted {
        usage: Some(TurnUsage {
            input_tokens: final_call.map(|u| u.input_tokens).unwrap_or(0),
            output_tokens: final_call.map(|u| u.output_tokens).unwrap_or(0),
            cache_read_input_tokens: final_call.map(|u| u.cache_read_input_tokens).unwrap_or(0),
            cache_creation_input_tokens: final_call
                .map(|u| u.cache_creation_input_tokens)
                .unwrap_or(0),
            model_id: primary_model.map(|(model_id, _)| model_id.clone()),
            context_window: primary_model.and_then(|(_, usage)| usage.context_window),
            cost_usd: result.total_cost_usd,
            duration_ms: result.duration_ms,
        }),
        is_error: result.is_error,
        error_message: result.is_error.then_some(result.result).flatten(),
    }
}

// --- Wire format (loosely typed: only fields we actually use) ---

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawLine {
    System(SystemLine),
    Assistant(AssistantLine),
    User(UserLine),
    Result(ResultLine),
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct SystemLine {
    subtype: Option<String>,
    session_id: Option<String>,
    model: Option<String>,
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct AssistantLine {
    message: MessageBody,
}

#[derive(Deserialize)]
struct UserLine {
    message: MessageBody,
    tool_use_result: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct MessageBody {
    #[serde(default)]
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: ToolResultContent,
        is_error: Option<bool>,
    },
    #[serde(other)]
    Unknown,
}

/// Claude sends `tool_result.content` as either a plain string or an array
/// of content blocks (mirroring the Messages API), depending on the tool.
#[derive(Deserialize, Default)]
#[serde(untagged)]
enum ToolResultContent {
    #[default]
    #[serde(skip)]
    _Empty,
    Text(String),
    Blocks(Vec<serde_json::Value>),
}

#[derive(Deserialize)]
struct ResultLine {
    is_error: bool,
    result: Option<String>,
    duration_ms: Option<u64>,
    total_cost_usd: Option<f64>,
    usage: Option<ResultUsage>,
    /// Per-model usage keyed by model id; carries the context-window size.
    #[serde(rename = "modelUsage", default)]
    model_usage: HashMap<String, ModelUsage>,
}

#[derive(Deserialize)]
struct ResultUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    /// Per-API-call usage rows; the last entry is the run's final call.
    #[serde(default)]
    iterations: Vec<ResultUsage>,
}

#[derive(Deserialize)]
struct ModelUsage {
    #[serde(rename = "inputTokens", default)]
    input_tokens: u64,
    #[serde(rename = "outputTokens", default)]
    output_tokens: u64,
    #[serde(rename = "cacheReadInputTokens", default)]
    cache_read_input_tokens: u64,
    #[serde(rename = "cacheCreationInputTokens", default)]
    cache_creation_input_tokens: u64,
    #[serde(rename = "contextWindow")]
    context_window: Option<u64>,
}

impl ModelUsage {
    fn total_tokens(&self) -> u64 {
        self.input_tokens
            + self.cache_read_input_tokens
            + self.cache_creation_input_tokens
            + self.output_tokens
    }
}

#[cfg(test)]
#[path = "claude_stream_tests.rs"]
mod tests;
