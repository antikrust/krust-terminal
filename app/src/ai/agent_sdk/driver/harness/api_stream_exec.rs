//! Runs a cloud model over the OpenAI chat-completions API — Z.ai's GLM,
//! Groq, Gemini (see `ai::api_providers`) — with the same built-in file/code
//! tools and exposed MCP servers the Ollama runner gives tool-capable local
//! models.
//!
//! The wire is curl, as for Ollama, so a turn stays off the Tokio reactor:
//! the request body goes over stdin and the streamed reply (server-sent
//! events) is parsed line by line into `TurnEvent`s. The API key reaches curl
//! through its environment (`--variable` + `--expand-header`, curl 8.3+),
//! never its arguments, so it is not visible in `ps` and never touches disk.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex, OnceLock};

use ai::agent::{PlanUpdate, ToolCallOutcome, TurnEvent, TurnUsage};
use command::r#async::Command;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::claude_stream_exec::{
    finish_harness_session, harness_cancel_for, register_harness_session,
    run_jsonl_stream_cancellable,
};
use super::ollama_stream_exec::{
    builtin_local_tool_specs, dispatch_builtin_tool_with_read_window, emit_failed_turn,
    is_builtin_local_tool, ollama_tool_system_prompt, parse_text_tool_call,
    remove_turn_from_messages, todo_list_entries, todo_list_reply, turn_user_message,
    without_turn_ids, McpToolCaller, McpToolSpec, BUILTIN_TODO_LIST,
};
use super::plan_tracker::PlanTracker;
use crate::ai::api_providers::{
    model_context_window, model_price, ApiRoute, CliRoute, FailoverTarget,
};
use crate::ai::failover_health::{self, Health};

/// The environment variable curl reads the key from.
pub const API_KEY_ENV: &str = "KRUST_API_KEY";

/// The header curl expands from [`API_KEY_ENV`]. A literal rather than a
/// `format!` because the `{{…}}` is curl's syntax, not Rust's.
const AUTHORIZATION_HEADER: &str = "Authorization: Bearer {{KRUST_API_KEY}}";

/// The window assumed for a model whose context window isn't listed.
const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;

/// The least and the most one `read_file` call returns to a cloud model,
/// whatever its window.
const MIN_READ_CHARS: usize = 48_000;
const MAX_READ_CHARS: usize = 1_000_000;

/// How much one `read_file` call returns to a model with `context_window`
/// tokens: about a quarter of the window. A token is roughly four characters,
/// so a quarter of N tokens is about N characters — ~128 KB (3,000 lines of
/// code) on a 128K model, ~1 MB on a 1M one — and most files come back whole.
/// Local models read far smaller windows (see
/// `ollama_stream_exec::READ_WINDOW_BYTES`).
fn read_window_chars(context_window: u64) -> usize {
    (context_window as usize).clamp(MIN_READ_CHARS, MAX_READ_CHARS)
}

/// How much tool output the conversation carries in full: about half the
/// window (half of N tokens is about 2N characters), leaving the rest for the
/// conversation itself and the reply.
fn tool_output_budget_chars(context_window: u64) -> usize {
    context_window as usize * 2
}

/// A tool result shorter than this is kept whatever its age: the note that
/// would replace it saves next to nothing.
const STALE_RESULT_MIN_CHARS: usize = 2_000;

/// How a dropped result's note begins — also how one is recognised, so it is
/// never replaced twice.
const DROPPED_RESULT_PREFIX: &str = "[An earlier tool result";

/// How the HTTP status curl prints after the reply (`--write-out`) begins,
/// so the parser can tell that line from the reply itself.
const HTTP_STATUS_MARKER: &str = "krust-http-status:";

/// The context window a local Ollama model is asked for: the one Krust's own
/// Ollama tool loop uses.
const LOCAL_NUM_CTX: u64 = 16_384;

/// curl arguments for one streamed chat-completions request to the
/// OpenAI-compatible API at `base_url`.
fn api_curl_args(base_url: &str) -> Vec<String> {
    vec![
        "--silent".to_string(),
        "--show-error".to_string(),
        "--no-buffer".to_string(),
        "--variable".to_string(),
        format!("%{API_KEY_ENV}"),
        "--expand-header".to_string(),
        AUTHORIZATION_HEADER.to_string(),
        "--header".to_string(),
        "Content-Type: application/json".to_string(),
        "--data-binary".to_string(),
        "@-".to_string(),
        "--write-out".to_string(),
        format!("\n{HTTP_STATUS_MARKER}%{{http_code}}\n"),
        format!("{base_url}/chat/completions"),
    ]
}

fn new_api_command(route: &ApiRoute, working_dir: &Path, path: &str) -> Command {
    let mut command = Command::new("/usr/bin/curl");
    command.args(api_curl_args(&route.base_url));
    command.env(API_KEY_ENV, &route.api_key);
    command.current_dir(working_dir);
    command.env("PATH", path);
    command
}

/// Per-session message history, so follow-ups keep their context. Held in
/// memory and saved to disk after every step (see [`save_history`]), so a
/// conversation picks up where it left off after an app restart, or after a
/// turn the user stopped. Not tied to one model: the chat-completions shape is
/// the same for every provider.
type ApiChatHistories = HashMap<Uuid, Vec<Value>>;

fn chat_histories() -> &'static Mutex<ApiChatHistories> {
    static HISTORIES: OnceLock<Mutex<ApiChatHistories>> = OnceLock::new();
    HISTORIES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Deregisters the session however the turn ends, so the cancel registry
/// can't leak entries.
struct SessionGuard(Uuid);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        finish_harness_session(&self.0);
    }
}

/// How an API turn ended.
pub enum ApiTurnEnd {
    /// Answered, failed or stopped: every event of the turn has been sent.
    Finished,
    /// Failover reached a local agent app: the caller runs the turn on the
    /// first of `targets` (the rest stand by should it fail) in a fresh
    /// session, with `recap` — the request and what the turn already did — as
    /// its prompt. The turn has sent no `TurnCompleted`.
    HandOff {
        targets: Vec<CliRoute>,
        recap: String,
    },
}

