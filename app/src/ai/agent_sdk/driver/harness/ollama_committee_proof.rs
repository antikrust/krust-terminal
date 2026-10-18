//! The committee's verification layer: how a claim about code becomes a
//! machine-checked fact.
//!
//! Two kinds of evidence live here, and neither asks a model's opinion:
//!
//! - **Proof scripts** — a reviewer writes a minimal program that exits 0 if
//!   the defect it claims is real and 1 if the code is actually fine; this
//!   module stages it, runs it in the repository under a hard timeout, and
//!   classifies the exit.
//! - **The project's own suite** — `cargo test`, `pytest`, `npm test`, `go
//!   test`: whatever this repo already uses to define "correct". Its output
//!   is the strongest evidence available, because the project wrote it.
//!
//! Deliberately NOT an MCP server. MCP exists so a *model* can choose to
//! call a tool mid-conversation; verification is the opposite arrangement —
//! the harness decides what runs, always, and the model never gets a vote on
//! whether its claim gets checked. Keeping it in-process also keeps it out
//! of reach of the local models that `is_exposed_to_local_models` already
//! withholds third-party tools from.

use std::path::Path;

use uuid::Uuid;

use super::ollama_stream_exec::{execute_shell, ShellExecution};

/// How many findings get a proof script before the budget runs out — each
/// costs a model call plus an execution.
pub(crate) const MAX_PROOFS: usize = 6;

/// Wall-clock ceiling for one proof script.
pub(crate) const PROOF_TIMEOUT_SECS: u64 = 15;

/// Wall-clock ceiling for the project's own suite. Generous compared to a
/// proof script — real suites are slow — but bounded, because a committee
/// run must not hang on someone's integration tests.
pub(crate) const SUITE_TIMEOUT_SECS: u64 = 180;

/// Exit code the timeout wrapper reports, so a timeout can never be
/// mistaken for the script's own verdict.
const TIMEOUT_EXIT_CODE: i32 = 124;

/// Exit code the shim reports when the proof script raised instead of
/// deciding.
///
/// This distinction is load-bearing. An uncaught Python exception exits 1 —
/// the same code the contract uses for "I checked, the code is fine" — so
/// without the shim a script that merely failed to import silently read as
/// a refutation, and the committee dismissed a real defect with false
/// confidence. A crash must never be a verdict.
const CRASHED_EXIT_CODE: i32 = 125;

/// Exit code the shim reports when the script ran to the end without ever
/// stating a verdict. Also not a verdict.
const NO_VERDICT_EXIT_CODE: i32 = 126;

/// Whether a failed proof is worth one more attempt by the same model. A
/// crash, a script that forgot to decide, or a syntax error are all the
/// prover breaking the contract — it can be told what it did and try again.
/// A timeout is not: the script ran, it just never finished, and a second
/// one will do the same. A clean 0 or 1 is a verdict and needs nothing.
pub(crate) fn proof_script_is_fixable(exit_code: i32) -> bool {
    matches!(exit_code, CRASHED_EXIT_CODE | NO_VERDICT_EXIT_CODE | 2)
}

// ---------------------------------------------------------------------------
// Proof scripts
// ---------------------------------------------------------------------------

/// The machine's word on one finding, from executing its proof script.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ProofOutcome {
    /// Exit 0 — the script demonstrated the defect on the real code.
    Demonstrated(String),
    /// Exit 1 — the script says the code behaves correctly.
    Refuted(String),
    /// Crash, timeout, or no usable script — proves nothing either way.
    Inconclusive(String),
}

impl ProofOutcome {
    pub(crate) fn label(&self) -> String {
        match self {
            ProofOutcome::Demonstrated(observation) => {
                format!("DEMONSTRATED by execution — {observation}")
            }
            ProofOutcome::Refuted(observation) => {
                format!("REFUTED by execution — {observation}")
            }
            ProofOutcome::Inconclusive(reason) => format!("execution inconclusive — {reason}"),
        }
    }
}

/// The last non-empty line of some output, capped — what a proof script
/// prints last is its observation.
fn last_line(text: &str, max_chars: usize) -> String {
    let line = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if line.chars().count() <= max_chars {
        return line.to_string();
    }
    format!("{}…", line.chars().take(max_chars).collect::<String>())
}

/// What a proof script said about its own verdict, read back off stdout.
///
/// The exit code alone is worth very little. A prover that writes
///
/// ```text
/// result = stock.cheapest([])
/// print(f"Observed: {result}")
/// exit(0)  # Defect is real if it returns a non-None value
/// ```
///
/// exits 0 whatever it saw: the comment does the comparing and the
/// interpreter never reads comments. That script certified a defect it had
/// not touched. So the contract asks for the comparison in the output, where
/// it can be checked without asking a model anything.
/// And once the comparison is in the output, it is worth more than the exit
/// code — because the two values were *computed* (one by running the code,
/// one from reading what it is for) while the exit code is the model's
/// reasoning **about** them, and that is the part it keeps getting backwards:
///
/// ```text
/// result = stock.needs_reorder(3, 3)
/// print('expected:', True)
/// print('actual:', result)          # False
/// if result == True: exit(0)
/// else: exit(1)                     # ← says "the code is fine"
/// ```
///
/// True is not False, and no opinion is required to see it. So when a script
/// prints both lines, the comparison decides and the exit code is noted and
/// ignored.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SelfCheck {
    /// Printed both lines. This is the verdict.
    Compared {
        expected: String,
        actual: String,
        /// Whether the two values disagree — which is the defect.
        differ: bool,
        /// Whether the script's own exit code agreed with them.
        agreed: bool,
    },
    /// Never said, so the exit code is the only thing to go on.
    Unstated,
}

/// The `expected:`/`actual:` value a script reported, if it reported one.
/// The colon is required — "expectedly slow" is prose, not a verdict.
fn reported(stdout: &str, label: &str) -> Option<String> {
    stdout.lines().rev().find_map(|line| {
        let line = line.trim();
        if !line.get(..label.len())?.eq_ignore_ascii_case(label) {
            return None;
        }
        let value = line[label.len()..].trim_start().strip_prefix(':')?;
        Some(value.trim().to_string())
    })
}

/// Reads a script's own comparison off its output, and notes whether its
/// exit code agreed. A script that printed neither line is
/// [`SelfCheck::Unstated`], which the caller weighs by what it claimed.
pub(crate) fn self_check(stdout: &str, exit_code: i32) -> SelfCheck {
    let (Some(expected), Some(actual)) = (reported(stdout, "expected"), reported(stdout, "actual"))
    else {
        return SelfCheck::Unstated;
    };
    let differ = !expected.eq_ignore_ascii_case(&actual);
    SelfCheck::Compared {
        expected,
        actual,
        differ,
        // The contract is exit 0 when they differ, exit 1 when they match.
        agreed: (exit_code == 0) == differ,
    }
}

