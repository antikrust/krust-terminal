//! Unit tests for the codex `exec --json` line parser, against line shapes
//! captured from a real codex-cli 0.144.x run plus documented item types.

use ai::agent::{ToolCallOutcome, TurnEvent};

use super::parse_codex_jsonl_line;

#[test]
fn thread_started_becomes_session_init() {
    let events = parse_codex_jsonl_line(
        r#"{"type":"thread.started","thread_id":"019f6c9d-ecae-72d0-9a96-09e4e246e2bc"}"#,
    );
    assert_eq!(
        events,
        vec![TurnEvent::SessionInit {
            session_id: "019f6c9d-ecae-72d0-9a96-09e4e246e2bc".to_string(),
            model: None,
            cwd: None,
        }]
    );
}

#[test]
fn turn_started_and_unknown_events_are_skipped() {
    assert!(parse_codex_jsonl_line(r#"{"type":"turn.started"}"#).is_empty());
    assert!(parse_codex_jsonl_line(
        r#"{"type":"item.started","item":{"id":"i","type":"command_execution","command":"ls"}}"#
    )
    .is_empty());
    assert!(parse_codex_jsonl_line(r#"{"type":"something.new","payload":1}"#).is_empty());
    assert!(parse_codex_jsonl_line("").is_empty());
    assert!(parse_codex_jsonl_line("not json").is_empty());
}

#[test]
fn agent_message_and_reasoning_render_as_text_and_thinking() {
    assert_eq!(
        parse_codex_jsonl_line(
            r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"OK"}}"#
        ),
        vec![TurnEvent::Text {
            text: "OK".to_string()
        }]
    );
    assert_eq!(
        parse_codex_jsonl_line(
            r#"{"type":"item.completed","item":{"id":"item_1","type":"reasoning","text":"hmm"}}"#
        ),
        vec![TurnEvent::Thinking {
            text: "hmm".to_string()
        }]
    );
}

#[test]
fn completed_command_execution_yields_tool_call_pair() {
    let events = parse_codex_jsonl_line(
        r#"{"type":"item.completed","item":{"id":"item_2","type":"command_execution","command":"ls -la","aggregated_output":"total 0\n","exit_code":0,"status":"completed"}}"#,
    );
    assert_eq!(events.len(), 2);
    match &events[0] {
        TurnEvent::ToolCallStarted {
            call_id,
            name,
            input,
        } => {
            assert_eq!(call_id, "item_2");
            assert_eq!(name, "Shell");
            assert_eq!(input["command"], "ls -la");
        }
        other => panic!("expected ToolCallStarted, got {other:?}"),
    }
    match &events[1] {
        TurnEvent::ToolCallResult { call_id, outcome } => {
            assert_eq!(call_id, "item_2");
            match outcome {
                ToolCallOutcome::Success(value) => assert_eq!(value["output"], "total 0\n"),
                other => panic!("expected Success, got {other:?}"),
            }
        }
        other => panic!("expected ToolCallResult, got {other:?}"),
    }
}

#[test]
fn failed_command_execution_yields_error_outcome() {
    let events = parse_codex_jsonl_line(
        r#"{"type":"item.completed","item":{"id":"item_3","type":"command_execution","command":"false","aggregated_output":"","exit_code":1,"status":"failed"}}"#,
    );
    match &events[1] {
        TurnEvent::ToolCallResult {
            outcome: ToolCallOutcome::Error(message),
            ..
        } => {
            assert!(message.contains("exit 1"), "message: {message}");
        }
        other => panic!("expected Error outcome, got {other:?}"),
    }
}

#[test]
fn turn_completed_normalizes_openai_style_usage() {
    let events = parse_codex_jsonl_line(
        r#"{"type":"turn.completed","usage":{"input_tokens":14199,"cached_input_tokens":9984,"output_tokens":5,"reasoning_output_tokens":7}}"#,
    );
    match &events[0] {
        TurnEvent::TurnCompleted {
            usage: Some(usage),
            is_error: false,
            ..
        } => {
            assert_eq!(usage.input_tokens, 4215);
            assert_eq!(usage.cache_read_input_tokens, 9984);
            assert_eq!(usage.cache_creation_input_tokens, 0);
            assert_eq!(usage.output_tokens, 12);
            assert_eq!(usage.model_id.as_deref(), Some("codex"));
            assert_eq!(usage.context_window, None);
        }
        other => panic!("expected TurnCompleted, got {other:?}"),
    }
}

#[test]
fn turn_failed_becomes_error_completion() {
    let events = parse_codex_jsonl_line(
        r#"{"type":"turn.failed","error":{"message":"stream disconnected"}}"#,
    );
    assert_eq!(
        events,
        vec![TurnEvent::TurnCompleted {
            usage: None,
            is_error: true,
            error_message: Some("stream disconnected".to_string()),
        }]
    );
}
