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

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex, OnceLock};

use ai::agent::{ToolCallOutcome, TurnEvent, TurnUsage};
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
    remove_turn_from_messages, turn_user_message, without_turn_ids, McpToolCaller, McpToolSpec,
};
use crate::ai::api_providers::{
    api_model_parts, model_context_window, model_display_name, model_price, ApiProvider,
    FailoverTarget,
};

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

/// How much of one tool result the model is sent: a whole read window and
/// its header. Anything bigger (a huge command dump) keeps head and tail.
fn max_tool_result_chars(context_window: u64) -> usize {
    read_window_chars(context_window) + 2_000
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

/// curl arguments for one streamed chat-completions request to `provider`.
pub fn api_curl_args(provider: &ApiProvider) -> Vec<String> {
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
        format!("{}/chat/completions", provider.base_url),
    ]
}

fn new_api_command(provider: &ApiProvider, api_key: &str, working_dir: &Path, path: &str) -> Command {
    let mut command = Command::new("/usr/bin/curl");
    command.args(api_curl_args(provider));
    command.env(API_KEY_ENV, api_key);
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

/// Runs one turn against a cloud model. `first_command` is the curl command
/// for the first request (built by the caller with [`api_curl_args`] and the
/// key in [`API_KEY_ENV`]); later requests in the turn build their own.
///
/// When a request fails — out of credit, rate limited, the provider down —
/// the same request goes to the next of `failover` (empty when BYOR failover
/// is off), and the rest of the turn stays there. Each is tried once; the
/// turn fails only when none is left.
///
/// There is no step limit: the turn runs until the model answers or the user
/// stops it.
#[allow(clippy::too_many_arguments)]
pub async fn run_api_stream(
    first_command: Command,
    prompt: &str,
    model_id: &str,
    api_key: &str,
    session_id: &Uuid,
    working_dir: &Path,
    path: &str,
    mcp_tools: &[McpToolSpec],
    tool_caller: Option<Arc<dyn McpToolCaller>>,
    failover: &[FailoverTarget],
    mut on_event: impl FnMut(TurnEvent),
) -> std::io::Result<ExitStatus> {
    register_harness_session(*session_id);
    let _session_guard = SessionGuard(*session_id);

    // Opens the exchange straight away, so the prompt and the Sending…
    // shimmer show while the first request is in flight.
    on_event(TurnEvent::SessionInit {
        session_id: session_id.to_string(),
        model: Some(model_id.to_string()),
        cwd: Some(working_dir.to_string_lossy().into_owned()),
    });

    let Some((mut provider, mut model)) = api_model_parts(model_id) else {
        let message = format!("'{model_id}' is not a cloud model this build knows");
        emit_failed_turn(&mut on_event, message.clone());
        return Err(std::io::Error::other(message));
    };
    let mut model_id = model_id;
    let mut api_key = api_key;
    let mut failover = failover.iter().filter_map(|target| {
        api_model_parts(&target.model_id).map(|(provider, model)| (target, provider, model))
    });

    let tools: Vec<McpToolSpec> = builtin_local_tool_specs()
        .into_iter()
        .chain(mcp_tools.iter().cloned())
        .collect();
    let tool_defs: Vec<Value> = tools.iter().map(McpToolSpec::to_ollama_tool_json).collect();

    // Rebuilt every turn, so its opening line names the model now in use —
    // the conversation may have started on another one.
    let system_for = |provider: &ApiProvider, model: &str| {
        json!({
            "role": "system",
            "content": ollama_tool_system_prompt(
                &tools,
                &format!(
                    "{} ({model}) from {}",
                    model_display_name(provider, model),
                    provider.display_name
                ),
            ),
        })
    };
    let system = system_for(provider, model);
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

    let mut price = model_price(provider, model);
    let mut listed_window = model_context_window(provider, model);
    let mut context_window = listed_window.unwrap_or(DEFAULT_CONTEXT_WINDOW);
    // What every request of this turn cost, at the model's listed price.
    let mut turn_cost_usd = 0.0;
    let mut next_command = Some(first_command);
    loop {
        compact_old_tool_results(&mut messages, tool_output_budget_chars(context_window));
        let request = json!({
            "model": model,
            "messages": without_turn_ids(&messages),
            "tools": tool_defs,
            "stream": true,
        })
        .to_string();

        let command = next_command
            .take()
            .unwrap_or_else(|| new_api_command(provider, api_key, working_dir, path));
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
            return Ok(status);
        }

        let response = if status.success() {
            accumulator
                .lock()
                .map(|accumulator| accumulator.clone().finish())
                .unwrap_or_else(|_| Err("the response state became unavailable".to_string()))
                .map_err(|error| format!("{}: {error}", provider.display_name))
        } else {
            Err(curl_failure_message(provider, status))
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                let Some((target, next_provider, next_model)) = failover.next() else {
                    emit_failed_turn(&mut on_event, error);
                    return Ok(status);
                };
                // The failed request added nothing to `messages`, so the
                // next provider gets exactly the same one.
                on_event(TurnEvent::Text {
                    text: format!(
                        "{error} — switching to {} on {}.",
                        model_display_name(next_provider, next_model),
                        next_provider.display_name
                    ),
                });
                log::info!(
                    "api_stream: {} failed; failing over to {}",
                    provider.display_name,
                    next_provider.display_name
                );
                provider = next_provider;
                model = next_model;
                model_id = &target.model_id;
                api_key = &target.api_key;
                price = model_price(provider, model);
                listed_window = model_context_window(provider, model);
                context_window = listed_window.unwrap_or(DEFAULT_CONTEXT_WINDOW);
                messages[0] = system_for(provider, model);
                continue;
            }
        };
        if let Some((input_price, output_price)) = price {
            turn_cost_usd += (response.prompt_tokens as f64 * input_price
                + response.completion_tokens as f64 * output_price)
                / 1_000_000.0;
        }
        // Token counts are the last request's — they say how full the
        // context is — while the cost covers every request in the turn.
        let usage = TurnUsage {
            input_tokens: response.prompt_tokens,
            output_tokens: response.completion_tokens,
            model_id: Some(model_id.to_string()),
            cost_usd: price.map(|_| turn_cost_usd),
            context_window: listed_window,
            ..Default::default()
        };

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
            return Ok(status);
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
                        read_window_chars(context_window),
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
                "content": clip_for_model(&tool_content, max_tool_result_chars(context_window)),
            }));
        }
        // Every call has its result now, so the history is a valid
        // conversation to resume from — keep it, in case the turn ends here.
        save_history(session_id, &messages);
    }
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
fn curl_failure_message(provider: &ApiProvider, status: ExitStatus) -> String {
    match status.code() {
        // An option this curl doesn't know: `--variable` and
        // `--expand-header` arrived in curl 8.3.
        Some(2) => format!(
            "Could not call {}: this machine's curl is too old (cloud models need curl 8.3 or \
             newer)",
            provider.display_name
        ),
        Some(6) | Some(7) => format!(
            "Could not reach {} — check the internet connection",
            provider.display_name
        ),
        Some(28) => format!("{} took too long to answer", provider.display_name),
        code => format!(
            "Could not reach {} (curl exited with {})",
            provider.display_name,
            code.map_or_else(|| "a signal".to_string(), |code| code.to_string())
        ),
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
