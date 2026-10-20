use std::path::Path;

use ai::agent::{PlanUpdate, ToolCallOutcome, TurnEvent};
use futures_lite::future::block_on;

use super::*;

/// The shape of a real runbook (bork's Firefox update), cut down: background
/// and rules before the steps, a heading inside a code block, a conditional
/// step, a report template at the end.
const RUNBOOK: &str = r#"# Runbook: rebuilding on a new release

This is an operating procedure for an AI agent.

## Background: how the pieces fit

The runtime lives in this repository.

## Hard rules

- Never force-push `main`.

## Step 0: is there anything to do?

```bash
V=158.0.1                          # the new version
T="FIREFOX_${V//./_}_RELEASE"
git ls-remote --tags https://example.invalid "refs/tags/$T"
```

No output means it was never tagged: STOP.

## Step B (one time only): bootstrap the snapshot history

Only if `git tag -l 'firefox-*'` is empty.

```sh
# ## not a heading, just a comment
git tag -l 'firefox-*'
```

## Step 1: import the new release

```bash
git status --short            # must be empty; if not, STOP
git rev-parse --is-shallow-repository           # must print false
```

```ini
[not]
runnable = true
```

`version.txt` must now read `$V`.

## Reporting template

```
release <old> -> <new>
stopped at step <n>: <why>
```
"#;

#[test]
fn parses_steps_rules_and_report_from_a_real_shaped_runbook() {
    let runbook = parse_runbook(RUNBOOK);
    assert_eq!(runbook.title, "Runbook: rebuilding on a new release");
    let ids: Vec<&str> = runbook.steps.iter().map(|step| step.id.as_str()).collect();
    assert_eq!(ids, ["step-0", "step-b", "step-1"]);
    assert_eq!(
        runbook.steps[1].title,
        "Step B (one time only): bootstrap the snapshot history"
    );
    // Context sections are everyone's preamble, not steps.
    assert!(runbook.preamble.contains("operating procedure"));
    assert!(runbook.preamble.contains("## Hard rules"));
    assert!(runbook.preamble.contains("Never force-push"));
    assert!(runbook.preamble.contains("## Background"));
    // A `##` inside a fenced block is code, not a section break.
    assert!(runbook.steps[1].blocks[0].code.contains("## not a heading"));
    // The template is the fenced body of the reporting section.
    assert_eq!(
        runbook.report_template.as_deref(),
        Some("release <old> -> <new>\nstopped at step <n>: <why>")
    );
    let step_1 = &runbook.steps[2];
    assert_eq!(step_1.blocks.len(), 2);
    assert!(step_1.blocks[0].runnable());
    assert!(
        !step_1.blocks[1].runnable(),
        "an ini sample is not a shell block"
    );
}

#[test]
fn sections_become_steps_when_a_runbook_never_says_step() {
    let runbook = parse_runbook(
        "# Deploy\n\n## Overview\n\nWhat this is.\n\n## Build it\n\n```bash\nmake\n```\n\n## Ship it\n\n```bash\nmake ship\n```\n",
    );
    let titles: Vec<&str> = runbook
        .steps
        .iter()
        .map(|step| step.title.as_str())
        .collect();
    assert_eq!(
        titles,
        ["Build it", "Ship it"],
        "the overview stays context"
    );
    assert_eq!(runbook.steps[0].id, "step-1");
    assert_eq!(runbook.steps[1].id, "step-2");
    assert!(runbook.preamble.contains("What this is."));
}

#[test]
fn duplicate_step_labels_still_get_distinct_ids() {
    let runbook = parse_runbook("## Step 1: a\n\ntext\n\n## Step 1: again\n\ntext\n");
    let ids: Vec<&str> = runbook.steps.iter().map(|step| step.id.as_str()).collect();
    assert_eq!(ids, ["step-1", "step-1-2"]);
}

#[test]
fn the_worker_sees_runnable_blocks_numbered_and_the_judge_sees_the_checks() {
    let runbook = parse_runbook(RUNBOOK);
    let step_1 = &runbook.steps[2];
    let annotated = step_1.annotated_body();
    assert!(annotated.contains("**Block 1** (`run_block` 1):\n```bash"));
    assert!(
        !annotated.contains("**Block 2**"),
        "the ini block is not offered to run_block"
    );

    let checks = step_1.stated_checks();
    assert!(checks
        .iter()
        .any(|check| check.contains("must be empty; if not, STOP")));
    assert!(checks
        .iter()
        .any(|check| check.contains("must print false")));
    assert!(checks.iter().any(|check| check.contains("must now read")));
    assert!(runbook.steps[0]
        .stated_checks()
        .iter()
        .any(|check| check.contains("never tagged: STOP")));
    assert_eq!(
        runbook.steps[1].summary(),
        "Only if git tag -l 'firefox-*' is empty."
    );
}

