//! Runs the `codex` CLI headless (`codex exec --json`) as a plain subprocess
//! and drives each stdout line through
//! [`parse_codex_jsonl_line`](super::codex_stream::parse_codex_jsonl_line)
//! into provider-agnostic [`TurnEvent`]s — the codex counterpart to
//! [`super::claude_stream_exec`], sharing its spawn/pump loop.

use std::process::ExitStatus;

use ai::agent::TurnEvent;
use command::r#async::Command;
use uuid::Uuid;

use super::claude_stream_exec::run_jsonl_stream;
use super::codex_stream::parse_codex_jsonl_line;
use super::plan_tracker::PlanTracker;

/// Builds the argument vector for a headless streaming `codex exec` run.
///
/// A fresh run lets codex mint the thread server-side (the id arrives in the
/// `thread.started` event); follow-ups resume by thread id. The prompt is
/// delivered over stdin (the trailing `-`), mirroring the claude runner, to
/// avoid arg-length and quoting issues.
///
/// `--dangerously-bypass-approvals-and-sandbox` is deliberate parity with the
/// claude runner's `--dangerously-skip-permissions`: this fork treats the
/// local agent CLI as fully trusted in the user's own terminal.
pub fn codex_exec_args(
    resume_thread_id: Option<&Uuid>,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
) -> Vec<String> {
    let mut args = vec!["exec".to_string()];
    if let Some(thread_id) = resume_thread_id {
        args.push("resume".to_string());
        args.push(thread_id.to_string());
    }
    args.extend([
        "--json".to_string(),
        "--skip-git-repo-check".to_string(),
        "--dangerously-bypass-approvals-and-sandbox".to_string(),
    ]);
    if let Some(model) = model {
        args.push("--model".to_string());
        args.push(model.to_string());
    }
    if let Some(effort) = reasoning_effort {
        args.push("-c".to_string());
        args.push(format!("model_reasoning_effort={effort}"));
    }
    args.push("-".to_string());
    args
}

/// Spawns `command` (expected to carry [`codex_exec_args`], working directory,
/// and environment), writes `prompt` to its stdin, and streams parsed
/// [`TurnEvent`]s to `on_event`. Resolves with the exit status at EOF.
pub async fn run_codex_stream(
    command: Command,
    prompt: &str,
    mut on_event: impl FnMut(TurnEvent),
) -> std::io::Result<ExitStatus> {
    // `todo_list` plans arrive whole; the tracker turns them into checklist
    // changes for this turn.
    let mut plan = PlanTracker::default();
    run_jsonl_stream(command, prompt, parse_codex_jsonl_line, |event| {
        plan.forward(event, &mut on_event)
    })
    .await
}

#[cfg(test)]
#[path = "codex_stream_exec_tests.rs"]
mod tests;
