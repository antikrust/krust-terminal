//! The runbook driver against a scripted stand-in for Ollama: the sequences
//! small models produced on the Firefox runbook, replayed in milliseconds.
//! Each of the driver bugs those runs found had cost an hour-long live run;
//! a sequence here fails in a second, before any model is loaded.
//!
//! The stand-in is a plain HTTP server on a port of its own. The driver
//! reaches it the way it reaches a model on another machine
//! (`name@@http://host:port`), so nothing in the driver is special-cased for
//! tests: the same curl, the same JSON, the same parsing.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use futures_lite::future::block_on;
use serde_json::{json, Value};
use uuid::Uuid;

use super::*;

/// What a scripted model says back: text, tool calls, or both.
#[derive(Clone, Default)]
struct Reply {
    content: String,
    tool_calls: Vec<Value>,
}

fn text(content: &str) -> Reply {
    Reply {
        content: content.to_string(),
        tool_calls: Vec::new(),
    }
}

fn call(name: &str, arguments: Value) -> Reply {
    Reply {
        content: String::new(),
        tool_calls: vec![json!({ "function": { "name": name, "arguments": arguments } })],
    }
}

fn block(number: usize) -> Reply {
    call(RUN_BLOCK, json!({ "block": number }))
}

fn shell(command: &str) -> Reply {
    call(SHELL, json!({ "command": command }))
}

fn verdict(kind: &str) -> Reply {
    text(&json!({ "verdict": kind, "reason": "as the output shows", "facts": {} }).to_string())
}

type Script = Box<dyn FnMut(&Value) -> Reply + Send>;

/// Which of the driver's questions a request to the judge is.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Worker,
    Verdict,
    Case,
    Done,
    Report,
    Other,
}

fn system_of(request: &Value) -> &str {
    request["messages"][0]["content"]
        .as_str()
        .unwrap_or_default()
}

