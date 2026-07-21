//! Streaming Claude Code harness: runs `claude` headless in
//! `--output-format stream-json` mode and renders its output as live inline AI
//! blocks in Warp's own UI, instead of handing the terminal to Claude Code's
//! full-screen interactive TUI (which is what [`super::claude_code`] does).
//!
//! Pipeline: [`super::claude_stream_exec::run_claude_stream`] pumps the CLI's
//! stdout into [`ai::agent::TurnEvent`]s on a background task; those are funneled
//! to the model thread and injected incrementally into `BlocklistAIHistoryModel`
//! so one exchange updates in place as the agent works.
//!
//! This is the live target of `harness_kind(Harness::Claude)` (see `mod.rs`):
//! within the `AgentDriver` path, `build_runner` + `start` spawn `claude`
//! headless and render its output inline via [`LocalHarnessStream`]. This
//! runner is reached through the `warp agent run` CLI subcommand; *terminal
//! agent input* takes a parallel route (`TerminalView::run_local_claude_query`)
//! that drives `run_claude_stream` + `LocalHarnessStream` directly, without an
//! `AgentDriver`. See `docs/agent-flow-map.md` for the full routing map.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ai::agent::TurnEvent;
use anyhow::Result;
use async_trait::async_trait;
use command::r#async::Command;
use futures::channel::{mpsc, oneshot};
use futures::StreamExt as _;
use parking_lot::Mutex;
use uuid::Uuid;
use warp_cli::agent::Harness;
use warp_core::command::ExitCode;
use warp_managed_secrets::ManagedSecretValue;
use warpui::{AppContext, ModelContext, ModelHandle, ModelSpawner};

use super::super::terminal::{CommandHandle, TerminalDriver};
use super::super::{AgentDriver, AgentDriverError};
use super::claude_code::prepare_claude_environment_config;
use super::claude_stream_exec::{run_claude_stream, stream_json_args};
use super::{
    HarnessCleanupDisposition, HarnessRunner, JSONMCPServer, ResumePayload, SavePoint,
    ThirdPartyHarness,
};
use crate::ai::agent_sdk::setup_observability::SetupClientEventReporter;
use crate::ai::ambient_agents::task::HarnessModelConfig;
use crate::ai::ambient_agents::AmbientAgentTaskId;
use crate::ai::blocklist::local_harness_stream::LocalHarnessStream;
use crate::server::server_api::ServerApi;
use crate::terminal::model::block::BlockId;
use crate::terminal::CLIAgent;

pub(crate) struct ClaudeStreamHarness;

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
impl ThirdPartyHarness for ClaudeStreamHarness {
    fn harness(&self) -> Harness {
        Harness::Claude
    }

    fn cli_agent(&self) -> CLIAgent {
        CLIAgent::Claude
    }

    fn install_docs_url(&self) -> Option<&'static str> {
        Some("https://code.claude.com/docs/en/quickstart")
    }

    fn auth_check_command(&self) -> Option<String> {
        let cli = self.cli_agent().command_prefix();
        Some(format!("{cli} auth status --json"))
    }

    #[allow(clippy::too_many_arguments)]
    fn build_runner(
        &self,
        prompt: &str,
        _system_prompt: Option<&str>,
        resumption_prompt: Option<&str>,
        context: Option<&str>,
        working_dir: &Path,
        task_id: Option<AmbientAgentTaskId>,
        server_api: Arc<ServerApi>,
        terminal_driver: ModelHandle<TerminalDriver>,
        _resume: Option<ResumePayload>,
        resolved_env_vars: &HashMap<OsString, OsString>,
        _resolved_secrets: &HashMap<String, ManagedSecretValue>,
        _resolved_mcp_servers: &HashMap<String, JSONMCPServer>,
        _third_party_harness_model_config: Option<&HarnessModelConfig>,
    ) -> Result<Box<dyn HarnessRunner>, AgentDriverError> {
        // Reuse Claude Code's on-disk environment/config setup so the streaming
        // run behaves like the interactive one (same `.claude` settings).
        prepare_claude_environment_config(working_dir, resolved_env_vars).map_err(|error| {
            AgentDriverError::HarnessConfigSetupFailed {
                harness: self.cli_agent().command_prefix().to_owned(),
                error,
            }
        })?;

        // Order matches the interactive harness: resumption preamble → server
        // context → user prompt, joined and delivered on stdin.
        let mut parts: Vec<&str> = Vec::new();
        if let Some(preamble) = resumption_prompt.filter(|p| !p.is_empty()) {
            parts.push(preamble);
        }
        if let Some(ctx) = context.filter(|c| !c.is_empty()) {
            parts.push(ctx);
        }
        parts.push(prompt);

        Ok(Box::new(ClaudeStreamHarnessRunner {
            cli_name: self.cli_agent().command_prefix().to_string(),
            prompt: parts.join("\n\n"),
            working_dir: working_dir.to_path_buf(),
            session_id: Uuid::new_v4(),
            env_vars: resolved_env_vars.clone(),
            task_id,
            server_api,
            terminal_driver,
            events: Arc::new(Mutex::new(Vec::new())),
        }))
    }

    fn requires_verified_platform_plugin(&self) -> bool {
        // The streaming harness parses the CLI's stdout directly, so it doesn't
        // depend on the notification-hook plugin (whose install/verify path is
        // server-backed and would fail under this fork's faked auth).
        false
    }
}