/// Maps a proof script's exit to its meaning. 0 and 1 are the contract;
/// [`TIMEOUT_EXIT_CODE`] is the wrapper's marker; anything else is a crash.
pub(crate) fn classify_proof(execution: &ShellExecution) -> ProofOutcome {
    let observation = last_line(&execution.stdout, 200);
    match execution.exit_code {
        0 => ProofOutcome::Demonstrated(if observation.is_empty() {
            "the proof script exited 0".to_string()
        } else {
            observation
        }),
        1 => ProofOutcome::Refuted(if observation.is_empty() {
            "the proof script exited 1".to_string()
        } else {
            observation
        }),
        TIMEOUT_EXIT_CODE => ProofOutcome::Inconclusive("the proof script timed out".to_string()),
        CRASHED_EXIT_CODE => ProofOutcome::Inconclusive(format!(
            "the proof script raised instead of deciding: {}",
            last_line(&execution.stderr, 200)
        )),
        NO_VERDICT_EXIT_CODE => {
            ProofOutcome::Inconclusive("the proof script ended without exiting 0 or 1".to_string())
        }
        code => ProofOutcome::Inconclusive(format!(
            "the proof script exited {code}: {}",
            last_line(&execution.stderr, 200)
        )),
    }
}

/// Strips one surrounding markdown code fence (with optional info string) —
/// provers are told not to fence, and fence anyway.
pub(crate) fn strip_code_fences(content: &str) -> String {
    let trimmed = content.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed.to_string();
    };
    let rest = rest.split_once('\n').map(|(_, body)| body).unwrap_or("");
    rest.strip_suffix("```").unwrap_or(rest).trim().to_string()
}

/// Shell-quotes a path for the single command string `execute_shell` takes.
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

/// Executes a proof script in the repository's working directory under a
/// hard timeout. Staged in the temp dir like `run_code`'s scripts; a small
/// wrapper turns a timeout into [`TIMEOUT_EXIT_CODE`] so it can't read as a
/// refutation.
pub(crate) async fn run_proof_script(
    code: &str,
    timeout_secs: u64,
    working_dir: &Path,
    path: &str,
) -> ShellExecution {
    let stage_dir = std::env::temp_dir();
    let id = Uuid::new_v4();
    let proof = stage_dir.join(format!("krust-committee-proof-{id}.py"));
    let wrapper = stage_dir.join(format!("krust-committee-wrap-{id}.py"));
    // The shim runs the proof with the repository first on `sys.path` — the
    // script is staged in the temp dir, so without this a perfectly sensible
    // `import payday` looks for the module next to the temp file and dies —
    // and maps a raise or a fall-off-the-end to codes the contract can tell
    // apart from a real verdict.
    let wrapper_source = format!(
        "import subprocess, sys\n\
         SHIM = '''\n\
         import runpy, sys, traceback\n\
         sys.path.insert(0, sys.argv[2])\n\
         try:\n\
         \x20   runpy.run_path(sys.argv[1], run_name=\"__main__\")\n\
         except SystemExit:\n\
         \x20   raise\n\
         except BaseException:\n\
         \x20   traceback.print_exc()\n\
         \x20   raise SystemExit({CRASHED_EXIT_CODE})\n\
         raise SystemExit({NO_VERDICT_EXIT_CODE})\n\
         '''\n\
         try:\n\
         \x20   completed = subprocess.run(\n\
         \x20       [sys.executable, '-c', SHIM, sys.argv[1], sys.argv[2]],\n\
         \x20       timeout={timeout_secs},\n\
         \x20   )\n\
         \x20   sys.exit(completed.returncode)\n\
         except subprocess.TimeoutExpired:\n\
         \x20   print('proof timed out')\n\
         \x20   sys.exit({TIMEOUT_EXIT_CODE})\n"
    );
    if std::fs::write(&proof, code).is_err() || std::fs::write(&wrapper, wrapper_source).is_err() {
        return ShellExecution {
            stdout: String::new(),
            stderr: "could not stage the proof script".to_string(),
            exit_code: 70,
        };
    }
    let command = format!(
        "python3 {} {} {}",
        shell_quote(&wrapper),
        shell_quote(&proof),
        shell_quote(working_dir)
    );
    let execution = execute_shell(&command, working_dir, path).await;
    let _ = std::fs::remove_file(&proof);
    let _ = std::fs::remove_file(&wrapper);
    execution
}

// ---------------------------------------------------------------------------
// Generated regression tests
// ---------------------------------------------------------------------------

/// What running a generated test file established.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TestRun {
    /// Test functions that passed — the behaviour is already correct.
    pub(crate) passed: Vec<String>,
    /// Test functions that failed, with the assertion's own message. On a
    /// regression test written against a real defect, this is the good
    /// outcome: it fails now and passes once the bug is fixed.
    pub(crate) failed: Vec<(String, String)>,
    /// The file didn't run at all — a syntax error, a bad import. Nothing
    /// was learned, and the model gets told exactly this.
    pub(crate) broken: Option<String>,
}

impl TestRun {
    pub(crate) fn total(&self) -> usize {
        self.passed.len() + self.failed.len()
    }
}

/// Runs a generated pytest-style file without needing pytest: the shim
/// imports it with the repository on `sys.path` and calls every `test_*`
/// function, recording pass or failure per test.
///
/// Deliberately not `python3 -m pytest`: the file is meant to be keepable as
/// a pytest file, but a machine that hasn't installed pytest must still get
/// an answer, and "pytest isn't installed" is not a fact about the user's
/// code.
pub(crate) async fn run_python_tests(
    source: &str,
    timeout_secs: u64,
    working_dir: &Path,
    path: &str,
) -> TestRun {
    let stage_dir = std::env::temp_dir();
    let id = Uuid::new_v4();
    let tests = stage_dir.join(format!("krust-committee-tests-{id}.py"));
    let runner = stage_dir.join(format!("krust-committee-runner-{id}.py"));
    // Each result is one machine-readable line, so the Rust side never has
    // to parse a traceback to know what happened.
    let runner_source = format!(
        "import subprocess, sys\n\
         SHIM = '''\n\
         import runpy, sys, traceback\n\
         sys.path.insert(0, sys.argv[2])\n\
         try:\n\
         \x20   module = runpy.run_path(sys.argv[1])\n\
         except BaseException:\n\
         \x20   traceback.print_exc()\n\
         \x20   raise SystemExit({CRASHED_EXIT_CODE})\n\
         tests = sorted(k for k, v in module.items() if k.startswith(\"test_\") and callable(v))\n\
         if not tests:\n\
         \x20   raise SystemExit({NO_VERDICT_EXIT_CODE})\n\
         for name in tests:\n\
         \x20   try:\n\
         \x20       module[name]()\n\
         \x20       print(\"PASS\\t\" + name)\n\
         \x20   except AssertionError as error:\n\
         \x20       print(\"FAIL\\t\" + name + \"\\t\" + (str(error) or \"assertion failed\"))\n\
         \x20   except BaseException as error:\n\
         \x20       print(\"FAIL\\t\" + name + \"\\t\" + type(error).__name__ + \": \" + str(error))\n\
         '''\n\
         try:\n\
         \x20   completed = subprocess.run(\n\
         \x20       [sys.executable, '-c', SHIM, sys.argv[1], sys.argv[2]],\n\
         \x20       timeout={timeout_secs},\n\
         \x20   )\n\
         \x20   sys.exit(completed.returncode)\n\
         except subprocess.TimeoutExpired:\n\
         \x20   sys.exit({TIMEOUT_EXIT_CODE})\n"
    );
    if std::fs::write(&tests, source).is_err() || std::fs::write(&runner, runner_source).is_err() {
        return TestRun {
            passed: Vec::new(),
            failed: Vec::new(),
            broken: Some("could not stage the test file".to_string()),
        };
    }
    let command = format!(
        "python3 {} {} {}",
        shell_quote(&runner),
        shell_quote(&tests),
        shell_quote(working_dir)
    );
    let execution = execute_shell(&command, working_dir, path).await;
    let _ = std::fs::remove_file(&tests);
    let _ = std::fs::remove_file(&runner);
    parse_test_output(&execution)
}