/// Runs one turn on `route`: a cloud model, or a local Ollama model over
/// Ollama's OpenAI-compatible endpoint. Every request builds its own curl
/// command, with the key in [`API_KEY_ENV`].
///
/// When a request fails, the same request goes to the next of `failover`
/// (empty when BYOR failover is off), and the rest of the turn stays there.
/// A failure of the whole account (no credit, a rejected key, the host
/// unreachable) also rules out that provider's other failover models; one
/// confined to the model (rate-limited, overloaded) doesn't. Each is tried
/// once; the turn fails only when none is left. The model that answered is
/// reported in the turn's usage, so the caller can make it the selected one.
///
/// When the next in `failover` is a local agent app (Claude Code, Codex),
/// the turn is handed back to the caller to run there ([`ApiTurnEnd::HandOff`]).
/// That app can't hand it back, so API models listed after it are left out.
///
/// There is no step limit: the turn runs until the model answers or the user
/// stops it.
#[allow(clippy::too_many_arguments)]
pub async fn run_api_stream(
    prompt: &str,
    route: ApiRoute,
    session_id: &Uuid,
    working_dir: &Path,
    path: &str,
    mcp_tools: &[McpToolSpec],
    tool_caller: Option<Arc<dyn McpToolCaller>>,
    failover: Vec<FailoverTarget>,
    mut on_event: impl FnMut(TurnEvent),
) -> std::io::Result<ApiTurnEnd> {
    register_harness_session(*session_id);
    let _session_guard = SessionGuard(*session_id);

    // Opens the exchange straight away, so the prompt and the Sending…
    // shimmer show while the first request is in flight.
    on_event(TurnEvent::SessionInit {
        session_id: session_id.to_string(),
        model: Some(route.model_id.clone()),
        cwd: Some(working_dir.to_string_lossy().into_owned()),
    });

    let mut route = route;
    let mut failover = failover.into_iter();
    // Accounts (providers, custom endpoints) that failed as a whole this turn.
    let mut failed_accounts: Vec<String> = Vec::new();

    let tools: Vec<McpToolSpec> = builtin_local_tool_specs()
        .into_iter()
        .chain(mcp_tools.iter().cloned())
        .collect();
    let tool_defs: Vec<Value> = tools.iter().map(McpToolSpec::to_ollama_tool_json).collect();

    // Rebuilt every turn, so its opening line names the model now in use —
    // the conversation may have started on another one.
    let system_for = |route: &ApiRoute| {
        json!({
            "role": "system",
            "content": ollama_tool_system_prompt(
                &tools,
                &format!(
                    "{} ({}) from {}",
                    route.model_name, route.model, route.provider_name
                ),
            ),
        })
    };
    let system = system_for(&route);
    let mut messages = load_history(session_id).unwrap_or_default();
    match messages.first_mut() {
        Some(first) if first.get("role").and_then(Value::as_str) == Some("system") => {
            *first = system;
        }
        _ => messages.insert(0, system),
    }
    messages.push(turn_user_message(
        session_id,
        format!(
            "Current working directory: {}\n\nUser request:\n{prompt}",
            working_dir.display()
        ),
    ));

    let mut model = RouteModel::of(&route);
    // What every request of this turn cost, at each model's listed price.
    let mut turn_cost_usd = 0.0;
    let mut plan = PlanTracker::default();
    // Set when the context is near full or a task just finished; acted on once
    // the round's tool calls all have their results, a clean point to compact.
    let mut compact_next = false;
    // Times this turn compacted an input the model refused as too long.
    let mut input_compactions = 0;
    loop {
        // A local model's window is a fraction of a cloud model's: it is sent
        // this turn only, trimmed on a copy so the saved history stays whole.
        let local_view;
        let sent: &[Value] = if route.is_local() {
            let mut view = local_turn_messages(&messages);
            compact_old_tool_results(&mut view, tool_output_budget_chars(model.context_window));
            local_view = view;
            &local_view
        } else {
            compact_old_tool_results(&mut messages, tool_output_budget_chars(model.context_window));
            &messages
        };
        let mut request = json!({
            "model": route.model,
            "messages": without_turn_ids(sent),
            "tools": tool_defs,
            "stream": true,
        });
        if route.is_local() {
            request["options"] = json!({ "num_ctx": LOCAL_NUM_CTX });
        }
        let request = request.to_string();

        let command = new_api_command(&route, working_dir, path);
        let accumulator = Arc::new(Mutex::new(ApiChatAccumulator::default()));
        let parser_accumulator = Arc::clone(&accumulator);
        let status = run_jsonl_stream_cancellable(
            command,
            &request,
            harness_cancel_for(session_id),
            move |line| {
                parser_accumulator
                    .lock()
                    .map(|mut accumulator| accumulator.push_line(line))
                    .unwrap_or_default()
            },
            &mut on_event,
        )
        .await?;

        // Killed by a stop, not broken: no error banner for a stop the user
        // asked for.
        if harness_cancel_for(session_id).is_some_and(|cancel| cancel.is_cancelled()) {
            log::info!("api_stream: turn cancelled");
            return Ok(ApiTurnEnd::Finished);
        }

        let response = if status.success() {
            let finished = accumulator
                .lock()
                .map(|accumulator| (accumulator.http_status, accumulator.clone().finish()));
            match finished {
                Ok((_, Ok(response))) => Ok(response),
                Ok((http_status, Err(error))) => Err(request_failure(&route, http_status, &error)),
                Err(_) => Err(request_failure(
                    &route,
                    None,
                    "the response state became unavailable",
                )),
            }
        } else {
            Err(curl_failure(&route, status))
        };
        let response = match response {
            Ok(response) => response,
            Err(failure) => {
                // Longer than the model takes (Z.ai's 1261): compact the input
                // in the background and retry on the same model, instead of
                // failing over to models that would refuse it just the same.
                if failure.too_long && input_compactions < MAX_INPUT_COMPACTIONS {
                    input_compactions += 1;
                    on_event(TurnEvent::Text {
                        text: format!(
                            "_The input is over {}'s max length — compacting it in the \
                             background…_",
                            route.model_name
                        ),
                    });
                    if let Some(parts) = compact_oversized_input(
                        &route,
                        &mut messages,
                        session_id,
                        working_dir,
                        path,
                    )
                    .await
                    {
                        save_history(session_id, &messages);
                        on_event(TurnEvent::Text {
                            text: format!(
                                "_Compacted the input into a summary ({parts} part{}); \
                                 continuing._",
                                if parts == 1 { "" } else { "s" }
                            ),
                        });
                        continue;
                    }
                }
                // The failover list's light for this row (and, for a failure
                // of the whole account, every row of that account).
                failover_health::record(
                    &route.model_id,
                    &route.account,
                    failure.health,
                    failure.scope == FailureScope::Provider,
                );
                if failure.scope == FailureScope::Provider {
                    failed_accounts.push(route.account.clone());
                }
                let next = failover.by_ref().find(|next| match next {
                    FailoverTarget::Api(next) => !failed_accounts.contains(&next.account),
                    FailoverTarget::Cli(_) => true,
                });
                let next = match next {
                    None => {
                        emit_failed_turn(&mut on_event, failure.detail);
                        return Ok(ApiTurnEnd::Finished);
                    }
                    Some(FailoverTarget::Cli(app)) => {
                        on_event(TurnEvent::Text {
                            text: format!(
                                "{} — switching to {} on {}.",
                                failure.reason, app.model_name, app.app_name
                            ),
                        });
                        log::info!(
                            "api_stream: {} failed ({}); handing the turn to {}",
                            route.model_id,
                            failure.detail,
                            app.model_id
                        );
                        let targets = std::iter::once(app)
                            .chain(failover.filter_map(|next| match next {
                                FailoverTarget::Cli(app) => Some(app),
                                FailoverTarget::Api(_) => None,
                            }))
                            .collect();
                        return Ok(ApiTurnEnd::HandOff {
                            targets,
                            recap: cli_recap(prompt, &messages),
                        });
                    }
                    Some(FailoverTarget::Api(next)) => next,
                };
                // The failed request added nothing to `messages`, so the
                // next model gets exactly the same one.
                on_event(TurnEvent::Text {
                    text: format!(
                        "{} — switching to {} on {}.",
                        failure.reason, next.model_name, next.provider_name
                    ),
                });
                log::info!(
                    "api_stream: {} failed ({}); failing over to {}",
                    route.model_id,
                    failure.detail,
                    next.model_id
                );
                route = next;
                model = RouteModel::of(&route);
                messages[0] = system_for(&route);
                continue;
            }
        };
        failover_health::record(&route.model_id, &route.account, Health::Good, false);
        if let Some((input_price, output_price)) = model.price {
            turn_cost_usd += (response.prompt_tokens as f64 * input_price
                + response.completion_tokens as f64 * output_price)
                / 1_000_000.0;
        }
        // Token counts are the last request's — they say how full the
        // context is — while the cost covers every request in the turn.
        let usage = TurnUsage {
            input_tokens: response.prompt_tokens,
            output_tokens: response.completion_tokens,
            model_id: Some(route.model_id.clone()),
            cost_usd: model.price.map(|_| turn_cost_usd),
            context_window: model.listed_window,
            ..Default::default()
        };

        // Near the model's window: compact before it fills and the provider
        // rejects the request. Measured on the real prompt-token count the
        // provider just reported, against the model's own window.
        if let Some(window) = model.listed_window {
            if response.prompt_tokens as f64 >= window as f64 * COMPACT_AT_FILL {
                compact_next = true;
            }
        }

        // Structured calls first; otherwise a call some models write as JSON
        // text in their answer.
        let (content, tool_calls) = if !response.tool_calls.is_empty() {
            (response.content, response.tool_calls)
        } else if let Some(call) = parse_text_tool_call(&response.content, &tools) {
            (
                String::new(),
                vec![ApiToolCall {
                    id: new_wire_call_id(),
                    name: call.name,
                    arguments: Ok(call.arguments),
                    extra_content: None,
                }],
            )
        } else {
            if !response.content.trim().is_empty() {
                on_event(TurnEvent::Text {
                    text: response.content.clone(),
                });
            }
            messages.push(json!({ "role": "assistant", "content": response.content }));
            save_history(session_id, &messages);
            on_event(TurnEvent::TurnCompleted {
                usage: Some(usage),
                is_error: false,
                error_message: None,
            });
            return Ok(ApiTurnEnd::Finished);
        };

        // What the model said before calling its tools ("Let me check the
        // file…") — shown, as the CLI harnesses show theirs.
        if !content.trim().is_empty() {
            on_event(TurnEvent::Text {
                text: content.clone(),
            });
        }
        messages.push(assistant_tool_call_message(&content, &tool_calls));

        for call in tool_calls {
            // The checklist is this call's whole display: no tool card.
            if call.name == BUILTIN_TODO_LIST {
                let entries = call
                    .arguments
                    .clone()
                    .and_then(|arguments| todo_list_entries(&arguments));
                let reply = match entries {
                    Ok(entries) => {
                        let updates = plan.updates(&entries);
                        // A finished step is a natural point to compact: the
                        // work it produced is done with.
                        if updates.iter().any(|update| {
                            matches!(update, PlanUpdate::Complete(ids) if !ids.is_empty())
                        }) {
                            compact_next = true;
                        }
                        for update in updates {
                            on_event(TurnEvent::PlanUpdated { update });
                        }
                        todo_list_reply(&entries)
                    }
                    Err(error) => error,
                };
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call.id,
                    "content": reply,
                }));
                continue;
            }
            // The card's id is ours: provider ids are only unique within one
            // response, and a card id must be unique in the conversation.
            let card_id = format!("api-tool-{}", Uuid::new_v4());
            on_event(TurnEvent::ToolCallStarted {
                call_id: card_id.clone(),
                name: call.name.clone(),
                input: call.arguments.clone().unwrap_or(Value::Null),
            });
            let result = match &call.arguments {
                Err(error) => Err(error.clone()),
                Ok(arguments) if is_builtin_local_tool(&call.name) => {
                    dispatch_builtin_tool_with_read_window(
                        &call.name,
                        arguments,
                        working_dir,
                        path,
                        model.read_window,
                    )
                    .await
                }
                Ok(arguments) => match &tool_caller {
                    Some(tool_caller) => tool_caller.call(&call.name, arguments.clone()).await,
                    None => Err(format!("no tool named '{}' is available", call.name)),
                },
            };
            let tool_content = match result {
                Ok(text) => {
                    on_event(TurnEvent::ToolCallResult {
                        call_id: card_id,
                        outcome: ToolCallOutcome::Success(json!({ "result": text })),
                    });
                    text
                }
                Err(error) => {
                    on_event(TurnEvent::ToolCallResult {
                        call_id: card_id,
                        outcome: ToolCallOutcome::Error(error.clone()),
                    });
                    error
                }
            };
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": clip_for_model(&tool_content, model.read_window + 2_000),
            }));
        }
        // Every call has its result now, so the history is a valid
        // conversation to resume from — keep it, in case the turn ends here.
        save_history(session_id, &messages);

        if compact_next {
            compact_next = false;
            compact_conversation(&route, &mut messages, &plan, session_id, working_dir, path, &mut on_event)
                .await;
        }
    }
}

