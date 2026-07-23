use std::path::Path;

use ai::agent::{ToolCallOutcome, TurnEvent};
use command::r#async::Command;
use futures_lite::future::block_on;
use uuid::Uuid;

use serde_json::{json, Value};

use super::{
    base_url_of, dispatch_builtin_tool, execute_shell, ollama_curl_args, ollama_generate_request,
    ollama_model_name, ollama_serve_args, run_ollama_stream_without_service_start, McpToolCaller,
    McpToolSpec, OllamaAgentReply, OllamaResponseAccumulator, CODELLAMA_OLLAMA_MODEL_ID,
    DEEPSEEK_CODER_OLLAMA_MODEL_ID,
};

struct FakeToolCaller;

#[async_trait::async_trait]
impl McpToolCaller for FakeToolCaller {
    async fn call(&self, name: &str, arguments: Value) -> Result<String, String> {
        match name {
            "web_search" => Ok(format!("results for {}", arguments["query"])),
            other => Err(format!("no such tool: {other}")),
        }
    }
}

fn web_search_tool_spec() -> McpToolSpec {
    McpToolSpec {
        name: "web_search".to_string(),
        description: "Search the web".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": { "query": { "type": "string" } },
            "required": ["query"],
        }),
    }
}

#[test]
fn picker_id_maps_to_native_ollama_model_name() {
    assert_eq!(
        ollama_model_name(CODELLAMA_OLLAMA_MODEL_ID),
        Some("codellama:13b")
    );
    assert_eq!(ollama_model_name("gpt-5"), None);
}

#[test]
fn headless_service_uses_the_cli_without_opening_ollama_app() {
    assert_eq!(ollama_serve_args(), vec!["serve"]);
}

#[test]
fn curl_posts_structured_streaming_request_body_from_stdin() {
    assert_eq!(
        ollama_curl_args(crate::ai::ollama::service::API_BASE),
        vec![
            "--silent",
            "--show-error",
            "--no-buffer",
            "--header",
            "Content-Type: application/json",
            "--data-binary",
            "@-",
            "http://127.0.0.1:11434/api/generate",
        ]
    );

    // A model on another machine is reached at that machine's address.
    assert_eq!(
        ollama_curl_args(base_url_of("codellama:13b@@http://192.168.1.150:11434")).last(),
        Some(&"http://192.168.1.150:11434/api/generate".to_string())
    );

    let request: serde_json::Value = serde_json::from_str(&ollama_generate_request(
        "codellama:13b",
        "quote: \"",
        Some(&[10, 20]),
        "You are the agent. Ordinary local developer commands are permitted.",
    ))
    .unwrap();
    assert_eq!(request["model"], "codellama:13b");
    assert_eq!(request["prompt"], "quote: \"");
    assert_eq!(request["stream"], true);
    // No `format` grammar constraint — it crushed output on weaker models.
    assert!(request.get("format").is_none());
    assert_eq!(request["keep_alive"], "5s");
    // A generous context window is set (fixes the tiny-default "lack of
    // context"); other runtime knobs stay at Ollama's defaults.
    assert_eq!(request["options"]["num_ctx"], 8192);
    assert!(request["options"].get("num_predict").is_none());
    assert!(request["options"].get("num_thread").is_none());
    assert!(request["options"].get("num_batch").is_none());
    assert_eq!(request["options"]["temperature"], 0);
    assert_eq!(request["context"], serde_json::json!([10, 20]));
    // The system prompt passed in is forwarded verbatim.
    assert!(request["system"]
        .as_str()
        .is_some_and(|system| system.contains("Ordinary local developer commands")));
}

#[test]
fn buffers_response_and_emits_nothing_while_streaming() {
    let mut accumulator = OllamaResponseAccumulator::default();
    let first = serde_json::json!({
        "response": "{\"type\":\"text\",",
        "done": false,
    })
    .to_string();
    let second = serde_json::json!({
        "response": "\"text\":\"OK\"}",
        "done": true,
        "prompt_eval_count": 12,
        "eval_count": 3,
        "total_duration": 2_500_000,
        "context": [10, 20],
    })
    .to_string();

    // No synthetic thinking and no token-level streaming — the whole answer
    // is delivered by `finish`.
    assert!(accumulator.push_line(&first).is_empty());
    assert!(accumulator.push_line(&second).is_empty());

    let result = accumulator.finish("codellama:13b").unwrap();
    assert_eq!(
        result.reply,
        OllamaAgentReply::Text {
            text: "OK".to_string()
        }
    );
    assert_eq!(result.context, Some(vec![10, 20]));
    assert_eq!(result.usage.input_tokens, 12);
    assert_eq!(result.usage.output_tokens, 3);
    assert_eq!(result.usage.duration_ms, Some(2));
}

