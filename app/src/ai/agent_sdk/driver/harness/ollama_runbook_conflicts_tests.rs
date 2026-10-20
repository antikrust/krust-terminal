use std::path::Path;

use ai::agent::{PlanUpdate, TurnEvent};
use futures_lite::future::block_on;

use super::*;
use crate::ai::agent_sdk::driver::harness::ollama_runbook::{
    parse_runbook, run_ollama_runbook, RunbookConfig,
};
use crate::ai::agent_sdk::driver::harness::ollama_stream_exec::execute_shell;

const PLAIN: &str =
    "keep 1\n<<<<<<< HEAD\nours a\nours b\n=======\ntheirs a\n>>>>>>> upstream\nkeep 2\n";

const DIFF3: &str = "top\n<<<<<<< HEAD\n    guard();\n    int speed = 3;\n||||||| base\n    int speed = 3;\n=======\n    int speed = get_speed();\n>>>>>>> upstream\nbottom\n<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> upstream\n";

#[test]
fn finds_plain_and_diff3_conflicts_with_their_lines() {
    let plain = find_conflicts(PLAIN);
    assert_eq!(plain.len(), 1);
    assert_eq!((plain[0].start_line, plain[0].end_line), (2, 7));
    assert_eq!(plain[0].ours, ["ours a", "ours b"]);
    assert_eq!(plain[0].base, None);
    assert_eq!(plain[0].theirs, ["theirs a"]);

    let diff3 = find_conflicts(DIFF3);
    assert_eq!(diff3.len(), 2);
    assert_eq!(diff3[0].ours, ["    guard();", "    int speed = 3;"]);
    assert_eq!(
        diff3[0].base.as_deref(),
        Some(&["    int speed = 3;".to_string()][..])
    );
    assert_eq!(diff3[0].theirs, ["    int speed = get_speed();"]);
    assert_eq!((diff3[1].start_line, diff3[1].end_line), (11, 15));
}

#[test]
fn a_conflict_that_never_closes_is_skipped_not_guessed() {
    let text = "<<<<<<< HEAD\nours\n=======\ntheirs\n<<<<<<< HEAD\na\n=======\nb\n>>>>>>> x\n";
    let conflicts = find_conflicts(text);
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].ours, ["a"]);
}

#[test]
fn splicing_replaces_one_conflict_and_keeps_line_endings() {
    let conflicts = find_conflicts(DIFF3);
    let spliced = splice(
        DIFF3,
        &conflicts[0],
        "    guard();\n    int speed = get_speed();",
    );
    assert!(spliced.starts_with("top\n    guard();\n    int speed = get_speed();\nbottom\n"));
    assert_eq!(
        find_conflicts(&spliced).len(),
        1,
        "the other conflict is untouched"
    );

    let crlf = PLAIN.replace('\n', "\r\n");
    let conflicts = find_conflicts(&crlf);
    assert_eq!(
        splice(&crlf, &conflicts[0], "merged"),
        "keep 1\r\nmerged\r\nkeep 2\r\n"
    );
}

#[test]
fn markers_in_a_fix_are_caught_but_a_lone_rule_line_does_not_hold_a_file() {
    assert!(has_conflict_markers("a\n=======\nb"));
    assert!(has_conflict_markers(">>>>>>> upstream"));
    assert!(!has_conflict_markers("a\n========\n<<<<<<<< not a marker"));
    assert!(has_open_conflicts(PLAIN));
    assert!(has_open_conflicts("fine\n<<<<<<< HEAD\nstray"));
    assert!(!has_open_conflicts("Title\n=======\ntext\n"));
}

#[test]
fn a_conflict_keeps_its_key_when_its_lines_move() {
    let first = &find_conflicts(PLAIN)[0];
    let moved = format!("new line\n{PLAIN}");
    let second = &find_conflicts(&moved)[0];
    assert_ne!(first.start_line, second.start_line);
    assert_eq!(conflict_key("a.c", first), conflict_key("a.c", second));
    assert_ne!(conflict_key("a.c", first), conflict_key("b.c", first));
    assert_eq!(conflict_key("a.c", first).len(), 16);
}