/// The share of the model's window at which a run is compacted — summarized,
/// and the step-by-step chatter dropped — so one model working a long task
/// doesn't grow the request until the provider rejects it.
const COMPACT_AT_FILL: f64 = 0.90;

/// Summarizes the work so far and replaces the turn's accumulated steps with
/// that summary, on the same model. A failed summary request leaves the
/// history untouched (the run goes on, uncompacted) rather than ending the
/// turn. The summary is the model's, so the model carries forward what the
/// next step needs; the plan still to do is appended so it is never lost.
async fn compact_conversation(
    route: &ApiRoute,
    messages: &mut Vec<Value>,
    plan: &PlanTracker,
    session_id: &Uuid,
    working_dir: &Path,
    path: &str,
    on_event: &mut impl FnMut(TurnEvent),
) {
    let Some(summary) =
        summarize_for_compaction(route, messages, session_id, working_dir, path).await
    else {
        return;
    };
    apply_compaction(messages, &summary, &plan.remaining());
    save_history(session_id, messages);
    on_event(TurnEvent::Text {
        text: "_Compacted the conversation to free up context._".to_string(),
    });
}

/// Asks the model to summarize the conversation for carrying into a fresh
/// context. Sends no tools, so the reply is prose; returns None (compaction
/// skipped) if the request fails or comes back empty.
async fn summarize_for_compaction(
    route: &ApiRoute,
    messages: &[Value],
    session_id: &Uuid,
    working_dir: &Path,
    path: &str,
) -> Option<String> {
    let mut summary_messages = without_turn_ids(messages);
    summary_messages.push(json!({
        "role": "user",
        "content": "Summarize this conversation so the work can continue in a fresh context. \
            Briefly cover what has been done, what was found (exact file paths, names, values, \
            decisions), and what the next step needs. Leave out tool mechanics and anything not \
            needed to carry on. Reply with only the summary.",
    }));
    let text = ask_once(route, summary_messages, session_id, working_dir, path)
        .await
        .ok()?;
    (!text.trim().is_empty()).then_some(text)
}

