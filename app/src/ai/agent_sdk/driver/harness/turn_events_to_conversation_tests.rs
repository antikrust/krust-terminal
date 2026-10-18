//! Tests that a stream of `TurnEvent`s maps to the expected sequence of
//! renderable conversation messages.

use ai::agent::{ToolCallOutcome, TurnEvent};
use serde_json::json;
use krust_multi_agent_api as api;

use super::turn_events_to_conversation;

/// Extracts the concrete message oneof from an envelope for assertions.
fn body(message: &api::Message) -> &api::message::Message {
    message.message.as_ref().expect("message body present")
}

#[test]
fn builds_single_task_with_leading_user_query() {
    let convo = turn_events_to_conversation("do the thing", &[]);
    assert_eq!(convo.tasks.len(), 1);
    let messages = &convo.tasks[0].messages;
    assert_eq!(messages.len(), 1, "only the user query for an empty run");
    match body(&messages[0]) {
        api::message::Message::UserQuery(q) => assert_eq!(q.query, "do the thing"),
        other => panic!("expected UserQuery, got {other:?}"),
    }
}

#[test]
fn maps_text_and_reasoning_to_native_message_types() {
    let events = vec![
        TurnEvent::Thinking {
            text: "let me think".to_string(),
        },
        TurnEvent::Text {
            text: "the answer".to_string(),
        },
    ];
    let convo = turn_events_to_conversation("q", &events);
    let messages = &convo.tasks[0].messages;
    // user query + reasoning + text
    assert_eq!(messages.len(), 3);
    match body(&messages[1]) {
        api::message::Message::AgentReasoning(r) => assert_eq!(r.reasoning, "let me think"),
        other => panic!("expected AgentReasoning, got {other:?}"),
    }
    match body(&messages[2]) {
        api::message::Message::AgentOutput(o) => assert_eq!(o.text, "the answer"),
        other => panic!("expected AgentOutput, got {other:?}"),
    }
}

#[test]
fn renders_shell_tools_as_inline_read_only_cards() {
    let events = vec![
        TurnEvent::ToolCallStarted {
            call_id: "t1".to_string(),
            name: "Bash".to_string(),
            input: json!({ "command": "ls -la", "description": "list" }),
        },
        TurnEvent::ToolCallResult {
            call_id: "t1".to_string(),
            outcome: ToolCallOutcome::Success(json!({ "stdout": "file1\nfile2" })),
        },
    ];
    let convo = turn_events_to_conversation("q", &events);
    let messages = &convo.tasks[0].messages;
    assert_eq!(messages.len(), 3); // user query + call + result

    // Shell transcripts use the generic read-only card shape. A native
    // RunShellCommand message would also create a terminal block below it.
    match body(&messages[1]) {
        api::message::Message::ToolCall(tc) => {
            assert_eq!(tc.tool_call_id, "t1");
            match tc.tool.as_ref() {
                Some(api::message::tool_call::Tool::CallMcpTool(tool)) => {
                    assert_eq!(tool.name, "Bash");
                    assert_eq!(
                        tool.args
                            .as_ref()
                            .and_then(|args| args.fields.get("command"))
                            .and_then(|value| value.kind.as_ref()),
                        Some(&prost_types::value::Kind::StringValue("ls -la".to_string()))
                    );
                }
                other => panic!("expected inline CallMcpTool, got {other:?}"),
            }
        }
        other => panic!("expected ToolCall for the call, got {other:?}"),
    }
    match body(&messages[2]) {
        api::message::Message::ToolCallResult(tcr) => {
            assert_eq!(tcr.tool_call_id, "t1");
            match tcr.result.as_ref() {
                Some(api::message::tool_call_result::Result::CallMcpTool(result)) => {
                    match result.result.as_ref() {
                        Some(api::call_mcp_tool_result::Result::Success(success)) => {
                            let text = match success.results[0].result.as_ref() {
                                Some(api::call_mcp_tool_result::success::result::Result::Text(
                                    text,
                                )) => &text.text,
                                other => panic!("expected text output, got {other:?}"),
                            };
                            assert!(text.contains("file1"), "{text}");
                        }
                        other => panic!("expected successful inline result, got {other:?}"),
                    }
                }
                other => panic!("expected inline CallMcpTool result, got {other:?}"),
            }
        }
        other => panic!("expected ToolCallResult for the result, got {other:?}"),
    }
}

#[test]
fn codex_shell_error_stays_in_the_inline_card() {
    let events = vec![
        TurnEvent::ToolCallStarted {
            call_id: "t1".to_string(),
            name: "Shell".to_string(),
            input: json!({ "command": "false" }),
        },
        TurnEvent::ToolCallResult {
            call_id: "t1".to_string(),
            outcome: ToolCallOutcome::Error("exit 2\nno such thing".to_string()),
        },
    ];
    let convo = turn_events_to_conversation("q", &events);
    match body(&convo.tasks[0].messages[2]) {
        api::message::Message::ToolCallResult(tcr) => match tcr.result.as_ref() {
            Some(api::message::tool_call_result::Result::CallMcpTool(result)) => {
                match result.result.as_ref() {
                    Some(api::call_mcp_tool_result::Result::Error(error)) => {
                        assert!(error.message.contains("exit 2"), "{}", error.message);
                        assert!(error.message.contains("no such thing"), "{}", error.message);
                    }
                    other => panic!("expected inline error result, got {other:?}"),
                }
            }
            other => panic!("expected inline CallMcpTool result, got {other:?}"),
        },
        other => panic!("expected ToolCallResult, got {other:?}"),
    }
}

