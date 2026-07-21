//! Maps a run's [`TurnEvent`]s into an [`api::ConversationData`] that Warp can
//! render as inline AI blocks via
//! `TerminalView::load_conversation_from_tasks`.
//!
//! This is the bridge between a local CLI harness's structured output and
//! Warp's own conversation UI — the thing that replaces the CLI's full-screen
//! TUI with inline blocks. Assistant prose and reasoning map to their native
//! `AgentOutput`/`AgentReasoning` message types. Tool calls map to native
//! `ToolCall`/`ToolCallResult` messages. Harness shell calls intentionally use
//! the generic, read-only `CallMcpTool` card too: representing an already-run
//! transcript as `RunShellCommand` makes Warp create a second terminal block
//! below the conversation card. See docs/local-harness-lifecycle.md.

use std::collections::HashMap;

use ai::agent::{ToolCallOutcome, TurnEvent};
use uuid::Uuid;
use warp_multi_agent_api as api;

use crate::ai::agent::task::TaskId;
use crate::ai::agent::{AIAgentActionResult, AIAgentActionResultType, CallMCPToolResult};

/// Tool output longer than this is truncated in the rendered block; the CLI's
/// own transcript remains the source of truth for full output.
const MAX_RESULT_CHARS: usize = 4000;

/// Builds a single-task conversation from the prompt and the events emitted
/// while the harness ran. IDs are client-minted (`Uuid`); they only need to be
/// unique within this rendered conversation.
#[cfg(test)]
pub fn turn_events_to_conversation(prompt: &str, events: &[TurnEvent]) -> api::ConversationData {
    let task_id = Uuid::new_v4().to_string();
    let request_id = Uuid::new_v4().to_string();

    let mut messages = vec![user_query_message(&task_id, &request_id, prompt)];

    let mut shell_commands = HashMap::new();
    for event in events {
        messages.extend(turn_event_to_messages(
            &task_id,
            &request_id,
            event,
            &mut shell_commands,
        ));
    }

    api::ConversationData {
        tasks: vec![api::Task {
            id: task_id,
            messages,
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// Tool names the harnesses use for plain shell execution (claude's `Bash`,
/// codex and Ollama's `Shell`). They stay in the inline read-only tool-card
/// path so a transcript never creates a terminal block below itself.
pub(crate) fn is_shell_tool(name: &str) -> bool {
    matches!(name, "Bash" | "Shell")
}

/// Renderable messages for a single event, in order. Empty for events that
/// carry no user-visible content (session metadata, the completion marker).
/// Shared by the whole-conversation builder above and the live-streaming
/// harness runner, so both render events identically.
///
/// `shell_commands` tracks which call ids are shell calls so their result is
/// kept on the same generic card. Values retain the command for diagnostics.
pub(crate) fn turn_event_to_messages(
    task_id: &str,
    request_id: &str,
    event: &TurnEvent,
    shell_commands: &mut HashMap<String, String>,
) -> Vec<api::Message> {
    match event {
        TurnEvent::Text { text } => {
            vec![agent_output_message(task_id, request_id, text.clone())]
        }
        // Token deltas are handled by `LocalHarnessStream::apply_text_delta`,
        // which appends them to one stable message instead of minting a
        // message (and apparent line) per token.
        TurnEvent::TextDelta { .. } => Vec::new(),
        TurnEvent::Thinking { text } => {
            vec![agent_reasoning_message(task_id, request_id, text.clone())]
        }
        TurnEvent::ToolCallStarted {
            call_id,
            name,
            input,
        } => {
            if is_shell_tool(name) {
                let command = shell_command_from_input(input);
                shell_commands.insert(call_id.clone(), command);
            }
            vec![tool_call_message(task_id, request_id, call_id, name, input)]
        }
        TurnEvent::ToolCallResult { call_id, outcome } => {
            shell_commands.remove(call_id);
            vec![tool_call_result_message(
                task_id, request_id, call_id, outcome,
            )]
        }
        // A failed turn renders its error so the run doesn't end invisibly
        // (e.g. the CLI exiting before producing any assistant output).
        TurnEvent::TurnCompleted {
            is_error: true,
            error_message,
            ..
        } => {
            let detail = error_message
                .as_deref()
                .unwrap_or("agent exited with an error");
            vec![agent_output_message(
                task_id,
                request_id,
                format!("**⚠ Error:** {detail}"),
            )]
        }
        // Session metadata and the successful completion marker carry no
        // renderable content of their own.
        TurnEvent::SessionInit { .. } | TurnEvent::TurnCompleted { .. } => Vec::new(),
    }
}

/// The user's prompt as a task message — the local analogue of the cloud
/// server echoing the query into the task. Restoration rebuilds exchange
/// inputs from these (`user_inputs_from_messages`), so without one in the
/// task, a restored conversation shows responses with no query bubbles.
pub(crate) fn user_query_message(task_id: &str, request_id: &str, prompt: &str) -> api::Message {
    api::Message {
        id: Uuid::new_v4().to_string(),
        task_id: task_id.to_string(),
        request_id: request_id.to_string(),
        message: Some(api::message::Message::UserQuery(api::message::UserQuery {
            query: prompt.to_string(),
            ..Default::default()
        })),
        ..Default::default()
    }
}

fn agent_output_message(task_id: &str, request_id: &str, text: String) -> api::Message {
    api::Message {
        id: Uuid::new_v4().to_string(),
        task_id: task_id.to_string(),
        request_id: request_id.to_string(),
        message: Some(api::message::Message::AgentOutput(
            api::message::AgentOutput { text },
        )),
        ..Default::default()
    }
}

fn agent_reasoning_message(task_id: &str, request_id: &str, reasoning: String) -> api::Message {
    api::Message {
        id: Uuid::new_v4().to_string(),
        task_id: task_id.to_string(),
        request_id: request_id.to_string(),
        message: Some(api::message::Message::AgentReasoning(
            api::message::AgentReasoning {
                reasoning,
                ..Default::default()
            },
        )),
        ..Default::default()
    }
}

/// A tool invocation as a native `ToolCall` message (`CallMcpTool` variant),
/// so the client renders it with its own collapsible, read-only tool card —
/// collapsed by default — instead of raw Markdown. The `call_id` becomes the
/// action id, correlating this call with its later `ToolCallResult`.
fn tool_call_message(
    task_id: &str,
    request_id: &str,
    call_id: &str,
    name: &str,
    input: &serde_json::Value,
) -> api::Message {
    let args = match serde_json_to_prost(input.clone()) {
        prost_types::Value {
            kind: Some(prost_types::value::Kind::StructValue(s)),
        } => Some(s),
        // Tool inputs are JSON objects; anything else (rare) carries no args.
        _ => None,
    };
    api::Message {
        id: Uuid::new_v4().to_string(),
        task_id: task_id.to_string(),
        request_id: request_id.to_string(),
        message: Some(api::message::Message::ToolCall(api::message::ToolCall {
            tool_call_id: call_id.to_string(),
            tool: Some(api::message::tool_call::Tool::CallMcpTool(
                api::message::tool_call::CallMcpTool {
                    name: name.to_string(),
                    args,
                    server_id: String::new(),
                },
            )),
        })),
        ..Default::default()
    }
}

/// The shell command string from a shell tool's input (both harnesses use a
/// `command` key).
fn shell_command_from_input(input: &serde_json::Value) -> String {
    input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// The matching `ToolCallResult` message. On restore this is what
/// reconstructs the card's result (via `restore_action_results_from_exchanges`);
/// live runs additionally register [`tool_call_action_result`] so the result
/// shows immediately.
fn tool_call_result_message(
    task_id: &str,
    request_id: &str,
    call_id: &str,
    outcome: &ToolCallOutcome,
) -> api::Message {
    use api::call_mcp_tool_result::success::result::Result as ResultItem;
    use api::call_mcp_tool_result::success::Result;
    use api::call_mcp_tool_result::Success;

    let result = match outcome {
        ToolCallOutcome::Success(value) => {
            let text = truncate(tool_result_text(value).trim_end());
            api::call_mcp_tool_result::Result::Success(Success {
                results: vec![Result {
                    result: Some(ResultItem::Text(
                        api::call_mcp_tool_result::success::result::Text { text },
                    )),
                }],
            })
        }
        ToolCallOutcome::Error(message) => {
            api::call_mcp_tool_result::Result::Error(api::call_mcp_tool_result::Error {
                message: truncate(message),
            })
        }
        // A cancelled call carries no result; leave it unset.
        ToolCallOutcome::Cancelled => {
            return api::Message {
                id: Uuid::new_v4().to_string(),
                task_id: task_id.to_string(),
                request_id: request_id.to_string(),
                message: Some(api::message::Message::ToolCallResult(
                    api::message::ToolCallResult {
                        tool_call_id: call_id.to_string(),
                        result: None,
                        ..Default::default()
                    },
                )),
                ..Default::default()
            };
        }
    };

    api::Message {
        id: Uuid::new_v4().to_string(),
        task_id: task_id.to_string(),
        request_id: request_id.to_string(),
        message: Some(api::message::Message::ToolCallResult(
            api::message::ToolCallResult {
                tool_call_id: call_id.to_string(),
                result: Some(api::message::tool_call_result::Result::CallMcpTool(
                    api::CallMcpToolResult {
                        result: Some(result),
                    },
                )),
                ..Default::default()
            },
        )),
        ..Default::default()
    }
}

/// The client-side action result for a finished tool call, used to register
/// the result live (the message path above only feeds restore). Local harness
/// tools all use the same read-only generic card shape, including shell, so
/// none can be mistaken for a command that should open a terminal block.
pub(crate) fn tool_call_action_result(
    task_id: &str,
    call_id: &str,
    outcome: &ToolCallOutcome,
) -> AIAgentActionResult {
    let result = match outcome {
        ToolCallOutcome::Success(value) => {
            let text = truncate(tool_result_text(value).trim_end());
            CallMCPToolResult::Success {
                result: rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(
                    text,
                )]),
            }
        }
        ToolCallOutcome::Error(message) => CallMCPToolResult::Error(truncate(message)),
        ToolCallOutcome::Cancelled => CallMCPToolResult::Cancelled,
    };
    AIAgentActionResult {
        id: call_id.to_string().into(),
        task_id: TaskId::new(task_id.to_string()),
        result: AIAgentActionResultType::CallMCPTool(result),
    }
}

/// Extracts the human-readable text from a tool's success value: claude's
/// `stdout` (+ `stderr`) fields, codex's `output` field, plain strings, and
/// pretty-printed JSON as the last resort.
fn tool_result_text(value: &serde_json::Value) -> String {
    let stdout = value.get("stdout").and_then(|v| v.as_str());
    let stderr = value
        .get("stderr")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    if stdout.is_some() || stderr.is_some() {
        let mut text = stdout.unwrap_or_default().to_string();
        if let Some(err) = stderr {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(err);
        }
        return text;
    }
    value
        .get("output")
        .or_else(|| value.get("result"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| match value {
            serde_json::Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
        })
}

/// Converts a `serde_json::Value` into a `prost_types::Value` for MCP tool
/// args. Mirrors the private helper in `convert_to.rs`; integer/float
/// distinctions are erased (all JSON numbers become `f64`), which the client
/// re-coerces from the tool's input schema when one exists.
fn serde_json_to_prost(value: serde_json::Value) -> prost_types::Value {
    use prost_types::value::Kind;
    use serde_json::Value;

    let kind = match value {
        Value::Null => Kind::NullValue(0),
        Value::Bool(b) => Kind::BoolValue(b),
        Value::Number(n) => Kind::NumberValue(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => Kind::StringValue(s),
        Value::Array(a) => Kind::ListValue(prost_types::ListValue {
            values: a.into_iter().map(serde_json_to_prost).collect(),
        }),
        Value::Object(o) => Kind::StructValue(prost_types::Struct {
            fields: o
                .into_iter()
                .map(|(k, v)| (k, serde_json_to_prost(v)))
                .collect(),
        }),
    };
    prost_types::Value { kind: Some(kind) }
}

fn truncate(text: &str) -> String {
    if text.chars().count() <= MAX_RESULT_CHARS {
        return text.to_string();
    }
    let kept: String = text.chars().take(MAX_RESULT_CHARS).collect();
    format!("{kept}\n… (truncated)")
}

#[cfg(test)]
#[path = "turn_events_to_conversation_tests.rs"]
mod tests;