/// Sends one tools-free request to the turn's model, apart from the
/// conversation, and returns the reply's text, or the provider's error.
/// Nothing is shown to the user: it is for background work like summarising.
async fn ask_once(
    route: &ApiRoute,
    messages: Vec<Value>,
    session_id: &Uuid,
    working_dir: &Path,
    path: &str,
) -> Result<String, String> {
    let mut request = json!({
        "model": route.model,
        "messages": messages,
        "stream": true,
    });
    if route.is_local() {
        request["options"] = json!({ "num_ctx": LOCAL_NUM_CTX });
    }
    let request = request.to_string();

    let command = new_api_command(route, working_dir, path);
    let accumulator = Arc::new(Mutex::new(ApiChatAccumulator::default()));
    let parser_accumulator = Arc::clone(&accumulator);
    let mut sink = |_event: TurnEvent| {};
    let status = run_jsonl_stream_cancellable(
        command,
        &request,
        harness_cancel_for(session_id),
        move |line| {
            parser_accumulator
                .lock()
                .map(|mut accumulator| accumulator.push_line(line))
                .unwrap_or_default()
        },
        &mut sink,
    )
    .await
    .map_err(|error| error.to_string())?;
    if !status.success() {
        return Err(curl_failure_message(&route.provider_name, status));
    }
    let accumulator = accumulator
        .lock()
        .map_err(|_| "the response state became unavailable".to_string())?
        .clone();
    accumulator.finish().map(|response| response.content)
}

/// How many times one turn compacts an input the model refused as too long
/// before failing over as before.
const MAX_INPUT_COMPACTIONS: usize = 2;
/// The most parts an over-long input is cut into for summarising.
const MAX_SUMMARY_PARTS: usize = 32;

/// Compacts a conversation the model refused as too long (Z.ai's 1261). All
/// of it, the latest request included, is summarised half at a time in
/// separate background requests — a half still too long is halved again —
/// and the history becomes the turn's own opening message carrying the
/// joined summary, so the turn keeps its ID. Returns how many parts were
/// summarised, or None if it couldn't be done (the caller then fails over).
async fn compact_oversized_input(
    route: &ApiRoute,
    messages: &mut Vec<Value>,
    session_id: &Uuid,
    working_dir: &Path,
    path: &str,
) -> Option<usize> {
    let last_user = messages
        .iter()
        .rposition(|message| message.get("role").and_then(Value::as_str) == Some("user"))?;
    let system = messages.first().cloned()?;
    let text = transcript_text(&messages[1..]);
    let mut queue: VecDeque<String> = split_in_half(&text).into();
    if queue.is_empty() {
        return None;
    }
    let mut parts = queue.len();
    let mut summaries = Vec::new();
    while let Some(part) = queue.pop_front() {
        match summarise_part(route, &part, session_id, working_dir, path).await {
            PartSummary::Done(summary) => summaries.push(summary),
            PartSummary::TooLong if parts < MAX_SUMMARY_PARTS => {
                let halves = split_in_half(&part);
                if halves.len() < 2 {
                    return None;
                }
                parts += halves.len() - 1;
                for half in halves.into_iter().rev() {
                    queue.push_front(half);
                }
            }
            PartSummary::TooLong | PartSummary::Failed => return None,
        }
    }
    let mut opening = messages[last_user].clone();
    opening["content"] = Value::String(format!(
        "[This conversation, the latest request included, was longer than this model accepts, \
         so it was compacted into the summary below. Carry on the work from it, starting with \
         the latest request.]\n\n{}",
        summaries.join("\n\n")
    ));
    *messages = vec![system, opening];
    Some(summaries.len())
}

/// The outcome of summarising one part of an over-long input.
enum PartSummary {
    Done(String),
    /// Still too long for the model: halve it and try again.
    TooLong,
    Failed,
}

