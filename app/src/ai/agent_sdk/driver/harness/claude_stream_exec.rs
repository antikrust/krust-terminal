//! Runs the `claude` CLI in headless streaming mode
//! (`--print --output-format stream-json`) as a plain subprocess — no PTY, no
//! interactive TUI — and drives each stdout line through
//! [`parse_stream_json_line`](super::claude_stream::parse_stream_json_line)
//! into provider-agnostic [`TurnEvent`]s delivered to a callback.
//!
//! This is the counterpart to the interactive invocation in
//! [`super::claude_code`]: instead of handing a shell string to the terminal
//! and letting Claude Code draw its own full-screen UI, we capture structured
//! output so Krust can render the conversation with its own components.

use std::collections::HashMap;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use ai::agent::TurnEvent;
use krustui::r#async::Timer;
use command::r#async::Command;
use futures::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;
use krust_core::safe_warn;

use super::claude_stream::parse_stream_json_line;
use super::plan_tracker::PlanTracker;


// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

/// A stop signal for one harness session.
///
/// Cancelling an agent has to stop the *work*, not just the UI. Marking a
/// conversation cancelled while the model keeps generating means the answer
/// still lands, in a conversation the user already abandoned — and on a local
/// model it also keeps a GPU busy for nothing.
#[derive(Clone, Default)]
pub struct HarnessCancel(Arc<AtomicBool>);

impl HarnessCancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Live harness sessions, so a stop arriving from the UI can find the run it
/// belongs to. Keyed by session id, which is what the driver already threads
/// through every harness call.
fn sessions() -> &'static Mutex<HashMap<Uuid, HarnessCancel>> {
    static SESSIONS: OnceLock<Mutex<HashMap<Uuid, HarnessCancel>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Registers a session and returns its stop signal. Replacing any previous
/// entry is deliberate: a new turn supersedes whatever the last one left.
pub fn register_harness_session(session_id: Uuid) -> HarnessCancel {
    let cancel = HarnessCancel::default();
    if let Ok(mut sessions) = sessions().lock() {
        sessions.insert(session_id, cancel.clone());
    }
    cancel
}

/// Drops a finished session so the map does not grow for the life of the app.
pub fn finish_harness_session(session_id: &Uuid) {
    if let Ok(mut sessions) = sessions().lock() {
        sessions.remove(session_id);
    }
}

/// The stop signal for a live session, if it has one.
pub fn harness_cancel_for(session_id: &Uuid) -> Option<HarnessCancel> {
    sessions().lock().ok()?.get(session_id).cloned()
}

/// Signals a running harness turn to stop. Returns whether one was listening.
pub fn cancel_harness_session(session_id: &Uuid) -> bool {
    let Ok(sessions) = sessions().lock() else {
        return false;
    };
    match sessions.get(session_id) {
        Some(cancel) => {
            cancel.cancel();
            true
        }
        None => false,
    }
}

/// Resolves once `cancel` is raised. Polled rather than pushed because the
/// flag is set from a UI thread with no runtime handle; 50ms is well under
/// the threshold where a stop feels sluggish.
pub(super) async fn cancelled(cancel: HarnessCancel) {
    loop {
        if cancel.is_cancelled() {
            return;
        }
        Timer::after(Duration::from_millis(50)).await;
    }
}

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
    mut on_event: impl FnMut(TurnEvent),
) -> std::io::Result<ExitStatus> {
    // `TodoWrite` plans arrive whole; the tracker turns them into checklist
    // changes for this turn.
    let mut plan = PlanTracker::default();
    run_jsonl_stream(command, prompt, parse_stream_json_line, |event| {
        plan.forward(event, &mut on_event)
    })
    .await
}

/// Provider-agnostic core of the headless runners: spawns `command`, delivers
/// `prompt` over stdin, and pumps stdout line-by-line through `parse_line`.
/// Each harness pairs this with its own parser ([`run_claude_stream`],
/// [`super::codex_stream_exec::run_codex_stream`]).
pub async fn run_jsonl_stream(
    command: Command,
    prompt: &str,
    parse_line: impl Fn(&str) -> Vec<TurnEvent>,
    on_event: impl FnMut(TurnEvent),
) -> std::io::Result<ExitStatus> {
    run_jsonl_stream_cancellable(command, prompt, None, parse_line, on_event).await
}

/// As [`run_jsonl_stream`], but a raised `cancel` kills the child mid-turn.
///
/// Killing is the point: the model is a separate process (and, for a local
/// model, a busy GPU). Letting the future drop would eventually reap it, but
/// only once something drops it — so the stop is issued here, explicitly, the
/// moment the flag is seen.
pub async fn run_jsonl_stream_cancellable(
    mut command: Command,
    prompt: &str,
    cancel: Option<HarnessCancel>,
    parse_line: impl Fn(&str) -> Vec<TurnEvent>,
    mut on_event: impl FnMut(TurnEvent),
) -> std::io::Result<ExitStatus> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Cancelling a turn drops this future, and without `kill_on_drop` that leaves the
        // child running: the model carries on generating and answers into a conversation
        // the user already stopped. Stopping the UI has to stop the work.
        .kill_on_drop(true);

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

    let pumps = async {
        let ((), stderr_output) = futures::join!(pump_stdout, drain_stderr);
        stderr_output
    };

    let stderr_output = match cancel {
        Some(cancel) => {
            futures::pin_mut!(pumps);
            match futures::future::select(pumps, Box::pin(cancelled(cancel))).await {
                futures::future::Either::Left((stderr_output, _)) => stderr_output,
                futures::future::Either::Right(((), _)) => {
                    // Stop now rather than waiting for the stream to end on its own.
                    log::info!("agent_stream_exec: cancelled, killing the model process");
                    let _ = child.kill();
                    String::new()
                }
            }
        }
        None => pumps.await,
    };

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
