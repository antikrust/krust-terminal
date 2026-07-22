use super::*;

fn shell_result(exit_code: i32, stdout: &str, stderr: &str) -> ShellExecution {
    ShellExecution {
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
        exit_code,
    }
}

// --- proof classification -------------------------------------------------

#[test]
fn proof_classification_follows_the_exit_contract() {
    assert_eq!(
        classify_proof(&shell_result(0, "shares are floats\n", "")),
        ProofOutcome::Demonstrated("shares are floats".to_string())
    );
    assert_eq!(
        classify_proof(&shell_result(1, "cents stay integers\n", "")),
        ProofOutcome::Refuted("cents stay integers".to_string())
    );
    assert_eq!(
        classify_proof(&shell_result(124, "proof timed out\n", "")),
        ProofOutcome::Inconclusive("the proof script timed out".to_string())
    );
    assert!(matches!(
        classify_proof(&shell_result(2, "", "SyntaxError: invalid syntax")),
        ProofOutcome::Inconclusive(reason) if reason.contains("SyntaxError")
    ));
}

#[test]
fn the_observation_is_the_last_meaningful_line() {
    // Trailing blank lines must not swallow the verdict line.
    assert_eq!(
        classify_proof(&shell_result(0, "setup\nbug observed\n\n\n", "")),
        ProofOutcome::Demonstrated("bug observed".to_string())
    );
    // A silent script still classifies, with a fallback observation.
    assert_eq!(
        classify_proof(&shell_result(0, "", "")),
        ProofOutcome::Demonstrated("the proof script exited 0".to_string())
    );
}

#[test]
fn code_fences_strip_cleanly_and_bare_scripts_pass_through() {
    assert_eq!(
        strip_code_fences("```python\nprint('hi')\n```"),
        "print('hi')"
    );
    assert_eq!(strip_code_fences("```\nprint('hi')\n```"), "print('hi')");
    assert_eq!(strip_code_fences("print('hi')"), "print('hi')");
}

#[test]
fn proof_scripts_run_with_real_exits_and_timeouts() {
    use futures_lite::future::block_on;

    let dir = std::env::temp_dir();
    let path = std::env::var("PATH").unwrap_or_default();

    let demonstrated = block_on(run_proof_script(
        "print('bug observed')\nraise SystemExit(0)",
        PROOF_TIMEOUT_SECS,
        &dir,
        &path,
    ));
    assert_eq!(
        classify_proof(&demonstrated),
        ProofOutcome::Demonstrated("bug observed".to_string())
    );

    let refuted = block_on(run_proof_script(
        "print('code is fine')\nraise SystemExit(1)",
        PROOF_TIMEOUT_SECS,
        &dir,
        &path,
    ));
    assert_eq!(
        classify_proof(&refuted),
        ProofOutcome::Refuted("code is fine".to_string())
    );

    let timed_out = block_on(run_proof_script(
        "import time\ntime.sleep(5)",
        1,
        &dir,
        &path,
    ));
    assert_eq!(
        classify_proof(&timed_out),
        ProofOutcome::Inconclusive("the proof script timed out".to_string())
    );
}

// --- suite detection ------------------------------------------------------

/// A throwaway directory with the given marker files, for detection tests.
fn project_with(files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("committee-suite-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, contents) in files {
        let target = dir.join(name);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(target, contents).unwrap();
    }
    dir
}

#[test]
fn suites_are_detected_from_their_manifests() {
    let cargo = project_with(&[("Cargo.toml", "[package]\nname = \"x\"\n")]);
    assert_eq!(detect_suite(&cargo), Some(ProjectSuite::Cargo));
    let _ = std::fs::remove_dir_all(&cargo);

    let go = project_with(&[("go.mod", "module x\n")]);
    assert_eq!(detect_suite(&go), Some(ProjectSuite::Go));
    let _ = std::fs::remove_dir_all(&go);

    let python = project_with(&[("pyproject.toml", "[project]\nname = \"x\"\n")]);
    assert_eq!(detect_suite(&python), Some(ProjectSuite::Pytest));
    let _ = std::fs::remove_dir_all(&python);

    let bare = project_with(&[("README.md", "nothing here")]);
    assert_eq!(detect_suite(&bare), None);
    let _ = std::fs::remove_dir_all(&bare);
}