async fn summarise_part(
    route: &ApiRoute,
    part: &str,
    session_id: &Uuid,
    working_dir: &Path,
    path: &str,
) -> PartSummary {
    let request = vec![json!({
        "role": "user",
        "content": format!(
            "Summarise this part of a long conversation so the work can continue from the \
             summary alone. Keep everything needed to carry on: the user's instructions and \
             requests, exact file paths, names, values, code that matters, decisions and \
             findings. Leave out tool mechanics. Reply with only the summary.\n\n---\n\n{part}"
        ),
    })];
    match ask_once(route, request, session_id, working_dir, path).await {
        Ok(text) if !text.trim().is_empty() => PartSummary::Done(text),
        Err(error) if is_too_long_error(&error) => PartSummary::TooLong,
        Ok(_) | Err(_) => PartSummary::Failed,
    }
}

/// A chat history as plain text to summarise: each message as `role: text`,
/// with the tool calls an assistant made.
fn transcript_text(messages: &[Value]) -> String {
    messages
        .iter()
        .filter_map(|message| {
            let role = message.get("role")?.as_str()?;
            let mut text = message
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            for call in message
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let function = &call["function"];
                text.push_str(&format!(
                    "\n[called {} with {}]",
                    function["name"].as_str().unwrap_or("a tool"),
                    function["arguments"]
                ));
            }
            (!text.trim().is_empty()).then(|| format!("{role}: {text}"))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `text` in two halves, cut at a paragraph or line break near the middle
/// when there is one, so a message isn't split mid-line. Empty halves are
/// left out.
fn split_in_half(text: &str) -> Vec<String> {
    let mid = text.floor_char_boundary(text.len() / 2);
    let cut = text[..mid]
        .rfind("\n\n")
        .or_else(|| text[..mid].rfind('\n'))
        .filter(|&at| at > mid / 2)
        .map_or(mid, |at| at + 1);
    let (first, second) = text.split_at(cut);
    [first, second]
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// Replaces the turn's accumulated steps with `summary`: keeps the system
/// message and every user turn, drops everything after the last user message,
/// and appends the summary as the assistant's word. `remaining` (the plan not
/// yet done) is appended so compaction never loses the checklist. The chat
/// history is the model's own; the conversation the user sees is built from
/// events separately, so this does not touch it.
fn apply_compaction(messages: &mut Vec<Value>, summary: &str, remaining: &[String]) {
    let Some(last_user) = messages
        .iter()
        .rposition(|message| message.get("role").and_then(Value::as_str) == Some("user"))
    else {
        return;
    };
    messages.truncate(last_user + 1);
    let mut content = summary.to_string();
    if !remaining.is_empty() {
        content.push_str("\n\nStill to do:");
        for item in remaining {
            content.push_str(&format!("\n- {item}"));
        }
    }
    messages.push(json!({ "role": "assistant", "content": content }));
}

/// What the turn's current model sizes: its price, its window, and how much
/// of a file one read returns to it.
struct RouteModel {
    /// USD per million (input, output) tokens; `None` when not known.
    price: Option<(f64, f64)>,
    /// The window as listed, for the context meter.
    listed_window: Option<u64>,
    /// The window the conversation is sized against.
    context_window: u64,
    /// How much one `read_file` returns. A tool result is clipped to this
    /// and its header (anything bigger keeps head and tail).
    read_window: usize,
}

impl RouteModel {
    fn of(route: &ApiRoute) -> Self {
        if route.is_local() {
            // Free, and asked for a 16K window: a read takes about a quarter.
            return Self {
                price: Some((0.0, 0.0)),
                listed_window: Some(LOCAL_NUM_CTX),
                context_window: LOCAL_NUM_CTX,
                read_window: LOCAL_NUM_CTX as usize,
            };
        }
        let listed_window = model_context_window(route.provider, &route.model);
        let context_window = listed_window.unwrap_or(DEFAULT_CONTEXT_WINDOW);
        Self {
            price: model_price(route.provider, &route.model),
            listed_window,
            context_window,
            read_window: read_window_chars(context_window),
        }
    }
}

/// How many earlier exchanges a handed-off turn's recap carries.
const RECAP_EARLIER_EXCHANGES: usize = 3;

/// The prompt a local agent app takes a turn over with, in a session of its
/// own that has seen none of this conversation: the last few exchanges, the
/// request word for word, and what this turn already did — what the model
/// said, and each tool call with the start of its result.
fn cli_recap(prompt: &str, messages: &[Value]) -> String {
    let role = |message: &Value| message.get("role").and_then(Value::as_str).map(str::to_owned);
    let text = |message: &Value| {
        message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let turn_start = messages
        .iter()
        .rposition(|message| role(message).as_deref() == Some("user"))
        .unwrap_or(messages.len());

    // Earlier exchanges: each request, with the last thing said in answer.
    let mut exchanges: Vec<(String, String)> = Vec::new();
    for message in &messages[..turn_start] {
        match role(message).as_deref() {
            Some("user") => {
                let request = text(message);
                // The runner's own wrapping of the request.
                let request = request
                    .split_once("User request:\n")
                    .map_or(request.as_str(), |(_, request)| request)
                    .to_owned();
                exchanges.push((request, String::new()));
            }
            Some("assistant") => {
                let said = text(message);
                if let Some(last) = exchanges.last_mut().filter(|_| !said.is_empty()) {
                    last.1 = said;
                }
            }
            _ => {}
        }
    }

    let mut recap = String::from(
        "You are taking over a request another model was working on in this terminal; it \
         stopped partway. Carry on from where it left off rather than starting over, and check \
         the current state of anything it changed.\n",
    );
    let earlier = &exchanges[exchanges.len().saturating_sub(RECAP_EARLIER_EXCHANGES)..];
    if !earlier.is_empty() {
        recap.push_str("\nEarlier in this conversation:\n");
        for (request, answer) in earlier {
            recap.push_str(&format!("- User: {}\n", recap_line(request, 600)));
            if !answer.is_empty() {
                recap.push_str(&format!("  Answer: {}\n", recap_line(answer, 1_200)));
            }
        }
    }
    recap.push_str(&format!("\nThe request:\n{prompt}\n\nWhat was already done for it:\n"));
    let mut done = 0;
    for message in messages.iter().skip(turn_start + 1) {
        match role(message).as_deref() {
            Some("assistant") => {
                let said = text(message);
                if !said.is_empty() {
                    recap.push_str(&format!("- Said: {}\n", recap_line(&said, 600)));
                    done += 1;
                }
                let calls = message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                for call in calls {
                    let name = call["function"]["name"].as_str().unwrap_or("a tool");
                    let arguments = call["function"]["arguments"].as_str().unwrap_or_default();
                    recap.push_str(&format!("- Ran {name} {}\n", recap_line(arguments, 200)));
                    done += 1;
                }
            }
            Some("tool") => {
                recap.push_str(&format!("  Result: {}\n", recap_line(&text(message), 300)));
            }
            _ => {}
        }
    }
    if done == 0 {
        recap.push_str("Nothing yet.\n");
    }
    recap
}

/// `text` on one line, cut to `limit` characters.
fn recap_line(text: &str, limit: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= limit {
        return line;
    }
    let cut: String = line.chars().take(limit).collect();
    format!("{cut}…")
}

/// What a local model is sent: the system message, noting that earlier turns
/// are left out, and this turn's messages (from its user request on).
fn local_turn_messages(messages: &[Value]) -> Vec<Value> {
    let turn_start = messages
        .iter()
        .rposition(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .unwrap_or(0);
    let mut sent = Vec::new();
    if let Some(system) = messages
        .first()
        .filter(|first| first.get("role").and_then(Value::as_str) == Some("system"))
    {
        let mut system = system.clone();
        if turn_start > 1 {
            let content = system["content"].as_str().unwrap_or_default();
            system["content"] = Value::String(format!(
                "{content}\n\nThis conversation had earlier turns; they are left out here to \
                 fit your context window. Work from the request below."
            ));
        }
        sent.push(system);
    }
    sent.extend(messages[turn_start..].iter().cloned());
    sent
}

/// Swaps the content of the oldest long tool results for a one-line note
/// until the tool output still carried in full fits in `budget_chars` (the
/// very latest result always stays). Until then nothing is dropped: a model
/// that forgets what it read a few calls ago reads it again, and again. Each
/// request resends the whole conversation, so on a small model a few
/// whole-file reads would otherwise overflow its window. The result's message
/// stays (its `tool_call_id` must still answer the assistant's call); only its
/// text shrinks. The model can run the tool again if it needs the output back.
fn compact_old_tool_results(messages: &mut [Value], budget_chars: usize) {
    let full_length = |message: &Value| {
        if message.get("role").and_then(Value::as_str) != Some("tool") {
            return None;
        }
        let content = message.get("content").and_then(Value::as_str)?;
        (!content.starts_with(DROPPED_RESULT_PREFIX)).then(|| content.chars().count())
    };
    let mut total_chars: usize = messages.iter().filter_map(full_length).sum();
    let Some(latest) = messages
        .iter()
        .rposition(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
    else {
        return;
    };
    for message in &mut messages[..latest] {
        if total_chars <= budget_chars {
            break;
        }
        let Some(length) = full_length(message) else {
            continue;
        };
        if length < STALE_RESULT_MIN_CHARS {
            continue;
        }
        message["content"] = Value::String(format!(
            "{DROPPED_RESULT_PREFIX} ({length} characters) was dropped to keep the conversation \
             small. Run the tool again if you need it.]"
        ));
        total_chars -= length;
    }
}

/// Where a session's history is saved between app runs.
fn history_path(session_id: &Uuid) -> std::path::PathBuf {
    krust_core::paths::state_dir()
        .join("api-chat-histories")
        .join(format!("{session_id}.json"))
}

/// A session's history: the in-memory copy, else the one saved to disk.
fn load_history(session_id: &Uuid) -> Option<Vec<Value>> {
    if let Some(messages) = chat_histories()
        .lock()
        .ok()
        .and_then(|histories| histories.get(session_id).cloned())
    {
        return Some(messages);
    }
    let saved = std::fs::read_to_string(history_path(session_id)).ok()?;
    serde_json::from_str(&saved).ok()
}

/// Keeps a session's history in memory and on disk. Only ever called when
/// every tool call in it has its result, so what is saved can be resumed.
fn save_history(session_id: &Uuid, messages: &[Value]) {
    if let Ok(mut histories) = chat_histories().lock() {
        histories.insert(*session_id, messages.to_vec());
    }
    let path = history_path(session_id);
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            let json = serde_json::to_string(messages).map_err(std::io::Error::other)?;
            std::fs::write(&path, json)
        });
    if let Err(error) = written {
        log::warn!("api_stream: could not save the conversation history: {error}");
    }
}

/// Makes the model forget a turn the user deleted, by dropping it from this
/// session's saved history.
pub(crate) fn forget_turn(session_id: &Uuid, turn_id: &str) {
    let Some(mut messages) = load_history(session_id) else {
        return;
    };
    if remove_turn_from_messages(&mut messages, turn_id) {
        save_history(session_id, &messages);
    }
}

/// Why curl itself failed (as opposed to the provider answering with an
/// error, which arrives as a body and is reported from the parse).
fn curl_failure_message(provider: &str, status: ExitStatus) -> String {
    match status.code() {
        // An option this curl doesn't know: `--variable` and
        // `--expand-header` arrived in curl 8.3.
        Some(2) => format!(
            "Could not call {provider}: this machine's curl is too old (cloud models need curl \
             8.3 or newer)"
        ),
        Some(6) | Some(7) => {
            format!("Could not reach {provider} — check the internet connection")
        }
        Some(28) => format!("{provider} took too long to answer"),
        code => format!(
            "Could not reach {provider} (curl exited with {})",
            code.map_or_else(|| "a signal".to_string(), |code| code.to_string())
        ),
    }
}

/// How far a failed request's failure reaches, for failover.
#[derive(Clone, Copy, Debug, PartialEq)]
enum FailureScope {
    /// The account or the provider itself — no credit, a rejected key, the
    /// host unreachable. Its other models would fail the same way.
    Provider,
    /// This one model — rate-limited, overloaded, missing, or the
    /// conversation too long for it. Another model, even on the same
    /// provider, may answer.
    Model,
}

/// A failed request, as failover and the user see it.
struct RequestFailure {
    scope: FailureScope,
    /// The light it leaves on the failover row: red for a failure that will
    /// last, amber for a busy or flaky model.
    health: Health,
    /// What the user is told on switching: "Gemini Flash is overloaded".
    reason: String,
    /// The full error, for the banner when no model is left.
    detail: String,
    /// The request was longer than the model accepts (Z.ai's code 1261, a
    /// context-length error): compacting the input can rescue it.
    too_long: bool,
}

/// Whether a provider's error says the request was too long for the model:
/// Z.ai's "Prompt exceeds max length (code 1261)", or a context-length error.
fn is_too_long_error(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("(code 1261)")
        || lower.contains("exceeds max length")
        || lower.contains("prompt is too long")
        || (lower.contains("context")
            && (lower.contains("length") || lower.contains("window") || lower.contains("too long")))
}

/// Classifies a request the provider answered with an error. `http_status`
/// is what curl reported; `error` the provider's own message.
fn request_failure(route: &ApiRoute, http_status: Option<u16>, error: &str) -> RequestFailure {
    let provider = &route.provider_name;
    let model = &route.model_name;
    let lower = error.to_lowercase();
    // "Insufficient balance / credits / funds", OpenAI's `insufficient_quota`:
    // the account, not the model. (A 429 that only says "quota" is usually a
    // free tier's per-model limit, so it isn't counted here.)
    let out_of_credit = lower.contains("insufficient") || lower.contains("balance");
    let too_long = is_too_long_error(error);
    let (scope, health, reason) = match http_status {
        Some(401 | 403) => (
            FailureScope::Provider,
            Health::Down,
            format!("{provider} rejected the key"),
        ),
        Some(402) => (
            FailureScope::Provider,
            Health::Down,
            format!("{provider} is out of credit"),
        ),
        _ if out_of_credit => (
            FailureScope::Provider,
            Health::Down,
            format!("{provider} is out of credit"),
        ),
        // Says nothing about the model's health.
        _ if too_long => (
            FailureScope::Model,
            Health::Unknown,
            format!("The conversation is too long for {model}"),
        ),
        Some(429) => (
            FailureScope::Model,
            Health::Degraded,
            format!("{model} is rate-limited"),
        ),
        Some(503 | 529) => (
            FailureScope::Model,
            Health::Degraded,
            format!("{model} is overloaded"),
        ),
        _ if lower.contains("overloaded") => (
            FailureScope::Model,
            Health::Degraded,
            format!("{model} is overloaded"),
        ),
        Some(404) => (
            FailureScope::Model,
            Health::Down,
            format!("{model} isn't available"),
        ),
        Some(500..=599) => (
            FailureScope::Model,
            Health::Degraded,
            format!("{model} had a server error"),
        ),
        _ => {
            let shown: String = error.chars().take(160).collect();
            (
                FailureScope::Model,
                Health::Down,
                format!("{model} failed ({shown})"),
            )
        }
    };
    RequestFailure {
        scope,
        health,
        reason,
        detail: format!("{provider}: {error}"),
        too_long,
    }
}

/// Classifies a request curl itself couldn't complete.
fn curl_failure(route: &ApiRoute, status: ExitStatus) -> RequestFailure {
    let provider = &route.provider_name;
    let (scope, health, reason) = match status.code() {
        Some(28) => (
            FailureScope::Model,
            Health::Degraded,
            format!("{} took too long to answer", route.model_name),
        ),
        Some(6) | Some(7) => (
            FailureScope::Provider,
            Health::Down,
            format!("{provider} can't be reached"),
        ),
        _ => (
            FailureScope::Provider,
            Health::Down,
            format!("{provider} couldn't be called"),
        ),
    };
    RequestFailure {
        scope,
        health,
        reason,
        detail: curl_failure_message(&route.provider_name, status),
        too_long: false,
    }
}

/// The assistant turn that made `tool_calls`, as the API expects it back:
/// each call carries its id, and its arguments as a JSON string.
fn assistant_tool_call_message(content: &str, tool_calls: &[ApiToolCall]) -> Value {
    json!({
        "role": "assistant",
        // Null rather than "" when the model said nothing: some providers
        // reject an empty string next to tool calls.
        "content": (!content.is_empty()).then_some(content),
        "tool_calls": tool_calls
            .iter()
            .map(|call| {
                let mut wire = json!({
                    "id": call.id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": match &call.arguments {
                            Ok(arguments) => arguments.to_string(),
                            Err(_) => "{}".to_string(),
                        },
                    },
                });
                // Sent back exactly as it came (see `ApiToolCall::extra_content`).
                if let Some(extra) = &call.extra_content {
                    wire["extra_content"] = extra.clone();
                }
                wire
            })
            .collect::<Vec<_>>(),
    })
}

/// The head and tail of a tool result longer than `limit` characters.
fn clip_for_model(text: &str, limit: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return text.to_string();
    }
    let half = limit / 2;
    format!(
        "{}\n… [{} characters omitted — read the rest in parts, e.g. read_file with \
         offset/limit] …\n{}",
        chars[..half].iter().collect::<String>(),
        chars.len() - 2 * half,
        chars[chars.len() - half..].iter().collect::<String>()
    )
}

fn new_wire_call_id() -> String {
    format!("call_{}", Uuid::new_v4().simple())
}

#[derive(Clone, Debug)]
struct ApiToolCall {
    /// The provider's id for the call, echoed back on the tool result.
    id: String,
    name: String,
    /// The parsed arguments, or why they couldn't be parsed — reported to the
    /// model as the tool's error so it can correct itself.
    arguments: Result<Value, String>,
    /// Provider data attached to the call that must go back with it — Gemini
    /// 3's `{"google": {"thought_signature": …}}`, without which its next
    /// request fails ("Function call is missing a thought_signature").
    extra_content: Option<Value>,
}

struct ApiChatResult {
    content: String,
    tool_calls: Vec<ApiToolCall>,
    prompt_tokens: u64,
    completion_tokens: u64,
}

/// A tool call as its fragments arrive: a stream sends the id and name first
/// and the arguments as string pieces after.
#[derive(Clone, Debug, Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
    extra_content: Option<Value>,
}

