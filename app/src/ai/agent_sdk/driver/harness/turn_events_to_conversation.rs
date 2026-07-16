//! Maps a run's [`TurnEvent`]s into an [`api::ConversationData`] that Warp can
//! render as inline AI blocks via
//! `TerminalView::load_conversation_from_tasks`.
//!
//! This is the bridge between a local CLI harness's structured output and
//! Warp's own conversation UI — the thing that replaces the CLI's full-screen
//! TUI with inline blocks. For this first version, assistant prose and
//! reasoning map to their native `AgentOutput`/`AgentReasoning` message types,
//! while tool calls and their results are rendered as lightweight Markdown
//! `AgentOutput` blocks. Mapping tool calls to Warp's native, interactive
//! command/diff cards (`Message::ToolCall`/`ToolCallResult`) is a deliberate
//! follow-up; representing them as Markdown keeps the whole conversation
//! readable inline without the per-tool proto plumbing.

use ai::agent::{ToolCallOutcome, TurnEvent};
use uuid::Uuid;
use warp_multi_agent_api as api;

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

    for event in events {
        messages.extend(turn_event_to_messages(&task_id, &request_id, event));
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

/// Renderable messages for a single event, in order. Empty for events that
/// carry no user-visible content (session metadata, the completion marker).
/// Shared by the whole-conversation builder above and the live-streaming
/// harness runner, so both render events identically.
pub(crate) fn turn_event_to_messages(
    task_id: &str,
    request_id: &str,
    event: &TurnEvent,
) -> Vec<api::Message> {
    match event {
        TurnEvent::Text { text } => {
            vec![agent_output_message(task_id, request_id, text.clone())]
        }
        TurnEvent::Thinking { text } => {
            vec![agent_reasoning_message(task_id, request_id, text.clone())]
        }
        TurnEvent::ToolCallStarted { name, input, .. } => {
            vec![agent_output_message(
                task_id,
                request_id,
                format_tool_call(name, input),
            )]
        }
        TurnEvent::ToolCallResult { outcome, .. } => {
            vec![agent_output_message(
                task_id,
                request_id,
                format_tool_result(outcome),
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
                format!("**⚠ error**\n```\n{detail}\n```"),
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

/// Renders a tool invocation as a Markdown block: a bold header with the tool
/// name and, when recognizable, the single most salient argument (command,
/// path, or pattern); otherwise the full pretty-printed input.
fn format_tool_call(name: &str, input: &serde_json::Value) -> String {
    let salient = ["command", "file_path", "path", "pattern", "query", "url"]
        .iter()
        .find_map(|key| input.get(*key).and_then(|v| v.as_str()));

    match salient {
        Some(detail) => format!("**› {name}**\n```\n{detail}\n```"),
        None => {
            let pretty = serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
            format!("**› {name}**\n```json\n{pretty}\n```")
        }
    }
}

/// Renders a tool result as a Markdown block, preferring the plain `stdout`
/// field when present (Bash and friends) and truncating very long output.
fn format_tool_result(outcome: &ToolCallOutcome) -> String {
    match outcome {
        ToolCallOutcome::Success(value) => {
            let text = value
                .get("stdout")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| match value {
                    serde_json::Value::String(s) => s.clone(),
                    other => {
                        serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string())
                    }
                });
            format!("```\n{}\n```", truncate(text.trim_end()))
        }
        ToolCallOutcome::Error(message) => format!("**⚠ error**\n```\n{}\n```", truncate(message)),
        ToolCallOutcome::Cancelled => "_tool call cancelled_".to_string(),
    }
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
