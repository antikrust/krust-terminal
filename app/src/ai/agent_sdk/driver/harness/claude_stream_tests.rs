//! Tests using real lines captured from an actual
//! `claude -p ... --output-format stream-json --verbose` run, not
//! hand-written fixtures, so schema drift in a future Claude Code version
//! would show up here.

use ai::agent::{ToolCallOutcome, TurnEvent};

use super::parse_stream_json_line;

#[test]
fn parses_system_init() {
    let line = r#"{"type":"system","subtype":"init","cwd":"/tmp/probe","session_id":"016162d0-e4b3-4460-834f-1f9d588751e0","tools":["Bash","Read","Write"],"mcp_servers":[],"model":"claude-opus-4-8[1m]","permissionMode":"bypassPermissions","slash_commands":[]}"#;
    let events = parse_stream_json_line(line);
    assert_eq!(
        events,
        vec![TurnEvent::SessionInit {
            session_id: "016162d0-e4b3-4460-834f-1f9d588751e0".to_string(),
            model: Some("claude-opus-4-8[1m]".to_string()),
            cwd: Some("/tmp/probe".to_string()),
        }]
    );
}

#[test]
fn skips_hook_and_rate_limit_noise() {
    let hook_started = r#"{"type":"system","subtype":"hook_started","hook_id":"a","hook_name":"SessionStart:startup","hook_event":"SessionStart","uuid":"u","session_id":"s"}"#;
    let hook_response = r#"{"type":"system","subtype":"hook_response","hook_id":"a","hook_name":"SessionStart:startup","hook_event":"SessionStart","output":"","stdout":"","stderr":"","exit_code":0,"outcome":"success","uuid":"u","session_id":"s"}"#;
    let rate_limit = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"},"uuid":"u","session_id":"s"}"#;
    let post_turn_summary = r#"{"type":"system","subtype":"post_turn_summary","summarizes_uuid":"u","status_category":"review_ready","status_detail":"done","needs_action":"","uuid":"u","session_id":"s"}"#;

    for line in [hook_started, hook_response, rate_limit, post_turn_summary] {
        assert_eq!(parse_stream_json_line(line), Vec::new(), "line: {line}");
    }
}

#[test]
fn parses_assistant_text() {
    let line = r#"{"type":"assistant","message":{"model":"claude-opus-4-8","id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"I'll list the directory contents and create the file."}],"stop_reason":null,"stop_sequence":null,"stop_details":null,"usage":{"input_tokens":2,"output_tokens":1}},"parent_tool_use_id":null,"session_id":"s","uuid":"u","timestamp":"t","request_id":"r"}"#;
    let events = parse_stream_json_line(line);
    assert_eq!(
        events,
        vec![TurnEvent::Text {
            text: "I'll list the directory contents and create the file.".to_string(),
        }]
    );
}

#[test]
fn parses_assistant_tool_use() {
    let line = r#"{"type":"assistant","message":{"model":"claude-opus-4-8","id":"msg_1","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_01Vcfe1BoGMXsmRQ7eLtTCrm","name":"Bash","input":{"command":"ls -la /tmp/probe","description":"List files in working directory"},"caller":{"type":"direct"}}],"stop_reason":null,"stop_sequence":null,"stop_details":null,"usage":{"input_tokens":2,"output_tokens":1}},"parent_tool_use_id":null,"session_id":"s","uuid":"u","timestamp":"t","request_id":"r"}"#;
    let events = parse_stream_json_line(line);
    assert_eq!(events.len(), 1);
    match &events[0] {
        TurnEvent::ToolCallStarted { call_id, name, input } => {
            assert_eq!(call_id, "toolu_01Vcfe1BoGMXsmRQ7eLtTCrm");
            assert_eq!(name, "Bash");
            assert_eq!(input["command"], "ls -la /tmp/probe");
        }
        other => panic!("expected ToolCallStarted, got {other:?}"),
    }
}

#[test]
fn parses_bash_tool_result_with_plain_string_content() {
    let line = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_01Vcfe1BoGMXsmRQ7eLtTCrm","type":"tool_result","content":"total 16\ndrwxr-xr-x@  4 blah  wheel   128 file1","is_error":false}]},"parent_tool_use_id":null,"session_id":"s","uuid":"u","timestamp":"t","tool_use_result":{"stdout":"total 16\ndrwxr-xr-x@  4 blah  wheel   128 file1","stderr":"","interrupted":false,"isImage":false,"noOutputExpected":false}}"#;
    let events = parse_stream_json_line(line);
    assert_eq!(events.len(), 1);
    match &events[0] {
        TurnEvent::ToolCallResult { call_id, outcome } => {
            assert_eq!(call_id, "toolu_01Vcfe1BoGMXsmRQ7eLtTCrm");
            match outcome {
                // Should prefer the richer tool_use_result sibling field
                // over the plain content string.
                ToolCallOutcome::Success(value) => {
                    assert_eq!(value["stdout"], "total 16\ndrwxr-xr-x@  4 blah  wheel   128 file1");
                }
                other => panic!("expected Success, got {other:?}"),
            }
        }
        other => panic!("expected ToolCallResult, got {other:?}"),
    }
}