/// Folds one response's server-sent events into the text, the reasoning
/// (emitted as it streams), and the tool calls.
#[derive(Clone, Debug, Default)]
struct ApiChatAccumulator {
    content: String,
    tool_calls: Vec<PartialToolCall>,
    finish_reason: Option<String>,
    /// `data: [DONE]` arrived.
    done: bool,
    /// At least one `data:` event arrived — a streamed reply, not an error body.
    saw_events: bool,
    /// Lines that weren't events: an error body, which some providers
    /// pretty-print over several lines.
    raw: String,
    error: Option<String>,
    prompt_tokens: u64,
    completion_tokens: u64,
    /// The HTTP status curl printed after the reply (see
    /// [`HTTP_STATUS_MARKER`]).
    http_status: Option<u16>,
}

#[derive(Deserialize)]
struct Chunk {
    choices: Option<Vec<ChunkChoice>>,
    usage: Option<ChunkUsage>,
    error: Option<Value>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    delta: Option<ChunkDelta>,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChunkDelta {
    content: Option<String>,
    /// Z.ai's (and DeepSeek-style) reasoning stream.
    reasoning_content: Option<String>,
    /// Groq's name for the same thing.
    reasoning: Option<String>,
    tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Deserialize)]
struct ToolCallDelta {
    index: Option<usize>,
    id: Option<String>,
    function: Option<FunctionDelta>,
    extra_content: Option<Value>,
}

