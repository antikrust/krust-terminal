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
use futures::channel::mpsc;
use futures::future::Either;
use futures::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use futures::StreamExt as _;
use serde::Deserialize;
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

/// Makes `cancel` reachable under `session_id` as well: the id a CLI reports
/// for its run, when that is not the one the run was started under.
pub fn alias_harness_session(session_id: Uuid, cancel: &HarnessCancel) {
    if let Ok(mut sessions) = sessions().lock() {
        sessions.insert(session_id, cancel.clone());
    }
}

/// Drops every key `cancel` is registered under, once its run has ended. A
/// newer run registered under the same session keeps its own entry.
pub fn finish_harness_run(cancel: &HarnessCancel) {
    if let Ok(mut sessions) = sessions().lock() {
        sessions.retain(|_, registered| !Arc::ptr_eq(&registered.0, &cancel.0));
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

// ---------------------------------------------------------------------------
// Prompts sent to a run that is already going
// ---------------------------------------------------------------------------

/// A prompt sent to a run that is already going.
pub struct RunPrompt {
    /// The prompt as typed, which is what the conversation shows.
    pub prompt: String,
    /// What the runner is sent: the prompt, with the paths of its images.
    pub runner_prompt: String,
    /// The ID of the exchange the prompt begins.
    pub turn_id: String,
    pub image_paths: Vec<String>,
}

/// One run's prompts: those sent while it works, waiting for its loop to
/// reach the end of a step and read them.
struct Steering {
    /// Tells this run's entry from a later run's on the same session.
    key: Arc<()>,
    /// Whether the run's loop reads prompts. A runner that never asks for
    /// them (one model call and done, or a CLI that takes no input) leaves
    /// this unset, and a prompt for it starts a run of its own.
    taking: bool,
    queued: Vec<RunPrompt>,
    /// Told of each prompt as the loop takes it, so the conversation shows it
    /// where the model read it.
    taken: mpsc::UnboundedSender<RunPrompt>,
}

fn steering() -> &'static Mutex<HashMap<Uuid, Steering>> {
    static STEERING: OnceLock<Mutex<HashMap<Uuid, Steering>>> = OnceLock::new();
    STEERING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A run's place in the steering table; closing it (or dropping it) ends the
/// run's taking of prompts.
pub struct SteeringWatch {
    session_id: Uuid,
    key: Arc<()>,
}

impl SteeringWatch {
    /// Stops the run taking prompts. Any it never read are still reported as
    /// taken, so the conversation shows them rather than losing them.
    pub fn close(&self) {
        let Ok(mut sessions) = steering().lock() else {
            return;
        };
        if sessions
            .get(&self.session_id)
            .is_some_and(|steering| Arc::ptr_eq(&steering.key, &self.key))
        {
            if let Some(steering) = sessions.remove(&self.session_id) {
                for prompt in steering.queued {
                    let _ = steering.taken.unbounded_send(prompt);
                }
            }
        }
    }
}

impl Drop for SteeringWatch {
    fn drop(&mut self) {
        self.close();
    }
}

/// Lets the run about to start on `session_id` be sent prompts while it
/// works, if its loop turns out to read them ([`take_steered_prompts`]).
/// Returns the run's place in the table, to close when it ends, and the
/// prompts as the loop takes them.
pub fn watch_steering(session_id: Uuid) -> (SteeringWatch, mpsc::UnboundedReceiver<RunPrompt>) {
    let (taken, prompts) = mpsc::unbounded();
    let key = Arc::new(());
    if let Ok(mut sessions) = steering().lock() {
        sessions.insert(
            session_id,
            Steering {
                key: key.clone(),
                taking: false,
                queued: Vec::new(),
                taken,
            },
        );
    }
    (SteeringWatch { session_id, key }, prompts)
}

/// Hands `prompt` to the run going on `session_id`, which reads it at the end
/// of its current step. False when no run there reads prompts (or it has just
/// finished): the prompt needs a run of its own.
pub fn steer_harness_session(session_id: &Uuid, prompt: RunPrompt) -> bool {
    let Ok(mut sessions) = steering().lock() else {
        return false;
    };
    match sessions.get_mut(session_id) {
        Some(steering) if steering.taking => {
            steering.queued.push(prompt);
            true
        }
        _ => false,
    }
}

/// For a run's loop, between two steps: the prompts sent to the run since the
/// last, as the text to give the model. Asking is what marks the run as one
/// that reads prompts.
pub(super) fn take_steered_prompts(session_id: &Uuid) -> Vec<String> {
    let Ok(mut sessions) = steering().lock() else {
        return Vec::new();
    };
    let Some(Steering {
        taking,
        queued,
        taken,
        ..
    }) = sessions.get_mut(session_id)
    else {
        return Vec::new();
    };
    *taking = true;
    queued
        .drain(..)
        .map(|prompt| {
            let text = prompt.runner_prompt.clone();
            let _ = taken.unbounded_send(prompt);
            text
        })
        .collect()
}

/// For a run's loop, as the model gives its answer: whether the run may end
/// there. False when prompts are waiting, which the loop takes and goes on
/// with. Decided under the lock [`steer_harness_session`] takes, so a prompt
/// is either waiting here or refused after this.
pub(super) fn steering_finished(session_id: &Uuid) -> bool {
    let Ok(mut sessions) = steering().lock() else {
        return true;
    };
    match sessions.get_mut(session_id) {
        Some(steering) if !steering.queued.is_empty() => false,
        Some(steering) => {
            steering.taking = false;
            true
        }
        None => true,
    }
}

/// For a runner whose loop turns out not to read prompts after all (it has
/// handed the turn to one that doesn't): none are accepted from here.
pub(super) fn refuse_steering(session_id: &Uuid) {
    if let Ok(mut sessions) = steering().lock() {
        if let Some(steering) = sessions.get_mut(session_id) {
            steering.taking = false;
        }
    }
}

// ---------------------------------------------------------------------------
// A Claude Code process that takes prompts while it works
// ---------------------------------------------------------------------------

/// How long a Claude Code process with nothing left to do is kept before its
/// input is closed. A prompt the CLI had queued begins its turn well inside
/// this, so the process is not written off between two turns.
const CLAUDE_IDLE_GRACE: Duration = Duration::from_millis(1500);

struct ClaudeInputState<P> {
    /// Where prompts go; `None` once the process takes no more.
    sender: Option<mpsc::UnboundedSender<P>>,
    /// Whether the main agent is in a turn, or has a prompt it has not begun.
    in_turn: bool,
    /// How many background tasks (agents, shell commands) the process runs.
    background_tasks: usize,
}

/// The way into a running Claude Code process: a prompt sent while it works
/// goes to that process, which queues it and reads it at its next step.
///
/// Starting a second process on the same session instead leaves two at work
/// in one conversation, and the second takes the background agents the first
/// started for dead and starts them again.
pub struct ClaudeInput<P>(Arc<Mutex<ClaudeInputState<P>>>);

impl<P> Clone for ClaudeInput<P> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<P> ClaudeInput<P> {
    /// The input of a process about to start on its first prompt, and the
    /// prompts sent to it after that.
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<P>) {
        let (sender, prompts) = mpsc::unbounded();
        let state = ClaudeInputState {
            sender: Some(sender),
            in_turn: true,
            background_tasks: 0,
        };
        (Self(Arc::new(Mutex::new(state))), prompts)
    }

    /// Hands `prompt` to the process. False once the process has finished its
    /// work and takes no more: the prompt needs a run of its own.
    pub fn send(&self, prompt: P) -> bool {
        let Ok(mut state) = self.0.lock() else {
            return false;
        };
        let sent = state
            .sender
            .as_ref()
            .is_some_and(|sender| sender.unbounded_send(prompt).is_ok());
        if sent {
            state.in_turn = true;
        }
        sent
    }

    /// Records what a line of the process's output says about its work, and
    /// returns whether that line may have left it with none.
    fn note(&self, turn_began: bool, turn_ended: bool, background_tasks: Option<usize>) -> bool {
        let Ok(mut state) = self.0.lock() else {
            return false;
        };
        if turn_ended {
            state.in_turn = false;
        }
        if turn_began {
            state.in_turn = true;
        }
        if let Some(background_tasks) = background_tasks {
            state.background_tasks = background_tasks;
        }
        (turn_ended || background_tasks.is_some())
            && !state.in_turn
            && state.background_tasks == 0
    }

    /// Stops taking prompts if the process has no work left, and returns
    /// whether it did. Decided under the same lock [`Self::send`] takes, so a
    /// prompt is either taken before this or refused after it.
    fn close_if_idle(&self) -> bool {
        let Ok(mut state) = self.0.lock() else {
            return true;
        };
        let idle = !state.in_turn && state.background_tasks == 0;
        if idle {
            state.sender = None;
        }
        idle
    }

    fn close(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.sender = None;
        }
    }
}