#[test]
fn parses_write_tool_result_with_structured_patch() {
    let line = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_01EZyFdTpkZeLJhmJ6waaPfH","type":"tool_result","content":"File created successfully at: /tmp/probe/hello.txt"}]},"parent_tool_use_id":null,"session_id":"s","uuid":"u","timestamp":"t","tool_use_result":{"type":"create","filePath":"/tmp/probe/hello.txt","content":"hi\n","structuredPatch":[],"originalFile":null,"userModified":false}}"#;
    let events = parse_stream_json_line(line);
    assert_eq!(events.len(), 1);
    match &events[0] {
        TurnEvent::ToolCallResult { outcome, .. } => match outcome {
            ToolCallOutcome::Success(value) => {
                assert_eq!(value["filePath"], "/tmp/probe/hello.txt");
                assert_eq!(value["content"], "hi\n");
            }
            other => panic!("expected Success, got {other:?}"),
        },
        other => panic!("expected ToolCallResult, got {other:?}"),
    }
}

#[test]
fn parses_final_result() {
    let line = r#"{"type":"result","subtype":"success","is_error":false,"api_error_status":null,"duration_ms":6921,"duration_api_ms":6794,"num_turns":3,"result":"Done.","stop_reason":"end_turn","session_id":"s","total_cost_usd":0.203424,"usage":{"input_tokens":6,"output_tokens":360},"permission_denials":[],"terminal_reason":"completed","uuid":"u"}"#;
    let events = parse_stream_json_line(line);
    assert_eq!(events.len(), 1);
    match &events[0] {
        TurnEvent::TurnCompleted {
            usage,
            is_error,
            error_message,
        } => {
            assert!(!is_error);
            assert_eq!(error_message, &None);
            let usage = usage.as_ref().expect("usage present");
            assert_eq!(usage.input_tokens, 6);
            assert_eq!(usage.output_tokens, 360);
            assert_eq!(usage.duration_ms, Some(6921));
            assert!((usage.cost_usd.unwrap() - 0.203424).abs() < 1e-9);
        }
        other => panic!("expected TurnCompleted, got {other:?}"),
    }
}

#[test]
fn result_usage_reads_final_call_cache_tokens_and_model_context_window() {
    // Shape confirmed against a real claude 2.1.x run: top-level `usage`
    // sums every API call and carries per-call `iterations`; `modelUsage`
    // is keyed by model id and reports the context window.
    let line = r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1988,"num_turns":2,"result":"OK","session_id":"s","total_cost_usd":0.043347,"usage":{"input_tokens":30,"cache_creation_input_tokens":30000,"cache_read_input_tokens":12000,"output_tokens":700,"iterations":[{"input_tokens":20,"output_tokens":635,"cache_read_input_tokens":0,"cache_creation_input_tokens":21506,"type":"message"},{"input_tokens":10,"output_tokens":65,"cache_read_input_tokens":12000,"cache_creation_input_tokens":8494,"type":"message"}]},"modelUsage":{"claude-haiku-4-5-20251001":{"inputTokens":30,"outputTokens":700,"cacheReadInputTokens":12000,"cacheCreationInputTokens":30000,"webSearchRequests":0,"costUSD":0.043347,"contextWindow":200000,"maxOutputTokens":32000}},"uuid":"u"}"#;
    let events = parse_stream_json_line(line);
    assert_eq!(events.len(), 1);
    match &events[0] {
        TurnEvent::TurnCompleted { usage, .. } => {
            let usage = usage.as_ref().expect("usage present");
            // Token counts come from the run's final call, not the sums.
            assert_eq!(usage.input_tokens, 10);
            assert_eq!(usage.output_tokens, 65);
            assert_eq!(usage.cache_read_input_tokens, 12000);
            assert_eq!(usage.cache_creation_input_tokens, 8494);
            assert_eq!(
                usage.model_id.as_deref(),
                Some("claude-haiku-4-5-20251001")
            );
            assert_eq!(usage.context_window, Some(200_000));
        }
        other => panic!("expected TurnCompleted, got {other:?}"),
    }
}

#[test]
fn empty_and_malformed_lines_are_ignored_not_fatal() {
    assert_eq!(parse_stream_json_line(""), Vec::new());
    assert_eq!(parse_stream_json_line("   "), Vec::new());
    assert_eq!(parse_stream_json_line("{not valid json"), Vec::new());
}
