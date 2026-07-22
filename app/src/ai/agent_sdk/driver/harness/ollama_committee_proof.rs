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
    pub(crate) arity: usize,
}

/// Free functions defined on added lines of the diff's python files —
/// single-line `def` signatures only, methods and underscore-private
/// functions skipped (no instance to call them on, no contract to check).
pub(crate) fn parse_probe_targets(diff: &str) -> Vec<ProbeTarget> {
    let mut targets: Vec<ProbeTarget> = Vec::new();
    let mut current_file: Option<String> = None;
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            current_file = path.ends_with(".py").then(|| path.to_string());
            continue;
        }
        let Some(file) = &current_file else { continue };
        // Top-level defs only: an added line, no indentation before `def`.
        let Some(rest) = line.strip_prefix("+def ") else {
            continue;
        };
        let Some((name, rest)) = rest.split_once('(') else {
            continue;
        };
        let Some((params, _)) = rest.split_once(')') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || name.starts_with('_') {
            continue;
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
            continue;
        }
        if targets.iter().any(|target| target.function == name) {
            continue;
        }
        targets.push(ProbeTarget {
            file: file.clone(),
            function: name.to_string(),
            arity: params.len(),
        });
    }
    targets.truncate(MAX_PROBE_FUNCTIONS);
    targets
}

/// The generic input battery per arity, as python argument lists. Broad on
/// purpose: empty things, zero, negatives, small collections, strings —
/// the situations code forgets.
pub(crate) fn probe_combos(arity: usize) -> &'static [&'static str] {
    match arity {
        0 => &[""],
        1 => &["0", "7", "-1", "[]", "[3, 1, 2]", "''", "'ab'", "None"],
        2 => &[
            "0, []",
            "100, [1, 2, 3]",
            "[3, 1, 2], 2",
            "[], 3",
            "'ab', 1",
            "7, 0",
        ],
        3 => &["1, 2, 3", "[], [], []"],
        _ => &[],
    }
}

/// The probe script for one file: load the module, call each target with
/// its battery, print one line per observation. Always exits 0 — the report
/// is the stdout, not the exit code.
pub(crate) fn behavior_probe_script(absolute_file: &str, targets: &[ProbeTarget]) -> String {
    let mut script = format!(
        "import importlib.util\n\
         spec = importlib.util.spec_from_file_location('probe_target', {absolute_file:?})\n\
         module = importlib.util.module_from_spec(spec)\n\
         try:\n\
         \x20   spec.loader.exec_module(module)\n\
         except Exception as error:\n\
         \x20   print(f'module load failed: {{type(error).__name__}}: {{error}}')\n\
         \x20   raise SystemExit(0)\n\
         def probe(name, *args):\n\
         \x20   fn = getattr(module, name, None)\n\
         \x20   if not callable(fn):\n\
         \x20       print(f'{{name}}: not found')\n\
         \x20       return\n\
         \x20   shown = ', '.join(repr(a) for a in args)\n\
         \x20   try:\n\
         \x20       result = fn(*args)\n\
         \x20       print(f'{{name}}({{shown}}) -> {{result!r}}'[:200])\n\
         \x20   except Exception as error:\n\
         \x20       print(f'{{name}}({{shown}}) !! {{type(error).__name__}}: {{error}}'[:200])\n"
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