#[test]
fn finish_decodes_the_full_json_text_including_escapes() {
    let mut accumulator = OllamaResponseAccumulator::default();
    accumulator.push_line(
        &serde_json::json!({
            "response": "{\"type\":\"text\",\"text\":\"alpha beta gamma delta epsilon",
            "done": false,
        })
        .to_string(),
    );
    accumulator.push_line(
        &serde_json::json!({
            "response": " zeta\\nreal line\"}",
            "done": true,
        })
        .to_string(),
    );

    // The whole answer — including the escaped newline — comes back intact at
    // finish; nothing is dropped from the tail.
    let result = accumulator.finish("codellama:13b").unwrap();
    assert_eq!(
        result.reply,
        OllamaAgentReply::Text {
            text: "alpha beta gamma delta epsilon zeta\nreal line".to_string()
        }
    );
}

#[test]
fn sets_context_window_but_leaves_other_runtime_knobs_default() {
    for model_id in [CODELLAMA_OLLAMA_MODEL_ID, DEEPSEEK_CODER_OLLAMA_MODEL_ID] {
        let model = ollama_model_name(model_id).unwrap();
        let request: serde_json::Value =
            serde_json::from_str(&ollama_generate_request(model, "test", None, "sys")).unwrap();
        // We do raise the context window (the tiny default caused "lack of
        // context"), but leave output length, threads, and batch to Ollama.
        assert_eq!(request["options"]["num_ctx"], 8192, "{model}");
        assert!(request["options"].get("num_predict").is_none(), "{model}");
        assert!(request["options"].get("num_thread").is_none(), "{model}");
        assert!(request["options"].get("num_batch").is_none(), "{model}");
    }
}

#[test]
fn structured_shell_reply_is_recognized() {
    let mut accumulator = OllamaResponseAccumulator::default();
    let response = serde_json::json!({
        "response": r#"{"type":"shell","command":"ls -la"}"#,
        "done": true,
    })
    .to_string();
    accumulator.push_line(&response);

    assert_eq!(
        accumulator.finish("deepseek-coder:6.7b").unwrap().reply,
        OllamaAgentReply::Shell {
            command: "ls -la".to_string()
        }
    );
}

#[test]
fn structured_search_reply_is_recognized() {
    let mut accumulator = OllamaResponseAccumulator::default();
    accumulator.push_line(
        &serde_json::json!({
            "response": r#"{"type":"search","query":"rust 2024 edition"}"#,
            "done": true,
        })
        .to_string(),
    );
    assert_eq!(
        accumulator.finish("codellama:13b").unwrap().reply,
        super::OllamaAgentReply::Search {
            query: "rust 2024 edition".to_string()
        }
    );
}

#[test]
fn structured_file_actions_are_recognized() {
    let mut read = OllamaResponseAccumulator::default();
    read.push_line(
        &serde_json::json!({
            "response": r#"{"type":"read_file","path":"src/main.rs"}"#,
            "done": true,
        })
        .to_string(),
    );
    assert_eq!(
        read.finish("codellama:13b").unwrap().reply,
        super::OllamaAgentReply::ReadFile {
            path: "src/main.rs".to_string()
        }
    );

    let mut write = OllamaResponseAccumulator::default();
    write.push_line(
        &serde_json::json!({
            "response": r#"{"type":"write_file","path":"notes.txt","content":"hello"}"#,
            "done": true,
        })
        .to_string(),
    );
    assert_eq!(
        write.finish("codellama:13b").unwrap().reply,
        super::OllamaAgentReply::WriteFile {
            path: "notes.txt".to_string(),
            content: "hello".to_string()
        }
    );
}

#[test]
fn web_search_intent_heuristic() {
    use super::wants_web_search;
    assert!(wants_web_search("search for therob on duckduckgo"));
    assert!(wants_web_search("what's the LATEST version of rust?"));
    assert!(wants_web_search("look up the price of a switch 2"));
    assert!(wants_web_search("summarise https://example.com/article"));
    assert!(!wants_web_search("write a haiku about the sea"));
    assert!(!wants_web_search("explain how a hash map works"));
    // Local-machine prompts never search, even when a cue word appears —
    // "current directory" is terminal talk, not live-information intent.
    // (Regression: this exact prompt was thrown wholesale at DuckDuckGo.)
    assert!(!wants_web_search(
        "its called ubuntu@15.204.244.136 and its in the current directory. \
         i wanted to know if the key inside it exists in my .ssh folder"
    ));
    assert!(!wants_web_search("what's in my current directory?"));
    assert!(!wants_web_search("look up that function in ~/code/main.rs"));
    assert!(!wants_web_search("search for TODOs in this folder"));
}