/// Turns the runner's PASS/FAIL lines into a [`TestRun`].
pub(crate) fn parse_test_output(execution: &ShellExecution) -> TestRun {
    let mut run = TestRun {
        passed: Vec::new(),
        failed: Vec::new(),
        broken: None,
    };
    for line in execution.stdout.lines() {
        match line.split('\t').collect::<Vec<_>>().as_slice() {
            ["PASS", name] => run.passed.push((*name).to_string()),
            ["FAIL", name, reason] => run
                .failed
                .push(((*name).to_string(), (*reason).to_string())),
            ["FAIL", name] => run.failed.push(((*name).to_string(), String::new())),
            _ => {}
        }
    }
    if run.total() == 0 {
        run.broken = Some(match execution.exit_code {
            NO_VERDICT_EXIT_CODE => "the file defined no test_ functions".to_string(),
            TIMEOUT_EXIT_CODE => "the tests timed out".to_string(),
            _ => last_line(&execution.stderr, 300),
        });
        if run.broken.as_deref().is_some_and(str::is_empty) {
            run.broken = Some("the test file produced no results".to_string());
        }
    }
    run
}

/// How many of a generated suite's tests a draft passes — the fixed yardstick
/// a rewrite round has never had.
///
/// The suite imports the draft by `module` name (the task's `save_as` without
/// `.py`), so the draft is staged as `<module>.py` with its directory on the
/// path the runner imports from, and [`run_python_tests`] does the rest.
///
/// `None` when the draft could not even be imported — a syntax error, or not
/// Python at all. That is *unknowable*, not zero: scoring it zero would read
/// as "failed every test" and reject a draft over a staging fault, which is
/// exactly the kind of false signal the whole pipeline exists to avoid.
///
/// Validated against the real arcade grader before it was wired to anything:
/// a correct wrap scores 8, the phantom-empty-line bug 6, a splitter 4. That
/// separation is the point — a revision is kept only when it raises the number.
pub(crate) async fn score_against_suite(
    draft: &str,
    module: &str,
    suite: &str,
    timeout_secs: u64,
    path: &str,
) -> Option<usize> {
    let dir = std::env::temp_dir().join(format!("krust-score-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).ok()?;
    let run = match std::fs::write(dir.join(format!("{module}.py")), draft) {
        Ok(()) => run_python_tests(suite, timeout_secs, &dir, path).await,
        Err(_) => TestRun {
            passed: Vec::new(),
            failed: Vec::new(),
            broken: Some("could not stage the draft".to_string()),
        },
    };
    let _ = std::fs::remove_dir_all(&dir);
    // `broken` already subsumes total == 0 (see `parse_test_output`), so a
    // clean `None` here means "the suite never ran against this draft".
    if run.broken.is_some() {
        return None;
    }
    Some(run.passed.len())
}

/// Whether a revised draft earns its place over the one it replaced: only when
/// it scores strictly higher.
///
/// Equal is churn — a different program of the same quality — and keeping
/// exactly those is the measured failure of the rewrite rounds: a 7/8 became a
/// different 7/8 three times over, and a *sharper* note took 7/8 to 3/8.
/// Strictly higher makes a round monotone: a floor it can never fall below,
/// and a ratchet for any real gain.
pub(crate) fn revision_improves(before: usize, after: usize) -> bool {
    after > before
}

// ---------------------------------------------------------------------------
// The project's own suite
// ---------------------------------------------------------------------------

/// A test suite this repository already has, detected from its manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectSuite {
    Cargo,
    Pytest,
    Npm,
    Go,
}

impl ProjectSuite {
    /// The command this suite runs under. Quiet flags where they exist —
    /// the committee reads the tail, not the whole log.
    pub(crate) fn command(self) -> &'static str {
        match self {
            ProjectSuite::Cargo => "cargo test --quiet",
            ProjectSuite::Pytest => "python3 -m pytest -q",
            ProjectSuite::Npm => "npm test --silent",
            ProjectSuite::Go => "go test ./...",
        }
    }

    pub(crate) fn display_name(self) -> &'static str {
        match self {
            ProjectSuite::Cargo => "cargo test",
            ProjectSuite::Pytest => "pytest",
            ProjectSuite::Npm => "npm test",
            ProjectSuite::Go => "go test",
        }
    }
}