fn facts(pairs: &[(&str, &str)]) -> Facts {
    pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

#[test]
fn a_known_value_beats_the_runbooks_example_value() {
    let code = "V=158.0.1                          # the new version\nT=\"FIREFOX_${V//./_}_RELEASE\"\necho \"$T\"";
    let prepared = prepare_block(code, &Facts::new(), &facts(&[("V", "158.0")]));
    assert!(
        prepared
            .display
            .starts_with("V='158.0'  # the new version\n"),
        "{}",
        prepared.display
    );
    // Computed values are left to the shell.
    assert!(prepared.display.contains("T=\"FIREFOX_${V//./_}_RELEASE\""));
    assert!(prepared.script.starts_with("export V='158.0'\n"));
    assert!(prepared.script.contains(FACTS_MARKER));
    assert!(
        !prepared.display.contains(FACTS_MARKER),
        "the card shows no plumbing"
    );
}

#[test]
fn set_wins_over_known_and_quotes_are_safe() {
    let prepared = prepare_block(
        "  NAME=placeholder",
        &facts(&[("NAME", "it's")]),
        &facts(&[("NAME", "older")]),
    );
    assert_eq!(prepared.display, "  NAME='it'\\''s'");
}

#[test]
fn exports_are_not_assignments_and_lowercase_is_local() {
    let code = "export GIT_INDEX_FILE=/tmp/x\ntree=$(git write-tree)\necho ok";
    let prepared = prepare_block(code, &Facts::new(), &facts(&[("GIT_INDEX_FILE", "stale")]));
    assert_eq!(prepared.display, code, "nothing here is a fact assignment");
    assert!(
        !prepared.script.contains(FACTS_MARKER),
        "nothing to capture, so no trailer"
    );
}

#[test]
fn a_prepared_block_runs_carries_values_and_keeps_its_exit_status() {
    let dir = std::env::temp_dir().join(format!("runbook-exec-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = std::env::var("PATH").unwrap_or_default();

    let block = "V=1.0  # example\nT=\"X_${V//./_}\"\necho \"tag $T\"";
    let prepared = prepare_block(block, &facts(&[("V", "2.5")]), &Facts::new());
    let execution = block_on(execute_shell(&prepared.script, &dir, &path));
    assert_eq!(execution.exit_code, 0, "stderr: {}", execution.stderr);
    let (stdout, captured) = split_captured_facts(&execution.stdout);
    assert_eq!(stdout, "tag X_2_5");
    assert_eq!(captured, facts(&[("T", "X_2_5"), ("V", "2.5")]));

    // A later block sees the earlier one's values without re-declaring them.
    let later = prepare_block("echo \"still $T\"", &Facts::new(), &captured);
    let execution = block_on(execute_shell(&later.script, &dir, &path));
    assert_eq!(execution.stdout.trim(), "still X_2_5");

    // The trailer reports values but never masks a failure.
    let failing = prepare_block("V=3\nfalse", &Facts::new(), &Facts::new());
    let execution = block_on(execute_shell(&failing.script, &dir, &path));
    assert_eq!(execution.exit_code, 1);
    assert_eq!(
        split_captured_facts(&execution.stdout).1,
        facts(&[("V", "3")])
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reported_values_skip_empty_and_non_fact_names() {
    let (stdout, captured) = split_captured_facts(&format!(
        "out\n\n{FACTS_MARKER}\nV=1\nEMPTY=\nlower=x\nNOEQUALS\n"
    ));
    assert_eq!(stdout, "out");
    assert_eq!(captured, facts(&[("V", "1")]));
    assert_eq!(split_captured_facts("no marker").0, "no marker");
}

#[test]
fn the_fence_catches_absolute_paths_outside_the_target() {
    let root = Path::new("/sandbox/repo");
    assert!(escapes_root("cd /Volumes/real/repo", root).is_some());
    assert!(escapes_root("git status && cd /elsewhere; ls", root).is_some());
    assert!(
        escapes_root("cd /sandbox/repo/../../real", root).is_some(),
        "`..` is resolved"
    );
    assert!(escapes_root("rm -rf /real/objdir", root).is_some());
    assert!(escapes_root("rm -r -f /real/objdir", root).is_some());

    assert!(escapes_root("cd /sandbox/repo/sub", root).is_none());
    assert!(escapes_root("cd ..", root).is_none(), "relative paths pass");
    assert!(
        escapes_root("rm -f /real/file", root).is_none(),
        "only recursive deletes are fenced"
    );
    assert!(escapes_root("rm -rf /sandbox/repo/tmp", root).is_none());
    assert!(
        escapes_root("rm -f \"$(git rev-parse --git-dir)/import.index\"", root).is_none(),
        "computed paths pass"
    );
}

#[test]
fn only_local_push_remotes_count_as_a_sandbox() {
    let remotes = "origin\t/Volumes/DATA/sandbox/remote.git (fetch)\n\
                   origin\t/Volumes/DATA/sandbox/remote.git (push)\n\
                   mirror\tgit@github.com:someone/repo.git (push)\n\
                   web\thttps://example.com/repo.git (fetch)\n";
    assert_eq!(
        network_push_remotes(remotes),
        ["mirror → git@github.com:someone/repo.git"]
    );
    assert!(
        network_push_remotes("origin\t../bare.git (push)\nup\tfile:///srv/x (push)").is_empty()
    );
}

#[test]
fn finds_the_runbook_file_however_the_prompt_names_it() {
    let dir = std::env::temp_dir().join(format!("runbook-req-{}", Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("with space")).unwrap();
    let file = dir.join("with space").join("rb.md");
    std::fs::write(&file, "# r").unwrap();

    let exact = parse_runbook_request(&file.to_string_lossy(), Path::new("/")).unwrap();
    assert_eq!(exact.path, file);
    assert_eq!(exact.notes, "");

    let escaped = format!(
        "{} V=158.0 go slowly",
        file.to_string_lossy().replace(' ', "\\ ")
    );
    let request = parse_runbook_request(&escaped, Path::new("/")).unwrap();
    assert_eq!(request.path, file);
    assert_eq!(request.notes, "V=158.0 go slowly");
    assert_eq!(request.facts, facts(&[("V", "158.0")]));

    let relative = parse_runbook_request("please run 'with space/rb.md'", &dir).unwrap();
    assert_eq!(relative.path, file);
    assert_eq!(relative.notes, "please run");

    assert!(parse_runbook_request("no file here", &dir).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reads_the_judges_verdict_through_prose_and_fences() {
    let verdict = parse_verdict(
        "Here you go:\n```json\n{\"verdict\": \"Pass\", \"reason\": \"printed false\", \
         \"facts\": {\"V\": \"158.0\", \"N\": 3, \"lower\": \"x\", \"EMPTY\": \"\"}}\n```",
    )
    .unwrap();
    assert_eq!(verdict.kind, VerdictKind::Pass);
    assert_eq!(verdict.reason, "printed false");
    assert_eq!(verdict.facts, facts(&[("N", "3"), ("V", "158.0")]));

    assert_eq!(
        parse_verdict("{\"verdict\":\"skipped\",\"reason\":\"tags exist\"}")
            .unwrap()
            .kind,
        VerdictKind::Skip
    );
    assert_eq!(
        parse_verdict("{\"verdict\":\"done\"}").unwrap().kind,
        VerdictKind::Done
    );
    assert_eq!(
        parse_verdict("{\"verdict\":\"failed\"}").unwrap().kind,
        VerdictKind::Stop
    );
    assert!(parse_verdict("{\"verdict\":\"maybe\"}").is_none());
    assert!(parse_verdict("no json at all").is_none());
}

#[test]
fn picks_a_preferred_judge_that_is_installed_and_switched_on() {
    let installed: Vec<String> = ["qwen2.5-coder:14b", "qwen3:14b", "deepseek-r1:14b"]
        .iter()
        .map(|tag| tag.to_string())
        .collect();
    assert_eq!(
        pick_judge(&installed, "qwen2.5-coder:14b", &[]),
        "deepseek-r1:14b"
    );
    assert_eq!(
        pick_judge(
            &installed,
            "qwen2.5-coder:14b",
            &["deepseek-r1:14b".to_string()]
        ),
        "qwen3:14b"
    );
    assert_eq!(
        pick_judge(&["llama3.1:8b".to_string()], "llama3.1:8b", &[]),
        "llama3.1:8b",
        "with nothing preferred installed, the worker judges itself"
    );
}

#[test]
fn the_judge_setting_round_trips_and_cycles() {
    assert_eq!(
        RunbookConfig::from_settings_json(None),
        RunbookConfig::default()
    );
    assert_eq!(
        RunbookConfig::from_settings_json(Some("{\"judge\":\"auto\"}")).judge,
        None
    );
    let chosen = RunbookConfig {
        judge: Some("qwen3:14b".to_string()),
        helper: Some("qwen3.5:9b".to_string()),
    };
    assert_eq!(
        RunbookConfig::from_settings_json(Some(&chosen.to_settings_json())),
        chosen
    );
    assert_eq!(
        RunbookConfig::from_settings_json(Some("{\"judge\":\"phi4:14b\"}")),
        RunbookConfig {
            judge: Some("phi4:14b".to_string()),
            helper: None,
        },
        "a setting saved before the helper existed still reads"
    );
    assert_eq!(
        RunbookConfig::from_settings_json(Some("not json")),
        RunbookConfig::default()
    );

    let installed = vec!["a:1b".to_string(), "b:2b".to_string()];
    let mut config = RunbookConfig::default();
    cycle_judge(&mut config, &installed);
    assert_eq!(config.judge.as_deref(), Some("a:1b"));
    cycle_judge(&mut config, &installed);
    assert_eq!(config.judge.as_deref(), Some("b:2b"));
    cycle_judge(&mut config, &installed);
    assert_eq!(config.judge, None, "past the last model comes back to auto");

    cycle_helper(&mut config, &installed);
    assert_eq!(config.helper.as_deref(), Some("a:1b"));
    assert_eq!(
        config.judge, None,
        "cycling the helper leaves the judge alone"
    );
}

#[test]
fn a_model_that_was_slow_to_load_is_tried_again_but_a_real_refusal_is_not() {
    assert!(is_transient_failure(
        "timed out waiting for llama-server to start - "
    ));
    assert!(is_transient_failure(
        "llama runner process has terminated: exit status 2"
    ));
    assert!(is_transient_failure(
        "could not reach Ollama for `qwen3:14b` at http://127.0.0.1:11434"
    ));
    assert!(!is_transient_failure(
        "model \"nope:1b\" not found, try pulling it first"
    ));
    assert!(!is_transient_failure(
        "\"qwen2.5-coder:14b\" does not support thinking"
    ));
    assert!(!is_transient_failure("stopped"));
}

#[test]
fn a_runbook_path_that_is_not_there_gets_a_suggestion() {
    let root = std::env::temp_dir().join(format!("runbook-missing-{}", Uuid::new_v4()));
    let work = root.join("sandbox").join("bork-runtime");
    std::fs::create_dir_all(&work).unwrap();
    let runbook = root.join("sandbox").join("update.sandbox.md");
    std::fs::write(&runbook, "# x\n").unwrap();

    let missing = missing_runbook_paths("/agent /Volumes/DATA/github/update.sandbox.md", &work);
    assert_eq!(
        missing,
        vec![(
            "/Volumes/DATA/github/update.sandbox.md".to_string(),
            Some(runbook.clone())
        )]
    );
    assert_eq!(path_from(&work, &runbook), "../update.sandbox.md");
    assert_eq!(
        missing_runbook_paths("nowhere/else.md", &work),
        vec![("nowhere/else.md".to_string(), None)]
    );
    assert!(missing_runbook_paths("just some words", &work).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

fn tool_call(name: &str, arguments: serde_json::Value) -> OllamaToolCall {
    OllamaToolCall {
        name: name.to_string(),
        arguments,
    }
}

#[test]
fn the_same_block_called_twice_is_recognized_as_one_call() {
    let one = tool_call("run_block", serde_json::json!({ "block": 1 }));
    let one_as_text = tool_call("run_block", serde_json::json!({ "block": "1" }));
    let one_with_value = tool_call(
        "run_block",
        serde_json::json!({ "block": 1, "set": { "V": "158.0" } }),
    );
    let two = tool_call("run_block", serde_json::json!({ "block": 2 }));
    assert_eq!(call_signature(&one), call_signature(&one_as_text));
    assert_ne!(call_signature(&one), call_signature(&one_with_value));
    assert_ne!(call_signature(&one), call_signature(&two));
    assert_eq!(run_block_number(&two), Some(2));
    let shell = tool_call("shell", serde_json::json!({ "command": "git status" }));
    assert_eq!(run_block_number(&shell), None);
    assert!(call_signature(&shell).starts_with("shell "));

    let runbook = parse_runbook(RUNBOOK);
    let step_1 = &runbook.steps[2];
    assert_eq!(
        offered_block_numbers(step_1)
            .into_iter()
            .collect::<Vec<_>>(),
        vec![1]
    );
}

/// What llama3.1:8b did on the first small-model Firefox run: `run_block`
/// with no number, block 1 pasted into the shell, and an answer about blocks
/// that never ran. The step is shaped like that run's step B: two blocks
/// that always run, one only when a check fails (`git tag -d … STOP`), and
/// the "otherwise" one that pushes.
#[test]
fn a_small_workers_loose_calls_are_read_as_the_blocks_they_mean() {
    let runbook = parse_runbook(
        "# Runbook: bootstrap\n\n## Step B: snapshot\n\nMake the snapshot:\n\n```bash\necho one\necho \"V=1\"\n```\n\n\
         Then check the difference:\n\n```bash\necho 37\n```\n\nA count in the hundreds means the wrong version: \
         delete the tag and STOP:\n\n```bash\ngit tag -d snap\n```\n\nOtherwise record it and push:\n\n```bash\n\
         git push origin main\n```\n",
    );
    let step = &runbook.steps[0];
    assert_eq!(step.blocks[0].lead_in, "Make the snapshot:");
    assert!(!step.blocks[0].is_conditional());
    assert!(!step.blocks[1].is_conditional());
    assert!(step.blocks[2].is_conditional());
    assert!(step.blocks[3].is_conditional());

    // No number: not guessed at — the next block could be the `git tag -d`.
    let bare = tool_call("run_block", serde_json::json!({}));
    assert_eq!(run_block_number(&normalize_call(&bare, step)), None);

    // The number some other way.
    for arguments in [
        serde_json::json!({ "number": 2 }),
        serde_json::json!({ "block": 2.0 }),
        serde_json::json!({ "block": "block 2" }),
        serde_json::json!({ "which": 2 }),
    ] {
        let call = normalize_call(&tool_call("run_block", arguments), step);
        assert_eq!(run_block_number(&call), Some(2));
    }
    let with_value = tool_call(
        "run_block",
        serde_json::json!({ "number": 1, "set": { "V": "2" } }),
    );
    assert_eq!(normalize_call(&with_value, step).arguments["set"]["V"], "2");

    // A block pasted into the shell is that block; any other command isn't.
    let pasted = tool_call(
        "shell",
        serde_json::json!({ "command": "  echo one\n\necho \"V=1\"  " }),
    );
    assert_eq!(run_block_number(&normalize_call(&pasted, step)), Some(1));
    let other = tool_call("shell", serde_json::json!({ "command": "echo one" }));
    assert_eq!(normalize_call(&other, step).name, "shell");

    // The nudge: block 2 first, which runs in any case; then blocks 3 and 4
    // to choose between, once block 2's output is there to choose by.
    let unrun: Vec<(usize, &RunbookBlock)> = (2..=4).map(|n| (n, &step.blocks[n - 1])).collect();
    assert_eq!(
        unrun_blocks_nudge(&unrun),
        "Block 2 of this step has not run yet, and the step runs it in any case (\"Then check the \
         difference:\"): run it with run_block ({\"block\": 2}). Then reply with your summary — or reply \
         now, if the step says this doesn't apply here."
    );
    let nudge = unrun_blocks_nudge(&unrun[1..]);
    assert!(
        nudge.starts_with(
            "The blocks left run only in some cases — block 3 (\"A count in the hundreds"
        ),
        "{nudge}"
    );
    assert!(nudge.contains("block 4 (\"Otherwise record it and push:\")"));
    assert!(nudge.contains("Run the one the output so far calls for, if any"));

    // After each call the worker is told where the step stands.
    assert_eq!(
        next_blocks_hint(step, &BTreeSet::from([1])).unwrap(),
        "Next in this step: block 2 (\"Then check the difference:\") — run_block {\"block\": 2}."
    );
    let choice = next_blocks_hint(step, &BTreeSet::from([1, 2])).unwrap();
    assert!(
        choice.starts_with(
            "What comes next in this step: block 3, only in some cases (\"A count in the hundreds"
        ),
        "{choice}"
    );
    assert!(choice.ends_with(
        "block 4, only in some cases (\"Otherwise record it and push:\"). Run whichever the output so far \
         calls for, in order."
    ));
    assert_eq!(next_blocks_hint(step, &BTreeSet::from([1, 2, 4])), None);

    // Arguments sent as a JSON string, or as a number on their own.
    for arguments in [
        serde_json::json!("{\"block\": 3}"),
        serde_json::json!("3"),
        serde_json::json!(3),
    ] {
        assert_eq!(loose_block_number(&arguments), Some(3));
    }
}

#[test]
fn must_print_comments_become_checks_the_driver_runs() {
    let block = r#"echo "shallow: $(git rev-parse --is-shallow-repository)"              # must print false
echo "snapshot version: $(git show "firefox-${V}:browser/config/version.txt")"   # must print V
echo "files changed by the merge: $(git diff HEAD~1 HEAD --name-only | wc -l)"   # must print 0
echo "main still ships: $(cat browser/config/version.txt)"              # the OLD version
git rev-parse --is-shallow-repository           # must print false"#;
    let checks = print_checks(block);
    assert_eq!(
        checks,
        vec![
            PrintCheck {
                label: "shallow:".into(),
                expected: "false".into()
            },
            PrintCheck {
                label: "snapshot version:".into(),
                expected: "V".into()
            },
            PrintCheck {
                label: "files changed by the merge:".into(),
                expected: "0".into()
            },
        ]
    );

    let facts = facts(&[("V", "153.0.4")]);
    let stdout =
        "shallow: false\nsnapshot version: 152.0.6\nfiles changed by the merge:        0\n";
    let verdicts = run_print_checks(&checks, stdout, &facts);
    assert!(verdicts[0].starts_with("✓") && verdicts[0].contains("printed false"));
    assert!(
        verdicts[1].starts_with("✗") && verdicts[1].contains("requires 153.0.4"),
        "{}",
        verdicts[1]
    );
    assert!(
        verdicts[2].starts_with("✓"),
        "wc's padding is trimmed: {}",
        verdicts[2]
    );
    let missing = run_print_checks(&checks[..1], "nothing here", &facts);
    assert!(missing[0].contains("no \"shallow:\" line was printed"));
}

#[test]
fn the_judge_can_only_carry_values_the_runbook_uses() {
    let runbook = parse_runbook(RUNBOOK);
    assert!(runbook_uses_name(&runbook, "V"));
    assert!(!runbook_uses_name(&runbook, "NAME"));
    assert!(!runbook_uses_name(&runbook, "lowercase"));
}

#[test]
fn picks_a_small_helper_that_is_installed_and_switched_on() {
    let installed: Vec<String> = ["qwen2.5-coder:14b", "granite4:7b-a1b-h", "qwen3.5:9b"]
        .iter()
        .map(|tag| tag.to_string())
        .collect();
    assert_eq!(
        pick_helper(&installed, "qwen2.5-coder:14b", &[]),
        "qwen3.5:9b"
    );
    assert_eq!(
        pick_helper(&installed, "qwen2.5-coder:14b", &["qwen3.5:9b".to_string()]),
        "granite4:7b-a1b-h"
    );
    assert_eq!(
        pick_helper(&["qwen2.5-coder:14b".to_string()], "qwen2.5-coder:14b", &[]),
        "qwen2.5-coder:14b",
        "with no small model installed, the worker checks its own fixes"
    );
}

#[test]
fn a_resolve_block_is_offered_to_run_block_with_its_own_label() {
    let runbook = parse_runbook(
        "## Step 1: merge\n\n```bash\ngit merge x\n```\n\n```resolve\nreference: refs/floorp/main\n```\n\n\
         ```ini\nnot = run\n```\n",
    );
    let step = &runbook.steps[0];
    assert!(step.blocks[1].is_resolve());
    assert!(step.blocks[1].offered() && !step.blocks[1].runnable());
    assert!(!step.blocks[2].offered());
    let annotated = step.annotated_body();
    assert!(annotated.contains("**Block 2** (`run_block` 2 — works through the merge conflicts"));
    assert!(!annotated.contains("**Block 3**"));
    assert_eq!(runnable_block_numbers(step), "1, 2");
}

#[test]
fn long_output_keeps_its_head_and_tail() {
    let text = format!("{}{}", "h".repeat(5_000), "TAIL");
    let clipped = clip_output(&text);
    assert!(clipped.ends_with("TAIL"));
    assert!(clipped.starts_with("hhh"));
    assert!(clipped.contains("characters omitted"));
    assert_eq!(clip_output("short"), "short");
}

/// The first Firefox run's report said "merge conflicts: none" and "pushed:
/// nothing" after a merge with five conflicts and four pushes: its writer saw
/// only the verdicts. It now sees the end of what each call printed.
#[test]
fn the_report_writer_sees_what_each_step_printed() {
    let merge = format!(
        "exit=1\n{}Automatic merge failed\n--- conflicted files:\nAGENTS.md\nconfig/milestone.txt",
        "Auto-merging some/file.cpp\n".repeat(60)
    );
    let records = vec![
        CallRecord {
            label: "run_block 1".to_string(),
            exit_code: Some(0),
            output: merge,
        },
        CallRecord {
            label: "resolve AGENTS.md".to_string(),
            exit_code: Some(0),
            output: "kept ours whole, by the runbook's rule".to_string(),
        },
        CallRecord {
            label: "run_block 3".to_string(),
            exit_code: Some(0),
            output: "To ../remote.git\n * [new tag] firefox-153.0.4 -> firefox-153.0.4".to_string(),
        },
    ];
    let digest = printed_digest(&records);
    assert!(
        digest.contains("--- conflicted files:\n    AGENTS.md"),
        "{digest}"
    );
    assert!(digest.contains("kept ours whole"), "{digest}");
    assert!(digest.contains("[new tag] firefox-153.0.4"), "{digest}");
    assert!(digest.len() < 3_000, "{}", digest.len());

    let results = vec![
        (
            "Step 1b: merge".to_string(),
            StepResult::Judged(VerdictKind::Pass, "merged".to_string(), digest),
        ),
        ("Step 3: tag".to_string(), StepResult::NotReached),
    ];
    let shown = run_record(&results, false);
    assert_eq!(
        shown,
        "- Step 1b: merge: passed — merged\n- Step 3: tag: not reached"
    );
    let for_writer = run_record(&results, true);
    assert!(for_writer.contains("AGENTS.md"));
    assert!(for_writer.ends_with("- Step 3: tag: not reached"));
}

/// A whole run, live: a two-step runbook in a throwaway repository whose
/// only remote is a local bare repo, worked by a real 14B model and judged by
/// a real one. Asserts the plan, the commands, the verdicts and the effect.
#[test]
#[ignore = "hits the live local Ollama service and loads real models"]
fn a_toy_runbook_runs_end_to_end_against_live_ollama() {
    let dir = std::env::temp_dir().join(format!("runbook-live-{}", Uuid::new_v4()));
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    let sh = |script: &str, cwd: &Path| {
        let execution = block_on(execute_shell(script, cwd, &path));
        assert_eq!(execution.exit_code, 0, "{script}: {}", execution.stderr);
    };
    sh(
        "git init -q --bare ../remote.git && git init -q && git commit -q --allow-empty -m init \
        && git remote add origin ../remote.git && git push -q origin HEAD:refs/heads/main",
        &repo,
    );
    let runbook_path = dir.join("runbook.md");
    std::fs::write(
        &runbook_path,
        "# Toy release\n\n## Hard rules\n\n- Never push anything but the tag.\n\n\
         ## Step 1: choose the version\n\n```bash\nV=0.0.0      # placeholder\necho \"version is $V\"\n```\n\n\
         Run block 1 with V set to 1.4.2. It must print `version is 1.4.2`.\n\n\
         ## Step 2: tag it\n\n```bash\ngit tag \"v$V\"\ngit tag -l 'v*'\n```\n\n\
         The tag list must show `v1.4.2`; if not, STOP.\n",
    )
    .unwrap();

    // Trying a different worker: RUNBOOK_LIVE_WORKER=qwen3.5:9b.
    let worker =
        std::env::var("RUNBOOK_LIVE_WORKER").unwrap_or_else(|_| "qwen2.5-coder:14b".to_string());
    let mut events = Vec::new();
    block_on(run_ollama_runbook(
        &runbook_path.to_string_lossy(),
        &Uuid::new_v4(),
        &repo,
        &path,
        &worker,
        RunbookConfig::default(),
        Vec::new(),
        |event| events.push(event),
    ))
    .unwrap();

    let texts: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            TurnEvent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let transcript = texts.join("\n\n");
    assert!(
        events.iter().any(|event| matches!(
            event,
            TurnEvent::PlanUpdated { update: PlanUpdate::Create(items) } if items.len() == 2
        )),
        "the plan is shown up front"
    );
    let completed: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            TurnEvent::PlanUpdated {
                update: PlanUpdate::Complete(ids),
            } => Some(ids.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(completed, ["step-1", "step-2"], "transcript:\n{transcript}");
    assert!(events.iter().any(|event| matches!(
        event,
        TurnEvent::ToolCallResult {
            outcome: ToolCallOutcome::Success(_),
            ..
        }
    )));
    let tags = block_on(execute_shell("git tag -l", &repo, &path));
    assert_eq!(tags.stdout.trim(), "v1.4.2", "transcript:\n{transcript}");
    assert!(matches!(
        events.last(),
        Some(TurnEvent::TurnCompleted {
            is_error: false,
            ..
        })
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Runs the runbook at `RUNBOOK_RUN_PATH` in `RUNBOOK_RUN_DIR` on live models,
/// appending a timestamped transcript to `RUNBOOK_RUN_LOG` as it goes: the
/// driver the app runs, without the app around it. A tool for trying runbooks
/// and models (`RUNBOOK_RUN_WORKER`, `_JUDGE`, `_HELPER`), not a check: it
/// asserts nothing.
#[test]
#[ignore = "runs a whole runbook on live Ollama models; set RUNBOOK_RUN_PATH to use it"]
fn run_the_runbook_named_in_the_environment() {
    use std::io::Write;

    let Ok(runbook) = std::env::var("RUNBOOK_RUN_PATH") else {
        return;
    };
    let dir = PathBuf::from(std::env::var("RUNBOOK_RUN_DIR").expect("RUNBOOK_RUN_DIR"));
    let log_path =
        std::env::var("RUNBOOK_RUN_LOG").unwrap_or_else(|_| "/tmp/runbook-run.log".to_string());
    let worker =
        std::env::var("RUNBOOK_RUN_WORKER").unwrap_or_else(|_| "qwen2.5-coder:14b".to_string());
    let config = RunbookConfig {
        judge: std::env::var("RUNBOOK_RUN_JUDGE").ok(),
        helper: std::env::var("RUNBOOK_RUN_HELPER").ok(),
    };
    let path = std::env::var("PATH").unwrap_or_default();
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .unwrap();
    let start = std::time::Instant::now();
    let mut write = |line: String| {
        let _ = writeln!(log, "[{:>6}s] {line}", start.elapsed().as_secs());
        let _ = log.flush();
    };
    write(format!(
        "=== {runbook} in {} — worker {worker}, judge {:?}, helper {:?}",
        dir.display(),
        config.judge,
        config.helper
    ));
    let mut thinking = 0usize;
    block_on(run_ollama_runbook(
        &runbook,
        &Uuid::new_v4(),
        &dir,
        &path,
        &worker,
        config,
        Vec::new(),
        |event| match event {
            TurnEvent::Text { text } | TurnEvent::TextDelta { text } => write(text),
            TurnEvent::Thinking { text } => thinking += text.len(),
            TurnEvent::ToolCallStarted { name, input, .. } => write(format!(
                "▶ {name} {}",
                truncate_chars(&input.to_string(), 800)
            )),
            TurnEvent::ToolCallResult { outcome, .. } => write(match outcome {
                ToolCallOutcome::Success(value) => {
                    let text = value
                        .get("result")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string());
                    format!("◀ {}", truncate_chars(&text, 2_000))
                }
                ToolCallOutcome::Error(error) => {
                    format!("◀ ERROR {}", truncate_chars(&error, 2_000))
                }
                ToolCallOutcome::Cancelled => "◀ cancelled".to_string(),
            }),
            TurnEvent::PlanUpdated { update } => write(format!("PLAN {update:?}")),
            TurnEvent::TurnCompleted {
                is_error,
                error_message,
                ..
            } => write(format!(
                "=== finished (error: {is_error}) {}",
                error_message.unwrap_or_default()
            )),
            TurnEvent::SessionInit { .. } => {}
        },
    ))
    .unwrap();
    write(format!(
        "=== {thinking} characters of model thinking not shown"
    ));
}