#[test]
fn session_starts_before_model_progress_and_text() {
    let native_response = serde_json::json!({
        "response": r#"{"type":"text","text":"OK"}"#,
        "done": true,
        "prompt_eval_count": 4,
        "eval_count": 2,
        "total_duration": 1_000_000,
    })
    .to_string();
    let mut command = Command::new("sh");
    command.args([
        "-c",
        "cat >/dev/null; printf '%s\\n' \"$OLLAMA_TEST_RESPONSE\"",
    ]);
    command.env("OLLAMA_TEST_RESPONSE", native_response);

    let mut events = Vec::new();
    let status = block_on(run_ollama_stream_without_service_start(
        command,
        "say OK",
        "codellama:13b",
        &Uuid::new_v4(),
        Path::new("/tmp"),
        "/usr/bin:/bin",
        &[],
        None,
        // Force agent mode so this exercises the /api/generate shell/text
        // path against the mock command (chat-only would hit real /api/chat).
        Some(true),
        |event| events.push(event),
    ))
    .unwrap();

    assert!(status.success());
    assert!(matches!(events[0], TurnEvent::SessionInit { .. }));
    // The answer arrives as one complete Text event — no synthetic thinking,
    // no streamed deltas.
    assert_eq!(events[1], TurnEvent::Text { text: "OK".into() });
    assert!(matches!(
        events[2],
        TurnEvent::TurnCompleted {
            is_error: false,
            ..
        }
    ));
}

#[test]
fn shell_execution_captures_multiline_output_for_warp_tool_card() {
    let execution = block_on(execute_shell(
        "printf 'one\\ntwo\\n'",
        Path::new("/tmp"),
        "/usr/bin:/bin",
    ));
    assert_eq!(execution.stdout, "one\ntwo\n");
    assert_eq!(execution.stderr, "");
    assert_eq!(execution.exit_code, 0);
    match execution.outcome() {
        ToolCallOutcome::Success(value) => {
            assert_eq!(value["stdout"], "one\ntwo\n");
            assert_eq!(value["stderr"], "");
        }
        other => panic!("expected successful shell output, got {other:?}"),
    }
}

#[test]
fn shell_execution_never_buffers_unbounded_output() {
    let execution = block_on(execute_shell(
        "yes x | head -c 70000",
        Path::new("/tmp"),
        "/usr/bin:/bin",
    ));
    assert_eq!(execution.exit_code, 0);
    assert!(execution.stdout.contains("[output truncated"));
    assert!(
        execution.stdout.len() < 66_000,
        "{}",
        execution.stdout.len()
    );
}

#[test]
fn mcp_tool_specs_become_ollama_function_definitions() {
    let tool_json = web_search_tool_spec().to_ollama_tool_json();
    assert_eq!(tool_json["type"], "function");
    assert_eq!(tool_json["function"]["name"], "web_search");
    assert_eq!(tool_json["function"]["description"], "Search the web");
    assert_eq!(
        tool_json["function"]["parameters"]["properties"]["query"]["type"],
        "string"
    );
}

#[test]
fn chat_accumulator_collects_streamed_text_and_final_tool_call() {
    use super::OllamaChatAccumulator;

    let mut accumulator = OllamaChatAccumulator::default();
    let first = json!({
        "message": { "role": "assistant", "content": "Looking that up" },
        "done": false,
    })
    .to_string();
    let second = json!({
        "message": {
            "role": "assistant",
            "content": "",
            "tool_calls": [
                { "function": { "name": "web_search", "arguments": { "query": "rust 2024" } } }
            ],
        },
        "done": true,
        "prompt_eval_count": 10,
        "eval_count": 4,
        "total_duration": 3_000_000,
    })
    .to_string();

    let first_events = accumulator.push_line(&first);
    // Content is buffered, not streamed, and non-reasoning models get no
    // synthetic thinking — nothing is emitted mid-stream, so a tool call
    // emitted as JSON text never reaches the user.
    assert!(first_events.is_empty(), "{first_events:?}");
    assert_eq!(accumulator.push_line(&second), Vec::new());

    let result = accumulator.finish("qwen2.5-coder:7b").unwrap();
    assert_eq!(result.content, "Looking that up");
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].name, "web_search");
    assert_eq!(result.tool_calls[0].arguments["query"], "rust 2024");
    assert_eq!(result.usage.input_tokens, 10);
    assert_eq!(result.usage.output_tokens, 4);
}