/// Whether `package.json` declares a test script that actually runs
/// something — the npm default is a stub that exits 1 with "no test
/// specified", which would read as a failing suite forever.
fn npm_has_real_test_script(working_dir: &Path) -> bool {
    let Ok(manifest) = std::fs::read_to_string(working_dir.join("package.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&manifest) else {
        return false;
    };
    value
        .get("scripts")
        .and_then(|scripts| scripts.get("test"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|script| !script.contains("no test specified"))
}

/// The suite this repository uses, if one is obvious from its manifests.
/// Checked in rough order of how definitive the marker is.
pub(crate) fn detect_suite(working_dir: &Path) -> Option<ProjectSuite> {
    let has = |name: &str| working_dir.join(name).exists();
    if has("Cargo.toml") {
        return Some(ProjectSuite::Cargo);
    }
    if has("go.mod") {
        return Some(ProjectSuite::Go);
    }
    if has("pytest.ini")
        || has("tox.ini")
        || has("pyproject.toml")
        || (has("tests") && has("setup.py"))
    {
        return Some(ProjectSuite::Pytest);
    }
    if has("package.json") && npm_has_real_test_script(working_dir) {
        return Some(ProjectSuite::Npm);
    }
    None
}

/// What the project's suite said about the working tree.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SuiteOutcome {
    /// Everything green — findings claiming broken behavior have to explain
    /// why the suite disagrees.
    Passed,
    /// Red, with the tail of the output as evidence.
    Failed(String),
    /// Ran but couldn't be judged (timeout, missing toolchain, crash).
    Unavailable(String),
}

impl SuiteOutcome {
    /// One line for the transcript and for the judge's prompt.
    pub(crate) fn label(&self, suite: ProjectSuite) -> String {
        match self {
            SuiteOutcome::Passed => {
                format!("`{}` passes on this working tree", suite.display_name())
            }
            SuiteOutcome::Failed(tail) => {
                format!("`{}` FAILS: {tail}", suite.display_name())
            }
            SuiteOutcome::Unavailable(reason) => {
                format!("`{}` could not be run — {reason}", suite.display_name())
            }
        }
    }
}

/// Runs the detected suite in the working directory under a timeout.
///
/// The timeout rides the same python wrapper as proof scripts — macOS has
/// no `timeout` binary — and the command runs under zsh with `pipefail`, so
/// capping the log with `tail` can't launder a failing exit into a pass.
/// A suite that can't run (no toolchain, a timeout) is `Unavailable`, never
/// `Failed` — a missing `cargo` is not evidence about the user's code.
pub(crate) async fn run_suite(suite: ProjectSuite, working_dir: &Path, path: &str) -> SuiteOutcome {
    let wrapper = std::env::temp_dir().join(format!("krust-committee-suite-{}.py", Uuid::new_v4()));
    let wrapper_source = format!(
        "import subprocess, sys\n\
         try:\n\
         \x20   sys.exit(subprocess.run(['/bin/zsh', '-lc', 'set -o pipefail; ' + sys.argv[1]], timeout={SUITE_TIMEOUT_SECS}).returncode)\n\
         except subprocess.TimeoutExpired:\n\
         \x20   print('suite timed out')\n\
         \x20   sys.exit({TIMEOUT_EXIT_CODE})\n"
    );
    if std::fs::write(&wrapper, wrapper_source).is_err() {
        return SuiteOutcome::Unavailable("could not stage the suite wrapper".to_string());
    }
    let piped = format!("{} 2>&1 | tail -40", suite.command());
    let command = format!(
        "python3 {} {}",
        shell_quote(&wrapper),
        shell_quote(Path::new(&piped))
    );
    let execution = execute_shell(&command, working_dir, path).await;
    let _ = std::fs::remove_file(&wrapper);
    let tail = last_line(&execution.stdout, 300);
    match execution.exit_code {
        0 => SuiteOutcome::Passed,
        TIMEOUT_EXIT_CODE => {
            SuiteOutcome::Unavailable(format!("it ran longer than {SUITE_TIMEOUT_SECS}s"))
        }
        // 127 is the shell's "command not found" — the toolchain is absent.
        127 => SuiteOutcome::Unavailable("the tool isn't installed".to_string()),
        _ if tail.is_empty() => SuiteOutcome::Unavailable("it produced no output".to_string()),
        _ => SuiteOutcome::Failed(tail),
    }
}

// ---------------------------------------------------------------------------
// Behavior report
// ---------------------------------------------------------------------------
//
// Reviewers can't call tools, so the harness runs the changed code FOR them
// before the review starts: every changed function gets called against a
// broad battery of generic inputs — broad because nothing here knows how the
// code is really called — and the observed outputs and exceptions go to the
// models as evidence. A reviewer that has already seen
// `split_pot(100, []) !! ZeroDivisionError` doesn't have to guess.

/// At most this many changed files get probed, and this many functions per
/// file — a report, not a fuzzing campaign.
const MAX_PROBE_FILES: usize = 3;
const MAX_PROBE_FUNCTIONS: usize = 8;
/// The report the models see is capped hard; it shares their context.
const MAX_REPORT_CHARS: usize = 2_400;

/// A function the diff added or touched, worth probing.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProbeTarget {
    pub(crate) file: String,
    pub(crate) function: String,
    /// How many arguments a caller has to pass — parameters with defaults
    /// don't count. Probing at the required arity is what lets the default
    /// itself misbehave: `restock(item, quantity, log=[])` only shares its
    /// list between calls when nobody passes a log, so a probe that always
    /// fills in all three parameters can never see the defect.
    pub(crate) arity: usize,
}

/// Parses a single-line top-level `def` into a name and its required arity.
/// Methods and underscore-private functions are skipped — no instance to
/// call them on, no contract to check.
fn parse_def(source: &str) -> Option<(String, usize)> {
    let (name, rest) = source.strip_prefix("def ")?.split_once('(')?;
    let (params, _) = rest.split_once(')')?;
    let name = name.trim();
    if name.is_empty() || name.starts_with('_') {
        return None;
    }
    let params: Vec<&str> = params
        .split(',')
        .map(str::trim)
        .filter(|param| !param.is_empty())
        .collect();
    if params
        .iter()
        .any(|param| *param == "self" || *param == "cls")
    {
        return None;
    }
    let required = params
        .iter()
        .filter(|param| !param.contains('=') && !param.starts_with('*'))
        .count();
    Some((name.to_string(), required))
}

/// How much of the surrounding code the reviewers are shown.
const MAX_IMPORTED_CHARS: usize = 1_600;

/// One call the probe made, with what the code returned.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Observation {
    pub(crate) call: String,
    pub(crate) result: String,
}

/// Splits a behaviour report back into its observations.
///
/// Including the ones that raised. A call that blew up is a call with an
/// answer — "it raised ZeroDivisionError" — and the docstring usually says
/// it shouldn't have: `split_pot(0, [])` on a roster of nobody should hand
/// back an empty split, not divide by zero. Skipping those lines meant the
/// oracle was never asked about them, and a planted defect sat in the
/// behaviour report, in plain sight, unexamined.
pub(crate) fn observations(report: &str) -> Vec<Observation> {
    report
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            // The repeat marker describes a pair of calls rather than one,
            // so there is no single value to hold it to.
            if line.contains(" a second time") {
                return None;
            }
            let (call, result) = match line.split_once(" -> ") {
                Some((call, result)) => (call, result.trim().to_string()),
                None => {
                    let (call, error) = line.split_once(" !! ")?;
                    // The type alone. The message is often the arguments
                    // read back, which would hand the oracle the answer.
                    let kind = error.split(':').next().unwrap_or(error).trim();
                    (call, format!("{RAISED} {kind}"))
                }
            };
            call.contains('(').then(|| Observation {
                call: call.trim().to_string(),
                result,
            })
        })
        .collect()
}

/// How an observation records an exception, so the mismatch line can read
/// like English rather than like "returned raised ValueError".
pub(crate) const RAISED: &str = "raised";

/// Reads an oracle's `call -> literal` reply, keeping only the calls that
/// were actually asked about. A model that invents a call, answers in prose,
/// or says UNKNOWN contributes nothing rather than something wrong.
pub(crate) fn parse_oracle(reply: &str, asked: &[Observation]) -> Vec<(String, String)> {
    let mut answers: Vec<(String, String)> = Vec::new();
    for line in reply.lines() {
        let line = line.trim().trim_start_matches(['-', '*', '•']).trim();
        let Some((call, expected)) = line.split_once("->") else {
            continue;
        };
        let call = call.trim().trim_matches('`').trim();
        let expected = expected.trim().trim_matches('`').trim();
        if expected.is_empty() || expected.eq_ignore_ascii_case("unknown") {
            continue;
        }
        // Long answers are prose, not literals, and prose can't be compared.
        if expected.len() > 80 {
            continue;
        }
        if !asked.iter().any(|observation| observation.call == call) {
            continue;
        }
        if answers.iter().any(|(known, _)| known == call) {
            continue;
        }
        answers.push((call.to_string(), expected.to_string()));
    }
    answers
}