/// One prompt as a line of the CLI's `--input-format stream-json` input.
fn user_message_line(prompt: &str) -> String {
    let message = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{ "type": "text", "text": prompt }],
        },
    });
    format!("{message}\n")
}

/// How many background tasks a `system` / `background_tasks_changed` line
/// lists (it names every one still running); `None` for any other line.
fn background_task_count(line: &str) -> Option<usize> {
    #[derive(Deserialize)]
    struct Line {
        subtype: Option<String>,
        #[serde(default)]
        tasks: Vec<serde::de::IgnoredAny>,
    }
    // Lines can be megabytes of tool output; only the one kind is parsed.
    if !line.contains("\"background_tasks_changed\"") {
        return None;
    }
    let line: Line = serde_json::from_str(line.trim()).ok()?;
    (line.subtype.as_deref() == Some("background_tasks_changed")).then_some(line.tasks.len())
}

/// Runs `command` (a `claude` invocation carrying [`stream_json_args`]) as a
/// process that outlives its first turn: it is given `prompt`, then every
/// prompt sent to `input` while it works, and it is kept for as long as it
/// has work — a turn of the main agent, or background agents and commands,
/// whose completion wakes the main agent for another turn. Its input is
/// closed once it has none, and it exits.
///
/// `on_prompt` is called as each later prompt is handed over, and returns the
/// text the CLI is sent for it. Raising `cancel` kills the process.
///
/// [`run_claude_stream`] instead closes the input after the one prompt. The
/// CLI then still waits for its background tasks, but only for ten minutes,
/// and nothing can be sent to it meanwhile.
pub async fn run_claude_session<P>(
    mut command: Command,
    prompt: &str,
    input: ClaudeInput<P>,
    mut prompts: mpsc::UnboundedReceiver<P>,
    cancel: HarnessCancel,
    mut on_prompt: impl FnMut(P) -> String,
    mut on_event: impl FnMut(TurnEvent),
) -> std::io::Result<ExitStatus> {
    command.args(["--input-format", "stream-json"]);
    // Print mode stops background tasks still running ten minutes after the
    // main agent's last turn, which cuts a background agent off mid-work.
    command.env("CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS", "0");
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command.spawn()?;

    let mut stdin = child.stdin.take();
    if let Some(stdin) = stdin.as_mut() {
        stdin.write_all(user_message_line(prompt).as_bytes()).await?;
        stdin.flush().await?;
    }
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Told each time the process may have run out of work.
    let (idle_tx, mut idle_rx) = mpsc::unbounded::<()>();

    let pump_stdout = async {
        // Moved in, so it is dropped when the output ends and the feed below
        // sees the end too.
        let idle_tx = idle_tx;
        let Some(stdout) = stdout else { return };
        // `TodoWrite` plans arrive whole; the tracker turns them into
        // checklist changes, a turn at a time.
        let mut plan = PlanTracker::default();
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break, // EOF
                Ok(_) => {
                    let background_tasks = background_task_count(&line);
                    let events = parse_stream_json_line(&line);
                    let turn_began = events
                        .iter()
                        .any(|event| matches!(event, TurnEvent::SessionInit { .. }));
                    let turn_ended = events
                        .iter()
                        .any(|event| matches!(event, TurnEvent::TurnCompleted { .. }));
                    if turn_began {
                        plan = PlanTracker::default();
                    }
                    for event in events {
                        plan.forward(event, &mut on_event);
                    }
                    if input.note(turn_began, turn_ended, background_tasks) {
                        let _ = idle_tx.unbounded_send(());
                    }
                }
                Err(error) => {
                    log::warn!("agent_stream_exec: error reading stdout: {error}");
                    break;
                }
            }
        }
    };

    let feed_stdin = async {
        let Some(mut stdin) = stdin else { return };
        loop {
            match futures::future::select(prompts.next(), idle_rx.next()).await {
                Either::Left((Some(prompt), _)) => {
                    let line = user_message_line(&on_prompt(prompt));
                    if stdin.write_all(line.as_bytes()).await.is_err()
                        || stdin.flush().await.is_err()
                    {
                        break;
                    }
                }
                Either::Right((Some(()), _)) => {
                    Timer::after(CLAUDE_IDLE_GRACE).await;
                    if input.close_if_idle() {
                        log::info!("agent_stream_exec: Claude Code has no work left, closing its input");
                        break;
                    }
                }
                // No one can send any more, or the process's output ended.
                Either::Left((None, _)) | Either::Right((None, _)) => break,
            }
        }
        input.close();
        // Dropping `stdin` closes the pipe: the process finishes what it has
        // and exits.
    };

    let drain_stderr = async {
        let mut buffer = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut buffer).await;
        }
        buffer
    };

    let pumps = async {
        let ((), (), stderr_output) = futures::join!(pump_stdout, feed_stdin, drain_stderr);
        stderr_output
    };
    futures::pin_mut!(pumps);
    let stderr_output = match futures::future::select(pumps, Box::pin(cancelled(cancel))).await {
        Either::Left((stderr_output, _)) => stderr_output,
        Either::Right(((), _)) => {
            log::info!("agent_stream_exec: cancelled, killing the Claude Code process");
            let _ = child.kill();
            String::new()
        }
    };
    input.close();

    let status = child.status().await;
    report_failed_exit(&status, &stderr_output);
    status
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
    report_failed_exit(&status, &stderr_output);
    status
}

/// Logs an agent CLI's unsuccessful exit, with the tail of what it wrote to
/// stderr: the only diagnostic when it dies before emitting any output.
fn report_failed_exit(status: &std::io::Result<ExitStatus>, stderr_output: &str) {
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
}

#[cfg(test)]
#[path = "claude_stream_exec_tests.rs"]
mod tests;