#[test]
fn renders_non_shell_tools_as_mcp_cards() {
    let events = vec![
        TurnEvent::ToolCallStarted {
            call_id: "t1".to_string(),
            name: "Read".to_string(),
            input: json!({ "file_path": "/tmp/x" }),
        },
        TurnEvent::ToolCallResult {
            call_id: "t1".to_string(),
            outcome: ToolCallOutcome::Success(json!({ "output": "contents" })),
        },
    ];
    let convo = turn_events_to_conversation("q", &events);
    let messages = &convo.tasks[0].messages;

    match body(&messages[1]) {
        api::message::Message::ToolCall(tc) => match tc.tool.as_ref() {
            Some(api::message::tool_call::Tool::CallMcpTool(mcp)) => {
                assert_eq!(mcp.name, "Read");
                let args = mcp.args.as_ref().expect("args present");
                match args.fields.get("file_path").and_then(|v| v.kind.as_ref()) {
                    Some(prost_types::value::Kind::StringValue(s)) => assert_eq!(s, "/tmp/x"),
                    other => panic!("expected file_path string, got {other:?}"),
                }
            }
            other => panic!("expected CallMcpTool, got {other:?}"),
        },
        other => panic!("expected ToolCall for the call, got {other:?}"),
    }
    match body(&messages[2]) {
        api::message::Message::ToolCallResult(tcr) => match tcr.result.as_ref() {
            Some(api::message::tool_call_result::Result::CallMcpTool(r)) => match r.result.as_ref()
            {
                Some(api::call_mcp_tool_result::Result::Success(s)) => {
                    let text = match s.results[0].result.as_ref() {
                        Some(api::call_mcp_tool_result::success::result::Result::Text(t)) => {
                            &t.text
                        }
                        other => panic!("expected text result, got {other:?}"),
                    };
                    assert!(text.contains("contents"), "shows output: {text}");
                }
                other => panic!("expected success, got {other:?}"),
            },
            other => panic!("expected CallMcpTool result, got {other:?}"),
        },
        other => panic!("expected ToolCallResult for the result, got {other:?}"),
    }
}

#[test]
fn tool_error_result_is_flagged() {
    let events = vec![TurnEvent::ToolCallResult {
        call_id: "t1".to_string(),
        outcome: ToolCallOutcome::Error("boom".to_string()),
    }];
    let convo = turn_events_to_conversation("q", &events);
    match body(&convo.tasks[0].messages[1]) {
        api::message::Message::ToolCallResult(tcr) => match tcr.result.as_ref() {
            Some(api::message::tool_call_result::Result::CallMcpTool(r)) => match r.result.as_ref()
            {
                Some(api::call_mcp_tool_result::Result::Error(e)) => {
                    assert!(
                        e.message.contains("boom"),
                        "carries the error: {}",
                        e.message
                    );
                }
                other => panic!("expected error result, got {other:?}"),
            },
            other => panic!("expected CallMcpTool result, got {other:?}"),
        },
        other => panic!("expected ToolCallResult, got {other:?}"),
    }
}

#[test]
fn failed_turn_renders_its_error_message() {
    let events = vec![TurnEvent::TurnCompleted {
        usage: None,
        is_error: true,
        error_message: Some("credit balance too low".to_string()),
    }];
    let convo = turn_events_to_conversation("q", &events);
    let messages = &convo.tasks[0].messages;
    assert_eq!(messages.len(), 2); // user query + error block
    match body(&messages[1]) {
        api::message::Message::AgentOutput(o) => {
            assert!(o.text.contains("⚠"), "flags error: {}", o.text);
            assert!(o.text.contains("credit balance too low"));
            // Rendered as clean text, not wrapped in a grey code block.
            assert!(!o.text.contains("```"), "no code fence: {}", o.text);
        }
        other => panic!("expected AgentOutput, got {other:?}"),
    }
}

#[test]
fn failed_turn_without_detail_still_renders_an_error() {
    let events = vec![TurnEvent::TurnCompleted {
        usage: None,
        is_error: true,
        error_message: None,
    }];
    let convo = turn_events_to_conversation("q", &events);
    let messages = &convo.tasks[0].messages;
    assert_eq!(messages.len(), 2);
    match body(&messages[1]) {
        api::message::Message::AgentOutput(o) => {
            assert!(o.text.contains("error"), "flags error: {}", o.text);
        }
        other => panic!("expected AgentOutput, got {other:?}"),
    }
}

#[test]
fn session_init_and_turn_completed_produce_no_messages() {
    let events = vec![
        TurnEvent::SessionInit {
            session_id: "s".to_string(),
            model: None,
            cwd: None,
        },
        TurnEvent::TurnCompleted {
            usage: None,
            is_error: false,
            error_message: None,
        },
    ];
    let convo = turn_events_to_conversation("q", &events);
    // Only the user query — bookkeeping events render nothing.
    assert_eq!(convo.tasks[0].messages.len(), 1);
}