/// Where a docstring and the code disagree about one call: the strongest
/// evidence this pipeline can produce, and the cheapest.
///
/// Only the calls whose answers differ, and only as text — the caller turns
/// them into a report.
pub(crate) fn oracle_mismatches(
    answers: &[(String, String)],
    observed: &[Observation],
) -> Vec<String> {
    let mut mismatches = Vec::new();
    for (call, expected) in answers {
        let Some(observation) = observed.iter().find(|entry| &entry.call == call) else {
            continue;
        };
        // Only flag a difference that survives the obvious spellings. An
        // oracle writing a literal from prose picks a notation as much as a
        // value: it answered `[[0, []]]` where the code returned
        // `[(0, [])]`, which is the same shape in different brackets and
        // absolutely not a defect. Manufacturing findings out of notation is
        // the mistake the behaviour probe already had to have taken out of
        // it.
        let normalise = |value: &str| match value.trim() {
            "1" => "True".to_string(),
            "0" => "False".to_string(),
            other => other
                .replace(['(', '['], "[")
                .replace([')', ']'], "]")
                .replace(' ', ""),
        };
        if normalise(expected).eq_ignore_ascii_case(&normalise(&observation.result)) {
            continue;
        }
        mismatches.push(match observation.result.strip_prefix(RAISED) {
            Some(kind) => {
                format!("{call} raised{kind}, but its docstring says it should return {expected}")
            }
            None => format!(
                "{call} returned {}, but its docstring says it should return {expected}",
                observation.result
            ),
        });
    }
    mismatches
}

/// Calls a model proposed for the changed functions, kept only where they
/// are plainly calls to those functions and nothing else.
///
/// A generic battery cannot invent a structured argument. The `orders`
/// fixture has `checkout(items, percent_off)` whose docstring says "`items`
/// are (name, unit_pence, quantity) triples", and no combination of zeros,
/// empty lists and short integers will ever produce one — so the defect,
/// which needs a real basket to show itself, was never exercised and was
/// duly missed. A model reading that docstring writes the argument without
/// difficulty. Proposing inputs is the thing models are best at here; it is
/// deciding what came back that they cannot do.
pub(crate) fn parse_proposed_calls(reply: &str, known: &[String]) -> Vec<String> {
    let mut calls: Vec<String> = Vec::new();
    for line in reply.lines() {
        let line = line
            .trim()
            .trim_start_matches(['-', '*', '•', '>'])
            .trim()
            .trim_matches('`')
            .trim();
        let Some(name) = line.split('(').next() else {
            continue;
        };
        if !known.iter().any(|function| function == name.trim()) {
            continue;
        }
        // It is evaluated, so it has to be a call and nothing else. Proof
        // scripts are already model-written and run, so the trust boundary
        // is unchanged — but a line that reaches outside the module is a
        // line the model misunderstood, not an input worth trying.
        if !line.ends_with(')') || line.len() > 200 {
            continue;
        }
        if [
            "import", "__", "open(", "exec(", "eval(", "os.", "sys.", ";", "=",
        ]
        .iter()
        .any(|banned| line.contains(banned))
        {
            continue;
        }
        if !calls.iter().any(|known| known == line) {
            calls.push(line.to_string());
        }
    }
    calls.truncate(MAX_PROBE_FUNCTIONS);
    calls
}

/// Runs whatever python a panelist wrote and reports what its functions do.
///
/// Panel rounds had no evidence at all. Review mode probes, asks an oracle
/// and executes proofs; a rewrite round was the judge reading code, forming
/// an opinion, and three small models revising on the strength of it, with
/// nothing ever run. Measured, that made things worse — a draft scoring 7 of
/// 8 came back scoring 6, the revision having spread its one bug into a
/// second case. Churn on a nearly-right answer breaks it more often than it
/// mends it.
///
/// A syntax error or an import that fails shows up here as "module load
/// failed", which is the other thing a round should never have to guess at.
pub(crate) async fn probe_source(
    source: &str,
    calls: &[String],
    working_dir: &Path,
    path: &str,
) -> Option<String> {
    let mut targets: Vec<ProbeTarget> = Vec::new();
    for line in source.lines() {
        let Some((function, arity)) = parse_def(line) else {
            continue;
        };
        if !targets.iter().any(|target| target.function == function) {
            targets.push(ProbeTarget {
                file: String::new(),
                function,
                arity,
            });
        }
    }
    if targets.is_empty() {
        return None;
    }
    targets.truncate(MAX_PROBE_FUNCTIONS);

    // Staged beside the repository so anything it imports resolves the same
    // way it will once saved, and removed afterwards — a round must not
    // leave files in a directory the user did not ask it to write to.
    let draft = working_dir.join(format!("committee-draft-{}.py", Uuid::new_v4()));
    std::fs::write(&draft, source).ok()?;
    let script = behavior_probe_script(&draft.to_string_lossy(), &targets, calls);
    let execution = run_proof_script(&script, PROOF_TIMEOUT_SECS, working_dir, path).await;
    let _ = std::fs::remove_file(&draft);

    let observations = execution.stdout.trim();
    (!observations.is_empty()).then(|| observations.to_string())
}

/// Whether a revised draft lost ground against the one it replaced.
///
/// A rewrite round asked a 7B to fix one named defect. It fixed exactly that
/// defect and broke five other cases doing it: a draft scoring 7 of 8 came
/// back scoring 3, with `wrap('hello world', 5)` — which had worked —
/// returning nothing at all. Sharpening the note made the targeted fix land
/// AND made the collateral damage worse, because a model this size rewrites
/// the function around the case it was shown.
///
/// So a round may not lose ground: a call that answered before and raises
/// now, or returned something and returns nothing now, is behaviour going
/// backwards and the revision is dropped.
///
/// Which calls it compares on matters as much as the rule. Run against the
/// generic battery alone it caught one revision and let another through
/// that had destroyed word-joining entirely — `wrap('a b c', 3)` coming
/// back as `['a', 'b', 'c']` — because nothing in a battery of zeros, empty
/// lists and two-character strings ever joins two words. The calls have to
/// look like the task.
pub(crate) fn lost_ground(before: &str, after: &str) -> Option<String> {
    let answers = |report: &str| {
        report
            .lines()
            .filter_map(|line| line.trim().split_once(" -> "))
            .map(|(call, result)| (call.trim().to_string(), result.trim().to_string()))
            .collect::<Vec<_>>()
    };
    let now = answers(after);
    for (call, was) in answers(before) {
        // Nothing to compare if the call has stopped being made at all,
        // which happens when the revision renamed or dropped a function —
        // itself a loss, and caught by the same rule.
        let Some((_, is)) = now.iter().find(|(again, _)| again == &call) else {
            return Some(format!(
                "{call} used to answer {was} and is not there any more"
            ));
        };
        let emptied = matches!(is.as_str(), "[]" | "{}" | "''" | "\"\"" | "None" | "0")
            && !matches!(was.as_str(), "[]" | "{}" | "''" | "\"\"" | "None" | "0");
        if emptied {
            return Some(format!("{call} used to answer {was} and now answers {is}"));
        }
    }
    None
}

