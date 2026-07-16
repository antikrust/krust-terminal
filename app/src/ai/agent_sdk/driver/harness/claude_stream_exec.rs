//! Runs the `claude` CLI in headless streaming mode
//! (`--print --output-format stream-json`) as a plain subprocess — no PTY, no
//! interactive TUI — and drives each stdout line through
//! [`parse_stream_json_line`](super::claude_stream::parse_stream_json_line)
//! into provider-agnostic [`TurnEvent`]s delivered to a callback.
//!
//! This is the counterpart to the interactive invocation in
//! [`super::claude_code`]: instead of handing a shell string to the terminal
//! and letting Claude Code draw its own full-screen UI, we capture structured
//! output so Warp can render the conversation with its own components.

use std::process::{ExitStatus, Stdio};

use ai::agent::TurnEvent;
use command::r#async::Command;
use futures::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;
use warp_core::safe_warn;

use super::claude_stream::parse_stream_json_line;

/// Builds the argument vector for a headless streaming `claude` run.
///
/// Kept as a pure function (mirroring [`super::claude_code`]'s
/// `claude_command`) so the exact flags are unit-testable without spawning a
/// process. The prompt is delivered separately over stdin by
/// [`run_claude_stream`], not as an argument, to avoid shell-quoting issues
/// with large or structured prompts.
pub fn stream_json_args(
    session_id: &Uuid,
    resuming: bool,
    model: Option<&str>,
    system_prompt_path: Option<&str>,
    mcp_config_path: Option<&str>,
) -> Vec<String> {
    let session_flag = if resuming { "--resume" } else { "--session-id" };
    let mut args = vec![
        "--print".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        // stream-json is only permitted alongside --verbose in print mode.
        "--verbose".to_string(),
        session_flag.to_string(),
        session_id.to_string(),
        "--dangerously-skip-permissions".to_string(),
    ];
    if let Some(model) = model {
        args.push("--model".to_string());
        args.push(model.to_string());
    }
    if let Some(path) = system_prompt_path {
        args.push("--append-system-prompt-file".to_string());
        args.push(path.to_string());
    }
    if let Some(path) = mcp_config_path {
        args.push("--mcp-config".to_string());
        args.push(path.to_string());
    }
    args
}

/// Spawns `command`, writes `prompt` to its stdin, and streams its stdout,
/// invoking `on_event` for every [`TurnEvent`] parsed from each line as it
/// arrives. Resolves with the process's exit status once stdout reaches EOF.
///
/// `command` is expected to already carry the streaming args (from
/// [`stream_json_args`]), working directory, and environment; this function
/// only configures stdio. Taking a pre-built [`Command`] keeps the spawn/read
/// loop decoupled from Claude-specific flag construction and lets tests point
/// it at a stand-in program that emits canned ndjson.
pub async fn run_claude_stream(
    command: Command,
    prompt: &str,
    on_event: impl FnMut(TurnEvent),
) -> std::io::Result<ExitStatus> {
    run_jsonl_stream(command, prompt, parse_stream_json_line, on_event).await
}

/// Provider-agnostic core of the headless runners: spawns `command`, delivers
/// `prompt` over stdin, and pumps stdout line-by-line through `parse_line`.
/// Each harness pairs this with its own parser ([`run_claude_stream`],
/// [`super::codex_stream_exec::run_codex_stream`]).
pub async fn run_jsonl_stream(
    mut command: Command,
    prompt: &str,
    parse_line: impl Fn(&str) -> Vec<TurnEvent>,
    mut on_event: impl FnMut(TurnEvent),
) -> std::io::Result<ExitStatus> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn()?;

    // Deliver the prompt over stdin and close it so Claude sees EOF and starts
    // the turn. Prompts are at most a few KB, well within the pipe buffer, so
    // writing fully before reading stdout can't deadlock in practice.
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(prompt.as_bytes()).await?;
        stdin.flush().await?;
        // Dropping `stdin` closes the pipe (EOF for the child).
    }

    let stdout = child.stdout.take();
    // Drain stderr concurrently with stdout so a chatty child can't fill the
    // pipe and stall; it's the only diagnostic when the CLI dies before
    // emitting any stream-json.
    let stderr = child.stderr.take();

    let pump_stdout = async {
        let Some(stdout) = stdout else { return };
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break, // EOF
                Ok(_) => {
                    for event in parse_line(&line) {
                        on_event(event);
                    }
                }
                Err(error) => {
                    log::warn!("agent_stream_exec: error reading stdout: {error}");
                    break;
                }
            }
        }
    };

    let drain_stderr = async {
        let mut buffer = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut buffer).await;
        }
        buffer
    };

    let ((), stderr_output) = futures::join!(pump_stdout, drain_stderr);
    let status = child.status().await;

    let failed = status.as_ref().map(|s| !s.success()).unwrap_or(true);
    if failed {
        let exit_code = status.as_ref().ok().and_then(|s| s.code());
        log::warn!("agent_stream_exec: agent CLI exited unsuccessfully, exit_code={exit_code:?}");
        let stderr_output = stderr_output.trim();
        if !stderr_output.is_empty() {
            // Stderr can quote arbitrary prompt/config content, so the full
            // text is dogfood-only; keep it bounded either way.
            let tail: String = stderr_output
                .chars()
                .rev()
                .take(2000)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            safe_warn!(
                safe: ("agent_stream_exec: agent CLI wrote to stderr before exiting"),
                full: ("agent_stream_exec: agent CLI stderr tail: {tail}")
            );
        }
    }

    status
}

#[cfg(test)]
#[path = "claude_stream_exec_tests.rs"]
mod tests;