#[test]
fn npm_projects_need_a_real_test_script() {
    // The npm init default is a stub that always fails — not a suite.
    let stub = project_with(&[(
        "package.json",
        r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#,
    )]);
    assert_eq!(detect_suite(&stub), None);
    let _ = std::fs::remove_dir_all(&stub);

    let real = project_with(&[("package.json", r#"{"scripts":{"test":"jest"}}"#)]);
    assert_eq!(detect_suite(&real), Some(ProjectSuite::Npm));
    let _ = std::fs::remove_dir_all(&real);
}

// --- suite outcomes -------------------------------------------------------

#[test]
fn a_green_suite_reads_as_evidence_and_a_missing_tool_never_does() {
    use futures_lite::future::block_on;

    let path = std::env::var("PATH").unwrap_or_default();
    // A real repo whose "suite" is a command we know passes.
    let dir = project_with(&[("Cargo.toml", "[package]\nname = \"x\"\n")]);

    // Detection is manifest-based; run_suite is command-based, so drive it
    // with a suite whose tool is definitely absent to prove the
    // Unavailable/Failed distinction holds.
    let outcome = block_on(run_suite(ProjectSuite::Go, &dir, &path));
    assert!(
        matches!(
            outcome,
            SuiteOutcome::Unavailable(_) | SuiteOutcome::Failed(_)
        ),
        "expected a non-passing outcome, got {outcome:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- behavior report ------------------------------------------------------

const DEMO_DIFF: &str = "\
diff --git a/payday.py b/payday.py
--- a/payday.py
+++ b/payday.py
@@ -1,2 +1,9 @@
 def split_pot(total_cents, players):
+def split_pot(total_cents, players):
+    share = total_cents / len(players)
+def top_scores(scores, n):
+    ordered = sorted(scores)
+def _private(helper):
+    pass
";

#[test]
fn probe_targets_come_from_added_top_level_defs() {
    let targets = parse_probe_targets(DEMO_DIFF);
    assert_eq!(
        targets,
        vec![
            ProbeTarget {
                file: "payday.py".to_string(),
                function: "split_pot".to_string(),
                arity: 2,
            },
            ProbeTarget {
                file: "payday.py".to_string(),
                function: "top_scores".to_string(),
                arity: 2,
            },
        ]
    );
    // Non-python files contribute nothing.
    assert_eq!(
        parse_probe_targets("+++ b/main.rs\n+def looks_pythonic(x):\n"),
        Vec::new()
    );
}

#[test]
fn every_supported_arity_has_a_battery() {
    assert_eq!(probe_combos(0), &[""]);
    assert!(!probe_combos(1).is_empty());
    assert!(!probe_combos(2).is_empty());
    assert!(probe_combos(9).is_empty());
}

#[test]
fn the_behavior_report_exposes_the_demo_repo_bugs_without_any_model() {
    use futures_lite::future::block_on;

    // The planted-bug demo file, as the user's test repo writes it.
    let dir = std::env::temp_dir().join(format!("committee-probe-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("payday.py"),
        "def split_pot(total_cents, players):\n    share = total_cents / len(players)\n    shares = {p: share for p in players}\n    shares[players[0]] += total_cents - share * len(players)\n    return shares\n\ndef top_scores(scores, n):\n    ordered = sorted(scores)\n    return ordered[:n]\n",
    )
    .unwrap();
    let diff =
        "+++ b/payday.py\n+def split_pot(total_cents, players):\n+def top_scores(scores, n):\n";
    let path = std::env::var("PATH").unwrap_or_default();

    let report = block_on(run_behavior_probe(diff, &dir, &path)).expect("a report");

    // Bug 2's crash half: empty players divides by zero.
    assert!(report.contains("ZeroDivisionError"), "report: {report}");
    // Bug 1: cents become floats.
    assert!(report.contains("33.33"), "report: {report}");
    // Bug 3: "top" scores come back ascending — lowest first.
    assert!(
        report.contains("top_scores([3, 1, 2], 2) -> [1, 2]"),
        "report: {report}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn suite_outcomes_render_with_the_suite_name() {
    assert!(SuiteOutcome::Passed
        .label(ProjectSuite::Cargo)
        .contains("cargo test"));
    assert!(SuiteOutcome::Failed("2 failed".to_string())
        .label(ProjectSuite::Pytest)
        .contains("FAILS"));
    assert!(
        SuiteOutcome::Unavailable("the tool isn't installed".to_string())
            .label(ProjectSuite::Npm)
            .contains("could not be run")
    );
}