/// Whether a round moved anything at all.
///
/// Measured over three paired runs, a notes round rewrote the panel's answer
/// into a *different program that scored exactly the same* — same failing
/// case, nothing else broken — and did it deterministically, three times,
/// while more than doubling the run. The loop had no way to notice, because
/// nothing in it ever compared where it started with where it got to.
///
/// A round that changed no observable behaviour has finished, whatever it
/// did to the text. Saying so is worth more than another round of it.
pub(crate) fn round_changed_nothing(
    before: &[(String, String)],
    after: &[(String, String)],
) -> bool {
    if before.len() != after.len() || before.is_empty() {
        return false;
    }
    before
        .iter()
        .zip(after)
        .all(|((_, was), (_, is))| was == is)
}

/// Whether an execution amounts to "the defect is real", by the same rule
/// the proof round uses: the comparison it printed decides, and a bare
/// exit 0 with nothing compared decides nothing.
pub(crate) fn demonstrates(execution: &ShellExecution) -> bool {
    match self_check(&execution.stdout, execution.exit_code) {
        SelfCheck::Compared { differ, .. } => differ,
        SelfCheck::Unstated => false,
    }
}

/// Runs the same proof script against the committed version of the
/// repository, so the harness can ask the one question that decides a
/// review: **did this change cause it?**
///
/// A defect the diff introduced is a script that demonstrates on the working
/// tree and does not demonstrate on `HEAD`. If it demonstrates on both, the
/// behaviour predates the change and the finding is not about the diff — and
/// far more usefully, if the script's claim was invented, it will invent
/// identically against the old code, so it says the same thing twice and
/// certifies nothing.
///
/// Measured: run three times against a refactor with nothing wrong with it,
/// the committee produced the same fabricated proof every time — `expected
/// {'a': 1, 'b': 2}` against a docstring describing string values. The
/// refactor is behaviour-preserving, so that script says exactly the same
/// thing about the committed code, and this is what notices.
///
/// `None` when there is no committed tree to compare against — an untracked
/// directory, or a git that will not archive — in which case the run keeps
/// the verdict it already had rather than inventing a reason to doubt it.
pub(crate) async fn run_against_head(
    code: &str,
    timeout_secs: u64,
    working_dir: &Path,
    path: &str,
) -> Option<ShellExecution> {
    let older = std::env::temp_dir().join(format!("committee-head-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&older).ok()?;
    // The whole committed tree, so anything the changed files import is
    // there too and resolves the way it did before the change.
    let unpack = execute_shell(
        &format!("git archive HEAD | tar -x -C {}", shell_quote(&older)),
        working_dir,
        path,
    )
    .await;
    if unpack.exit_code != 0 {
        let _ = std::fs::remove_dir_all(&older);
        return None;
    }
    let execution = run_proof_script(code, timeout_secs, &older, path).await;
    let _ = std::fs::remove_dir_all(&older);
    Some(execution)
}

/// The call an oracle mismatch is about, so a prover can be pointed at it.
pub(crate) fn mismatch_call(line: &str) -> Option<&str> {
    let call = line.split(" returned ").next()?;
    call.contains('(').then_some(call)
}

/// The behaviour report with every result cut off, leaving only the calls.
///
/// The report is the right thing to show a prover and the wrong thing to
/// show it whole. Given the full report, every prover in a measured run
/// picked a good input out of it — and then copied the observed output into
/// its `expected:` line, which is the bug written down as the specification.
/// Three defects, three refutations, `expected None, got None`.
///
/// So it gets the calls and not the answers. Where the input comes from and
/// where the expectation comes from have to be two different places, or the
/// comparison is between a value and itself.
pub(crate) fn calls_only(report: &str) -> String {
    let mut calls = String::new();
    for line in report.lines() {
        let line = line.trim();
        // "name(args) -> result", "name(args) !! Error", and the repeat
        // marker, which is worth keeping *as* a hint about method.
        let call = match line.find(" a second time") {
            Some(cut) => format!(
                "{} — and again, to see if it answers the same",
                &line[..cut]
            ),
            None => match line.find(" -> ").or_else(|| line.find(" !! ")) {
                Some(cut) => line[..cut].to_string(),
                None => continue,
            },
        };
        if !call.contains('(') {
            continue;
        }
        calls.push_str(&call);
        calls.push('\n');
    }
    calls
}

/// The signature and docstring of whichever changed function a claim is
/// about — the thing that says what "correct" would have been.
///
/// A prover needs this more than anything else it gets, and kept not
/// finding it. Handed a diff containing
///
/// ```text
///  def needs_reorder(stock, threshold):
///      """True once stock has fallen to the reorder threshold."""
/// -    return stock <= threshold
/// +    return stock < threshold
/// ```
///
/// it wrote `expected = False` for `needs_reorder(0, 0)` — copying what the
/// code does into the place where the specification goes, so of course the
/// two matched and of course the defect was refuted. The contract was two
/// lines above the change and went unread, so now it arrives on its own.
pub(crate) fn contract_for(diff: &str, subject: &str) -> Option<String> {
    let subject = subject.to_ascii_lowercase();
    contracts_in(diff)
        .into_iter()
        // The claim has to actually be about this function. Word-bounded, so
        // a title saying "restock" doesn't match a `stock` function.
        .find(|(name, _)| {
            subject
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|word| word == name)
        })
        .map(|(_, block)| block)
}

/// Every changed function's signature and docstring, by name. Lowercased
/// names, because everything that matches against them is a model's prose.
pub(crate) fn contracts_in(diff: &str) -> Vec<(String, String)> {
    let mut contracts: Vec<(String, String)> = Vec::new();
    let mut lines = diff.lines().peekable();
    while let Some(line) = lines.next() {
        // A signature reaches the diff two ways: as a line of its own, or —
        // for a function whose body changed under it — as the context git
        // writes after a hunk's `@@`.
        let content = match line.strip_prefix("@@") {
            Some(header) => match header.split_once("@@") {
                Some((_, context)) => context.trim(),
                None => continue,
            },
            None => match line.strip_prefix('+').or_else(|| line.strip_prefix(' ')) {
                Some(content) => content,
                None => continue,
            },
        };
        let Some((name, _)) = parse_def(content) else {
            continue;
        };
        let name = name.to_ascii_lowercase();
        if contracts.iter().any(|(known, _)| known == &name) {
            continue;
        }
        let mut block = content.to_string();
        if let Some(next) = lines.peek() {
            let doc = next
                .strip_prefix('+')
                .or_else(|| next.strip_prefix(' '))
                .unwrap_or("")
                .trim();
            if doc.starts_with("\"\"\"") || doc.starts_with('\'') {
                block.push('\n');
                block.push_str(doc);
            }
        }
        contracts.push((name, block));
    }
    contracts
}

/// The python files the diff imports but does not change, summarised.
///
/// A diff is not enough to review a diff. The `orders` fixture makes the
/// point: one file formats pence with a pounds sign, another hands a
/// percentage to something that wants a fraction, and *neither changed file
/// is wrong on its own reading*. What makes them wrong is `pricing.py` —
/// "money is pence, always pence", "a fifth off is 0.2" — which doesn't
/// change and so never appears in `git diff HEAD`. Reviewers shown only the
/// diff cannot pass that review, and no amount of prompting fixes it,
/// because the information is not in the room.
///
/// Signatures and their first docstring line only. The contract is what's
/// needed; the bodies would crowd out the diff itself.
pub(crate) fn imported_context(diff: &str, working_dir: &Path) -> Option<String> {
    let mut changed: Vec<String> = Vec::new();
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            changed.push(path.to_string());
        }
    }
    // Read the imports out of the changed FILES, not out of the diff.
    //
    // Imports live at the top of a file and a hunk is three lines of
    // context, so they are almost never in it. The `orders` fixture proved
    // the point: its whole reason for existing is that `cart.py` and
    // `invoice.py` both `import pricing`, and in the diff git actually
    // produces, one of those lines appears only inside a `@@` header and the
    // other does not appear at all. The feature was inert on the exact case
    // it was written for. The diff says which files changed; the files say
    // what they import.
    let mut modules: Vec<String> = Vec::new();
    for file in &changed {
        let Ok(source) = std::fs::read_to_string(working_dir.join(file)) else {
            continue;
        };
        for line in source.lines() {
            // Top-level only: an indented import is inside a function and
            // usually a fallback. A dotted path is a package, which is more
            // than a summary can usefully carry.
            let module = line
                .strip_prefix("import ")
                .or_else(|| line.strip_prefix("from "))
                .map(|rest| {
                    rest.split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim_end_matches(',')
                })
                .filter(|module| !module.is_empty() && !module.contains('.'));
            let Some(module) = module else { continue };
            if changed
                .iter()
                .any(|path| path.ends_with(&format!("{module}.py")))
            {
                continue;
            }
            if !modules.iter().any(|known| known == module) {
                modules.push(module.to_string());
            }
        }
    }

    let mut summary = String::new();
    for module in modules {
        let Ok(source) = std::fs::read_to_string(working_dir.join(format!("{module}.py"))) else {
            continue;
        };
        let mut lines = source.lines().peekable();
        let mut block = String::new();
        while let Some(line) = lines.next() {
            if !line.starts_with("def ") {
                continue;
            }
            block.push_str(&format!("{line}\n"));
            // The docstring's first line is the contract, when there is one.
            if let Some(doc) = lines.peek() {
                let doc = doc.trim();
                if let Some(text) = doc
                    .strip_prefix("\"\"\"")
                    .or_else(|| doc.strip_prefix('\''))
                {
                    let text = text
                        .trim_end_matches("\"\"\"")
                        .trim_end_matches('\'')
                        .trim();
                    if !text.is_empty() {
                        block.push_str(&format!("    {text}\n"));
                    }
                }
            }
        }
        if block.is_empty() {
            continue;
        }
        summary.push_str(&format!("{module}.py (unchanged):\n{block}\n"));
    }
    if summary.is_empty() {
        return None;
    }
    if summary.len() > MAX_IMPORTED_CHARS {
        let mut cut = MAX_IMPORTED_CHARS;
        while !summary.is_char_boundary(cut) {
            cut -= 1;
        }
        summary.truncate(cut);
        summary.push_str("…\n");
    }
    Some(summary)
}

