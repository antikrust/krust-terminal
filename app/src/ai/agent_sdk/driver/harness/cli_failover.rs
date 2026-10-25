//! Runs a turn BYOR failover handed from the API runner to a local agent
//! app — the user's own `claude` or `codex` CLI, which needs no API key (see
//! `api_stream_exec::ApiTurnEnd::HandOff`).
//!
//! Each app starts a fresh session of its own with the recap as its prompt,
//! and its events go to the same conversation through the usual parsers. When
//! one fails before answering, the next stands in with the same recap; only
//! the last one's failure ends the turn.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;

use ai::agent::TurnEvent;
use command::r#async::Command;
use krust_cli::agent::Harness;
use uuid::Uuid;

use super::claude_code::prepare_claude_environment_config;
use super::claude_stream_exec::{run_claude_stream, stream_json_args};
use super::codex_stream_exec::{codex_exec_args, run_codex_stream};
use super::ollama_stream_exec::emit_failed_turn;
use super::LocalHarnessKind;
use crate::ai::api_providers::CliRoute;

/// Runs the handed-off turn on the first of `targets`, moving to the next
/// when one fails. Every event is reported with the app that produced it and
/// that app's harness.
pub(crate) async fn run_handed_off_turn(
    targets: Vec<CliRoute>,
    recap: &str,
    working_dir: &Path,
    path: &str,
    mut on_event: impl FnMut(&CliRoute, LocalHarnessKind, TurnEvent),
) -> std::io::Result<()> {
    let mut targets = targets.into_iter().peekable();
    while let Some(app) = targets.next() {
        let (kind, command) = match app.harness {
            Harness::Claude => {
                // So the headless run doesn't stop at first-run prompts.
                if let Err(error) = prepare_claude_environment_config(
                    working_dir,
                    &HashMap::<OsString, OsString>::new(),
                ) {
                    log::error!("cli_failover: failed to prepare claude env config: {error}");
                }
                let mut command = Command::new("claude");
                command.args(stream_json_args(
                    &Uuid::new_v4(),
                    false,
                    Some(&app.model),
                    None,
                    None,
                ));
                (LocalHarnessKind::Claude, command)
            }
            Harness::Codex => {
                let mut command = Command::new("codex");
                command.args(codex_exec_args(None, Some(&app.model), None));
                (LocalHarnessKind::Codex, command)
            }
            Harness::Oz | Harness::OpenCode | Harness::Gemini | Harness::Unknown => continue,
        };
        let mut command = command;
        command.current_dir(working_dir);
        command.env("PATH", path);

        let next = targets.peek().cloned();
        // An error result while another app stands by is held back: the
        // turn goes on there instead of ending.
        let mut failure: Option<String> = None;
        let mut completed = false;
        let status = {
            let forward = |event: TurnEvent| {
                if let TurnEvent::TurnCompleted {
                    is_error,
                    error_message,
                    ..
                } = &event
                {
                    if *is_error && next.is_some() {
                        failure = Some(error_message.clone().unwrap_or_default());
                        return;
                    }
                    completed = true;
                }
                on_event(&app, kind, event);
            };
            if kind == LocalHarnessKind::Codex {
                run_codex_stream(command, recap, forward).await
            } else {
                run_claude_stream(command, recap, forward).await
            }
        };
        if completed {
            return Ok(());
        }
        let failure = match (failure, status) {
            (Some(failure), _) => failure,
            (None, Ok(status)) if status.success() => {
                // Ended without a result, but cleanly: the drain completes the
                // turn at the end of the stream.
                return Ok(());
            }
            (None, Ok(status)) => format!("{} exited with {status}", app.app_name),
            (None, Err(error)) => format!("{} couldn't be started: {error}", app.app_name),
        };
        log::info!("cli_failover: {} failed ({failure})", app.model_id);
        match &next {
            Some(next) => {
                let shown: String = failure.chars().take(160).collect();
                on_event(
                    &app,
                    kind,
                    TurnEvent::Text {
                        text: format!(
                            "{} failed ({shown}) — switching to {} on {}.",
                            app.app_name, next.model_name, next.app_name
                        ),
                    },
                );
            }
            None => {
                emit_failed_turn(&mut |event: TurnEvent| on_event(&app, kind, event), failure);
                return Ok(());
            }
        }
    }
    Ok(())
}
