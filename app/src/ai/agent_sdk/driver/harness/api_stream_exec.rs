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
    builtin_local_tool_specs, dispatch_builtin_tool, emit_failed_turn, emit_step_limit_reached,
    is_builtin_local_tool, ollama_tool_system_prompt, parse_text_tool_call, McpToolCaller,
    McpToolSpec, MAX_TOOL_CALLS_PER_TURN,
};
use crate::ai::api_providers::{api_model_parts, ApiProvider};

/// The environment variable curl reads the key from.
pub const API_KEY_ENV: &str = "KRUST_API_KEY";

/// The header curl expands from [`API_KEY_ENV`]. A literal rather than a
/// `format!` because the `{{…}}` is curl's syntax, not Rust's.
const AUTHORIZATION_HEADER: &str = "Authorization: Bearer {{KRUST_API_KEY}}";

/// How much of one tool result the model is sent. These models have large
/// windows, but free tiers count tokens per day, so a 60 KB file dump would
/// spend a day's allowance in a few calls. The head and tail survive.
const MAX_TOOL_RESULT_CHARS: usize = 24_000;

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

/// Per-session message history, so follow-ups keep their context. Kept for
/// the life of the app, like the Ollama runner's.
type ApiChatHistories = HashMap<Uuid, (String, Vec<Value>)>;

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

    let Some((provider, model)) = api_model_parts(model_id) else {
        let message = format!("'{model_id}' is not a cloud model this build knows");
        emit_failed_turn(&mut on_event, message.clone());
        return Err(std::io::Error::other(message));
    };

    let tools: Vec<McpToolSpec> = builtin_local_tool_specs()
        .into_iter()
        .chain(mcp_tools.iter().cloned())
        .collect();
    let tool_defs: Vec<Value> = tools.iter().map(McpToolSpec::to_ollama_tool_json).collect();

    let mut messages = chat_histories()
        .lock()
        .ok()
        .and_then(|histories| histories.get(session_id).cloned())
        .filter(|(saved_model, _)| saved_model == model_id)
        .map(|(_, messages)| messages)
        .unwrap_or_else(|| {
            vec![json!({ "role": "system", "content": ollama_tool_system_prompt(&tools) })]
        });
    messages.push(json!({
        "role": "user",
        "content": format!(
            "Current working directory: {}\n\nUser request:\n{prompt}",
            working_dir.display()
        ),
    }));

    let mut next_command = Some(first_command);
    for call_index in 0..=MAX_TOOL_CALLS_PER_TURN {
        let request = json!({
            "model": model,
            "messages": messages,
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

        if !status.success() {
            emit_failed_turn(&mut on_event, curl_failure_message(provider, status));
            return Ok(status);
        }

        let response = accumulator
            .lock()
            .map(|accumulator| accumulator.clone().finish())
            .unwrap_or_else(|_| Err("the response state became unavailable".to_string()));
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                emit_failed_turn(&mut on_event, format!("{}: {error}", provider.display_name));
                return Ok(status);
            }
        };
        let usage = TurnUsage {
            input_tokens: response.prompt_tokens,
            output_tokens: response.completion_tokens,
            model_id: Some(model_id.to_string()),
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
                }],
            )
        } else {
            if !response.content.trim().is_empty() {
                on_event(TurnEvent::Text {
                    text: response.content.clone(),
                });
            }
            messages.push(json!({ "role": "assistant", "content": response.content }));
            save_history(session_id, model_id, messages);
            on_event(TurnEvent::TurnCompleted {
                usage: Some(usage),
                is_error: false,
                error_message: None,
            });
            return Ok(status);
        };

        if call_index == MAX_TOOL_CALLS_PER_TURN {
            save_history(session_id, model_id, messages);
            emit_step_limit_reached(&mut on_event);
            return Ok(status);
        }

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
                    dispatch_builtin_tool(&call.name, arguments, working_dir, path).await
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
                "content": clip_for_model(&tool_content),
            }));
        }
    }

    unreachable!("the bounded tool-call loop always returns")
}

fn save_history(session_id: &Uuid, model_id: &str, messages: Vec<Value>) {
    if let Ok(mut histories) = chat_histories().lock() {
        histories.insert(*session_id, (model_id.to_string(), messages));
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
            .map(|call| json!({
                "id": call.id,
                "type": "function",
                "function": {
                    "name": call.name,
                    "arguments": match &call.arguments {
                        Ok(arguments) => arguments.to_string(),
                        Err(_) => "{}".to_string(),
                    },
                },
            }))
            .collect::<Vec<_>>(),
    })
}

/// The head and tail of a tool result too long to send whole.
fn clip_for_model(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= MAX_TOOL_RESULT_CHARS {
        return text.to_string();
    }
    let half = MAX_TOOL_RESULT_CHARS / 2;
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