/// Every top-level function of the diff's python files that the change
/// actually reaches.
///
/// Not just the ones whose `def` line moved. Two runs against the stock
/// fixture probed `restock` and nothing else, because only `restock` had its
/// signature edited — while the two defects that kept being missed were in
/// `cheapest` and `needs_reorder`, whose bodies changed under an untouched
/// signature. A function whose insides changed is exactly a function worth
/// running.
///
/// Both places a still-standing signature shows up count: git's hunk header
/// names the function a hunk starts inside, and a def further down the hunk
/// arrives as an ordinary context line.
pub(crate) fn parse_probe_targets(diff: &str) -> Vec<ProbeTarget> {
    let mut targets: Vec<ProbeTarget> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut enclosing: Option<(String, usize)> = None;

    let mut touch = |file: &str, enclosing: &Option<(String, usize)>| {
        let Some((name, arity)) = enclosing else {
            return;
        };
        if targets
            .iter()
            .any(|target| &target.function == name && target.file == file)
        {
            return;
        }
        targets.push(ProbeTarget {
            file: file.to_string(),
            function: name.clone(),
            arity: *arity,
        });
    };

    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            current_file = path.ends_with(".py").then(|| path.to_string());
            enclosing = None;
            continue;
        }
        let Some(file) = current_file.clone() else {
            continue;
        };
        if let Some(header) = line.strip_prefix("@@") {
            // `@@ -8,19 +8,17 @@ def cheapest(prices):` — git says which
            // function the hunk opened inside, for free.
            enclosing = header
                .split_once("@@")
                .and_then(|(_, context)| parse_def(context.trim()));
            continue;
        }
        // Removed lines describe the old file; a def among them may not
        // exist any more, so they never set the target — only mark the
        // enclosing function as touched.
        let content = if let Some(rest) = line.strip_prefix('+') {
            rest
        } else if let Some(rest) = line.strip_prefix(' ') {
            rest
        } else if line.starts_with('-') {
            touch(&file, &enclosing);
            continue;
        } else {
            continue;
        };
        if !content.starts_with("def ") {
            if line.starts_with('+') {
                touch(&file, &enclosing);
            }
            continue;
        }
        enclosing = parse_def(content);
        // An added signature is a new function, which is worth probing on
        // its own account.
        if line.starts_with('+') {
            touch(&file, &enclosing);
        }
    }
    targets.truncate(MAX_PROBE_FUNCTIONS);
    targets
}

