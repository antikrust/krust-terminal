use std::path::Path;

use ai::agent::{ToolCallOutcome, TurnEvent};
use command::r#async::Command;
use futures_lite::future::block_on;
use uuid::Uuid;

use super::{
    execute_shell, ollama_curl_args, ollama_generate_request, ollama_model_name, ollama_serve_args,
    run_ollama_stream_without_service_start, OllamaAgentReply, OllamaResponseAccumulator,
    CODELLAMA_OLLAMA_MODEL_ID, DEEPSEEK_CODER_OLLAMA_MODEL_ID,
};

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
        ollama_curl_args(),
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

    let request: serde_json::Value = serde_json::from_str(&ollama_generate_request(
        "codellama:13b",
        "quote: \"",
        Some(&[10, 20]),
    ))
    .unwrap();
    assert_eq!(request["model"], "codellama:13b");
    assert_eq!(request["prompt"], "quote: \"");
    assert_eq!(request["stream"], true);
    assert_eq!(request["format"]["properties"]["type"]["enum"][0], "shell");
    assert_eq!(request["keep_alive"], "5s");
    assert!(request["options"].get("num_ctx").is_none());
    assert!(request["options"].get("num_predict").is_none());
    assert!(request["options"].get("num_thread").is_none());
    assert!(request["options"].get("num_batch").is_none());
    assert_eq!(request["options"]["temperature"], 0);
    assert_eq!(request["context"], serde_json::json!([10, 20]));
    assert!(request["system"]
        .as_str()
        .is_some_and(|system| system.contains("Ordinary local developer commands")));
}

#[test]
fn streamed_tokens_are_coalesced_into_one_text_reply() {
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

    let first_events = accumulator.push_line(&first);
    let second_events = accumulator.push_line(&second);
    assert!(matches!(
        first_events.as_slice(),
        [TurnEvent::Thinking { .. }]
    ));
    assert_eq!(
        second_events,
        vec![TurnEvent::TextDelta {
            text: "OK".to_string()
        }]
    );

    let result = accumulator.finish("codellama:13b").unwrap();
    assert_eq!(
        result.reply,
        OllamaAgentReply::Text {
            text: "OK".to_string()
        }
    );
    assert_eq!(result.context, Some(vec![10, 20]));
    assert_eq!(result.emitted_text, "OK");
    assert_eq!(result.usage.input_tokens, 12);
    assert_eq!(result.usage.output_tokens, 3);
    assert_eq!(result.usage.duration_ms, Some(2));
}

#[test]
fn streamed_text_chunks_preserve_real_spacing_and_newlines_only() {
    let mut accumulator = OllamaResponseAccumulator::default();
    let first = serde_json::json!({
        "response": "{\"type\":\"text\",\"text\":\"alpha beta gamma delta epsilon",
        "done": false,
    })
    .to_string();
    let second = serde_json::json!({
        "response": " zeta\\nreal line\"}",
        "done": true,
    })
    .to_string();

    let first_events = accumulator.push_line(&first);
    assert!(matches!(first_events[0], TurnEvent::Thinking { .. }));
    assert_eq!(
        first_events[1],
        TurnEvent::TextDelta {
            text: "alpha beta gamma delta epsilon".to_string(),
        }
    );
    assert_eq!(
        accumulator.push_line(&second),
        vec![TurnEvent::TextDelta {
            text: " zeta\nreal line".to_string(),
        }]
    );
}

#[test]
fn neither_local_model_overrides_ollamas_runtime_capacity() {
    for model_id in [CODELLAMA_OLLAMA_MODEL_ID, DEEPSEEK_CODER_OLLAMA_MODEL_ID] {
        let model = ollama_model_name(model_id).unwrap();
        let request: serde_json::Value =
            serde_json::from_str(&ollama_generate_request(model, "test", None)).unwrap();
        assert!(request["options"].get("num_ctx").is_none(), "{model}");
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
        |event| events.push(event),
    ))
    .unwrap();

    assert!(status.success());
    assert!(matches!(events[0], TurnEvent::SessionInit { .. }));
    assert!(matches!(events[1], TurnEvent::Thinking { .. }));
    assert_eq!(events[2], TurnEvent::TextDelta { text: "OK".into() });
    assert!(matches!(
        events[3],
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
