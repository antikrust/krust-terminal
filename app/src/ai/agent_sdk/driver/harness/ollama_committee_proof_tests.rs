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

#[test]
fn a_crashing_proof_script_is_never_a_refutation() {
    use futures_lite::future::block_on;

    // The exact shape that misfired: qwen wrote `import payday`, the script
    // was staged in the temp dir so the import failed, Python exited 1, and
    // exit 1 means "refuted" — a real defect dismissed by a broken script.
    let dir = std::env::temp_dir().join(format!("committee-crash-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = std::env::var("PATH").unwrap_or_default();

    let missing_import = block_on(run_proof_script(
        "import nonexistent_package_xyz\nexit(0)",
        PROOF_TIMEOUT_SECS,
        &dir,
        &path,
    ));
    assert_eq!(missing_import.exit_code, 125, "crash must not exit 1");
    assert!(
        matches!(
            classify_proof(&missing_import),
            ProofOutcome::Inconclusive(_)
        ),
        "a failed import must never refute a finding"
    );

    // A script that forgets to decide is also not a verdict.
    let no_verdict = block_on(run_proof_script(
        "print('I looked at it')",
        PROOF_TIMEOUT_SECS,
        &dir,
        &path,
    ));
    assert!(matches!(
        classify_proof(&no_verdict),
        ProofOutcome::Inconclusive(_)
    ));

    // A deliberate exit(1) still refutes — the contract survives the fix.
    let refuted = block_on(run_proof_script(
        "print('behaves correctly')\nexit(1)",
        PROOF_TIMEOUT_SECS,
        &dir,
        &path,
    ));
    assert_eq!(
        classify_proof(&refuted),
        ProofOutcome::Refuted("behaves correctly".to_string())
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_proof_script_can_import_the_repository_module_by_name() {
    use futures_lite::future::block_on;

    // The prover's natural instinct — `import payday` — must work, since
    // the script is staged in the temp dir but runs against the repo.
    let dir = std::env::temp_dir().join(format!("committee-import-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("payday.py"),
        "def split_pot(total_cents, players):\n    return total_cents / len(players)\n",
    )
    .unwrap();
    let path = std::env::var("PATH").unwrap_or_default();

    let execution = block_on(run_proof_script(
        "import payday\nshare = payday.split_pot(101, ['a', 'b'])\nprint(f'share={share!r}')\nexit(0) if isinstance(share, float) else exit(1)",
        PROOF_TIMEOUT_SECS,
        &dir,
        &path,
    ));
    assert_eq!(
        classify_proof(&execution),
        ProofOutcome::Demonstrated("share=50.5".to_string()),
        "stderr: {}",
        execution.stderr
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

#[test]
fn a_prover_that_broke_the_contract_gets_another_go_but_a_timeout_does_not() {
    // The shapes worth retrying are the ones the model can fix if told: it
    // raised (assert is the usual culprit), it forgot to decide, or it wrote
    // something python couldn't parse.
    assert!(proof_script_is_fixable(CRASHED_EXIT_CODE));
    assert!(proof_script_is_fixable(NO_VERDICT_EXIT_CODE));
    assert!(proof_script_is_fixable(2), "a syntax error is fixable");

    // A verdict is not a failure, and a timeout will just time out again.
    assert!(!proof_script_is_fixable(0));
    assert!(!proof_script_is_fixable(1));
    assert!(!proof_script_is_fixable(TIMEOUT_EXIT_CODE));
}

#[test]
fn an_assertion_failure_is_a_crash_not_a_refutation() {
    use futures_lite::future::block_on;

    // The exact shape from the user's run: the prover asserted the correct
    // behaviour instead of deciding. The assertion fails, python exits 1 —
    // and exit 1 means "the code is fine", the opposite of what happened.
    let dir = std::env::temp_dir();
    let path = std::env::var("PATH").unwrap_or_default();
    let execution = block_on(run_proof_script(
        "share = 101 / 2\nassert isinstance(share, int), \"share should be an integer\"\nexit(1)",
        PROOF_TIMEOUT_SECS,
        &dir,
        &path,
    ));
    assert_eq!(execution.exit_code, CRASHED_EXIT_CODE);
    assert!(matches!(
        classify_proof(&execution),
        ProofOutcome::Inconclusive(_)
    ));
    assert!(
        proof_script_is_fixable(execution.exit_code),
        "and the prover is asked to try again"
    );
}

// --- generated regression tests -------------------------------------------

#[test]
fn a_failing_generated_test_is_the_point_of_a_generated_test() {
    use futures_lite::future::block_on;

    // A test file written against the planted bug: it asserts the CORRECT
    // behaviour, so it fails while the bug is there. That failure is the
    // deliverable, not an error.
    let dir = std::env::temp_dir().join(format!("committee-tests-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("payday.py"),
        "def top_scores(scores, n):\n    return sorted(scores)[:n]\n",
    )
    .unwrap();
    let path = std::env::var("PATH").unwrap_or_default();

    let run = block_on(run_python_tests(
        "import payday\n\
         \n\
         def test_top_scores_returns_the_highest():\n\
         \x20   assert payday.top_scores([3, 1, 2], 2) == [3, 2], 'top scores must be the highest'\n\
         \n\
         def test_top_scores_handles_an_empty_list():\n\
         \x20   assert payday.top_scores([], 2) == []\n",
        PROOF_TIMEOUT_SECS,
        &dir,
        &path,
    ));
    assert_eq!(run.broken, None, "the file ran");
    assert_eq!(run.total(), 2);
    assert_eq!(run.passed, vec!["test_top_scores_handles_an_empty_list"]);
    assert_eq!(run.failed.len(), 1);
    assert_eq!(run.failed[0].0, "test_top_scores_returns_the_highest");
    assert!(run.failed[0].1.contains("must be the highest"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_test_file_that_cannot_run_says_so_instead_of_reporting_zero_failures() {
    use futures_lite::future::block_on;

    let dir = std::env::temp_dir();
    let path = std::env::var("PATH").unwrap_or_default();

    // A bad import must never read as "nothing failed" — that would be a
    // green light drawn from a file that never ran.
    let broken = block_on(run_python_tests(
        "import nonexistent_package_xyz\n\ndef test_x():\n    assert True\n",
        PROOF_TIMEOUT_SECS,
        &dir,
        &path,
    ));
    assert_eq!(broken.total(), 0);
    assert!(broken.broken.is_some());

    // A file with no tests in it is also not a pass.
    let empty = block_on(run_python_tests(
        "value = 1 + 1\n",
        PROOF_TIMEOUT_SECS,
        &dir,
        &path,
    ));
    assert!(empty
        .broken
        .as_deref()
        .is_some_and(|reason| reason.contains("no test_")));
}

#[test]
fn a_proof_that_compared_nothing_is_not_a_proof() {
    // Verbatim from a real run: the comparison lived in a comment, and the
    // interpreter does not read comments. It exited 0 having looked at the
    // empty list, which is not the defect, and the committee filed it under
    // Definite.
    assert_eq!(self_check("Observed: None\n", 0), SelfCheck::Unstated);

    // A stray word beginning with the label is not the label.
    assert_eq!(
        self_check("expectedly slow\nactually fine\n", 0),
        SelfCheck::Unstated
    );

    // Models capitalise as the mood takes them, and print things first.
    assert_eq!(
        self_check("Loaded stock.py\nEXPECTED: 0\nActual:  5\n", 0),
        SelfCheck::Compared {
            expected: "0".to_string(),
            actual: "5".to_string(),
            differ: true,
            agreed: true,
        }
    );
}

#[test]
fn the_values_a_proof_printed_outrank_the_exit_code_it_chose() {
    // Verbatim from a real run. It ran the code, wrote down what the
    // docstring promises, printed both — and then exited "the code is
    // fine". True is not False, and seeing that takes no opinion.
    let inverted = self_check("expected: True\nactual: False\n", 1);
    assert_eq!(
        inverted,
        SelfCheck::Compared {
            expected: "True".to_string(),
            actual: "False".to_string(),
            differ: true,
            agreed: false,
        }
    );

    // Also real: the prover wrote the buggy value down as the expected one
    // and exited 0 on the strength of it. Two identical values cannot
    // demonstrate a defect however the script feels about it.
    assert_eq!(
        self_check("expected: False\nactual: False\n", 0),
        SelfCheck::Compared {
            expected: "False".to_string(),
            actual: "False".to_string(),
            differ: false,
            agreed: false,
        }
    );

    // The contract, both ways round, with the exit code agreeing.
    assert!(matches!(
        self_check("expected: 0\nactual: 5\n", 0),
        SelfCheck::Compared {
            differ: true,
            agreed: true,
            ..
        }
    ));
    assert!(matches!(
        self_check("expected: 90\nactual: 90\n", 1),
        SelfCheck::Compared {
            differ: false,
            agreed: true,
            ..
        }
    ));
}

/// The stock fixture's diff, as `git diff HEAD` really prints it — hunk
/// header, context lines and all. Two of its three defects live under
/// signatures that never changed, which is the whole point of it being here.
const STOCK_DIFF: &str = r#"diff --git a/stock.py b/stock.py
index 2273b46..2d34e66 100644
--- a/stock.py
+++ b/stock.py
@@ -8,19 +8,17 @@ def cheapest(prices):
     """The lowest price in the list, or None when the list is empty."""
     best = None
     for price in prices:
-        if best is None or price < best:
+        if not best or price < best:
             best = price
     return best
 
 
 def needs_reorder(stock, threshold):
     """True once stock has fallen to the reorder threshold."""
-    return stock <= threshold
+    return stock < threshold
 
 
-def restock(item, quantity, log=None):
+def restock(item, quantity, log=[]):
     """Records one delivery and returns the log for this call."""
-    if log is None:
-        log = []
     log.append((item, quantity))
     return log
"#;

#[test]
fn a_function_whose_body_changed_is_probed_even_though_its_signature_did_not() {
    let targets = parse_probe_targets(STOCK_DIFF);
    assert_eq!(
        targets,
        vec![
            // Named only by the hunk header.
            ProbeTarget {
                file: "stock.py".to_string(),
                function: "cheapest".to_string(),
                arity: 1,
            },
            // Its `def` arrives as an ordinary context line.
            ProbeTarget {
                file: "stock.py".to_string(),
                function: "needs_reorder".to_string(),
                arity: 2,
            },
            // Signature changed, and `log=[]` doesn't count towards what a
            // caller has to pass — probing it at 3 would supply the log and
            // hide the defect in the default.
            ProbeTarget {
                file: "stock.py".to_string(),
                function: "restock".to_string(),
                arity: 2,
            },
        ]
    );
}

#[test]
fn the_behavior_report_exposes_the_stock_defects_without_any_model() {
    use futures_lite::future::block_on;

    let dir = std::env::temp_dir().join(format!("committee-stock-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("stock.py"),
        "def cheapest(prices):\n\
         \x20   \"\"\"The lowest price in the list, or None when the list is empty.\"\"\"\n\
         \x20   best = None\n\
         \x20   for price in prices:\n\
         \x20       if not best or price < best:\n\
         \x20           best = price\n\
         \x20   return best\n\n\
         def needs_reorder(stock, threshold):\n\
         \x20   \"\"\"True once stock has fallen to the reorder threshold.\"\"\"\n\
         \x20   return stock < threshold\n\n\
         def restock(item, quantity, log=[]):\n\
         \x20   \"\"\"Records one delivery and returns the log for this call.\"\"\"\n\
         \x20   log.append((item, quantity))\n\
         \x20   return log\n",
    )
    .unwrap();
    let path = std::env::var("PATH").unwrap_or_default();

    let report = block_on(run_behavior_probe(STOCK_DIFF, &dir, &path)).expect("a report");

    // All three defects, stated by the interpreter, before any model has
    // been asked anything. Every run so far missed the first two.
    assert!(
        report.contains("cheapest([0, 5]) -> 5"),
        "the zero-is-falsy defect: {report}"
    );
    assert!(
        report.contains("needs_reorder(3, 3) -> False"),
        "the fallen-*to*-the-threshold defect: {report}"
    );
    assert!(
        report.contains("a second time"),
        "the shared default log: {report}"
    );

    // And none of the battery's own fumbles: `cheapest(0)` iterating over an
    // int says nothing about the code under review.
    assert!(
        !report.contains("TypeError"),
        "the probe's own mistakes are not findings: {report}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_contract_a_diff_breaks_can_be_in_a_file_the_diff_never_touches() {
    let dir = std::env::temp_dir().join(format!("committee-orders-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("pricing.py"),
        "\"\"\"Money is pence.\"\"\"\n\n\
         def line_total(unit_pence, quantity):\n\
         \x20   \"\"\"What `quantity` of an item at `unit_pence` each costs, in pence.\"\"\"\n\
         \x20   return unit_pence * quantity\n\n\
         def apply_discount(total_pence, fraction):\n\
         \x20   \"\"\"`total_pence` with `fraction` of it taken off, rounded to whole pence.\"\"\"\n\
         \x20   return round(total_pence * (1 - fraction))\n",
    )
    .unwrap();

    // A raw literal, because a diff's leading space is what marks a context
    // line and `\`-continuation eats it.
    let diff = r#"+++ b/invoice.py
@@ -1,6 +1,6 @@
 import pricing

 def render_line(name, unit_pence, quantity):
     total = pricing.line_total(unit_pence, quantity)
-    return f"{name}: £{total / 100:.2f}"
+    return f"{name}: £{total:.2f}"
"#;

    let context = imported_context(diff, &dir).expect("pricing.py is reachable");

    // The units contract, which is the only thing that makes the changed
    // line wrong, and which `git diff HEAD` will never show.
    assert!(
        context.contains("def line_total(unit_pence, quantity)"),
        "{context}"
    );
    assert!(context.contains("in pence"), "{context}");
    assert!(
        context.contains("def apply_discount(total_pence, fraction)"),
        "{context}"
    );
    // Signatures and contracts, not bodies — the diff has to stay readable.
    assert!(
        !context.contains("return unit_pence * quantity"),
        "{context}"
    );

    // A module that isn't in the repository contributes nothing, and neither
    // does one the diff already shows.
    assert_eq!(imported_context("+++ b/a.py\n+import os\n", &dir), None);
    assert_eq!(
        imported_context("+++ b/pricing.py\n+import pricing\n", &dir),
        None
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_prover_is_handed_the_promise_the_code_is_breaking() {
    // The contract sits two lines above the changed line, and provers kept
    // reading past it — writing `expected = False` for a function whose
    // docstring says True, which is the bug copied into the specification.
    let contract = contract_for(STOCK_DIFF, "Incorrect logic in needs_reorder()")
        .expect("needs_reorder is in the diff");
    assert!(
        contract.contains("def needs_reorder(stock, threshold):"),
        "{contract}"
    );
    assert!(
        contract.contains("fallen to the reorder threshold"),
        "{contract}"
    );

    // Named by the hunk header rather than by a context line, and its
    // docstring is a context line too.
    let cheapest = contract_for(STOCK_DIFF, "Incorrect handling of empty list in cheapest()")
        .expect("cheapest");
    assert!(
        cheapest.contains("or None when the list is empty"),
        "{cheapest}"
    );

    // A changed signature is found from its added line.
    assert!(
        contract_for(STOCK_DIFF, "Incorrect default argument in restock()")
            .is_some_and(|block| block.contains("log=[]"))
    );

    // Word-bounded: "restock" must not resolve to a `stock` function, and a
    // claim naming nothing in the diff gets nothing.
    assert_eq!(
        contract_for(STOCK_DIFF, "something is wrong somewhere"),
        None
    );
}

#[test]
fn a_prover_sees_which_inputs_were_tried_and_not_what_they_returned() {
    let report = "stock.py:\n\
                  cheapest([0, 5]) -> 5\n\
                  needs_reorder(3, 3) -> False\n\
                  split_pot(100, []) !! ZeroDivisionError: division by zero\n\
                  restock(0, []) -> [(0, [])]\n\
                  restock(0, []) a second time -> [(0, []), (0, [])]\n";
    let calls = calls_only(report);

    // The inputs, so a prover stops inventing `cheapest([])`.
    assert!(calls.contains("cheapest([0, 5])"), "{calls}");
    assert!(calls.contains("needs_reorder(3, 3)"), "{calls}");
    assert!(calls.contains("split_pot(100, [])"), "{calls}");

    // And none of the answers, because a prover handed an observed value
    // writes it into `expected:` and compares it against itself. Every proof
    // in one measured run did exactly that.
    assert!(!calls.contains("-> 5"), "{calls}");
    assert!(!calls.contains("False"), "{calls}");
    assert!(!calls.contains("ZeroDivisionError"), "{calls}");
    assert!(!calls.contains("[(0, [])]"), "{calls}");

    // The repeat is a hint about method, not a value, so it survives.
    assert!(
        calls.contains("and again, to see if it answers the same"),
        "{calls}"
    );

    // The file heading isn't a call.
    assert!(!calls.contains("stock.py:"), "{calls}");
}

#[test]
fn the_oracle_is_asked_what_should_have_happened_and_the_harness_does_the_comparing() {
    let report = "stock.py:\n\
                  cheapest([0, 5]) -> 5\n\
                  needs_reorder(3, 3) -> False\n\
                  needs_reorder(7, 0) -> False\n\
                  restock(0, []) -> [(0, [])]\n\
                  restock(0, []) a second time -> [(0, []), (0, [])]\n";
    let observed = observations(report);
    assert_eq!(observed.len(), 4, "the repeat marker is not one call");
    assert_eq!(observed[0].call, "cheapest([0, 5])");
    assert_eq!(observed[0].result, "5");

    // A plausible reply, with everything models actually do to it: bullets,
    // backticks, an UNKNOWN, prose instead of a literal, and a call nobody
    // asked about.
    let reply = "- `cheapest([0, 5])` -> 0\n\
                 needs_reorder(3, 3) -> True\n\
                 needs_reorder(7, 0) -> False\n\
                 restock(0, []) -> UNKNOWN\n\
                 cheapest([9]) -> 9\n\
                 split_pot(1, []) -> it depends on whether the roster is empty, which the \
                 docstring does not say, so this is hard to answer definitively\n";
    let answers = parse_oracle(reply, &observed);
    assert_eq!(
        answers,
        vec![
            ("cheapest([0, 5])".to_string(), "0".to_string()),
            ("needs_reorder(3, 3)".to_string(), "True".to_string()),
            ("needs_reorder(7, 0)".to_string(), "False".to_string()),
        ]
    );

    // And the comparison, which no model gets a vote on. Two defects, each
    // stated as a contradiction between the code and its own documentation —
    // and `needs_reorder(7, 0)`, which agrees, stays quiet.
    let mismatches = oracle_mismatches(&answers, &observed);
    assert_eq!(
        mismatches,
        vec![
            "cheapest([0, 5]) returned 5, but its docstring says it should return 0".to_string(),
            "needs_reorder(3, 3) returned False, but its docstring says it should return True"
                .to_string(),
        ]
    );

    // 0 and False are the same value wearing different hats, and an oracle
    // that answers in the other spelling has not found a defect.
    let spelling = vec![("needs_reorder(7, 0)".to_string(), "0".to_string())];
    assert!(oracle_mismatches(&spelling, &observed).is_empty());

    // But the same spelling against a genuinely different value still is
    // one: `1` where the code said `False` is True against False.
    assert_eq!(
        oracle_mismatches(
            &[("needs_reorder(3, 3)".to_string(), "1".to_string())],
            &observed
        )
        .len(),
        1
    );

    // Real, from a live run: the oracle answered `[[0, []]]` where the code
    // returned `[(0, [])]` — the same shape in different brackets, which is
    // a notation it picked and not a defect it found.
    let brackets = vec![("restock(0, [])".to_string(), "[[0, []]]".to_string())];
    assert!(oracle_mismatches(&brackets, &observed).is_empty());
}