/// The generic input battery per arity, as python argument lists. Broad on
/// purpose: empty things, zero, negatives, small collections, strings —
/// the situations code forgets.
///
/// Two entries are here because runs kept missing the defects they catch.
/// `[0, 5]` is the list whose smallest member is falsy, which is how
/// `if not best` gets away with it; `3, 3` is the pair that makes `<` and
/// `<=` disagree, which is every off-by-one on a threshold. A prover left to
/// invent its own inputs reaches for the empty list and for arguments that
/// differ, and neither says anything about either defect.
pub(crate) fn probe_combos(arity: usize) -> &'static [&'static str] {
    match arity {
        0 => &[""],
        1 => &["0", "7", "-1", "[]", "[3, 1, 2]", "[0, 5]", "None"],
        2 => &[
            "0, []",
            "100, [1, 2, 3]",
            "[3, 1, 2], 2",
            "[], 3",
            "'ab', 1",
            "7, 0",
            "3, 3",
            "0, 0",
            // A string with a space in it. Without one, anything that joins
            // or splits words is indistinguishable from anything else that
            // does: `wrap('ab', 1)` and `text.split()` agree exactly, so a
            // rewrite that stopped wrapping altogether passed a guard whose
            // whole job was to catch it.
            "'a b c', 3",
            "'one two three', 7",
        ],
        // `'Tea', 250, 5` is the name/unit-price/quantity shape that turns up
        // everywhere, and it is the one that makes a units mistake obvious:
        // a function documented to render pounds answering £1250.00 has said
        // what is wrong with it.
        3 => &["1, 2, 3", "[], [], []", "'ab', 1, []", "'Tea', 250, 5"],
        _ => &[],
    }
}

/// The probe script for one file: load the module, call each target with
/// its battery, print one line per observation. Always exits 0 — the report
/// is the stdout, not the exit code.
///
/// Every call is made twice. A function that answers differently the second
/// time is holding state between calls, which no amount of reading the diff
/// reliably shows and which the interpreter settles in one line. Once a
/// function has been caught doing it the rest of its battery is dropped:
/// its later answers describe the state it has accumulated rather than the
/// arguments it was given, and they crowd out everything else in a report
/// that has to fit in the models' context.
pub(crate) fn behavior_probe_script(
    absolute_file: &str,
    targets: &[ProbeTarget],
    proposed: &[String],
) -> String {
    // TypeError and AttributeError are almost always the battery's own
    // fault — it deliberately passes an int where a list goes — and showing
    // them costs more than it pays. One run printed
    // `restock(1, 2, 3) !! AttributeError: 'int' object has no attribute
    // 'append'`, a reviewer copied it out as a finding, a prover reproduced
    // it exactly, and the judge confirmed a defect that was nothing but the
    // probe calling the function wrongly. Errors about values — dividing by
    // zero, indexing off the end — still earn their line.
    let mut script = format!(
        "import importlib.util\n\
         spec = importlib.util.spec_from_file_location('probe_target', {absolute_file:?})\n\
         module = importlib.util.module_from_spec(spec)\n\
         try:\n\
         \x20   spec.loader.exec_module(module)\n\
         except Exception as error:\n\
         \x20   print(f'module load failed: {{type(error).__name__}}: {{error}}')\n\
         \x20   raise SystemExit(0)\n\
         MISCALLED = (TypeError, AttributeError)\n\
         repeats = set()\n\
         def probe(name, *args):\n\
         \x20   fn = getattr(module, name, None)\n\
         \x20   if not callable(fn):\n\
         \x20       print(f'{{name}}: not found')\n\
         \x20       return\n\
         \x20   if name in repeats:\n\
         \x20       return\n\
         \x20   shown = ', '.join(repr(a) for a in args)\n\
         \x20   try:\n\
         \x20       result = fn(*args)\n\
         \x20   except MISCALLED:\n\
         \x20       return\n\
         \x20   except Exception as error:\n\
         \x20       print(f'{{name}}({{shown}}) !! {{type(error).__name__}}: {{error}}'[:200])\n\
         \x20       return\n\
         \x20   first = repr(result)\n\
         \x20   print(f'{{name}}({{shown}}) -> {{first}}'[:200])\n\
         \x20   try:\n\
         \x20       again = repr(fn(*args))\n\
         \x20   except Exception:\n\
         \x20       return\n\
         \x20   if again != first:\n\
         \x20       repeats.add(name)\n\
         \x20       print(f'{{name}}({{shown}}) a second time -> {{again}}'[:200])\n"
    );
    // A whole call as written, for the ones a model proposed: the generic
    // battery cannot build `[('Tea', 250, 5)]` and a docstring saying so
    // makes it obvious.
    script.push_str(
        "def probe_call(text):\n\
         \x20   try:\n\
         \x20       result = eval(text, vars(module))\n\
         \x20   except MISCALLED:\n\
         \x20       return\n\
         \x20   except Exception as error:\n\
         \x20       print(f'{text} !! {type(error).__name__}: {error}'[:200])\n\
         \x20       return\n\
         \x20   first = repr(result)\n\
         \x20   print(f'{text} -> {first}'[:200])\n",
    );
    for target in targets {
        for combo in probe_combos(target.arity) {
            if combo.is_empty() {
                script.push_str(&format!("probe('{}')\n", target.function));
            } else {
                script.push_str(&format!("probe('{}', {combo})\n", target.function));
            }
        }
    }
    for call in proposed {
        script.push_str(&format!("probe_call({call:?})\n"));
    }
    script.push_str("raise SystemExit(0)\n");
    script
}

/// Probes every changed python file's functions and returns the combined
/// report, or `None` when the diff has nothing probeable. Runs under the
/// proof timeout — a module whose import hangs is skipped, not fatal.
pub(crate) async fn run_behavior_probe(
    diff: &str,
    proposed: &[String],
    working_dir: &Path,
    path: &str,
) -> Option<String> {
    let targets = parse_probe_targets(diff);
    if targets.is_empty() {
        return None;
    }
    let mut files: Vec<String> = Vec::new();
    for target in &targets {
        if !files.contains(&target.file) {
            files.push(target.file.clone());
        }
    }
    files.truncate(MAX_PROBE_FILES);

    let mut report = String::new();
    for file in &files {
        let file_targets: Vec<ProbeTarget> = targets
            .iter()
            .filter(|target| &target.file == file)
            .cloned()
            .collect();
        let absolute = working_dir.join(file);
        // A proposed call only belongs with the file that defines it.
        let mine: Vec<String> = proposed
            .iter()
            .filter(|call| {
                call.split('(')
                    .next()
                    .is_some_and(|name| file_targets.iter().any(|target| target.function == name))
            })
            .cloned()
            .collect();
        let script = behavior_probe_script(&absolute.to_string_lossy(), &file_targets, &mine);
        let execution = run_proof_script(&script, PROOF_TIMEOUT_SECS, working_dir, path).await;
        let observations = execution.stdout.trim();
        if observations.is_empty() {
            continue;
        }
        report.push_str(&format!("{file}:\n{observations}\n"));
    }
    if report.len() > MAX_REPORT_CHARS {
        let mut cut = MAX_REPORT_CHARS;
        while !report.is_char_boundary(cut) {
            cut -= 1;
        }
        report.truncate(cut);
        report.push_str("\n[report truncated]");
    }
    let report = report.trim().to_string();
    (!report.is_empty()).then_some(report)
}

#[cfg(test)]
#[path = "ollama_committee_proof_tests.rs"]
mod tests;