fn last_of(request: &Value) -> &str {
    request["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .and_then(|message| message["content"].as_str())
        .unwrap_or_default()
}

fn kind_of(request: &Value) -> Kind {
    if request["model"].as_str() == Some("worker") {
        return Kind::Worker;
    }
    let system = system_of(request);
    if system.starts_with("You check one step of a runbook run") {
        Kind::Verdict
    } else if system.starts_with("You decide whether one block") {
        Kind::Case
    } else if system.starts_with("You check one decision about a runbook run") {
        Kind::Done
    } else if system.starts_with("You write the final report") {
        Kind::Report
    } else {
        Kind::Other
    }
}

/// The block a case check asks about.
fn case_block(request: &Value) -> usize {
    let user = last_of(request);
    let at = user
        .find("The block in question is block ")
        .expect("a case check names its block");
    user[at + "The block in question is block ".len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap()
}

/// A script: the worker's replies in order, and the judge's by question.
fn script(
    worker: Vec<Reply>,
    mut judge: impl FnMut(Kind, &Value) -> Reply + Send + 'static,
) -> Script {
    let mut worker: VecDeque<Reply> = worker.into();
    Box::new(move |request| match kind_of(request) {
        Kind::Worker => worker.pop_front().unwrap_or_else(|| text("Done.")),
        kind => judge(kind, request),
    })
}

/// A judge that passes every step, answers each block's case by `case`
/// and writes a plain report.
fn plain_judge(
    case: impl Fn(usize) -> &'static str + Send + 'static,
) -> impl FnMut(Kind, &Value) -> Reply + Send {
    move |kind, request| match kind {
        Kind::Verdict => verdict("pass"),
        Kind::Case => text(case(case_block(request))),
        Kind::Done => text("NO — the step did its own work"),
        Kind::Report => text("count: 3\ntag: v1"),
        Kind::Worker | Kind::Other => text("Done."),
    }
}

/// The stand-in: `/api/show` says every model can call tools (and thinks if
/// its name says so); `/api/chat` answers from the script, streamed the way
/// Ollama streams.
struct FakeOllama {
    base_url: String,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl FakeOllama {
    fn start(script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let script = Arc::new(Mutex::new(script));
        std::thread::spawn(move || {
            // One at a time: the driver's calls are sequential.
            for stream in listener.incoming().flatten() {
                serve(stream, &script, &seen);
            }
        });
        Self { base_url, requests }
    }

    fn model(&self, name: &str) -> String {
        format!(
            "{name}{}{}",
            crate::ai::ollama::remote::HOST_SEPARATOR,
            self.base_url
        )
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

fn serve(mut stream: TcpStream, script: &Mutex<Script>, seen: &Mutex<Vec<Value>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let mut length = 0usize;
    let mut expects_continue = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("expect:") && lower.contains("100-continue") {
            expects_continue = true;
        }
    }
    if expects_continue {
        let _ = stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
        let _ = stream.flush();
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).unwrap();
    }
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let body = match path.as_str() {
        "/api/show" => {
            let model = request["model"].as_str().unwrap_or_default();
            let mut capabilities = vec!["completion", "tools"];
            if model.contains("think") {
                capabilities.push("thinking");
            }
            json!({ "capabilities": capabilities }).to_string()
        }
        "/api/chat" => {
            seen.lock().unwrap().push(request.clone());
            let reply = (script.lock().unwrap())(&request);
            let model = request["model"].clone();
            let first = json!({
                "model": model,
                "message": { "role": "assistant", "content": reply.content, "tool_calls": reply.tool_calls },
                "done": false,
            });
            let last = json!({
                "model": model,
                "message": { "role": "assistant", "content": "" },
                "done": true,
                "prompt_eval_count": 10,
                "eval_count": 5,
                "total_duration": 1_000,
            });
            format!("{first}\n{last}\n")
        }
        _ => "{}".to_string(),
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// A step shaped like the Firefox runbook's step B: a count, a block that
/// runs only when the count is wrong and ends the step, and the block that
/// records and pushes otherwise. Then a step with one block.
const RUNBOOK: &str = "# Runbook: replay\n\n\
## Rules\n\n- Never leave the folder.\n\n\
## Step 1: count the files\n\n\
Count them:\n\n\
```bash\necho \"count: $(ls | wc -l | tr -d ' ')\"   # must print 3\n```\n\n\
A count in the hundreds means the clone is wrong: delete the marker and STOP:\n\n\
```bash\nrm -f a.txt\necho \"deleted: yes\"\n```\n\n\
Otherwise record the count and push:\n\n\
```bash\nn=$(ls | wc -l | tr -d ' ')\necho \"$n\" > count.txt\necho \"recorded: $n\"   # must print 3\n```\n\n\
## Step 2: tag it\n\n\
```bash\ngit tag v1 && echo \"tag: $(git tag -l v1)\"   # must print v1\n```\n\n\
## Reporting template\n\n\
```\ncount: <n>\ntag: <tag or n/a>\n```\n";

/// One run of [`RUNBOOK`] in a throwaway repository, every call recorded.
struct Run {
    events: Vec<TurnEvent>,
    requests: Vec<Value>,
    repo: PathBuf,
    dir: PathBuf,
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Run {
    fn transcript(&self) -> String {
        self.events
            .iter()
            .filter_map(|event| match event {
                TurnEvent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Every tool result, success or refusal, as text.
    fn outcomes(&self) -> Vec<String> {
        self.events
            .iter()
            .filter_map(|event| match event {
                TurnEvent::ToolCallResult { outcome, .. } => Some(match outcome {
                    ToolCallOutcome::Success(value) => value.to_string(),
                    ToolCallOutcome::Error(error) => error.clone(),
                    ToolCallOutcome::Cancelled => "(cancelled)".to_string(),
                }),
                _ => None,
            })
            .collect()
    }

    fn printed(&self, needle: &str) -> bool {
        self.outcomes()
            .iter()
            .any(|outcome| outcome.contains(needle))
    }

    fn completed(&self) -> Vec<String> {
        self.events
            .iter()
            .filter_map(|event| match event {
                TurnEvent::PlanUpdated {
                    update: PlanUpdate::Complete(ids),
                } => Some(ids.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn requests_of(&self, kind: Kind) -> Vec<Value> {
        self.requests
            .iter()
            .filter(|request| kind_of(request) == kind)
            .cloned()
            .collect()
    }

    fn file(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.repo.join(name)).ok()
    }
}

/// Runs [`RUNBOOK`] with `worker` scripted and the judge named `judge`
/// (one whose name says "think" thinks).
fn replay(judge: &str, script: Script) -> Run {
    let dir = std::env::temp_dir().join(format!("runbook-replay-{}", Uuid::new_v4()));
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    let setup = block_on(execute_shell(
        "git init -q --bare ../remote.git && git init -q && touch a.txt b.txt c.txt && git add . \
         && git commit -q -m init && git remote add origin ../remote.git && git push -q origin HEAD:refs/heads/main",
        &repo,
        &path,
    ));
    assert_eq!(setup.exit_code, 0, "{}", setup.stderr);
    let runbook_path = dir.join("runbook.md");
    std::fs::write(&runbook_path, RUNBOOK).unwrap();

    let fake = FakeOllama::start(script);
    let config = RunbookConfig {
        judge: Some(fake.model(judge)),
        helper: Some(fake.model("helper")),
    };
    let mut events = Vec::new();
    block_on(run_ollama_runbook(
        &runbook_path.to_string_lossy(),
        &Uuid::new_v4(),
        &repo,
        &path,
        &fake.model("worker"),
        config,
        Vec::new(),
        |event| events.push(event),
    ))
    .unwrap();
    Run {
        events,
        requests: fake.requests(),
        repo,
        dir,
    }
}

/// The step-B case a check settles: the count is 3, so block 2 (hundreds)
/// is out and block 3 (otherwise) is in.
fn count_is_three(number: usize) -> &'static str {
    match number {
        2 => "NO — the count printed was 3, not in the hundreds",
        3 => "YES — the count was 3, so the otherwise applies",
        _ => "YES",
    }
}

/// qwen3.5:4b (attempt 9) ran the count and answered that neither of the
/// blocks after it applied. The driver now puts block 2's case to the judge
/// (no), then block 3's (yes), and sends the worker to block 3.
#[test]
fn a_worker_that_stops_early_is_sent_to_the_block_whose_case_holds() {
    let run = replay(
        "judge",
        script(
            vec![
                block(1),
                text("The count is 3, so blocks 2 and 3 do not apply."),
                block(3),
                text("Recorded and pushed."),
                block(1),
                text("Tagged."),
            ],
            plain_judge(count_is_three),
        ),
    );
    let transcript = run.transcript();
    assert!(run.printed("recorded: 3"), "{transcript}");
    assert!(!run.printed("deleted: yes"), "{transcript}");
    assert_eq!(run.file("a.txt").as_deref(), Some(""), "block 2 never ran");
    assert_eq!(run.completed(), ["step-1", "step-2"], "{transcript}");
    let nudge = &run.requests_of(Kind::Worker)[2];
    assert!(
        last_of(nudge).starts_with(
            "Block 3 of this step has not run yet, and a check of what the step has printed says the case it \
             runs in holds"
        ),
        "{}",
        last_of(nudge)
    );
    // The judge was told block 2 was ruled out, not that it merely didn't run.
    let judged = &run.requests_of(Kind::Verdict)[0];
    assert!(last_of(judged)
        .contains("block 2 was not run, as a check found the case it runs in doesn't hold"));
    assert!(!last_of(judged).contains("block 3 of this step did not run"));
    // The report writer saw what the steps printed.
    let report = &run.requests_of(Kind::Report)[0];
    assert!(
        last_of(report).contains("recorded: 3"),
        "{}",
        last_of(report)
    );
    assert!(last_of(report).contains("tag: v1"));
    assert!(transcript.contains("count: 3\ntag: v1"), "{transcript}");
}

/// qwen3.5:4b (attempt 13), judging, thought its whole cap away on a
/// block's case and answered nothing, twice; the block went unrun. An empty
/// or unclear answer is now asked for again, in one word, without thinking.
#[test]
fn a_case_check_that_comes_back_empty_is_asked_again_plainly() {
    let mut asked = 0;
    let run = replay(
        "judge-think",
        script(
            vec![
                block(1),
                text("Done with the count."),
                block(3),
                text("Pushed."),
                block(1),
                text("Tagged."),
            ],
            move |kind, request| match kind {
                Kind::Case if case_block(request) == 2 => {
                    asked += 1;
                    if asked == 1 {
                        text("")
                    } else {
                        text("NO — the count printed was 3, not in the hundreds")
                    }
                }
                Kind::Case => text(count_is_three(case_block(request))),
                Kind::Verdict => verdict("pass"),
                Kind::Report => text("count: 3\ntag: v1"),
                Kind::Done | Kind::Worker | Kind::Other => text("NO"),
            },
        ),
    );
    let transcript = run.transcript();
    assert!(run.printed("recorded: 3"), "{transcript}");
    assert!(!run.printed("deleted: yes"), "{transcript}");
    let checks: Vec<Value> = run
        .requests_of(Kind::Case)
        .into_iter()
        .filter(|request| case_block(request) == 2)
        .collect();
    assert_eq!(checks.len(), 2, "asked once more");
    assert_eq!(checks[0]["think"], json!(true));
    assert_eq!(checks[1]["think"], json!(false));
    assert!(last_of(&checks[1]).ends_with("Start your reply with the one word YES or NO."));
    // A thinking judge doesn't think while filling in the report.
    assert_eq!(run.requests_of(Kind::Report)[0]["think"], json!(false));
}

/// qwen3.5:4b (attempt 15), asked whether "Otherwise record the snapshot
/// … and push" applied, read the refusal of the block before it in the
/// record and answered NO to this one too; the push was skipped and the
/// step passed. An "Otherwise" block is the alternative to the conditional
/// block before it: that block's ruling decides it, and no judge is asked.
#[test]
fn an_otherwise_block_runs_on_the_ruling_of_the_block_before_it() {
    let run = replay(
        "judge",
        script(
            vec![
                block(1),
                block(2),
                block(3),
                text("Recorded."),
                block(1),
                text("Tagged."),
            ],
            |kind, _| match kind {
                // A judge that says NO to everything, block 3 included.
                Kind::Case => text("NO — the count printed was 3, not in the hundreds"),
                Kind::Verdict => verdict("pass"),
                Kind::Report => text("count: 3\ntag: v1"),
                Kind::Done | Kind::Worker | Kind::Other => text("NO"),
            },
        ),
    );
    let transcript = run.transcript();
    assert!(!run.printed("deleted: yes"), "{transcript}");
    assert!(run.printed("recorded: 3"), "{transcript}");
    assert!(
        run.requests_of(Kind::Case)
            .iter()
            .all(|request| case_block(request) == 2),
        "block 3 was never put to the judge"
    );
    assert_eq!(run.completed(), ["step-1", "step-2"], "{transcript}");
    assert!(system_of(&run.requests_of(Kind::Case)[0]).contains("Decide this block's own case"));
}

/// qwen3.5:9b (attempts 5–7) ran "abort the merge and STOP" after a pass
/// that had left nothing over. A block whose case the judge rules out is
/// refused; asked for again, it is refused without a second check.
#[test]
fn a_block_whose_case_fails_is_refused_once_checked() {
    let run = replay(
        "judge",
        script(
            vec![
                block(1),
                block(2),
                block(2),
                block(3),
                text("Recorded."),
                block(1),
                text("Tagged."),
            ],
            plain_judge(count_is_three),
        ),
    );
    let transcript = run.transcript();
    assert!(!run.printed("deleted: yes"), "{transcript}");
    assert!(run.printed("recorded: 3"), "{transcript}");
    let outcomes = run.outcomes();
    assert!(
        outcomes
            .iter()
            .any(|outcome| outcome.starts_with("Not run: block 2 runs only in the case")),
        "{outcomes:?}"
    );
    let checks = run.requests_of(Kind::Case);
    assert_eq!(
        checks
            .iter()
            .filter(|request| case_block(request) == 2)
            .count(),
        1,
        "the second call got the ruling, not a second check"
    );
    let second = &run.requests_of(Kind::Worker)[3];
    assert!(
        last_of(second).contains("That was checked already"),
        "{}",
        last_of(second)
    );
    assert_eq!(run.completed(), ["step-1", "step-2"]);
}

/// A check that can't say either way lets an ordinary block run on the
/// worker's judgement, but not one that ends the step.
#[test]
fn an_unclear_check_keeps_a_stop_block_from_running() {
    let run = replay(
        "judge",
        script(
            vec![
                block(1),
                block(2),
                text("The count is fine."),
                block(1),
                text("Tagged."),
            ],
            |kind, _| match kind {
                Kind::Case => text("It depends on what the count means."),
                Kind::Verdict => verdict("pass"),
                Kind::Report => text("count: 3\ntag: n/a"),
                Kind::Done | Kind::Worker | Kind::Other => text("NO"),
            },
        ),
    );
    let transcript = run.transcript();
    assert!(!run.printed("deleted: yes"), "{transcript}");
    assert!(
        run.outcomes().iter().any(|outcome| outcome
            .contains("the check gave no clear answer, and this block ends the step")),
        "{:?}",
        run.outcomes()
    );
    assert!(
        transcript.contains("↳ Block 2's case, checked: no clear answer"),
        "{transcript}"
    );
    // With no clear answer on block 3 either, the judge's pass stands.
    assert_eq!(run.completed(), ["step-1", "step-2"], "{transcript}");
}

/// qwen3.5:4b (attempt 12), after a merge failed, improvised in its shell:
/// a branch, a merge, a commit. The shell is for looking.
#[test]
fn the_free_shell_is_refused_a_change_and_allowed_a_look() {
    let run = replay(
        "judge",
        script(
            vec![
                shell("git checkout -b temp && git commit --allow-empty -m x"),
                shell("git status --short"),
                block(1),
                text("Counted."),
                block(3),
                text("Done."),
                block(1),
                text("Tagged."),
            ],
            plain_judge(count_is_three),
        ),
    );
    let outcomes = run.outcomes();
    assert!(
        outcomes[0].starts_with("Refused: the shell tool is for looking at things"),
        "{}",
        outcomes[0]
    );
    assert!(
        outcomes[1].contains("\"stdout\""),
        "the look ran: {}",
        outcomes[1]
    );
    let branches = block_on(execute_shell(
        "git branch --list temp",
        &run.repo,
        &std::env::var("PATH").unwrap_or_default(),
    ));
    assert_eq!(branches.stdout.trim(), "", "no branch was made");
    assert_eq!(run.completed(), ["step-1", "step-2"]);
}

/// llama3.1:8b (attempts 3–4) wrote its calls into its text, in its own
/// format, and every one came through with no arguments.
#[test]
fn a_call_written_as_text_in_llamas_format_runs_the_block() {
    let text_call = |number: usize| {
        text(&format!(
            "Here is the JSON for the function call:\n\n```json\n{{\n  \"name\": \"run_block\",\n  \
             \"parameters\": {{\n    \"block\": {number},\n    \"set\": {{}}\n  }}\n}}\n```"
        ))
    };
    let run = replay(
        "judge",
        script(
            vec![
                text_call(1),
                text("The count is 3."),
                text_call(3),
                text("Recorded."),
                text_call(1),
                text("Tagged."),
            ],
            plain_judge(count_is_three),
        ),
    );
    let transcript = run.transcript();
    assert!(run.printed("count: 3"), "{transcript}");
    assert!(run.printed("recorded: 3"), "{transcript}");
    // The result reached a model that ignores the tool role as a user message.
    let after_first = &run.requests_of(Kind::Worker)[1];
    let last = after_first["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(last["role"], "user");
    assert!(last["content"]
        .as_str()
        .unwrap()
        .starts_with("Result of run_block:"));
    assert_eq!(run.completed(), ["step-1", "step-2"]);
}

/// llama3.1:8b (attempt 4) sent `run_block` with no arguments fifteen times.
/// It is refused with what to send, never guessed at.
#[test]
fn a_bare_run_block_is_refused_with_what_to_send() {
    let run = replay(
        "judge",
        script(
            vec![
                call(RUN_BLOCK, json!({})),
                block(1),
                text("Counted."),
                block(3),
                text("Done."),
                block(1),
                text("Tagged."),
            ],
            plain_judge(count_is_three),
        ),
    );
    let outcomes = run.outcomes();
    assert!(
        outcomes[0].starts_with(
            "That call named no block (it sent {}). Call run_block with the block's number"
        ),
        "{}",
        outcomes[0]
    );
    assert!(run.printed("count: 3"));
    assert_eq!(run.completed(), ["step-1", "step-2"]);
}

/// qwen3.5:4b (attempt 10), judging, called a finished step "done", which
/// ends the whole run; the last step never ran. With steps left, "done" is
/// put to the judge as a question.
#[test]
fn a_done_verdict_with_steps_left_is_put_to_the_judge() {
    let mut verdicts = 0;
    let run = replay(
        "judge",
        script(
            vec![block(1), block(3), text("Done."), block(1), text("Tagged.")],
            move |kind, request| match kind {
                Kind::Verdict => {
                    verdicts += 1;
                    verdict(if verdicts == 1 { "done" } else { "pass" })
                }
                Kind::Done => {
                    text("NO. The step counted and recorded; the tag step is still to do.")
                }
                Kind::Case => text(count_is_three(case_block(request))),
                Kind::Report => text("count: 3\ntag: v1"),
                Kind::Worker | Kind::Other => text("Done."),
            },
        ),
    );
    let transcript = run.transcript();
    assert_eq!(run.requests_of(Kind::Done).len(), 1, "{transcript}");
    assert!(run.printed("tag: v1"), "{transcript}");
    assert_eq!(run.completed(), ["step-1", "step-2"], "{transcript}");
    assert!(
        !transcript.contains("Done — nothing more to do"),
        "{transcript}"
    );
}

/// qwen3.5:4b (attempt 9), judging, passed step B on the checks it could
/// see while the block that records and pushes never ran. A block with a
/// `# must print` check that neither ran nor was ruled out fails the pass.
#[test]
fn a_pass_with_a_printed_check_never_made_is_a_stop() {
    let run = replay(
        "judge",
        script(
            vec![
                block(1),
                text("Neither block applies."),
                text("Still nothing to do."),
            ],
            plain_judge(count_is_three),
        ),
    );
    let transcript = run.transcript();
    assert!(
        transcript.contains("■ **Stopped at Step 1: count the files.**"),
        "{transcript}"
    );
    assert!(
        transcript.contains(
            "block 3 did not run, and its case wasn't ruled out (\"recorded:\" must print 3)"
        ),
        "{transcript}"
    );
    assert_eq!(run.completed(), Vec::<String>::new());
}

/// llama3.1:8b pasted a block's text into the shell instead of calling
/// `run_block`. It counts as that block: its checks run, and it isn't asked
/// for again.
#[test]
fn a_block_pasted_into_the_shell_counts_as_that_block() {
    let run = replay(
        "judge",
        script(
            vec![
                shell("echo \"count: $(ls | wc -l | tr -d ' ')\"   # must print 3"),
                text("Counted."),
                block(3),
                text("Done."),
                block(1),
                text("Tagged."),
            ],
            plain_judge(count_is_three),
        ),
    );
    let transcript = run.transcript();
    assert!(run.printed("count: 3"), "{transcript}");
    let judged = &run.requests_of(Kind::Verdict)[0];
    assert!(
        last_of(judged).contains("[1] block 1"),
        "{}",
        last_of(judged)
    );
    assert!(
        last_of(judged).contains("checked by the driver: \"count:\" printed 3"),
        "the block's check ran: {}",
        last_of(judged)
    );
    let nudge = &run.requests_of(Kind::Worker)[2];
    assert!(
        last_of(nudge).starts_with("Block 3 of this step"),
        "{}",
        last_of(nudge)
    );
    assert_eq!(run.completed(), ["step-1", "step-2"]);
}

/// When the block the runbook stops at has run, nothing after it does.
#[test]
fn after_a_stop_block_runs_nothing_else_does() {
    let run = replay(
        "judge",
        script(
            vec![block(1), block(2), block(3), text("Deleted and stopped.")],
            |kind, request| match kind {
                Kind::Case if case_block(request) == 2 => {
                    text("YES — the count is in the hundreds")
                }
                Kind::Case => text("YES"),
                Kind::Verdict => verdict("stop"),
                Kind::Report => text("count: n/a\ntag: n/a"),
                Kind::Done | Kind::Worker | Kind::Other => text("NO"),
            },
        ),
    );
    let transcript = run.transcript();
    assert!(run.printed("deleted: yes"), "{transcript}");
    assert!(!run.printed("recorded: 3"), "{transcript}");
    let outcomes = run.outcomes();
    assert!(
        outcomes
            .iter()
            .any(|outcome| outcome.starts_with("Not run: the runbook says to STOP after block 2")),
        "{outcomes:?}"
    );
    let told = &run.requests_of(Kind::Worker)[2];
    assert!(
        last_of(told).contains("The runbook says to STOP after this block"),
        "{}",
        last_of(told)
    );
    assert!(transcript.contains("■ **Stopped at Step 1"), "{transcript}");
}