const TABLE: &str = "\
| Files | Our change | On conflict |
|-------|-----------|-------------|
| `browser/branding/bork/**`, `.github/**` | bork branding | cannot conflict; if they appear, abort and STOP |
| `browser/config/version.txt`, `config/milestone.txt` | the unreleased bump | take Mozilla's (the next block) |
| `intl/l10n/Localization.cpp` | Firefox→Bork substitution | keep our substitution, take Mozilla's surrounding code |
| `dom/events/WheelHandlingHelper.{cpp,h}` | wheel guards | re-apply onto Mozilla's version |
| any file touched by a patch in `.github/patches/upstream/` | that patch, applied in-tree | keep the patch's change, take Mozilla's surrounding code |
| `README.md`, `agents/**` | ours | keep ours |
";

#[test]
fn reads_the_conflict_table_into_rules() {
    let rules = table_rules(TABLE);
    assert_eq!(rules.len(), 6);
    assert_eq!(rules[0].action, RuleAction::Stop);
    assert_eq!(rules[1].action, RuleAction::TakeTheirs);
    assert_eq!(
        rules[2].action,
        RuleAction::Merge,
        "\"take Mozilla's surrounding code\" is a merge"
    );
    assert_eq!(
        rules[3].patterns,
        [
            "dom/events/WheelHandlingHelper.cpp",
            "dom/events/WheelHandlingHelper.h"
        ]
    );
    assert!(rules[4].by_patch);
    assert_eq!(rules[5].action, RuleAction::KeepOurs);
}

#[test]
fn finds_the_rule_for_a_file() {
    let rules = table_rules(TABLE);
    let mut patched = BTreeMap::new();
    patched.insert(
        "toolkit/xre/nsAppRunner.cpp".to_string(),
        "diff".to_string(),
    );
    let action = |file: &str| rule_for(&rules, file, &patched).map(|rule| rule.action);
    assert_eq!(
        action("browser/branding/bork/icon.png"),
        Some(RuleAction::Stop)
    );
    assert_eq!(
        action("dom/events/WheelHandlingHelper.h"),
        Some(RuleAction::Merge)
    );
    assert_eq!(action("agents/notes.md"), Some(RuleAction::KeepOurs));
    assert_eq!(
        action("toolkit/xre/nsAppRunner.cpp"),
        Some(RuleAction::Merge)
    );
    assert_eq!(action("layout/base/nsLayoutUtils.cpp"), None);
}