#[derive(Deserialize)]
struct FunctionDelta {
    name: Option<String>,
    /// A string fragment, per the spec — but Z.ai documents a whole object
    /// in places, so both are accepted.
    arguments: Option<Value>,
}

#[derive(Deserialize)]
struct ChunkUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

impl ApiChatAccumulator {
    fn push_line(&mut self, line: &str) -> Vec<TurnEvent> {
        let line = line.trim();
        if line.is_empty() || line.starts_with(':') || line.starts_with("event:") {
            return Vec::new();
        }
        if let Some(code) = line.strip_prefix(HTTP_STATUS_MARKER) {
            self.http_status = code.trim().parse().ok();
            return Vec::new();
        }
        let Some(data) = line.strip_prefix("data:") else {
            self.raw.push_str(line);
            self.raw.push('\n');
            return Vec::new();
        };
        self.saw_events = true;
        let data = data.trim();
        if data == "[DONE]" {
            self.done = true;
            return Vec::new();
        }
        let chunk: Chunk = match serde_json::from_str(data) {
            Ok(chunk) => chunk,
            Err(error) => {
                log::warn!("api_stream: unreadable event: {error}");
                return Vec::new();
            }
        };
        if let Some(error) = chunk.error {
            self.error = Some(error_text(&error));
        }
        if let Some(usage) = chunk.usage {
            self.prompt_tokens = usage.prompt_tokens;
            self.completion_tokens = usage.completion_tokens;
        }
        let mut events = Vec::new();
        for choice in chunk.choices.unwrap_or_default() {
            if let Some(reason) = choice.finish_reason {
                self.finish_reason = Some(reason);
            }
            let Some(delta) = choice.delta else {
                continue;
            };
            if let Some(text) = delta.content {
                self.content.push_str(&text);
            }
            if let Some(thinking) = delta.reasoning_content.or(delta.reasoning) {
                if !thinking.is_empty() {
                    events.push(TurnEvent::Thinking { text: thinking });
                }
            }
            for fragment in delta.tool_calls.unwrap_or_default() {
                self.push_tool_call_fragment(fragment);
            }
        }
        events
    }

