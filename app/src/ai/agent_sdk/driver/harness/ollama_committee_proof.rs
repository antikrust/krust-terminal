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
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SelfCheck {
    /// Said what it expected and what it got, and its exit code agrees.
    Consistent,
    /// Said both, and its exit code says the opposite of what they say.
    Contradicted(String),
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

/// Checks a script's exit code against the comparison it printed. Only an
/// outright contradiction is called one — a script that printed neither line
/// is [`SelfCheck::Unstated`], which the caller weighs by what it claimed.
pub(crate) fn self_check(stdout: &str, exit_code: i32) -> SelfCheck {
    let (Some(expected), Some(actual)) = (reported(stdout, "expected"), reported(stdout, "actual"))
    else {
        return SelfCheck::Unstated;
    };
    let same = expected.eq_ignore_ascii_case(&actual);
    match (exit_code, same) {
        (0, true) => SelfCheck::Contradicted(format!(
            "the script claimed the defect is real but reported the same value twice \
             (expected {expected}, actual {actual}) — that is the code behaving correctly"
        )),
        (1, false) => SelfCheck::Contradicted(format!(
            "the script claimed the code is fine but reported expected {expected} and actual \
             {actual} — those differ, which is the defect"
        )),
        _ => SelfCheck::Consistent,
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
    let proof = stage_dir.join(format!("derp-committee-proof-{id}.py"));
    let wrapper = stage_dir.join(format!("derp-committee-wrap-{id}.py"));
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
    let tests = stage_dir.join(format!("derp-committee-tests-{id}.py"));
    let runner = stage_dir.join(format!("derp-committee-runner-{id}.py"));
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
    let wrapper = std::env::temp_dir().join(format!("derp-committee-suite-{}.py", Uuid::new_v4()));
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
    let mut modules: Vec<String> = Vec::new();
    for line in diff.lines() {
        let content = match line.strip_prefix('+').or_else(|| line.strip_prefix(' ')) {
            Some(content) => content,
            None => continue,
        };
        // Top-level `import x` and `from x import y`. A dotted path is a
        // package, which is more than a summary can usefully carry.
        let module = content
            .strip_prefix("import ")
            .or_else(|| content.strip_prefix("from "))
            .map(|rest| rest.split_whitespace().next().unwrap_or("").trim_end_matches(','))
            .filter(|module| !module.is_empty() && !module.contains('.'));
        let Some(module) = module else { continue };
        let file = format!("{module}.py");
        if changed.iter().any(|path| path.ends_with(&file)) {
            continue;
        }
        if !modules.iter().any(|known| known == module) {
            modules.push(module.to_string());
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
                if let Some(text) = doc.strip_prefix("\"\"\"").or_else(|| doc.strip_prefix('\'')) {
                    let text = text.trim_end_matches("\"\"\"").trim_end_matches('\'').trim();
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
        1 => &[
            "0",
            "7",
            "-1",
            "[]",
            "[3, 1, 2]",
            "[0, 5]",
            "''",
            "'ab'",
            "None",
        ],
        2 => &[
            "0, []",
            "100, [1, 2, 3]",
            "[3, 1, 2], 2",
            "[], 3",
            "'ab', 1",
            "7, 0",
            "3, 3",
            "0, 0",
        ],
        3 => &["1, 2, 3", "[], [], []", "'ab', 1, []"],
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
pub(crate) fn behavior_probe_script(absolute_file: &str, targets: &[ProbeTarget]) -> String {
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
    for target in targets {
        for combo in probe_combos(target.arity) {
            if combo.is_empty() {
                script.push_str(&format!("probe('{}')\n", target.function));
            } else {
                script.push_str(&format!("probe('{}', {combo})\n", target.function));
            }
        }
    }
    script.push_str("raise SystemExit(0)\n");
    script
}

/// Probes every changed python file's functions and returns the combined
/// report, or `None` when the diff has nothing probeable. Runs under the
/// proof timeout — a module whose import hangs is skipped, not fatal.
pub(crate) async fn run_behavior_probe(
    diff: &str,
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
        let script = behavior_probe_script(&absolute.to_string_lossy(), &file_targets);
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