#[test]
fn splits_a_patch_into_its_files() {
    let patch = "From abc\nSubject: x\n---\ndiff --git a/a/one.c b/a/one.c\n--- a/a/one.c\n+++ b/a/one.c\n@@ -1 +1 @@\n-x\n+y\ndiff --git a/two.h b/two.h\n@@ -1 +1 @@\n-p\n+q\n";
    let sections = split_patch(patch);
    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0].0, "a/one.c");
    assert!(sections[0].1.contains("+y") && !sections[0].1.contains("+q"));
    assert_eq!(sections[1].0, "two.h");

    let dir = std::env::temp_dir().join(format!("runbook-patches-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("0001-fix.patch"), patch).unwrap();
    std::fs::write(dir.join("notes.txt"), "not a patch").unwrap();
    let by_file = patch_sections(&dir);
    assert_eq!(by_file.len(), 2);
    assert!(by_file["two.h"].starts_with("from 0001-fix.patch:"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shows_only_the_reference_hunks_that_touch_the_conflict() {
    let conflict = &find_conflicts(DIFF3)[0];
    let diff = "diff --git a/f b/f\n@@ -10,3 +10,4 @@\n     int speed = get_speed();\n+    reference_guard();\n     return 1;\n@@ -90,2 +91,2 @@\n-unrelated line of code here\n+something else entirely\n";
    let related = related_hunks(diff, conflict, 80);
    assert!(related.contains("reference_guard"));
    assert!(!related.contains("unrelated"));
    assert_eq!(
        related_hunks(diff, &find_conflicts(PLAIN)[0], 80),
        "",
        "short lines anchor nothing"
    );
}

#[test]
fn reads_the_analysts_note() {
    let reply = "OURS: adds a guard\nTHEIRS: speed comes from get_speed()\nCONFIDENCE: High\nWHY: both survive.\nRESOLUTION:\n```cpp\n    guard();\n    int speed = get_speed();\n```\n";
    let analysis = parse_analysis(reply).unwrap();
    assert_eq!(analysis.ours, "adds a guard");
    assert_eq!(analysis.confidence, Confidence::High);
    assert_eq!(
        analysis.resolution.as_deref(),
        Some("    guard();\n    int speed = get_speed();")
    );

    let bold = "**OURS:** a\n**CONFIDENCE:** medium\nRESOLUTION:\n```\nx\n```";
    assert_eq!(parse_analysis(bold).unwrap().confidence, Confidence::Medium);

    let handed_over = parse_analysis("HUMAN: the API was removed upstream").unwrap();
    assert_eq!(handed_over.resolution, None);
    assert_eq!(handed_over.why, "the API was removed upstream");

    assert_eq!(parse_analysis("OURS: a\nCONFIDENCE: high\n(no code)"), None);
}

#[test]
fn reads_the_helpers_check() {
    assert_eq!(parse_check("APPLY"), Check::Apply);
    assert_eq!(parse_check("**APPLY** — looks right"), Check::Apply);
    assert_eq!(
        parse_check("EDIT\n```\nfixed\n```"),
        Check::Edit("fixed".to_string())
    );
    assert_eq!(
        parse_check("HUMAN: drops the guard"),
        Check::Human("drops the guard".to_string())
    );
    assert!(matches!(parse_check("I think it is fine"), Check::Human(_)));
    assert!(matches!(parse_check("EDIT but no lines"), Check::Human(_)));
}

fn entry(file: &str, conflict: usize, status: EntryStatus) -> WorkEntry {
    WorkEntry {
        key: format!("{file}-{conflict}"),
        file: file.to_string(),
        conflict,
        of: 2,
        lines: (10 * conflict, 10 * conflict + 6),
        ours: "adds a guard".to_string(),
        theirs: "renames speed".to_string(),
        confidence: Some(Confidence::High),
        why: "both survive".to_string(),
        resolution: Some("merged()".to_string()),
        status,
        note: "high confidence".to_string(),
    }
}

#[test]
fn the_notes_survive_a_restart_and_end_with_progress() {
    let dir = std::env::temp_dir().join(format!("runbook-work-{}", Uuid::new_v4()));
    let mut state = WorkState::default();
    state.upsert(entry("a/b.cpp", 1, EntryStatus::Applied));
    state.upsert(entry("a/b.cpp", 2, EntryStatus::LeftForYou));
    state.upsert(entry("a/b.cpp", 1, EntryStatus::Applied));
    assert_eq!(state.entries.len(), 2, "upsert replaces by key");
    state.save(&dir).unwrap();
    let loaded = WorkState::load(&dir);
    assert_eq!(loaded, state);
    assert!(loaded.find("a/b.cpp-2").is_some());

    let entries: Vec<&WorkEntry> = loaded.entries.iter().collect();
    let notes = render_notes("a/b.cpp", &entries, "02:41");
    assert!(notes.contains("## Conflict 1 of 2 — lines 10–16"));
    assert!(notes.contains("- Status: left for you — high confidence"));
    assert!(notes
        .trim_end()
        .ends_with("Progress: 2 of 2 conflicts worked · 1 applied · 1 for you · updated 02:41"));
    assert_eq!(notes_file_name("a/b.cpp"), "a__b.cpp.md");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_long_pass_summary_keeps_its_totals_and_caps_its_lists() {
    let tally = PassTally {
        applied: 40,
        left: (0..60)
            .map(|n| format!("big/file{n}.cpp lines 1–9: low confidence"))
            .collect(),
        still_conflicted: (0..60).map(|n| format!("big/file{n}.cpp")).collect(),
        ..PassTally::default()
    };
    let summary = pass_summary(&tally, Path::new("/tmp/notes"));
    assert!(summary.starts_with("Work-file pass: 40 conflict fix(es) applied, 60 left for you."));
    assert!(summary.contains("… and 40 more"), "{summary}");
    assert!(summary.contains("… and 35 more"), "{summary}");
    assert!(summary.len() < 6_000, "{} chars", summary.len());
    let (record, reply) = record("resolve".to_string(), 1, summary);
    assert!(reply.starts_with("exit 1\n"));
    assert_eq!(record.exit_code, Some(1));
}

#[test]
fn reads_a_resolve_blocks_settings() {
    assert_eq!(
        parse_pass_settings("reference: refs/floorp/nora-0.2.0\nwork: ../notes\nnonsense"),
        PassSettings {
            reference: Some("refs/floorp/nora-0.2.0".to_string()),
            work: Some("../notes".to_string()),
        }
    );
    assert_eq!(parse_pass_settings(""), PassSettings::default());
}

/// A repo whose merge conflicts in four files: one the table says to keep,
/// one to take whole, one it says must never conflict, and one too big to
/// merge by model. None of them needs a model.
fn conflicted_repo(dir: &Path, path: &str) {
    let sh = |script: &str| {
        let execution = block_on(execute_shell(script, dir, path));
        assert_eq!(execution.exit_code, 0, "{script}: {}", execution.stderr);
    };
    let big_ours: String = (0..250).map(|i| format!("ours {i}\n")).collect();
    let big_theirs: String = (0..250).map(|i| format!("theirs {i}\n")).collect();
    sh("git init -q -b main && git config user.email t@t && git config user.name t \
        && mkdir -p agents branding && printf 'base\\n' > agents/notes.md && printf 'base\\n' > version.txt \
        && printf 'base\\n' > branding/logo.txt && printf 'base\\n' > big.txt && git add -A && git commit -qm base \
        && git checkout -qb upstream && printf 'theirs\\n' > agents/notes.md && printf '2.0\\n' > version.txt \
        && printf 'theirs\\n' > branding/logo.txt");
    std::fs::write(dir.join("big.txt"), &big_theirs).unwrap();
    sh(
        "git commit -qam upstream && git checkout -q main && printf 'ours\\n' > agents/notes.md \
        && printf '1.0.1\\n' > version.txt && printf 'ours\\n' > branding/logo.txt",
    );
    std::fs::write(dir.join("big.txt"), &big_ours).unwrap();
    let merge = block_on(execute_shell(
        "git commit -qam ours && git merge upstream",
        dir,
        path,
    ));
    assert_ne!(merge.exit_code, 0, "the merge must conflict");
}

#[test]
fn the_pass_applies_the_tables_rules_and_leaves_what_it_must() {
    let root = std::env::temp_dir().join(format!("runbook-pass-{}", Uuid::new_v4()));
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    conflicted_repo(&repo, &path);

    let runbook = parse_runbook(
        "## Step 1: merge\n\n\
         | Files | Our change | On conflict |\n|---|---|---|\n\
         | `agents/**` | ours | keep ours |\n\
         | `version.txt` | the bump | take Mozilla's |\n\
         | `branding/**` | logo | cannot conflict; if they appear, STOP |\n\
         | `big.txt` | everything | merge them |\n\n\
         ```resolve\n```\n",
    );
    let work_dir = root.join("notes");
    let context = RunContext {
        working_dir: &repo,
        path: &path,
        worker: "unused",
        judge: "unused",
        judge_thinks: false,
        helper: "unused",
        helper_thinks: false,
        work_dir: &work_dir,
        cancel: None,
    };
    let mut events = Vec::new();
    let (record, reply) = block_on(run_conflict_pass(
        &context,
        &runbook.steps[0],
        "",
        &mut |event| events.push(event),
    ))
    .unwrap();

    assert_eq!(record.exit_code, Some(1), "two files are still conflicted");
    assert!(
        reply.contains("Resolved and staged: agents/notes.md, version.txt"),
        "{reply}"
    );
    assert!(
        reply.contains("branding/logo.txt: the runbook says to stop"),
        "{reply}"
    );
    assert!(reply.contains("too big to merge by model"), "{reply}");
    assert_eq!(
        std::fs::read_to_string(repo.join("agents/notes.md")).unwrap(),
        "ours\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("version.txt")).unwrap(),
        "2.0\n"
    );
    let still = block_on(execute_shell(
        "git diff --name-only --diff-filter=U",
        &repo,
        &path,
    ));
    assert_eq!(still.stdout.trim(), "big.txt\nbranding/logo.txt");

    let notes = std::fs::read_to_string(work_dir.join("big.txt.md")).unwrap();
    assert!(notes.contains("## Conflict 1 of 1"), "{notes}");
    assert!(notes.contains("left for you"), "{notes}");
    assert!(work_dir.join("state.json").exists());
    assert!(std::fs::read_to_string(work_dir.join("progress.md"))
        .unwrap()
        .contains("Progress:"));
    assert!(events.iter().any(
        |event| matches!(event, TurnEvent::ToolCallStarted { name, .. } if name == "resolve")
    ));
    let _ = std::fs::remove_dir_all(&root);
}

/// End to end on real models: a C function both sides changed next to each
/// other, merged by the analyst, checked by the helper, committed by the
/// worker. Run with `--ignored` when Ollama has the models.
#[test]
#[ignore = "hits the live local Ollama service and loads real models"]
fn a_real_conflict_is_merged_one_piece_at_a_time_against_live_ollama() {
    let root = std::env::temp_dir().join(format!("runbook-live-pass-{}", Uuid::new_v4()));
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    let sh = |script: &str| {
        let execution = block_on(execute_shell(script, &repo, &path));
        assert_eq!(execution.exit_code, 0, "{script}: {}", execution.stderr);
    };
    let base = "int scroll(int delta) {\n    int speed = 3;\n    return delta * speed;\n}\n";
    let ours = "int scroll(int delta) {\n    if (delta == 0) return 0;  // ours: ignore empty wheel events\n    int speed = 3;\n    return delta * speed;\n}\n";
    let theirs = "int scroll(int delta) {\n    int speed = get_scroll_speed();\n    return delta * speed;\n}\n";
    sh(
        "git init -q --bare ../remote.git && git init -q -b main && git config user.email t@t \
        && git config user.name t && git remote add origin ../remote.git && mkdir src",
    );
    std::fs::write(repo.join("src/scroll.c"), base).unwrap();
    sh("git add -A && git commit -qm base && git checkout -qb upstream");
    std::fs::write(repo.join("src/scroll.c"), theirs).unwrap();
    sh("git commit -qam upstream && git checkout -q main");
    std::fs::write(repo.join("src/scroll.c"), ours).unwrap();
    sh("git commit -qam ours");

    let runbook_path = root.join("merge.md");
    std::fs::write(
        &runbook_path,
        "# Toy merge\n\n## Step 1: merge upstream\n\n```bash\ngit merge --no-edit upstream; echo \"exit=$?\"\ngit diff --name-only --diff-filter=U\n```\n\n\
         | Files | Our change | On conflict |\n|---|---|---|\n\
         | `src/scroll.c` | the empty-wheel guard | re-apply our guard onto upstream's version |\n\n\
         If anything is conflicted, work through it:\n\n```resolve\n```\n\n\
         When nothing is left conflicted, conclude the merge:\n\n```bash\ngit commit --no-edit\ngit status --short | wc -l   # must print 0\n```\n",
    )
    .unwrap();

    let mut events = Vec::new();
    block_on(run_ollama_runbook(
        &runbook_path.to_string_lossy(),
        &Uuid::new_v4(),
        &repo,
        &path,
        "qwen2.5-coder:14b",
        RunbookConfig::default(),
        Vec::new(),
        |event| events.push(event),
    ))
    .unwrap();

    let transcript: String = events
        .iter()
        .filter_map(|event| match event {
            TurnEvent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let merged = std::fs::read_to_string(repo.join("src/scroll.c")).unwrap();
    assert!(!has_conflict_markers(&merged), "{merged}\n\n{transcript}");
    assert!(
        merged.contains("if (delta == 0)"),
        "our guard survives:\n{merged}\n\n{transcript}"
    );
    assert!(
        merged.contains("get_scroll_speed()"),
        "their change survives:\n{merged}\n\n{transcript}"
    );
    let head = block_on(execute_shell(
        "git rev-list --parents -n 1 HEAD",
        &repo,
        &path,
    ));
    assert_eq!(
        head.stdout.split_whitespace().count(),
        3,
        "the merge was committed:\n{transcript}"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        TurnEvent::PlanUpdated { update: PlanUpdate::Complete(ids) } if ids.iter().any(|id| id == "step-1")
    )));
    assert!(root.join("merge.work").join("src__scroll.c.md").exists());
    let _ = std::fs::remove_dir_all(&root);
}