    fn push_tool_call_fragment(&mut self, fragment: ToolCallDelta) {
        // Without an index (Gemini sends each call whole), a fragment that
        // carries an id starts a new call and anything else continues the last.
        let slot = match fragment.index {
            Some(index) => index,
            None if fragment.id.is_some() || self.tool_calls.is_empty() => self.tool_calls.len(),
            None => self.tool_calls.len() - 1,
        };
        while self.tool_calls.len() <= slot {
            self.tool_calls.push(PartialToolCall::default());
        }
        let call = &mut self.tool_calls[slot];
        if let Some(id) = fragment.id.filter(|id| !id.is_empty()) {
            call.id = id;
        }
        if let Some(extra) = fragment.extra_content.filter(|extra| !extra.is_null()) {
            call.extra_content = Some(extra);
        }
        let Some(function) = fragment.function else {
            return;
        };
        if let Some(name) = function.name.filter(|name| !name.is_empty()) {
            call.name = name;
        }
        match function.arguments {
            Some(Value::String(piece)) => call.arguments.push_str(&piece),
            Some(Value::Null) | None => {}
            Some(whole) => call.arguments = whole.to_string(),
        }
    }

    fn finish(self) -> Result<ApiChatResult, String> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if !self.saw_events {
            let raw = self.raw.trim();
            if raw.is_empty() {
                return Err("the provider returned an empty response".to_string());
            }
            return Err(error_from_body(raw).unwrap_or_else(|| {
                let shown: String = raw.chars().take(300).collect();
                format!("unexpected response: {shown}")
            }));
        }
        match self.finish_reason.as_deref() {
            Some("sensitive") => {
                return Err("the provider declined to answer (it flagged the content)".to_string());
            }
            Some("network_error") => {
                return Err("the provider hit a network error mid-answer".to_string());
            }
            Some("length" | "model_context_window_exceeded")
                if self.content.trim().is_empty() && self.tool_calls.is_empty() =>
            {
                return Err("the conversation no longer fits the model's context window — \
                     start a new conversation"
                    .to_string());
            }
            _ => {}
        }
        if !self.done && self.finish_reason.is_none() {
            return Err("the response ended before it was complete".to_string());
        }
        let tool_calls = self
            .tool_calls
            .into_iter()
            .filter(|call| !call.name.is_empty())
            .map(|call| {
                let arguments = if call.arguments.trim().is_empty() {
                    Ok(Value::Object(serde_json::Map::new()))
                } else {
                    serde_json::from_str::<Value>(&call.arguments).map_err(|error| {
                        format!(
                            "the arguments for {} were not valid JSON ({error}); call it again \
                             with a JSON object",
                            call.name
                        )
                    })
                };
                ApiToolCall {
                    id: if call.id.is_empty() {
                        new_wire_call_id()
                    } else {
                        call.id
                    },
                    name: call.name,
                    arguments,
                    extra_content: call.extra_content,
                }
            })
            .collect();
        Ok(ApiChatResult {
            content: self.content,
            tool_calls,
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
        })
    }
}

/// The message in an error body. Providers disagree on the shape:
/// `{"error":{"message":…}}` (OpenAI, Groq, Z.ai), `[{"error":{…}}]`
/// (Gemini), or a bare `{"message":…}`.
fn error_from_body(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let value = match value {
        Value::Array(items) => items.into_iter().next()?,
        other => other,
    };
    match value.get("error") {
        Some(error) => Some(error_text(error)),
        None => value
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn error_text(error: &Value) -> String {
    match error {
        Value::String(text) => text.clone(),
        Value::Object(_) => {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            match error.get("code").filter(|code| !code.is_null()) {
                Some(Value::String(code)) => format!("{message} (code {code})"),
                Some(code) => format!("{message} (code {code})"),
                None => message.to_string(),
            }
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
#[path = "api_stream_exec_tests.rs"]
mod tests;
