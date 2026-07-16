//! Tests for headless streaming execution. `stream_json_args` is checked as a
//! pure function; `run_claude_stream` is driven end-to-end against a stand-in
//! program (`sh`) that discards stdin and emits canned ndjson, exercising the
//! real spawn → line-read → parse → callback path without needing the `claude`
//! binary installed.

use std::io::Write;

use ai::agent::{ToolCallOutcome, TurnEvent};
use command::r#async::Command;
use futures::executor::block_on;
use tempfile::NamedTempFile;
use uuid::Uuid;

use super::{run_claude_stream, stream_json_args};

#[test]
fn fresh_run_args() {
    let id = Uuid::nil();
    let args = stream_json_args(&id, false, None, None, None);
    assert_eq!(
        args,
        vec![
            "--print",
            "--output-format",
            "stream-json",
            "--verbose",
            "--session-id",
            "00000000-0000-0000-0000-000000000000",
            "--dangerously-skip-permissions",
        ]
    );
}

#[test]
fn model_selection_appends_model_flag() {
    let id = Uuid::nil();
    let args = stream_json_args(&id, false, Some("opus"), None, None);
    assert!(args
        .windows(2)
        .any(|w| w == ["--model".to_string(), "opus".to_string()]));
}

#[test]
fn resume_run_swaps_session_flag_and_appends_optional_paths() {
    let id = Uuid::nil();
    let args = stream_json_args(&id, true, None, Some("/tmp/sys.txt"), Some("/tmp/mcp.json"));
    assert_eq!(args[4], "--resume");
    assert!(args.windows(2).any(|w| w
        == [
            "--append-system-prompt-file".to_string(),
            "/tmp/sys.txt".to_string()
        ]));
    assert!(args
        .windows(2)
        .any(|w| w == ["--mcp-config".to_string(), "/tmp/mcp.json".to_string()]));
}

/// A stand-in for `claude`: reads and discards stdin (the prompt), then prints
/// the ndjson in `fixture` to stdout. Returns a `Command` shaped exactly like
/// what `run_claude_stream` expects to spawn.
fn fake_claude(fixture: &str) -> (Command, NamedTempFile) {
    let mut file = NamedTempFile::new().expect("temp file");
    file.write_all(fixture.as_bytes()).expect("write fixture");
    file.flush().expect("flush fixture");
    let path = file.path().display().to_string();

    let mut command = Command::new("sh");
    // `$0` inside the -c script binds to the trailing arg (the fixture path).
    command.args(["-c", "cat >/dev/null; cat \"$0\"", &path]);
    (command, file)
}

#[test]
fn streams_and_parses_a_full_turn() {
    let fixture = concat!(
        r#"{"type":"system","subtype":"init","session_id":"S","model":"claude-opus-4-8","cwd":"/tmp/probe"}"#,
        "\n",
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#,
        "\n",
        r#"{"type":"user","message":{"content":[{"tool_use_id":"t1","type":"tool_result","content":"out","is_error":false}]},"tool_use_result":{"stdout":"out"}}"#,
        "\n",
        r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":10,"total_cost_usd":0.01,"usage":{"input_tokens":1,"output_tokens":2},"result":"done"}"#,
        "\n",
    );
    let (command, _fixture_file) = fake_claude(fixture);

    let mut events = Vec::new();
    let status = block_on(run_claude_stream(command, "the prompt", |event| {
        events.push(event)
    }))
    .expect("run completes");

    assert!(status.success());
    assert_eq!(events.len(), 5);
    assert!(matches!(events[0], TurnEvent::SessionInit { .. }));
    assert_eq!(
        events[1],
        TurnEvent::Text {
            text: "hello".to_string()
        }
    );
    match &events[2] {
        TurnEvent::ToolCallStarted { call_id, name, .. } => {
            assert_eq!(call_id, "t1");
            assert_eq!(name, "Bash");
        }
        other => panic!("expected ToolCallStarted, got {other:?}"),
    }
    match &events[3] {
        TurnEvent::ToolCallResult { call_id, outcome } => {
            assert_eq!(call_id, "t1");
            assert!(matches!(outcome, ToolCallOutcome::Success(_)));
        }
        other => panic!("expected ToolCallResult, got {other:?}"),
    }
    assert!(matches!(
        events[4],
        TurnEvent::TurnCompleted {
            is_error: false,
            ..
        }
    ));
}

#[test]
fn failing_child_with_stderr_output_still_resolves_with_failure_status() {
    let mut command = Command::new("sh");
    command.args(["-c", "cat >/dev/null; echo oops >&2; exit 3"]);

    let mut events = Vec::new();
    let status = block_on(run_claude_stream(command, "p", |event| events.push(event)))
        .expect("run completes");

    assert!(!status.success());
    assert!(events.is_empty());
}

#[test]
fn malformed_lines_do_not_abort_the_stream() {
    let fixture = concat!(
        "{not json\n",
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"still here"}]}}"#,
        "\n",
    );
    let (command, _fixture_file) = fake_claude(fixture);

    let mut events = Vec::new();
    block_on(run_claude_stream(command, "p", |event| events.push(event))).expect("run completes");

    assert_eq!(
        events,
        vec![TurnEvent::Text {
            text: "still here".to_string()
        }]
    );
}