#[test]
fn chat_turn_without_tool_calls_completes_directly() {
    let response = json!({
        "message": { "role": "assistant", "content": "It's sunny." },
        "done": true,
        "prompt_eval_count": 5,
        "eval_count": 2,
    })
    .to_string();
    let mut command = Command::new("sh");
    command.args([
        "-c",
        "cat >/dev/null; printf '%s\\n' \"$OLLAMA_TEST_RESPONSE\"",
    ]);
    command.env("OLLAMA_TEST_RESPONSE", response);

    let mut events = Vec::new();
    let outcome = block_on(super::run_ollama_chat_with_tools(
        command,
        "what's the weather",
        "qwen2.5-coder:7b",
        &Uuid::new_v4(),
        Path::new("/tmp"),
        "/usr/bin:/bin",
        &[web_search_tool_spec()],
        Some(&FakeToolCaller),
        &mut |event| events.push(event),
    ))
    .unwrap();

    assert!(matches!(outcome, super::ToolChatOutcome::Completed(status) if status.success()));
    // The answer, then completion — no synthetic thinking row.
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(
        events[0],
        TurnEvent::Text {
            text: "It's sunny.".to_string()
        }
    );
    match &events[1] {
        TurnEvent::TurnCompleted {
            usage: Some(usage),
            is_error: false,
            error_message: None,
        } => {
            assert_eq!(usage.input_tokens, 5);
            assert_eq!(usage.output_tokens, 2);
        }
        other => panic!("expected a successful TurnCompleted, got {other:?}"),
    }
}

#[test]
fn chat_turn_falls_back_when_model_lacks_tool_support() {
    let response = json!({ "error": "\"qwen2.5-coder:7b\" does not support tools" }).to_string();
    let mut command = Command::new("sh");
    command.args([
        "-c",
        "cat >/dev/null; printf '%s\\n' \"$OLLAMA_TEST_RESPONSE\"",
    ]);
    command.env("OLLAMA_TEST_RESPONSE", response);

    let mut events = Vec::new();
    let outcome = block_on(super::run_ollama_chat_with_tools(
        command,
        "what's the weather",
        "qwen2.5-coder:7b",
        &Uuid::new_v4(),
        Path::new("/tmp"),
        "/usr/bin:/bin",
        &[web_search_tool_spec()],
        Some(&FakeToolCaller),
        &mut |event| events.push(event),
    ))
    .unwrap();

    assert!(matches!(
        outcome,
        super::ToolChatOutcome::UnsupportedByModel
    ));
    assert!(events.is_empty(), "{events:?}");
}

#[test]
fn fake_tool_caller_dispatches_by_name() {
    let ok = block_on(FakeToolCaller.call("web_search", json!({ "query": "warp" })));
    assert_eq!(ok, Ok("results for \"warp\"".to_string()));

    let err = block_on(FakeToolCaller.call("unknown_tool", Value::Null));
    assert_eq!(err, Err("no such tool: unknown_tool".to_string()));
}

#[test]
fn builtin_tool_specs_cover_the_in_harness_tools() {
    use super::{builtin_local_tool_specs, is_builtin_local_tool};
    let names: Vec<String> = builtin_local_tool_specs()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(names, vec!["read_file", "write_file", "run_code"]);
    assert!(is_builtin_local_tool("run_code"));
    assert!(!is_builtin_local_tool("web_search"));
}

