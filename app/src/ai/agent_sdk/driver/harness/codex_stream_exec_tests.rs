//! Tests for headless codex streaming execution. `codex_exec_args` is checked
//! as a pure function; `run_codex_stream` is driven end-to-end against a
//! stand-in program (`sh`) that discards stdin and emits canned JSONL,
//! exercising the real spawn → line-read → parse → callback path without
//! needing the `codex` binary installed.

use std::io::Write;

use ai::agent::TurnEvent;
use command::r#async::Command;
use futures::executor::block_on;
use tempfile::NamedTempFile;
use uuid::Uuid;

use super::{codex_exec_args, run_codex_stream};

#[test]
fn fresh_run_args_let_codex_mint_the_thread() {
    let args = codex_exec_args(None, None);
    assert_eq!(
        args,
        vec![
            "exec",
            "--json",
            "--skip-git-repo-check",
            "--dangerously-bypass-approvals-and-sandbox",
            "-",
        ]
    );
}

#[test]
fn resume_run_inserts_resume_subcommand_and_model_flag() {
    let id = Uuid::nil();
    let args = codex_exec_args(Some(&id), Some("gpt-5.2-codex"));
    assert_eq!(
        args,
        vec![
            "exec",
            "resume",
            "00000000-0000-0000-0000-000000000000",
            "--json",
            "--skip-git-repo-check",
            "--dangerously-bypass-approvals-and-sandbox",
            "--model",
            "gpt-5.2-codex",
            "-",
        ]
    );
}

/// A stand-in for `codex`: reads and discards stdin (the prompt), then prints
/// the JSONL in `fixture` to stdout.
fn fake_codex(fixture: &str) -> (Command, NamedTempFile) {
    let mut file = NamedTempFile::new().expect("temp file");
    file.write_all(fixture.as_bytes()).expect("write fixture");
    file.flush().expect("flush fixture");
    let path = file.path().display().to_string();

    let mut command = Command::new("sh");
    command.args(["-c", "cat >/dev/null; cat \"$0\"", &path]);
    (command, file)
}

#[test]
fn streams_thread_id_reply_and_usage_from_canned_run() {
    let fixture = concat!(
        r#"{"type":"thread.started","thread_id":"019f6c9d-ecae-72d0-9a96-09e4e246e2bc"}"#,
        "\n",
        r#"{"type":"turn.started"}"#,
        "\n",
        r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"OK"}}"#,
        "\n",
        r#"{"type":"turn.completed","usage":{"input_tokens":14199,"cached_input_tokens":9984,"output_tokens":5,"reasoning_output_tokens":0}}"#,
        "\n",
    );
    let (command, _fixture_file) = fake_codex(fixture);

    let mut events = Vec::new();
    let status = block_on(run_codex_stream(command, "prompt", |event| {
        events.push(event)
    }))
    .expect("run completes");
    assert!(status.success());

    assert_eq!(events.len(), 3);
    match &events[0] {
        TurnEvent::SessionInit { session_id, .. } => {
            assert_eq!(session_id, "019f6c9d-ecae-72d0-9a96-09e4e246e2bc");
        }
        other => panic!("expected SessionInit, got {other:?}"),
    }
    assert_eq!(
        events[1],
        TurnEvent::Text {
            text: "OK".to_string()
        }
    );
    match &events[2] {
        TurnEvent::TurnCompleted {
            usage: Some(usage),
            is_error: false,
            ..
        } => {
            // OpenAI-style counts are normalized: cached tokens are split
            // out of `input_tokens` so the fields can be summed.
            assert_eq!(usage.input_tokens, 14199 - 9984);
            assert_eq!(usage.cache_read_input_tokens, 9984);
            assert_eq!(usage.output_tokens, 5);
        }
        other => panic!("expected successful TurnCompleted, got {other:?}"),
    }
}