/// Per-run state for a streaming Claude Code invocation.
struct ClaudeStreamHarnessRunner {
    cli_name: String,
    prompt: String,
    working_dir: PathBuf,
    /// Client-minted session id, passed to `claude --session-id` so resumes and
    /// transcript lookups can find this run.
    session_id: Uuid,
    env_vars: HashMap<OsString, OsString>,
    #[allow(dead_code)]
    task_id: Option<AmbientAgentTaskId>,
    #[allow(dead_code)]
    server_api: Arc<ServerApi>,
    #[allow(dead_code)]
    terminal_driver: ModelHandle<TerminalDriver>,
    /// Accumulated events, retained for periodic saves and end-of-run
    /// transcript handling.
    events: Arc<Mutex<Vec<TurnEvent>>>,
}

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
impl HarnessRunner for ClaudeStreamHarnessRunner {
    fn harness_name(&self) -> &str {
        &self.cli_name
    }

    async fn start(
        &self,
        foreground: &ModelSpawner<AgentDriver>,
        _setup_events: &SetupClientEventReporter,
    ) -> Result<CommandHandle, AgentDriverError> {
        // Build the headless streaming invocation. The prompt is delivered on
        // stdin by `run_claude_stream`, not as an argument.
        let mut command = Command::new(&self.cli_name);
        command.args(stream_json_args(&self.session_id, false, None, None, None));
        command.current_dir(&self.working_dir);
        for (key, value) in &self.env_vars {
            command.env(key, value);
        }
        // Spawning `claude` directly (not through a shell) means the subprocess
        // inherits the app's PATH — which is launchd's minimal PATH when the app
        // is launched from Finder/Dock, and may not contain `claude`. Build a
        // robust PATH: the interactive-shell PATH Warp captured (if any), else
        // the process PATH, then ensure the common CLI install dirs are present.
        let mut path = warp_util::path::warp_shell_path()
            .or_else(|| std::env::var("PATH").ok())
            .unwrap_or_default();
        let mut extra_dirs = vec![
            "/opt/homebrew/bin".to_string(),
            "/usr/local/bin".to_string(),
        ];
        if let Some(home) = std::env::var_os("HOME") {
            let home = home.to_string_lossy();
            extra_dirs.push(format!("{home}/.local/bin"));
            extra_dirs.push(format!("{home}/.npm-global/bin"));
        }
        for dir in extra_dirs {
            if !path.split(':').any(|segment| segment == dir) {
                path = if path.is_empty() {
                    dir
                } else {
                    format!("{dir}:{path}")
                };
            }
        }
        command.env("PATH", path);

        // The driver's error scanner / snapshot bookkeeping wants a block id;
        // this streaming harness renders its own AI blocks rather than a PTY
        // block, so mint a standalone one.
        let block_id = BlockId::new();

        let (exit_tx, exit_rx) = oneshot::channel::<ExitCode>();
        let (event_tx, mut event_rx) = mpsc::unbounded::<TurnEvent>();

        // The terminal view (surface) whose conversation the blocks render into.
        let terminal_driver = self.terminal_driver.clone();
        let terminal_surface_id = foreground
            .spawn(move |_, ctx| terminal_driver.as_ref(ctx).terminal_view().id())
            .await
            .map_err(|_| AgentDriverError::InvalidRuntimeState)?;

        let drain_spawner = foreground.clone();
        let begin_prompt = self.prompt.clone();
        let pump_prompt = self.prompt.clone();
        let events = self.events.clone();

        // Obtain the background executor on the model thread, then launch the
        // stdout pump and the render drain on it.
        foreground
            .spawn(move |_, ctx| {
                let background = ctx.background_executor();

                // Drain: begin the exchange on the first event, inject each
                // event's messages, and complete at EOF. Every model mutation
                // hops back to the model thread via `drain_spawner.spawn`; the
                // `LocalHarnessStream` is threaded through each closure so the
                // whole run updates one exchange in place.
                background
                    .spawn(async move {
                        let mut stream: Option<LocalHarnessStream> = None;
                        // Correlates shell tool calls with their results across
                        // events; threaded through each closure like `stream`.
                        let mut shell_commands = std::collections::HashMap::new();
                        while let Some(event) = event_rx.next().await {
                            let taken = stream.take();
                            let taken_shell_commands = std::mem::take(&mut shell_commands);
                            let begin_prompt = begin_prompt.clone();
                            let result = drain_spawner
                                .spawn(move |_, ctx: &mut ModelContext<AgentDriver>| {
                                    let app: &mut AppContext = ctx;
                                    let mut stream = taken.unwrap_or_else(|| {
                                        LocalHarnessStream::begin(
                                            terminal_surface_id,
                                            &begin_prompt,
                                            app,
                                        )
                                    });
                                    let mut shell_commands = taken_shell_commands;
                                    // Streamed reasoning grows one stable
                                    // "Thinking" section instead of a
                                    // message per delta.
                                    if let TurnEvent::Thinking { text } = &event {
                                        stream.apply_thinking(text, app);
                                    } else {
                                        let messages = stream
                                            .messages_for_event(&event, &mut shell_commands);
                                        stream.apply_messages(messages, app);
                                    }
                                    (stream, shell_commands)
                                })
                                .await;
                            if let Ok((returned_stream, returned_shell_commands)) = result {
                                stream = Some(returned_stream);
                                shell_commands = returned_shell_commands;
                            }
                        }
                        if let Some(stream) = stream {
                            let _ = drain_spawner
                                .spawn(move |_, ctx: &mut ModelContext<AgentDriver>| {
                                    stream.complete(ctx);
                                })
                                .await;
                        }
                    })
                    .detach();

                // Pump: run claude headless, forwarding each parsed event to the
                // drain (and retaining it for saves), then report the exit code.
                background
                    .spawn(async move {
                        let status = run_claude_stream(command, &pump_prompt, move |event| {
                            events.lock().push(event.clone());
                            let _ = event_tx.unbounded_send(event);
                        })
                        .await;
                        let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
                        let _ = exit_tx.send(ExitCode::from(code));
                        // Dropping `event_tx` ends the drain loop, which then
                        // marks the exchange complete.
                    })
                    .detach();
            })
            .await
            .map_err(|_| AgentDriverError::InvalidRuntimeState)?;

        Ok(CommandHandle::new(exit_rx, block_id))
    }

    async fn save_conversation(
        &self,
        _save_point: SavePoint,
        _foreground: &ModelSpawner<AgentDriver>,
    ) -> Result<()> {
        Ok(())
    }

    async fn exit(&self, _foreground: &ModelSpawner<AgentDriver>) -> Result<()> {
        Ok(())
    }

    async fn cleanup(
        &self,
        _cleanup_disposition: HarnessCleanupDisposition,
        _foreground: &ModelSpawner<AgentDriver>,
    ) -> Result<()> {
        Ok(())
    }
}