#[test]
fn builtin_file_tools_write_then_read_round_trip() {
    use super::dispatch_builtin_tool;
    // Paths resolve under the working dir, so a relative path is enough.
    let dir = std::env::temp_dir().join(format!("derp-ollama-test-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();

    let write = block_on(dispatch_builtin_tool(
        "write_file",
        &json!({ "path": "note.txt", "content": "hello world" }),
        &dir,
        "/usr/bin:/bin",
    ));
    assert!(write.is_ok(), "{write:?}");

    let read = block_on(dispatch_builtin_tool(
        "read_file",
        &json!({ "path": "note.txt" }),
        &dir,
        "/usr/bin:/bin",
    ));
    assert_eq!(read, Ok("hello world".to_string()));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_paths_expand_tilde_to_home() {
    use super::resolve_under;
    let home = dirs::home_dir().expect("home dir exists in tests");
    // "~/.ssh/config" must reach the real home, not "<cwd>/~/.ssh/config".
    assert_eq!(
        resolve_under(Path::new("/tmp"), "~/.ssh/config"),
        home.join(".ssh/config")
    );
    assert_eq!(resolve_under(Path::new("/tmp"), "~"), home);
    // Everything else keeps the existing rules.
    assert_eq!(
        resolve_under(Path::new("/tmp"), "notes.txt"),
        Path::new("/tmp/notes.txt")
    );
    assert_eq!(
        resolve_under(Path::new("/tmp"), "/etc/hosts"),
        Path::new("/etc/hosts")
    );
}

#[test]
fn read_file_on_a_directory_returns_its_listing() {
    use super::dispatch_builtin_tool;
    // Models regularly ask to "read" a folder; that must answer with the
    // listing, not burn the tool call on an "Is a directory" error.
    let dir = std::env::temp_dir().join(format!("derp-ollama-dir-{}", Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("keys")).unwrap();
    std::fs::write(dir.join("config"), "x").unwrap();

    let listing = block_on(dispatch_builtin_tool(
        "read_file",
        &json!({ "path": dir.to_string_lossy() }),
        Path::new("/tmp"),
        "/usr/bin:/bin",
    ))
    .expect("directory read succeeds");
    assert!(listing.contains("is a directory"), "{listing}");
    assert!(listing.contains("config"), "{listing}");
    // Subdirectories are marked with a trailing slash.
    assert!(listing.contains("keys/"), "{listing}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_code_runs_bash_statelessly() {
    use super::run_code;
    let execution = block_on(run_code(
        "bash",
        "printf 'hi'",
        Path::new("/tmp"),
        "/usr/bin:/bin",
    ));
    assert_eq!(execution.exit_code, 0);
    assert_eq!(execution.stdout, "hi");
}

#[test]
fn run_code_rejects_unknown_language() {
    use super::run_code;
    let execution = block_on(run_code(
        "cobol",
        "DISPLAY 1",
        Path::new("/tmp"),
        "/usr/bin:/bin",
    ));
    assert_eq!(execution.exit_code, 1);
    assert!(
        execution.stderr.contains("unsupported"),
        "{}",
        execution.stderr
    );
}

#[test]
fn recovers_tool_call_emitted_as_json_text() {
    use super::parse_text_tool_call;
    let tools = [web_search_tool_spec()];

    // qwen2.5-coder emits the call as JSON text with `tool_calls` null.
    let call = parse_text_tool_call(
        "{\n  \"name\": \"web_search\",\n  \"arguments\": {\"query\": \"rust\"}\n}",
        &tools,
    )
    .expect("recovers the call from text");
    assert_eq!(call.name, "web_search");
    assert_eq!(call.arguments["query"], "rust");

    // Tolerates a code fence and surrounding prose.
    assert!(parse_text_tool_call(
        "Sure:\n```json\n{\"name\":\"web_search\",\"arguments\":{\"query\":\"x\"}}\n```",
        &tools,
    )
    .is_some());

    // Plain prose isn't a tool call.
    assert!(parse_text_tool_call("Here's your answer.", &tools).is_none());
    // A JSON object naming an unknown tool is left alone.
    assert!(parse_text_tool_call(r#"{"name":"nope","arguments":{}}"#, &tools).is_none());
}

#[test]
fn tool_chat_resumes_session_history_across_turns() {
    use super::{chat_histories, run_ollama_chat_with_tools};

    let session = Uuid::new_v4();
    let make_command = |answer: &str| {
        let response = json!({
            "message": { "role": "assistant", "content": answer },
            "done": true,
            "prompt_eval_count": 1,
            "eval_count": 1,
        })
        .to_string();
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "cat >/dev/null; printf '%s\\n' \"$OLLAMA_TEST_RESPONSE\"",
        ]);
        command.env("OLLAMA_TEST_RESPONSE", response);
        command
    };

    // Turn 1.
    block_on(run_ollama_chat_with_tools(
        make_command("First answer."),
        "first question",
        "qwen2.5-coder:7b",
        &session,
        Path::new("/tmp"),
        "/usr/bin:/bin",
        &[],
        Some(&FakeToolCaller),
        &mut |_| {},
    ))
    .unwrap();

    // Turn 2 in the same session must resume turn 1's messages.
    block_on(run_ollama_chat_with_tools(
        make_command("Second answer."),
        "second question",
        "qwen2.5-coder:7b",
        &session,
        Path::new("/tmp"),
        "/usr/bin:/bin",
        &[],
        Some(&FakeToolCaller),
        &mut |_| {},
    ))
    .unwrap();

    let (_, messages) = chat_histories()
        .lock()
        .unwrap()
        .get(&session)
        .cloned()
        .expect("session history is stored");
    let has = |needle: &str| {
        messages
            .iter()
            .any(|m| m["content"].as_str().is_some_and(|c| c.contains(needle)))
    };
    // Both turns' questions and answers are retained → context carries over.
    assert!(
        has("first question"),
        "turn 1 question retained: {messages:?}"
    );
    assert!(has("First answer."), "turn 1 answer retained");
    assert!(has("second question"), "turn 2 question present");
    assert!(has("Second answer."), "turn 2 answer stored");
}

#[test]
fn a_rewrite_that_loses_behaviour_is_refused_and_the_file_kept() {
    use futures_lite::future::block_on;

    let dir = std::env::temp_dir().join(format!("ollama-write-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    let working = "def wrap(text, width):\n\
                   \x20   lines, current = [], ''\n\
                   \x20   for word in text.split():\n\
                   \x20       if not current:\n\
                   \x20           current = word\n\
                   \x20       elif len(current) + 1 + len(word) <= width:\n\
                   \x20           current += ' ' + word\n\
                   \x20       else:\n\
                   \x20           lines.append(current)\n\
                   \x20           current = word\n\
                   \x20   if current:\n\
                   \x20       lines.append(current)\n\
                   \x20   return lines\n";
    std::fs::write(dir.join("w.py"), working).unwrap();

    let write = |content: &str| {
        block_on(dispatch_builtin_tool(
            "write_file",
            &serde_json::json!({ "path": "w.py", "content": content }),
            &dir,
            &path,
        ))
    };

    // What the guard promises is narrow and deliberate: it catches a
    // rewrite that LOSES behaviour, not one that changes it — an edit is
    // usually asked to change something, and blocking that would make the
    // tool useless. So: a call that used to answer now raises. This is the
    // fireworks script's failure exactly, a name belonging to another
    // function, and the commonest way a whole-file rewrite destroys a file.
    let broken = "def wrap(text, width):\n    return joiner(text, width)\n";
    let refused = write(broken).expect_err("a rewrite that loses behaviour is refused");
    assert!(refused.contains("loses behaviour"), "{refused}");
    assert_eq!(
        std::fs::read_to_string(dir.join("w.py")).unwrap(),
        working,
        "the file it refused to break is left exactly as it was"
    );

    // Restructuring freely is fine as long as the behaviour survives — the
    // guard judges what the code does, never how it is written.
    let tidied = "def wrap(text, width):\n\
                  \x20   lines = []\n\
                  \x20   current = ''\n\
                  \x20   for word in text.split():\n\
                  \x20       candidate = f'{current} {word}'.strip()\n\
                  \x20       if current and len(candidate) > width:\n\
                  \x20           lines.append(current)\n\
                  \x20           current = word\n\
                  \x20       else:\n\
                  \x20           current = candidate\n\
                  \x20   if current:\n\
                  \x20       lines.append(current)\n\
                  \x20   return lines\n";
    assert!(write(tidied).is_ok(), "an equivalent rewrite goes through");

    // And so does one that deliberately changes what the code does — the
    // guard is not a spec, and an edit that was asked for is not a loss.
    let changed = "def wrap(text, width):\n    return [w.upper() for w in text.split()]\n";
    assert!(
        write(changed).is_ok(),
        "a deliberate behaviour change is allowed"
    );
    assert!(write(tidied).is_ok(), "and can be changed back");
    assert_eq!(std::fs::read_to_string(dir.join("w.py")).unwrap(), tidied);

    // A file that did not exist has no behaviour to lose.
    assert!(write_new(&dir, &path));

    let _ = std::fs::remove_dir_all(&dir);
}

/// Writing a brand-new python file is never blocked by the guard.
fn write_new(dir: &std::path::Path, path: &str) -> bool {
    use futures_lite::future::block_on;
    block_on(dispatch_builtin_tool(
        "write_file",
        &serde_json::json!({ "path": "fresh.py", "content": "def f():\n    return 1\n" }),
        dir,
        path,
    ))
    .is_ok()
}
