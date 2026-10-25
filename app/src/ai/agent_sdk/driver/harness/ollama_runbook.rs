//! Runbook mode: a local model works through a written operating procedure one
//! step at a time, with the procedure shown as the conversation's checklist.
//!
//! A runbook is a markdown file whose `## Step …` sections are the steps, each
//! carrying its commands in fenced blocks and its success criteria in prose
//! ("must print false", "if not, STOP"). Handing a 7–14B model the whole
//! document plus a merge log and a diff stat does not work: it loses the thread
//! by the third step and then reports success at a gate it never checked. So
//! the model never holds the runbook — this driver does:
//!
//! - it parses the steps and shows them as the plan checklist before anything
//!   runs;
//! - for each step, a **worker** model gets a fresh conversation holding only
//!   the runbook's rules, that one step, and the values earlier steps found;
//! - the worker runs the step's fenced blocks through `run_block`, which
//!   executes a block *verbatim*: small models mistype long git plumbing, so
//!   they choose which block runs and supply values, but never retype commands;
//! - values a block assigns in UPPERCASE (`V=158.0`) are read back after it
//!   runs and exported into every later command, so "run each line in the same
//!   shell" holds across steps even though every call is a fresh shell;
//! - a **judge** model then reads the step's criteria against the actual
//!   output and rules pass, skip, done or stop. The worker's own account of
//!   what happened is never what advances the plan.
//!
//! Runs are fenced to a sandbox. In a git repository, every push remote must be
//! a local path, so nothing the procedure pushes can leave the machine; and a
//! command that would `cd` to, or recursively delete, an absolute path outside
//! the target directory is refused before it runs.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ai::agent::{PlanItem, PlanUpdate, ToolCallOutcome, TurnEvent, TurnUsage};
use futures::future::Either;
use krustui::r#async::Timer;
use serde_json::{json, Value};
use uuid::Uuid;

use super::claude_stream_exec::{
    finish_harness_session, harness_cancel_for, register_harness_session,
    run_jsonl_stream_cancellable, HarnessCancel,
};
use super::ollama_committee::{fetch_installed_model_names, success_status};
use super::ollama_stream_exec::{
    base_url_of, builtin_local_tool_specs, dispatch_builtin_tool, emit_failed_turn,
    ensure_ollama_service, execute_shell, new_ollama_chat_command, parse_text_tool_call,
    probe_model_caps_checked, wire_name, McpToolSpec, OllamaChatAccumulator, OllamaChatResult,
    OllamaToolCall, ShellExecution,
};
use super::ollama_work_notes::{WorkNotes, LARGE_TEXT_CHARS};

/// Model calls a worker may make on one step before the step is called off.
/// Repeats are caught on their own; this is for a worker that wanders — a
/// six-block step with a few looks at files ran out at 16.
const MAX_WORKER_CALLS: usize = 24;
/// Context window asked of both models: the runbook's rules, one step and a
/// handful of command outputs.
const RUNBOOK_NUM_CTX: u32 = 16_384;
/// How long a model stays loaded between calls. Worker and judge alternate on
/// every step, so unloading either after each call would reload it each time.
const KEEP_ALIVE: &str = "10m";
/// What a model sees of one command's output: the head for context, and a
/// longer tail, because that is where errors land.
const OUTPUT_HEAD_CHARS: usize = 1_200;
const OUTPUT_TAIL_CHARS: usize = 2_400;
/// The most a model may generate in one call, by job. Without a cap a model
/// can loop — a thinking model reasoning in circles held one reading call for
/// half an hour — and the run waits forever. Thinking counts against the cap.
const WORKER_MAX_TOKENS: u32 = 2_048;
const JUDGE_MAX_TOKENS: u32 = 8_192;
const ANALYST_MAX_TOKENS: u32 = 6_144;
const REPORT_MAX_TOKENS: u32 = 4_096;
pub(crate) const SHORT_REPLY_MAX_TOKENS: u32 = 1_024;
/// The output cap of a condition check (see `check_case`), room for a
/// thinking judge to think.
const CONDITION_MAX_TOKENS: u32 = 4_096;
/// Judges tried, in order, when none is chosen: reasoning models that fit
/// beside a 14B worker on a 24GB machine.
const JUDGE_PREFERENCE: &[&str] = &[
    "deepseek-r1:14b",
    "qwen3:14b",
    "phi4:14b",
    "qwen3.5:9b",
    "qwen3:8b",
    "deepseek-r1:8b",
];
/// Helpers tried, in order, when none is chosen: small, quick models that fit
/// beside a 14B judge, for checking one proposed fix at a time.
const HELPER_PREFERENCE: &[&str] = &[
    "qwen3.5:9b",
    "qwen2.5-coder:7b",
    "qwen3:8b",
    "granite4:7b-a1b-h",
    "llama3.1:8b",
];
/// Separates a block's own output from the values it reports back.
const FACTS_MARKER: &str = "__RUNBOOK_FACTS__";
const RUN_BLOCK: &str = "run_block";
const SHELL: &str = "shell";
const BUILTIN_RUN_CODE: &str = "run_code";
const BUILTIN_WRITE_FILE: &str = "write_file";
const BUILTIN_REPLACE_LINES: &str = "replace_lines";
const BUILTIN_TODO_LIST: &str = "todo_list";

/// Named values a run has established, e.g. `V=158.0`. UPPERCASE names only:
/// that is how the runbooks spell their parameters, and it keeps a block's
/// lowercase locals out of every later command.
pub(crate) type Facts = BTreeMap<String, String>;

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// Runbook mode's knobs, stored as one JSON string setting so later versions
/// can add fields without a migration (the committee's pattern).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RunbookConfig {
    /// The judge's model tag; `None` picks one automatically.
    pub(crate) judge: Option<String>,
    /// The helper's model tag — the small model that checks each fix in the
    /// work-file pass; `None` picks one automatically.
    pub(crate) helper: Option<String>,
}

impl RunbookConfig {
    pub(crate) fn from_settings_json(json: Option<&str>) -> Self {
        let value = json.and_then(|json| serde_json::from_str::<Value>(json).ok());
        let choice = |name: &str| {
            value.as_ref().and_then(|value| {
                value
                    .get(name)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|choice| !choice.is_empty() && *choice != "auto")
                    .map(str::to_owned)
            })
        };
        Self {
            judge: choice("judge"),
            helper: choice("helper"),
        }
    }

    pub(crate) fn to_settings_json(&self) -> String {
        json!({
            "judge": self.judge.as_deref().unwrap_or("auto"),
            "helper": self.helper.as_deref().unwrap_or("auto"),
        })
        .to_string()
    }
}

/// Steps a model choice through `auto` and then each installed model, for the
/// settings page's cycling links.
fn cycle_choice(choice: &mut Option<String>, installed: &[String]) {
    let next = match choice {
        None => installed.first().cloned(),
        Some(current) => installed
            .iter()
            .position(|tag| tag == current)
            .and_then(|index| installed.get(index + 1))
            .cloned(),
    };
    *choice = next;
}

pub(crate) fn cycle_judge(config: &mut RunbookConfig, installed: &[String]) {
    cycle_choice(&mut config.judge, installed);
}

pub(crate) fn cycle_helper(config: &mut RunbookConfig, installed: &[String]) {
    cycle_choice(&mut config.helper, installed);
}

/// The first model of `preference` installed and switched on, else `fallback`.
fn pick_preferred(
    preference: &[&str],
    installed: &[String],
    fallback: &str,
    disabled: &[String],
) -> String {
    preference
        .iter()
        .find(|preferred| {
            installed.iter().any(|tag| tag == *preferred)
                && !disabled.iter().any(|off| off == *preferred)
        })
        .map(|preferred| preferred.to_string())
        .unwrap_or_else(|| fallback.to_string())
}

/// The judge to use when none is chosen: the first preferred reasoning model
/// installed and switched on, else the worker itself.
pub(crate) fn pick_judge(installed: &[String], worker: &str, disabled: &[String]) -> String {
    pick_preferred(JUDGE_PREFERENCE, installed, worker, disabled)
}

/// The helper to use when none is chosen: the first preferred small model
/// installed and switched on, else the worker itself.
pub(crate) fn pick_helper(installed: &[String], worker: &str, disabled: &[String]) -> String {
    pick_preferred(HELPER_PREFERENCE, installed, worker, disabled)
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// A runbook, as parsed from markdown.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Runbook {
    pub(crate) title: String,
    /// What every worker reads before its step: the introduction and the
    /// runbook's other non-step sections (background, hard rules).
    pub(crate) preamble: String,
    pub(crate) steps: Vec<RunbookStep>,
    /// The runbook's own reporting template, if it has one.
    pub(crate) report_template: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RunbookStep {
    /// Stable id for the checklist, e.g. `step-0`, `step-b`.
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) blocks: Vec<RunbookBlock>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RunbookBlock {
    pub(crate) lang: String,
    pub(crate) code: String,
    /// The paragraph just before the block, which says when it runs ("Then
    /// check that …", "Otherwise … push:", "A count in the hundreds means …
    /// STOP:").
    pub(crate) lead_in: String,
}

impl RunbookBlock {
    /// Shell blocks run; anything else (a config sample, a template) is shown
    /// to the worker but cannot be executed.
    pub(crate) fn runnable(&self) -> bool {
        matches!(
            self.lang.as_str(),
            "" | "bash" | "sh" | "zsh" | "shell" | "console"
        )
    }

    /// A `resolve` block runs the work-file pass over the merge conflicts
    /// (see `conflicts`) instead of a shell.
    pub(crate) fn is_resolve(&self) -> bool {
        self.lang == "resolve"
    }

    /// Whether `run_block` runs this block.
    pub(crate) fn offered(&self) -> bool {
        self.runnable() || self.is_resolve()
    }

    /// Whether the lead-in opens with the case the block runs in — "If …",
    /// "When …", "Otherwise …", "Unless …", "A count in the hundreds means
    /// …" — so whether to run it is a question the output so far answers.
    pub(crate) fn has_condition(&self) -> bool {
        let lead_in = self
            .lead_in
            .trim_start()
            .trim_start_matches(['*', '_', '>', ' '])
            .to_ascii_lowercase();
        let opening = lead_in.split([':', ';']).next().unwrap_or_default();
        [
            "if ",
            "when ",
            "otherwise",
            "unless ",
            "only if ",
            "in that case",
            "in case ",
        ]
        .iter()
        .any(|word| lead_in.starts_with(word))
            || opening.contains(" means ")
    }

    /// Whether the block is the alternative to the conditional block before
    /// it: its lead-in opens with "Otherwise".
    pub(crate) fn is_otherwise(&self) -> bool {
        self.lead_in
            .trim_start()
            .trim_start_matches(['*', '_', '>', ' '])
            .to_ascii_lowercase()
            .starts_with("otherwise")
    }

    /// Whether the runbook says to stop once this block has run: "abort the
    /// merge and STOP", "delete the tag and STOP:" — STOP as an order, not a
    /// mention ("the table's STOP rows are never touched").
    pub(crate) fn says_stop(&self) -> bool {
        ["and STOP", "then STOP", ", STOP", ": STOP", "; STOP"]
            .iter()
            .any(|order| self.lead_in.contains(order))
            || self.lead_in.starts_with("STOP")
    }
}

impl RunbookStep {
    fn has_runnable_blocks(&self) -> bool {
        self.blocks.iter().any(RunbookBlock::offered)
    }

    /// The step as the worker sees it: its text with each runnable block
    /// labelled by the number `run_block` takes.
    pub(crate) fn annotated_body(&self) -> String {
        let mut out = Vec::new();
        let mut in_fence = false;
        let mut block_index = 0;
        for line in self.body.lines() {
            if is_fence(line.trim_start()) {
                if !in_fence {
                    block_index += 1;
                    match self.blocks.get(block_index - 1) {
                        Some(block) if block.is_resolve() => out.push(format!(
                            "**Block {block_index}** (`run_block` {block_index} — works through the merge \
                             conflicts one at a time):"
                        )),
                        Some(block) if block.runnable() => out.push(format!(
                            "**Block {block_index}** (`run_block` {block_index}):"
                        )),
                        _ => {}
                    }
                }
                in_fence = !in_fence;
            }
            out.push(line.to_string());
        }
        out.join("\n")
    }

    /// One line describing the step for its checklist entry: the first line of
    /// prose, plainly.
    pub(crate) fn summary(&self) -> String {
        let mut in_fence = false;
        for line in self.body.lines() {
            let trimmed = line.trim();
            if is_fence(trimmed) {
                in_fence = !in_fence;
                continue;
            }
            if in_fence || trimmed.is_empty() || trimmed.starts_with(['#', '|', '-', '>']) {
                continue;
            }
            // Emphasis markers go; a code span loses its backticks but keeps
            // its text, `*` and all ("git tag -l 'firefox-*'").
            let mut plain = String::new();
            let mut in_code = false;
            for c in trimmed.chars() {
                match c {
                    '`' => in_code = !in_code,
                    '*' if !in_code => {}
                    _ => plain.push(c),
                }
            }
            return truncate_chars(&plain, 140);
        }
        String::new()
    }

    /// The checks the step spells out — "must …" lines and anything telling
    /// the operator to STOP — listed for the judge, so a single sentence buried
    /// in a paragraph is not what the verdict misses.
    pub(crate) fn stated_checks(&self) -> Vec<String> {
        let mut checks: Vec<String> = Vec::new();
        for line in self.body.lines() {
            let trimmed = line.trim().trim_start_matches(['-', '*', ' ']).trim();
            let lower = trimmed.to_ascii_lowercase();
            let says_must = lower
                .split(|c: char| !c.is_ascii_alphabetic())
                .any(|word| word == "must");
            let says_stop = trimmed
                .split(|c: char| !c.is_ascii_alphabetic())
                .any(|word| word == "STOP");
            if (says_must || says_stop) && !checks.iter().any(|seen| seen == trimmed) {
                checks.push(truncate_chars(trimmed, 220));
            }
            if checks.len() == 24 {
                break;
            }
        }
        checks
    }
}

pub(crate) fn parse_runbook(markdown: &str) -> Runbook {
    let mut title = String::new();
    let mut intro: Vec<&str> = Vec::new();
    let mut sections: Vec<(String, Vec<&str>)> = Vec::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        let fence = is_fence(trimmed);
        if !in_fence && !fence {
            if let Some(heading) = trimmed.strip_prefix("## ") {
                sections.push((heading.trim().to_string(), Vec::new()));
                continue;
            }
            if title.is_empty() && sections.is_empty() {
                if let Some(heading) = trimmed.strip_prefix("# ") {
                    title = heading.trim().to_string();
                    continue;
                }
            }
        }
        if fence {
            in_fence = !in_fence;
        }
        match sections.last_mut() {
            Some((_, body)) => body.push(line),
            None => intro.push(line),
        }
    }

    // A runbook that never says "Step" still has an order: every section that
    // isn't obviously context becomes a step.
    let has_step_headings = sections.iter().any(|(heading, _)| is_step_heading(heading));
    let mut preamble = intro.join("\n").trim().to_string();
    let mut steps: Vec<RunbookStep> = Vec::new();
    let mut report_template = None;
    for (heading, body) in sections {
        let body = body.join("\n").trim().to_string();
        let is_step = is_step_heading(&heading);
        if !is_step && heading.to_ascii_lowercase().contains("report") {
            report_template = Some(
                fenced_blocks(&body)
                    .into_iter()
                    .next()
                    .map(|block| block.code)
                    .unwrap_or(body),
            );
        } else if is_step || (!has_step_headings && !is_context_heading(&heading)) {
            let id = unique_step_id(&heading, steps.len(), &steps);
            let blocks = fenced_blocks(&body);
            steps.push(RunbookStep {
                id,
                title: heading,
                body,
                blocks,
            });
        } else {
            if !preamble.is_empty() {
                preamble.push_str("\n\n");
            }
            preamble.push_str(&format!("## {heading}\n\n{body}"));
        }
    }
    if title.is_empty() {
        title = "Runbook".to_string();
    }
    Runbook {
        title,
        preamble,
        steps,
        report_template,
    }
}

fn is_fence(line: &str) -> bool {
    line.starts_with("```") || line.starts_with("~~~")
}

fn fence_lang(line: &str) -> String {
    line.trim_start_matches(['`', '~'])
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn fenced_blocks(body: &str) -> Vec<RunbookBlock> {
    let mut blocks = Vec::new();
    let mut current: Option<(String, String, Vec<&str>)> = None;
    // The prose since the last block, for the next block's lead-in.
    let mut prose: Vec<&str> = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim_start();
        if is_fence(trimmed) {
            match current.take() {
                Some((lang, lead_in, lines)) => blocks.push(RunbookBlock {
                    lang,
                    code: lines.join("\n"),
                    lead_in,
                }),
                None => {
                    current = Some((fence_lang(trimmed), last_paragraph(&prose), Vec::new()));
                    prose.clear();
                }
            }
            continue;
        }
        match current.as_mut() {
            Some((_, _, lines)) => lines.push(line),
            None => prose.push(line),
        }
    }
    blocks
}

/// The last paragraph of `lines`, on one line.
fn last_paragraph(lines: &[&str]) -> String {
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(0, |index| index + 1);
    let start = lines[..end]
        .iter()
        .rposition(|line| line.trim().is_empty())
        .map_or(0, |index| index + 1);
    lines[start..end]
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_step_heading(heading: &str) -> bool {
    let lower = heading.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("step") else {
        return false;
    };
    rest.is_empty()
        || rest.starts_with([' ', ':', '-'])
        || rest.starts_with(|c: char| c.is_ascii_digit())
}

fn is_context_heading(heading: &str) -> bool {
    let lower = heading.to_ascii_lowercase();
    [
        "background",
        "overview",
        "rule",
        "prerequisite",
        "context",
        "note",
        "about",
        "requirement",
    ]
    .iter()
    .any(|word| lower.contains(word))
}

/// `Step 0: …` → `step-0`, `Step B (one time only): …` → `step-b`; anything
/// else is numbered by position. Ids stay unique within the runbook.
fn unique_step_id(heading: &str, index: usize, existing: &[RunbookStep]) -> String {
    let label: String = if is_step_heading(heading) {
        heading[4..]
            .trim_start_matches([' ', ':', '-'])
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '.')
            .collect::<String>()
            .to_ascii_lowercase()
    } else {
        String::new()
    };
    let base = if label.is_empty() {
        format!("step-{}", index + 1)
    } else {
        format!("step-{label}")
    };
    let mut id = base.clone();
    let mut suffix = 2;
    while existing.iter().any(|step| step.id == id) {
        id = format!("{base}-{suffix}");
        suffix += 1;
    }
    id
}

/// Whether the runbook refers to a value named `name` — as `$NAME`, `${NAME`,
/// `NAME=` or `` `NAME` `` — so a judge can't invent a value nothing reads.
fn runbook_uses_name(runbook: &Runbook, name: &str) -> bool {
    if !is_fact_name(name) {
        return false;
    }
    let forms = [
        format!("${name}"),
        format!("${{{name}"),
        format!("{name}="),
        format!("`{name}`"),
    ];
    std::iter::once(runbook.preamble.as_str())
        .chain(runbook.steps.iter().map(|step| step.body.as_str()))
        .any(|text| forms.iter().any(|form| text.contains(form.as_str())))
}

/// A `# must print X` comment on an `echo "label: …"` line: the driver finds
/// the output line starting with the label and compares the rest with X.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PrintCheck {
    pub(crate) label: String,
    /// The value, or an UPPERCASE name whose known value is meant.
    pub(crate) expected: String,
}

/// The `# must print` checks a block states.
pub(crate) fn print_checks(code: &str) -> Vec<PrintCheck> {
    code.lines()
        .filter_map(|line| {
            let comment_at = comment_start(line)?;
            let comment = line[comment_at..].trim_start_matches('#').trim();
            let expected = comment
                .strip_prefix("must print")?
                .trim()
                .trim_matches(['`', '"', '\''])
                .to_string();
            let command = line[..comment_at].trim();
            let quoted = command.strip_prefix("echo \"")?;
            let label: String = quoted
                .chars()
                .take_while(|c| *c != '$' && *c != '"')
                .collect();
            let label = label.trim_end().to_string();
            (label.len() >= 3 && !expected.is_empty()).then_some(PrintCheck { label, expected })
        })
        .collect()
}

/// Each check's verdict against what the block printed, as lines the judge
/// reads; empty when the block states none.
pub(crate) fn run_print_checks(checks: &[PrintCheck], stdout: &str, facts: &Facts) -> Vec<String> {
    checks
        .iter()
        .map(|check| {
            let wanted = check
                .expected
                .trim_start_matches('$')
                .trim_start_matches('{')
                .trim_end_matches('}');
            let expected = if is_fact_name(wanted) {
                facts.get(wanted).cloned().unwrap_or_else(|| check.expected.clone())
            } else {
                check.expected.clone()
            };
            let printed = stdout
                .lines()
                .find_map(|line| line.trim_start().strip_prefix(check.label.as_str()))
                .map(str::trim);
            match printed {
                Some(printed) if printed == expected => format!(
                    "✓ checked by the driver: \"{}\" printed {printed}, as the step requires",
                    check.label
                ),
                Some(printed) => format!(
                    "✗ checked by the driver: \"{}\" printed {printed}, but the step requires {expected}",
                    check.label
                ),
                None => format!(
                    "✗ checked by the driver: no \"{}\" line was printed (the step requires {expected})",
                    check.label
                ),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The request
// ---------------------------------------------------------------------------

/// The runbook file a prompt names, and whatever else the prompt says.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RunbookRequest {
    pub(crate) path: PathBuf,
    /// The prompt with the path taken out: the operator's notes for the run.
    pub(crate) notes: String,
    /// `NAME=value` words in the notes, given to the run as known values.
    pub(crate) facts: Facts,
}

/// Finds the runbook file in a prompt: the whole prompt as a path, or any word
/// of it (quoted, or with escaped spaces as a drag-and-drop pastes it),
/// resolved against the working directory.
pub(crate) fn parse_runbook_request(prompt: &str, working_dir: &Path) -> Option<RunbookRequest> {
    let resolve = |candidate: &str| -> Option<PathBuf> {
        let candidate = candidate.trim();
        if candidate.is_empty() {
            return None;
        }
        let path = expand_home(candidate).unwrap_or_else(|| {
            let path = PathBuf::from(candidate);
            if path.is_absolute() {
                path
            } else {
                working_dir.join(path)
            }
        });
        path.is_file().then_some(path)
    };

    let whole = unquote(prompt.trim());
    if let Some(path) = resolve(&whole) {
        return Some(RunbookRequest {
            path,
            notes: String::new(),
            facts: Facts::new(),
        });
    }
    let words = prompt_words(prompt);
    let (range, path) = words
        .iter()
        .find_map(|(range, word)| resolve(word).map(|path| (range.clone(), path)))?;
    let notes = format!("{} {}", &prompt[..range.start], &prompt[range.end..])
        .trim()
        .to_string();
    let facts = prompt_words(&notes)
        .into_iter()
        .filter_map(|(_, word)| {
            let (name, value) = word.split_once('=')?;
            (is_fact_name(name) && !value.is_empty()).then(|| (name.to_string(), value.to_string()))
        })
        .collect();
    Some(RunbookRequest { path, notes, facts })
}

/// The markdown paths a prompt names that aren't files, each with a file of
/// the same name found in the target directory or one or two levels above
/// it — so a mistyped or misplaced path gets "did you mean" instead of the
/// bare usage text.
pub(crate) fn missing_runbook_paths(
    prompt: &str,
    working_dir: &Path,
) -> Vec<(String, Option<PathBuf>)> {
    prompt_words(prompt)
        .into_iter()
        .map(|(_, word)| word)
        .filter(|word| {
            let lower = word.to_ascii_lowercase();
            lower.ends_with(".md") || lower.ends_with(".markdown")
        })
        .map(|word| {
            let suggestion = Path::new(&word).file_name().and_then(|name| {
                working_dir
                    .ancestors()
                    .take(3)
                    .map(|dir| dir.join(name))
                    .find(|candidate| candidate.is_file())
            });
            (word, suggestion)
        })
        .collect()
}

/// How to name `path` from `working_dir`: `../name` style when it sits in an
/// ancestor a level or two up, else the absolute path.
fn path_from(working_dir: &Path, path: &Path) -> String {
    let Some(name) = path.file_name().map(|name| name.to_string_lossy()) else {
        return path.display().to_string();
    };
    match working_dir
        .ancestors()
        .take(3)
        .position(|dir| Some(dir) == path.parent())
    {
        Some(0) => name.into_owned(),
        Some(levels) => format!("{}{name}", "../".repeat(levels)),
        None => path.display().to_string(),
    }
}

/// Words of a prompt with shell-style quoting and `\ ` escapes undone, each
/// with the byte range it came from.
fn prompt_words(prompt: &str) -> Vec<(Range<usize>, String)> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut start: Option<usize> = None;
    let mut quote: Option<char> = None;
    let mut chars = prompt.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => {
                start.get_or_insert(index);
                quote = Some(c);
            }
            (None, '\\') => {
                start.get_or_insert(index);
                if let Some((_, escaped)) = chars.next() {
                    current.push(escaped);
                }
            }
            (None, c) if c.is_whitespace() => {
                if let Some(begin) = start.take() {
                    words.push((begin..index, std::mem::take(&mut current)));
                }
            }
            (None, c) => {
                start.get_or_insert(index);
                current.push(c);
            }
        }
    }
    if let Some(begin) = start {
        words.push((begin..prompt.len(), current));
    }
    words
}

fn unquote(text: &str) -> String {
    let text = text.trim();
    for quote in ['\'', '"'] {
        if let Some(inner) = text
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner.to_string();
        }
    }
    text.replace("\\ ", " ")
}

fn expand_home(path: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    if path == "~" {
        return Some(home);
    }
    path.strip_prefix("~/").map(|rest| home.join(rest))
}

// ---------------------------------------------------------------------------
// Blocks, values and the sandbox fence
// ---------------------------------------------------------------------------

pub(crate) fn is_fact_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// An `NAME=value` assignment opening a line (not `export NAME=`), split so the
/// value can be replaced while indentation and any comment survive.
#[derive(Debug, PartialEq)]
struct Assignment<'a> {
    indent: &'a str,
    name: &'a str,
    value: &'a str,
    comment: &'a str,
}

fn parse_assignment(line: &str) -> Option<Assignment<'_>> {
    let indent_len = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(indent_len);
    let equals = rest.find('=')?;
    let name = &rest[..equals];
    if !is_fact_name(name) {
        return None;
    }
    let after = &rest[equals + 1..];
    let comment_at = comment_start(after).unwrap_or(after.len());
    Some(Assignment {
        indent,
        name,
        value: after[..comment_at].trim_end(),
        comment: &after[comment_at..],
    })
}

/// Where an unquoted `#` that follows whitespace opens a shell comment.
fn comment_start(text: &str) -> Option<usize> {
    let mut single = false;
    let mut double = false;
    let mut after_space = false;
    for (index, c) in text.char_indices() {
        match c {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '#' if !single && !double && after_space => return Some(index),
            _ => {}
        }
        after_space = c.is_whitespace();
    }
    None
}

/// A literal can be overridden by a known value; anything computed (`$(…)`,
/// `${V//./_}`) is left for the shell to work out from the values it gets.
fn is_literal(value: &str) -> bool {
    !value.contains('$') && !value.contains('`')
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn assigned_names(code: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for assignment in code.lines().filter_map(parse_assignment) {
        if !names.iter().any(|name| name == assignment.name) {
            names.push(assignment.name.to_string());
        }
    }
    names
}

/// A block made ready to run.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PreparedScript {
    /// What runs: the known values exported, the block, then a trailer that
    /// reports the block's UPPERCASE assignments back.
    pub(crate) script: String,
    /// What the command card shows: the block as it runs, without plumbing.
    pub(crate) display: String,
}

/// Prepares a runbook block: a literal assignment of a value given in `set`,
/// or already known, is rewritten to that value (runbooks carry example
/// values — `V=158.0.1 # the new version` — that must not win over the one
/// the run found), and every known value is exported.
pub(crate) fn prepare_block(code: &str, set: &Facts, facts: &Facts) -> PreparedScript {
    let lines: Vec<String> = code
        .lines()
        .map(|line| {
            let Some(assignment) = parse_assignment(line) else {
                return line.to_string();
            };
            let replacement = set.get(assignment.name).or_else(|| {
                is_literal(assignment.value)
                    .then(|| facts.get(assignment.name))
                    .flatten()
            });
            match replacement {
                Some(value) => {
                    let comment = if assignment.comment.is_empty() {
                        String::new()
                    } else {
                        format!("  {}", assignment.comment)
                    };
                    format!(
                        "{}{}={}{comment}",
                        assignment.indent,
                        assignment.name,
                        shell_quote(value)
                    )
                }
                None => line.to_string(),
            }
        })
        .collect();
    let display = lines.join("\n");
    let mut known = facts.clone();
    known.extend(
        set.iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
    let script = wrap_script(&display, &known, &assigned_names(&display));
    PreparedScript { script, display }
}

/// Exports `known`, runs `body`, then (when there is anything to capture)
/// prints `capture`'s values after a marker and exits with the body's own
/// status, so the trailer never changes whether a block failed.
pub(crate) fn wrap_script(body: &str, known: &Facts, capture: &[String]) -> String {
    let mut script = String::new();
    for (name, value) in known.iter().filter(|(name, _)| is_fact_name(name)) {
        script.push_str(&format!("export {name}={}\n", shell_quote(value)));
    }
    script.push_str(body);
    script.push('\n');
    if !capture.is_empty() {
        script.push_str("__runbook_status=$?\n");
        script.push_str(&format!("printf '\\n%s\\n' '{FACTS_MARKER}'\n"));
        for name in capture {
            script.push_str(&format!("printf '%s=%s\\n' {name} \"${{{name}-}}\"\n"));
        }
        script.push_str("exit $__runbook_status\n");
    }
    script
}

/// Splits a run's stdout into the block's own output and the values its
/// trailer reported. Unset or empty values are not facts.
pub(crate) fn split_captured_facts(stdout: &str) -> (String, Facts) {
    let marker = format!("\n{FACTS_MARKER}\n");
    let Some(at) = stdout.rfind(&marker) else {
        return (stdout.to_string(), Facts::new());
    };
    let facts = stdout[at + marker.len()..]
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once('=')?;
            (is_fact_name(name) && !value.is_empty()).then(|| (name.to_string(), value.to_string()))
        })
        .collect();
    (stdout[..at].trim_end().to_string(), facts)
}

/// Why `script` would act outside `root`, if it would: a `cd` or `pushd` to an
/// absolute path elsewhere, or a recursive `rm` of one. A tripwire for the way
/// a runbook written for the real repository goes wrong in a sandbox — its
/// absolute paths — not a shell parser; relative paths and computed ones
/// (`"$(git rev-parse --git-dir)/…"`) pass.
pub(crate) fn escapes_root(script: &str, root: &Path) -> Option<String> {
    let root = normalize(root);
    let outside =
        |word: &str| absolute(word).is_some_and(|path| !normalize(&path).starts_with(&root));
    for command in simple_commands(script) {
        let words: Vec<String> = command.split_whitespace().map(unquote).collect();
        let Some(program) = words.first() else {
            continue;
        };
        match program.as_str() {
            "cd" | "pushd" => {
                if let Some(target) = words.get(1).filter(|target| outside(target)) {
                    return Some(format!(
                        "`{program} {target}` would leave the target directory"
                    ));
                }
            }
            "rm" => {
                let recursive = words.iter().skip(1).any(|word| {
                    word == "--recursive"
                        || (word.starts_with('-')
                            && !word.starts_with("--")
                            && word.contains(['r', 'R']))
                });
                if let Some(target) = words
                    .iter()
                    .skip(1)
                    .filter(|word| !word.starts_with('-'))
                    .find(|word| recursive && outside(word))
                {
                    return Some(format!(
                        "`rm` would delete `{target}`, which is outside the target directory"
                    ));
                }
            }
            _ => {}
        }
    }
    None
}

/// What a free shell command would change, if it changes anything: the
/// worker's shell is for looking, and changes are made by the step's own
/// blocks. qwen3.5:4b, after a merge failed, made a branch from pristine
/// Firefox, "merged" it into itself and committed there, and the step's
/// "main now ships: 153.0.4" printed from the wrong branch.
pub(crate) fn changes_something(script: &str) -> Option<String> {
    for command in simple_commands(script) {
        let raw: Vec<&str> = command.split_whitespace().collect();
        let words: Vec<String> = raw.iter().map(|word| unquote(word)).collect();
        // Redirection into a file (`> out`, `2>log`, `>>x`); `2>&1` and
        // `/dev/null` write nothing, and a quoted `>` is an argument.
        for (index, word) in raw.iter().enumerate() {
            if word.starts_with(['\'', '"']) {
                continue;
            }
            let Some(at) = word.find('>') else {
                continue;
            };
            let after = word[at..].trim_start_matches('>');
            let target = if after.is_empty() {
                words.get(index + 1).map(String::as_str).unwrap_or_default()
            } else {
                after
            };
            if !target.starts_with('&') && target != "/dev/null" {
                return Some(format!("`{command}` writes to a file"));
            }
        }
        // Past `VAR=value` words before the program.
        let first = words
            .iter()
            .position(|word| !is_shell_assignment(word))
            .unwrap_or(words.len());
        let Some(program) = words.get(first).map(String::as_str) else {
            continue;
        };
        let args: Vec<&str> = words.iter().skip(first + 1).map(String::as_str).collect();
        let changes = match program {
            "rm" | "rmdir" | "mv" | "cp" | "mkdir" | "touch" | "chmod" | "chown" | "ln" | "tee"
            | "truncate" | "dd" | "install" | "patch" => true,
            "sed" | "perl" => args.iter().any(|arg| arg.starts_with("-i")),
            "find" => args
                .iter()
                .any(|arg| matches!(*arg, "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir")),
            "git" => git_changes_something(&args),
            // A command run through another is the command it runs: the
            // quotes around it are read past, and the wrapper's own options
            // and their values (`sh -c`, `sudo -u who`) dropped a word at a
            // time until what is left is checked as a command.
            "sh" | "bash" | "zsh" | "env" | "nice" | "time" | "command" | "xargs" | "sudo"
            | "eval" | "exec" => {
                let inner = raw[first + 1..].join(" ").replace(['"', '\''], "");
                let inner: Vec<&str> = inner.split_whitespace().collect();
                (0..=inner.len().min(3))
                    .any(|skip| changes_something(&inner[skip..].join(" ")).is_some())
            }
            _ => false,
        };
        if changes {
            return Some(format!("`{command}` changes the repository or its files"));
        }
    }
    None
}

/// `FOO=bar`, as it precedes a program.
fn is_shell_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether a git command (its arguments after `git`) changes refs, the index
/// or the working tree.
fn git_changes_something(args: &[&str]) -> bool {
    // Past global options like `-C dir` or `--no-pager`.
    let mut rest = args;
    while let Some((first, tail)) = rest.split_first() {
        match *first {
            "-C" | "-c" => rest = tail.get(1..).unwrap_or_default(),
            option if option.starts_with('-') => rest = tail,
            _ => break,
        }
    }
    let Some((subcommand, options)) = rest.split_first() else {
        return false;
    };
    let has = |names: &[&str]| options.iter().any(|option| names.contains(option));
    let positional = options
        .iter()
        .filter(|option| !option.starts_with('-'))
        .count();
    let named = positional > 0;
    match *subcommand {
        "checkout" | "switch" | "merge" | "commit" | "reset" | "revert" | "rebase"
        | "cherry-pick" | "push" | "pull" | "am" | "rm" | "mv" | "clean" | "restore" | "add"
        | "update-ref" | "gc" | "prune" | "filter-branch" | "read-tree" | "update-index"
        | "commit-tree" | "write-tree" | "repack" | "sparse-checkout" | "maintenance" | "init"
        | "clone" => true,
        "stash" => !has(&["list", "show"]),
        // A fetch writes only under refs/remotes, unless a refspec names a
        // local ref to write.
        "fetch" => options
            .iter()
            .any(|option| option.contains(':') || *option == "--update-head-ok"),
        "submodule" => !has(&["status", "summary"]),
        "apply" => !has(&["--check", "--stat", "--numstat", "--summary"]),
        "branch" => {
            has(&[
                "-d",
                "-D",
                "-m",
                "-M",
                "-c",
                "-C",
                "-f",
                "-u",
                "--delete",
                "--move",
                "--copy",
                "--force",
                "--set-upstream-to",
                "--unset-upstream",
                "--edit-description",
            ]) || (named
                && !has(&[
                    "-l",
                    "--list",
                    "--contains",
                    "--merged",
                    "--no-merged",
                    "--points-at",
                ]))
        }
        "tag" => {
            has(&["-d", "--delete", "-f", "--force", "-a", "-s", "-m"])
                || (named
                    && !has(&[
                        "-l",
                        "--list",
                        "--contains",
                        "--points-at",
                        "--merged",
                        "-n",
                    ]))
        }
        "replace" => {
            has(&["-d", "--delete", "-f", "--force", "--edit", "--graft"])
                || (named && !has(&["-l", "--list"]))
        }
        "notes" => options
            .first()
            .is_some_and(|action| !matches!(*action, "show" | "list" | "get-ref")),
        "symbolic-ref" => has(&["-d", "--delete"]) || positional >= 2,
        "config" => {
            has(&[
                "--unset",
                "--unset-all",
                "--add",
                "--replace-all",
                "--remove-section",
                "--rename-section",
                "-e",
                "--edit",
            ]) || positional >= 2
        }
        "remote" => options.first().is_some_and(|action| {
            matches!(
                *action,
                "add"
                    | "remove"
                    | "rm"
                    | "rename"
                    | "set-url"
                    | "set-head"
                    | "set-branches"
                    | "prune"
            )
        }),
        "worktree" => options.first().is_some_and(|action| *action != "list"),
        "format-patch" => !has(&["--stdout"]),
        "archive" => {
            has(&["-o", "--output"]) || options.iter().any(|option| option.starts_with("--output="))
        }
        "bundle" => options.first() == Some(&"create"),
        "reflog" => options
            .first()
            .is_some_and(|action| matches!(*action, "expire" | "delete")),
        "lfs" => options.first().is_some_and(|action| {
            !matches!(*action, "ls-files" | "status" | "env" | "version" | "logs")
        }),
        _ => false,
    }
}

fn simple_commands(script: &str) -> Vec<String> {
    script
        .replace("&&", "\n")
        .replace("||", "\n")
        .split(['\n', ';', '|', '(', ')', '{', '}', '`'])
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

fn absolute(word: &str) -> Option<PathBuf> {
    if word.starts_with('/') {
        return Some(PathBuf::from(word));
    }
    expand_home(word)
}

/// Lexically resolves `.` and `..`, so `/root/../elsewhere` reads as
/// `/elsewhere` without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// A push remote that stays on this machine.
pub(crate) fn is_local_remote(url: &str) -> bool {
    url.starts_with('/')
        || url.starts_with("./")
        || url.starts_with("../")
        || url.starts_with("file://")
        || url.starts_with("~/")
}

/// `name → url` for each push remote in `git remote -v` output that would
/// leave the machine.
pub(crate) fn network_push_remotes(remote_v: &str) -> Vec<String> {
    remote_v
        .lines()
        .filter(|line| line.trim_end().ends_with("(push)"))
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let name = parts.next()?;
            let url = parts.next()?;
            (!is_local_remote(url)).then(|| format!("{name} → {url}"))
        })
        .collect()
}

/// Runbooks push and tag, so this mode only runs where that is harmless: in a
/// git repository whose every push remote is a local path. Outside a
/// repository nothing can be pushed, so the run goes ahead with a note.
async fn sandbox_check(working_dir: &Path, path: &str) -> Result<Option<String>, String> {
    let inside = execute_shell("git rev-parse --is-inside-work-tree", working_dir, path).await;
    if inside.exit_code != 0 || inside.stdout.trim() != "true" {
        return Ok(Some(
            "This folder is not a git repository: nothing here can be pushed, but nothing can be rolled back either."
                .to_string(),
        ));
    }
    let remotes = execute_shell("git remote -v", working_dir, path).await;
    let offending = network_push_remotes(&remotes.stdout);
    if offending.is_empty() {
        Ok(None)
    } else {
        Err(format!(
            "Runbook mode only runs in a sandbox, and this repository can push off this machine:\n\n{}\n\n\
             Make a sandbox clone whose remotes are local — for example \
             `git clone --local <repo> sandbox` then `git -C sandbox remote set-url origin <a local bare repo>` — \
             and run the runbook from inside it.",
            offending
                .iter()
                .map(|remote| format!("- `{remote}`"))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }
}

// ---------------------------------------------------------------------------
// Verdicts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerdictKind {
    /// Every check the step states holds.
    Pass,
    /// The step says it does not apply here, and the output shows that.
    Skip,
    /// The step concluded there is nothing left to do; the run ends, well.
    Done,
    /// A check failed, or the step said to stop.
    Stop,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Verdict {
    pub(crate) kind: VerdictKind,
    pub(crate) reason: String,
    pub(crate) facts: Facts,
}

/// Reads the judge's JSON answer, tolerating prose or a fence around it.
pub(crate) fn parse_verdict(content: &str) -> Option<Verdict> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    if end < start {
        return None;
    }
    let value: Value = serde_json::from_str(&content[start..=end]).ok()?;
    let kind = match value
        .get("verdict")?
        .as_str()?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "pass" | "passed" | "ok" => VerdictKind::Pass,
        "skip" | "skipped" => VerdictKind::Skip,
        "done" | "finished" | "complete" => VerdictKind::Done,
        "stop" | "fail" | "failed" => VerdictKind::Stop,
        _ => return None,
    };
    let reason = value
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let facts = value
        .get("facts")
        .and_then(Value::as_object)
        .map(|facts| {
            facts
                .iter()
                .filter(|(name, _)| is_fact_name(name))
                .filter_map(|(name, value)| {
                    let text = match value {
                        Value::String(text) => text.trim().to_string(),
                        Value::Number(number) => number.to_string(),
                        Value::Bool(flag) => flag.to_string(),
                        _ => return None,
                    };
                    (!text.is_empty()).then(|| (name.clone(), text))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Verdict {
        kind,
        reason,
        facts,
    })
}

// ---------------------------------------------------------------------------
// Prompts
// ---------------------------------------------------------------------------

const WORKER_SYSTEM_PROMPT: &str = "You run ONE step of a written runbook, on a sandbox copy of the user's \
project. Follow the step's text exactly.

How to work:
- The step's commands are in numbered blocks. Run them with run_block, by number, in the order the step gives. \
Never retype a block's commands into shell.
- Values found by earlier steps are already set in every command; they are listed under \"Known values\". If the \
step asks you to supply a value (for example a version, V=…), pass it in run_block's \"set\".
- Use shell only for a check the step asks for that is not in a block, or to look at files.
- Every command runs in the target directory. Never cd anywhere else.
- If the step says it does not apply in this situation, run only what proves that, then stop.
- If a check the step states fails, or the step says STOP, stop at once — run nothing further.
- If resolving something would mean rewriting a large file, do not attempt it: do what the step says for that case \
(usually abort and STOP).
- Run each block once. Never run a block again unless the step tells you to.
- When the blocks the step needs have run, stop calling tools and reply in plain text: what you ran and what the \
output showed. Do not start another step.";

const JUDGE_SYSTEM_PROMPT: &str = "You check one step of a runbook run. Another model ran it; you only read \
what happened. Decide from the step's own success criteria and the actual command output below — never from what \
the worker says it did.

Reply with one JSON object and nothing else:
{\"verdict\": \"pass\" | \"skip\" | \"done\" | \"stop\", \"reason\": \"one or two sentences quoting the output \
that decided it\", \"facts\": {}}

Judge the step by its own text. The values it names itself — a version it sets, a number it says a command must \
print — are what its checks mean. Values known from earlier steps are context, not targets: a step may rightly \
work on a different version than the one an earlier step found. Lines marked \"checked by the driver\" were \
compared with the real output by the program running the step; trust them.

- pass: every check this step states holds in the output.
- skip: the step says it does not apply in this situation, and the output proves that.
- done: the step concluded there is nothing left to do (for example, already up to date). The whole run ends here, \
successfully.
- stop: a check failed, the step says to STOP, a command failed in a way the step does not expect, or the output \
you would need is missing. When in doubt, stop.
- facts: values a later step will need that this step established, under the variable name the runbook itself \
uses (for example V). Usually empty; leave out anything already known.";

/// The reason comes first and the verdict last, labelled: qwen3.5:4b, asked
/// for the verdict first, wrote "NO — the output shows AGENTS.md is still
/// conflicted, so the condition holds, making this block run".
const CONDITION_SYSTEM_PROMPT: &str = "You decide whether one block of a runbook step runs now. The runbook runs \
it only in the case it states just before the block. From what the step's commands have printed so far, does that \
case hold? Decide this block's own case, as stated before it, not another block's: that an earlier block was not \
run because its own case failed says nothing about this one. In one or two sentences, say what the output shows \
that decides it, quoting it; then on the last line write VERDICT: YES or VERDICT: NO.";

const DONE_CHECK_SYSTEM_PROMPT: &str = "You check one decision about a runbook run. A step was judged to end the \
whole run, as if the steps after it weren't needed. That is right only when the step's own text or output says \
there is nothing more to do — for example, already up to date. In one sentence, say what the step's text or output \
says about that; then on the last line write VERDICT: YES if it says there is nothing more to do, or VERDICT: NO if \
the step simply did its own work.";

const REPORT_SYSTEM_PROMPT: &str = "You write the final report of a runbook run by filling in the runbook's own \
reporting template from the run record. Under each step the record has what its commands printed: take what \
conflicted, what was looked up and what was pushed from that output, not from the verdicts alone. Use only what \
the record shows; write n/a for anything the run did not reach. Reply with the filled-in template only.";

fn facts_lines(facts: &Facts) -> String {
    if facts.is_empty() {
        return "(none yet)".to_string();
    }
    facts
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn facts_inline(facts: &Facts) -> String {
    facts
        .iter()
        .map(|(name, value)| format!("`{name}={value}`"))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn worker_system_prompt(runbook: &Runbook) -> String {
    format!(
        "{WORKER_SYSTEM_PROMPT}\n\nThe runbook's own rules and background (they apply to every step):\n\n{}",
        truncate_chars(&runbook.preamble, 6_000)
    )
}

fn worker_user_prompt(
    step: &RunbookStep,
    facts: &Facts,
    working_dir: &Path,
    notes: &str,
) -> String {
    let notes = if notes.trim().is_empty() {
        String::new()
    } else {
        format!("Operator's notes for this run: {}\n\n", notes.trim())
    };
    format!(
        "Target directory (every command runs here): {}\n\n{notes}Known values (already set in every command):\n{}\n\n\
         Your step — {}:\n\n{}",
        working_dir.display(),
        facts_lines(facts),
        step.title,
        step.annotated_body()
    )
}

/// What a step ran, as both the judge and the report read it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CallRecord {
    pub(crate) label: String,
    /// The shell exit status; `None` for file tools.
    pub(crate) exit_code: Option<i32>,
    pub(crate) output: String,
}

fn records_text(records: &[CallRecord]) -> String {
    if records.is_empty() {
        return "(nothing was run)".to_string();
    }
    records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let status = match record.exit_code {
                Some(code) => format!(" — exit {code}"),
                None => String::new(),
            };
            let output = if record.output.trim().is_empty() {
                "(no output)".to_string()
            } else {
                record.output.trim_end().to_string()
            };
            format!("[{}] {}{status}\n{output}", index + 1, record.label)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn judge_user_prompt(
    runbook: &Runbook,
    step: &RunbookStep,
    facts_before: &Facts,
    worker_run: &WorkerRun,
) -> String {
    let (records, summary) = (&worker_run.records, &worker_run.summary);
    // A judge took a small worker's word for what a block that never ran
    // "confirmed"; the driver knows which blocks ran, so it says.
    let unrun: Vec<usize> = offered_block_numbers(step)
        .into_iter()
        .filter(|number| {
            !worker_run.ran_blocks.contains(number) && ruled_out_now(worker_run, *number).is_none()
        })
        .collect();
    let unrun = match unrun.as_slice() {
        [] => String::new(),
        [only] => format!(
            "\n\nChecked by the driver: block {only} of this step did not run. Whether it needed to is for the \
             step's text to say; but nothing above shows its output, so anything the worker says it printed is \
             unproven."
        ),
        _ => format!(
            "\n\nChecked by the driver: blocks {} of this step did not run. Whether they needed to is for the \
             step's text to say; but nothing above shows their output, so anything the worker says they printed \
             is unproven.",
            join_and(&unrun)
        ),
    };
    let ruled_out: String = worker_run
        .rulings
        .iter()
        .filter(|(number, ruling)| !ruling.holds && ruled_out_now(worker_run, **number).is_some())
        .map(|(number, ruling)| {
            if ruling.unclear {
                format!(
                    "\n\nChecked by the driver: block {number} was not run: the check of its case gave no clear \
                     answer, and the block ends the step (the record above says so)."
                )
            } else {
                format!(
                    "\n\nChecked by the driver: block {number} was not run, as a check found the case it runs \
                     in doesn't hold (the record above says why)."
                )
            }
        })
        .collect();
    let checks = step.stated_checks();
    let checks = if checks.is_empty() {
        String::new()
    } else {
        format!(
            "Checks this step states:\n{}\n\n",
            checks
                .iter()
                .map(|check| format!("- {check}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    format!(
        "The runbook's rules, for context:\n{}\n\nStep — {}:\n\n{}\n\n{checks}Known values before this step:\n{}\n\n\
         What was run, in order:\n\n{}{unrun}{ruled_out}\n\nThe worker's summary (its own account, not evidence):\n{}",
        truncate_chars(&runbook.preamble, 3_000),
        step.title,
        step.body,
        facts_lines(facts_before),
        records_text(records),
        if summary.trim().is_empty() {
            "(none)"
        } else {
            summary.trim()
        }
    )
}

// ---------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------

/// Deregisters the run's stop signal however the run ends.
struct SessionGuard(Uuid);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        finish_harness_session(&self.0);
    }
}

/// Everything a run's calls share.
struct RunContext<'a> {
    working_dir: &'a Path,
    path: &'a str,
    worker: &'a str,
    /// A thinking worker is told not to: its job is running the step's
    /// blocks, and reasoning before each call can use up its output cap
    /// before the call is made.
    worker_thinks: bool,
    judge: &'a str,
    judge_thinks: bool,
    /// The small model that checks each fix in the work-file pass.
    helper: &'a str,
    helper_thinks: bool,
    /// Where the work-file pass keeps its notes unless a block says otherwise:
    /// beside the runbook, named after it.
    work_dir: &'a Path,
    cancel: Option<HarnessCancel>,
}

impl RunContext<'_> {
    fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(HarnessCancel::is_cancelled)
    }
}

/// How one worker turn on a step went.
#[derive(Debug, Default)]
struct WorkerRun {
    records: Vec<CallRecord>,
    /// The step's blocks that ran, by number, so the judge can be told which
    /// never did.
    ran_blocks: BTreeSet<usize>,
    /// What a check of a block's case found, with when: a hold is not asked
    /// about again, a refusal is given back without a second check.
    rulings: BTreeMap<usize, CaseRuling>,
    /// The block whose lead-in orders a STOP, once it has run: the runbook
    /// ends the step there, so no block runs after it.
    stopped_at: Option<usize>,
    summary: String,
    usage: Option<TurnUsage>,
    cancelled: bool,
    /// The turn broke down (service unreachable, call limit) before the step
    /// could be judged.
    failure: Option<String>,
}

/// What a check found of a block's case, and which of the blocks before it
/// had run then. Once another of those runs, the case may have changed, and
/// it is checked again; blocks after it running change nothing (attempt 11
/// lost a right ruling to the commit after it).
#[derive(Debug, Clone, Default, PartialEq)]
struct CaseRuling {
    holds: bool,
    /// A refusal because the check gave no clear answer, for a block that
    /// ends the step — not a finding that the case fails.
    unclear: bool,
    /// For a refusal, the reply the worker got.
    why: String,
    ran_before: BTreeSet<usize>,
}

/// What a check of a block's case came back with.
#[derive(Debug, Clone, PartialEq)]
enum CaseCheck {
    Holds,
    Fails(String),
    /// No answer, or none that says yes or no.
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
enum StepResult {
    /// The verdict, its reason, and what the step's calls printed, cut down
    /// for the closing report ([`printed_digest`]).
    Judged(VerdictKind, String, String),
    NotReached,
}

/// What a step's calls printed, for the closing report: each call with its
/// exit status and the end of its output — where a command's result usually
/// is — cut so a whole run's record fits the report writer's window. The
/// verdicts alone don't say which files conflicted or what was pushed, and a
/// report written from them guesses.
fn printed_digest(records: &[CallRecord]) -> String {
    const CALL_CHARS: usize = 600;
    const STEP_CHARS: usize = 2_400;
    // Every call gets a share, so the last — often the one that pushed or
    // committed — isn't the one cut.
    let share = (STEP_CHARS / records.len().max(1)).clamp(160, CALL_CHARS);
    let mut digest = String::new();
    for record in records {
        let status = record
            .exit_code
            .map(|code| format!(" — exit {code}"))
            .unwrap_or_default();
        let output = record.output.trim();
        let output = if output.is_empty() {
            "(no output)".to_string()
        } else {
            last_chars(output, share)
        };
        let entry = format!(
            "  {}{status}\n{}\n",
            truncate_chars(record.label.trim(), 120),
            output
                .lines()
                .map(|line| format!("    {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        if digest.len() + entry.len() > STEP_CHARS + 1_000 {
            digest.push_str("  …\n");
            break;
        }
        digest.push_str(&entry);
    }
    digest
}

/// The end of `text`, at most `max` characters, from the start of a line
/// when one is near.
fn last_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let tail: String = text.chars().skip(count - max).collect();
    let tail = match tail.find('\n') {
        Some(at) if at < max / 3 => tail[at + 1..].to_string(),
        _ => tail,
    };
    format!("…\n{tail}")
}

/// Runs the runbook named in `prompt` against `working_dir`, step by step,
/// with `worker` doing each step and a judge checking it.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_ollama_runbook(
    prompt: &str,
    session_id: &Uuid,
    working_dir: &Path,
    path: &str,
    worker: &str,
    config: RunbookConfig,
    disabled: Vec<String>,
    mut on_event: impl FnMut(TurnEvent),
) -> std::io::Result<ExitStatus> {
    register_harness_session(*session_id);
    let _session_guard = SessionGuard(*session_id);
    on_event(TurnEvent::SessionInit {
        session_id: session_id.to_string(),
        model: Some(worker.to_string()),
        cwd: Some(working_dir.to_string_lossy().into_owned()),
    });

    let Some(request) = parse_runbook_request(prompt, working_dir) else {
        let missing = missing_runbook_paths(prompt, working_dir);
        let text = match missing.first() {
            Some((word, Some(found))) => format!(
                "**Runbook mode** — there's no file at `{word}`. Did you mean `{}` (`{}`)? Send that path to \
                 run it here, in `{}`.",
                path_from(working_dir, found),
                found.display(),
                working_dir.display()
            ),
            Some((word, None)) => format!(
                "**Runbook mode** — there's no file at `{word}`. Check the path: it can be absolute, `~/…`, or \
                 relative to `{}`, where the runbook will run.",
                working_dir.display()
            ),
            None => format!(
                "**Runbook mode** — give me the runbook file to follow, as a path (absolute, `~/…`, or relative \
                 to `{}`). The runbook runs in the folder this terminal is in, so `cd` into the sandbox first.",
                working_dir.display()
            ),
        };
        finish_with_text(&mut on_event, text);
        return Ok(success_status());
    };
    let markdown = match std::fs::read_to_string(&request.path) {
        Ok(markdown) => markdown,
        Err(error) => {
            emit_failed_turn(
                &mut on_event,
                format!("Could not read {}: {error}", request.path.display()),
            );
            return Ok(success_status());
        }
    };
    let runbook = parse_runbook(&markdown);
    if runbook.steps.is_empty() {
        finish_with_text(
            &mut on_event,
            format!(
                "`{}` has no steps I can follow: a runbook needs `## Step …` sections (or at least `##` sections) to \
                 work through.",
                request.path.display()
            ),
        );
        return Ok(success_status());
    }

    let sandbox_note = match sandbox_check(working_dir, path).await {
        Ok(note) => note,
        Err(refusal) => {
            finish_with_text(&mut on_event, refusal);
            return Ok(success_status());
        }
    };

    // A worker on another machine (`name@@http://host:port`) needs no
    // service started here.
    if crate::ai::ollama::remote::split(worker).1.is_none() {
        if let Err(error) = ensure_ollama_service(path).await {
            emit_failed_turn(
                &mut on_event,
                format!("Could not start the local Ollama service: {error}"),
            );
            return Err(error);
        }
    }
    let worker_thinks = match probe_model_caps_checked(worker, path).await {
        Some(caps) if caps.tools => caps.thinking,
        Some(_) => {
            finish_with_text(
                &mut on_event,
                format!(
                    "`{worker}` can't call tools, and a runbook worker runs commands through them. Pick a \
                     tool-capable model for this run — `qwen2.5-coder:14b`, `qwen3:14b` or `devstral` — and send the \
                     runbook again."
                ),
            );
            return Ok(success_status());
        }
        None => {
            emit_failed_turn(
                &mut on_event,
                format!("Ollama didn't answer when asked what `{worker}` can do, so the run can't start."),
            );
            return Ok(success_status());
        }
    };
    let installed = if config.judge.is_none() || config.helper.is_none() {
        fetch_installed_model_names(path).await.unwrap_or_default()
    } else {
        Vec::new()
    };
    let judge = config
        .judge
        .clone()
        .unwrap_or_else(|| pick_judge(&installed, worker, &disabled));
    let helper = config
        .helper
        .clone()
        .unwrap_or_else(|| pick_helper(&installed, worker, &disabled));
    let judge_thinks = probe_model_caps_checked(&judge, path)
        .await
        .is_some_and(|caps| caps.thinking);
    let helper_thinks = probe_model_caps_checked(&helper, path)
        .await
        .is_some_and(|caps| caps.thinking);
    let work_dir = request.path.with_extension("work");

    let context = RunContext {
        working_dir,
        path,
        worker,
        worker_thinks,
        judge: &judge,
        judge_thinks,
        helper: &helper,
        helper_thinks,
        work_dir: &work_dir,
        cancel: harness_cancel_for(session_id),
    };

    let has_pass = runbook
        .steps
        .iter()
        .any(|step| step.blocks.iter().any(RunbookBlock::is_resolve));
    on_event(TurnEvent::Text {
        text: format!(
            "**Runbook:** {} — {} steps, run in `{}`.\n\nWorker **`{worker}`** runs each step in a fresh \
             conversation; judge **`{judge}`** checks it against the runbook's own criteria.{}{}",
            runbook.title,
            runbook.steps.len(),
            working_dir.display(),
            if has_pass {
                format!(
                    " Merge conflicts are worked one at a time: `{judge}` proposes each fix and helper **`{helper}`** \
                     checks it, with notes kept in `{}`.",
                    work_dir.display()
                )
            } else {
                String::new()
            },
            sandbox_note
                .map(|note| format!("\n\n{note}"))
                .unwrap_or_default()
        ),
    });
    on_event(TurnEvent::PlanUpdated {
        update: PlanUpdate::Create(
            runbook
                .steps
                .iter()
                .map(|step| PlanItem {
                    id: step.id.clone(),
                    title: step.title.clone(),
                    description: step.summary(),
                })
                .collect(),
        ),
    });

    let mut facts = request.facts.clone();
    let mut results: Vec<(String, StepResult)> = runbook
        .steps
        .iter()
        .map(|step| (step.title.clone(), StepResult::NotReached))
        .collect();
    let mut last_usage: Option<TurnUsage> = None;
    let mut stopped_by_user = false;

    for (index, step) in runbook.steps.iter().enumerate() {
        if context.cancelled() {
            stopped_by_user = true;
            break;
        }
        let known = facts_inline(&facts);
        on_event(TurnEvent::Text {
            text: if known.is_empty() {
                format!("**{}**", step.title)
            } else {
                format!("**{}**\n\nKnown values: {known}", step.title)
            },
        });

        let facts_before = facts.clone();
        let mut worker_run = run_worker(
            &context,
            &runbook,
            step,
            &mut facts,
            &request.notes,
            &mut on_event,
        )
        .await?;
        if let Some(usage) = worker_run.usage.clone() {
            last_usage = Some(usage);
        }
        if worker_run.cancelled || context.cancelled() {
            stopped_by_user = true;
            break;
        }
        if !worker_run.summary.trim().is_empty() {
            on_event(TurnEvent::Text {
                text: quote_block(&truncate_chars(worker_run.summary.trim(), 800)),
            });
        }

        let verdict = match &worker_run.failure {
            Some(failure) => Verdict {
                kind: VerdictKind::Stop,
                reason: failure.clone(),
                facts: Facts::new(),
            },
            None => {
                match judge_step(
                    &context,
                    &runbook,
                    step,
                    &facts_before,
                    &worker_run,
                    &mut on_event,
                )
                .await?
                {
                    Some(verdict) => verdict,
                    None if context.cancelled() => {
                        stopped_by_user = true;
                        break;
                    }
                    None => Verdict {
                        kind: VerdictKind::Stop,
                        reason: format!("the judge (`{judge}`) gave no verdict I could read"),
                        facts: Facts::new(),
                    },
                }
            }
        };
        // "Done" ends the whole run: qwen3.5:4b, judging, called a finished
        // step 1c done, and the runtime tag (step 3) was never chosen. With
        // steps still to come, that is put to the judge again as a question.
        let verdict = if verdict.kind == VerdictKind::Done
            && index + 1 < runbook.steps.len()
            && !run_really_ends(&context, &runbook, index, &worker_run, &mut on_event).await?
        {
            Verdict {
                kind: VerdictKind::Pass,
                ..verdict
            }
        } else {
            verdict
        };

        // A pass with `# must print` checks never made is no pass: qwen3.5:4b
        // skipped step B's "record the merge and push" block, and as judge
        // passed the step on the checks it could see.
        let verdict = if verdict.kind == VerdictKind::Pass {
            let unmet = unmet_print_checks(&context, step, &mut worker_run, &mut on_event).await?;
            if unmet.is_empty() {
                verdict
            } else {
                Verdict {
                    kind: VerdictKind::Stop,
                    reason: format!(
                        "The judge passed this step, but the driver found checks it states that were never made: {}.",
                        unmet
                            .iter()
                            .map(|(number, checks)| format!(
                                "block {number} did not run, and its case wasn't ruled out ({checks})"
                            ))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                    facts: Facts::new(),
                }
            }
        } else {
            verdict
        };
        let new_facts: Facts = verdict
            .facts
            .iter()
            .filter(|(name, value)| facts.get(*name) != Some(*value))
            .filter(|(name, _)| runbook_uses_name(&runbook, name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let reason = if verdict.reason.is_empty() {
            "no reason given".to_string()
        } else {
            verdict.reason.clone()
        };
        results[index].1 = StepResult::Judged(
            verdict.kind,
            reason.clone(),
            printed_digest(&worker_run.records),
        );
        match verdict.kind {
            VerdictKind::Pass | VerdictKind::Skip => {
                facts.extend(new_facts.clone());
                let mark = if verdict.kind == VerdictKind::Pass {
                    "✓ **Passed.**"
                } else {
                    "↷ **Skipped** — doesn't apply here."
                };
                on_event(TurnEvent::Text {
                    text: carried_forward(format!("{mark} {reason}"), &new_facts),
                });
                on_event(TurnEvent::PlanUpdated {
                    update: PlanUpdate::Complete(vec![step.id.clone()]),
                });
            }
            VerdictKind::Done => {
                facts.extend(new_facts.clone());
                on_event(TurnEvent::Text {
                    text: carried_forward(
                        format!("✓ **Done — nothing more to do.** {reason}"),
                        &new_facts,
                    ),
                });
                on_event(TurnEvent::PlanUpdated {
                    update: PlanUpdate::Complete(
                        runbook.steps[index..]
                            .iter()
                            .map(|step| step.id.clone())
                            .collect(),
                    ),
                });
                break;
            }
            VerdictKind::Stop => {
                on_event(TurnEvent::Text {
                    text: format!("■ **Stopped at {}.** {reason}", step.title),
                });
                break;
            }
        }
    }

    if stopped_by_user {
        on_event(TurnEvent::Text {
            text: "Stopped — you ended the run.".to_string(),
        });
    } else {
        let report = write_report(&context, &runbook, &results, &facts, &mut on_event).await?;
        on_event(TurnEvent::Text { text: report });
    }
    on_event(TurnEvent::TurnCompleted {
        usage: last_usage,
        is_error: false,
        error_message: None,
    });
    Ok(success_status())
}

fn finish_with_text(on_event: &mut impl FnMut(TurnEvent), text: String) {
    on_event(TurnEvent::Text { text });
    on_event(TurnEvent::TurnCompleted {
        usage: None,
        is_error: false,
        error_message: None,
    });
}

fn carried_forward(line: String, new_facts: &Facts) -> String {
    if new_facts.is_empty() {
        line
    } else {
        format!("{line}\n\nCarrying forward: {}", facts_inline(new_facts))
    }
}

fn quote_block(text: &str) -> String {
    text.lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One worker turn on one step: a fresh conversation, a bounded tool loop.
async fn run_worker(
    context: &RunContext<'_>,
    runbook: &Runbook,
    step: &RunbookStep,
    facts: &mut Facts,
    notes: &str,
    on_event: &mut impl FnMut(TurnEvent),
) -> std::io::Result<WorkerRun> {
    let tools = worker_tools();
    let tool_defs: Vec<Value> = tools.iter().map(McpToolSpec::to_ollama_tool_json).collect();
    let mut messages = vec![
        json!({ "role": "system", "content": worker_system_prompt(runbook) }),
        json!({
            "role": "user",
            "content": worker_user_prompt(step, facts, context.working_dir, notes),
        }),
    ];
    let mut run = WorkerRun::default();
    let mut nudged = false;
    // The block a worker was last sent back to run, and how often it was.
    let mut nudged_about: Option<usize> = None;
    let mut unrun_nudges = 0;
    // Calls that already succeeded this step, with what they returned: a
    // small model that has run every block sometimes starts over from block
    // 1 rather than answer, and would loop until it ran out of calls.
    let mut succeeded: BTreeMap<String, String> = BTreeMap::new();
    let mut repeats = 0;
    let offered = offered_block_numbers(step);

    for call_index in 0..=MAX_WORKER_CALLS {
        if context.cancelled() {
            run.cancelled = true;
            return Ok(run);
        }
        let mut request = json!({
            "model": wire_name(context.worker),
            "messages": messages,
            "tools": tool_defs,
            "stream": true,
            "keep_alive": KEEP_ALIVE,
            "options": {
                "temperature": 0,
                "num_ctx": RUNBOOK_NUM_CTX,
                "num_predict": WORKER_MAX_TOKENS,
            },
        });
        if context.worker_thinks {
            request["think"] = json!(false);
        }
        let request = request.to_string();
        let response = match chat_call(context, context.worker, &request, on_event).await? {
            Ok(response) => response,
            Err(error) => {
                if context.cancelled() {
                    run.cancelled = true;
                } else {
                    run.failure = Some(error);
                }
                return Ok(run);
            }
        };
        run.usage = Some(response.usage.clone());

        // Prefer structured tool calls; otherwise recover one a model wrote as
        // JSON text (qwen2.5-coder does this, with `tool_calls` null).
        let (calls, recovered) = if !response.tool_calls.is_empty() {
            (response.tool_calls.clone(), false)
        } else if let Some(call) = parse_text_tool_call(&response.content, &tools) {
            (vec![call], true)
        } else {
            // A plain answer. One that comes before anything has run is a
            // model skipping the work, so it is sent back once.
            if run.records.is_empty() && step.has_runnable_blocks() && !nudged {
                nudged = true;
                messages.push(json!({ "role": "assistant", "content": response.content }));
                messages.push(json!({
                    "role": "user",
                    "content": "You have not run anything yet. Run this step's blocks with run_block, \
                                starting with block 1, before you answer.",
                }));
                continue;
            }
            // One that comes with blocks still to run — after the last call
            // went through, so not a step stopping on a failure — is sent back
            // to run the next one, at most four times: llama3.1:8b ran block 1
            // of 2 and then reported what block 2 "confirmed"; qwen3.5:4b
            // decided step B's "Otherwise … push" didn't apply. Whether a
            // block's case holds is put to the judge here, as a call to it
            // would be, not left to the worker.
            let last_failed = run
                .records
                .last()
                .is_some_and(|record| record.exit_code.is_some_and(|code| code != 0));
            if !run.records.is_empty() && !last_failed && unrun_nudges < 4 {
                if let Some((number, checked)) =
                    next_block_to_run(context, step, &mut run, on_event).await?
                {
                    if nudged_about != Some(number) {
                        unrun_nudges += 1;
                        nudged_about = Some(number);
                        messages.push(json!({ "role": "assistant", "content": response.content }));
                        messages.push(json!({
                            "role": "user",
                            "content": run_block_nudge(number, &step.blocks[number - 1], checked),
                        }));
                        continue;
                    }
                }
            }
            run.summary = response.content;
            return Ok(run);
        };
        if call_index == MAX_WORKER_CALLS {
            // What ran is still on record; the judge decides from it.
            run.summary = format!(
                "(The worker made {MAX_WORKER_CALLS} calls on this step without giving its summary.)"
            );
            return Ok(run);
        }

        if recovered {
            messages.push(json!({ "role": "assistant", "content": response.content }));
        } else {
            messages.push(json!({
                "role": "assistant",
                "content": response.content,
                "tool_calls": calls
                    .iter()
                    .map(|call| json!({ "function": { "name": call.name, "arguments": call.arguments } }))
                    .collect::<Vec<_>>(),
            }));
        }
        for call in &calls {
            let call = &normalize_call(call, step);
            let signature = call_signature(call);
            let reply = if let Some(earlier) = succeeded.get(&signature) {
                repeats += 1;
                if repeats > 2 {
                    run.summary =
                        "(The worker kept repeating calls it had already made.)".to_string();
                    return Ok(run);
                }
                format!(
                    "Not run again: this exact call already ran and succeeded. Its result was:\n{earlier}\n\n\
                     If the blocks this step needs have run, stop calling tools and reply with your summary."
                )
            } else if let Some(why) = run_block_number(call)
                .and_then(|number| ruled_out_now(&run, number))
                .map(str::to_string)
            {
                repeats += 1;
                if repeats > 2 {
                    run.summary =
                        "(The worker kept asking for blocks a check had ruled out.)".to_string();
                    return Ok(run);
                }
                format!("{why}\n\nThat was checked already: go on with the rest of the step.")
            } else if let (Some(stopped), Some(number)) = (run.stopped_at, run_block_number(call)) {
                // qwen3.5:9b ran the merge step's "abort the merge and STOP"
                // block and then the commit after it.
                repeats += 1;
                if repeats > 2 {
                    run.summary =
                        "(The worker kept calling blocks after the one the runbook stops at.)"
                            .to_string();
                    return Ok(run);
                }
                // A card, and a record the judge reads.
                let (record, reply) = refusal(
                    &format!("runbook-{}", Uuid::new_v4()),
                    format!("block {number}"),
                    format!(
                        "Not run: the runbook says to STOP after block {stopped}, which has run. Block {number} \
                         is not run. Reply with your summary."
                    ),
                    on_event,
                );
                run.records.push(record);
                reply
            } else if let Some(reply) = gate(context, step, &mut run, call, on_event).await? {
                match next_blocks_hint(step, &decided_blocks(&run)) {
                    Some(next) => format!("{reply}\n\n{next}"),
                    None => format!(
                        "{reply}\n\nNo block of this step is left to run: reply with your summary."
                    ),
                }
            } else {
                let Some((record, reply)) =
                    execute_call(context, step, facts, call, on_event).await
                else {
                    run.cancelled = true;
                    return Ok(run);
                };
                let succeeded_now = record.exit_code == Some(0);
                if succeeded_now {
                    succeeded.insert(signature, reply.clone());
                }
                run.records.push(record);
                if let Some(number) = run_block_number(call) {
                    run.ran_blocks.insert(number);
                }
                let says_stop = run_block_number(call)
                    .and_then(|number| step.blocks.get(number - 1))
                    .is_some_and(RunbookBlock::says_stop);
                if says_stop && succeeded_now {
                    run.stopped_at = run_block_number(call);
                }
                if says_stop {
                    format!(
                        "{reply}\n\nThe runbook says to STOP after this block: run nothing further, and reply \
                         with your summary."
                    )
                } else if !offered.is_empty() && offered.is_subset(&decided_blocks(&run)) {
                    format!(
                        "{reply}\n\nEvery block in this step has now run, or been ruled out by its case. Unless \
                         the step asks for something more, reply with your summary instead of another tool call."
                    )
                } else if let Some(next) = next_blocks_hint(step, &decided_blocks(&run)) {
                    format!("{reply}\n\n{next}")
                } else {
                    reply
                }
            };
            // A model that ignores native tool calls ignores the `tool` role
            // too, but always reads a user message.
            if recovered {
                messages.push(json!({
                    "role": "user",
                    "content": format!("Result of {}:\n{reply}", call.name),
                }));
            } else {
                messages.push(json!({ "role": "tool", "content": reply }));
            }
        }
    }
    unreachable!("the bounded worker loop always returns")
}

fn worker_tools() -> Vec<McpToolSpec> {
    let mut tools = vec![
        McpToolSpec {
            name: RUN_BLOCK.to_string(),
            description: "Run one of this step's numbered command blocks exactly as the runbook wrote it, in the \
                target directory. Values found by earlier steps are already set. Use `set` only to supply a value \
                the step itself asks for, e.g. {\"V\": \"158.0\"}."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "block": { "type": "integer", "description": "The block number shown in the step." },
                    "set": {
                        "type": "object",
                        "description": "Optional values to use, by NAME, e.g. {\"V\": \"158.0\"}.",
                        "additionalProperties": { "type": "string" }
                    }
                },
                "required": ["block"]
            }),
        },
        McpToolSpec {
            name: SHELL.to_string(),
            description: "Run one shell command in the target directory, for a check the step asks for that is \
                not in a block, or to look at files. Known values are set."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": { "command": { "type": "string", "description": "The command to run." } },
                "required": ["command"]
            }),
        },
    ];
    tools.extend(
        builtin_local_tool_specs()
            .into_iter()
            // The run keeps its own checklist, of the runbook's steps.
            .filter(|tool| tool.name != BUILTIN_RUN_CODE && tool.name != BUILTIN_TODO_LIST),
    );
    tools
}

/// Executes one worker tool call, showing it as a card. `None` when the user
/// stopped the run while it executed.
async fn execute_call(
    context: &RunContext<'_>,
    step: &RunbookStep,
    facts: &mut Facts,
    call: &OllamaToolCall,
    on_event: &mut impl FnMut(TurnEvent),
) -> Option<(CallRecord, String)> {
    let call_id = format!("runbook-{}", Uuid::new_v4());
    match call.name.as_str() {
        RUN_BLOCK => {
            let number = call.arguments.get("block").and_then(|block| {
                block
                    .as_u64()
                    .or_else(|| block.as_str().and_then(|text| text.trim().parse().ok()))
            });
            let block = number
                .and_then(|number| usize::try_from(number).ok())
                .and_then(|number| number.checked_sub(1))
                .and_then(|index| step.blocks.get(index))
                .filter(|block| block.offered());
            let Some(block) = block else {
                let label = format!(
                    "run_block {}",
                    number.map(|n| n.to_string()).unwrap_or_default()
                );
                let error = match number {
                    Some(number) => format!(
                        "There is no runnable block {number} in this step. Its runnable blocks are: {}.",
                        runnable_block_numbers(step)
                    ),
                    None => format!(
                        "That call named no block (it sent {}). Call run_block with the block's number, e.g. \
                         {{\"block\": 1}}. This step's runnable blocks are: {}.",
                        call.arguments,
                        runnable_block_numbers(step)
                    ),
                };
                return Some(refusal(&call_id, label, error, on_event));
            };
            if block.is_resolve() {
                return conflicts::run_conflict_pass(context, step, &block.code, on_event).await;
            }
            let set = string_map(call.arguments.get("set"));
            let prepared = prepare_block(&block.code, &set, facts);
            let label = format!("block {}", number.unwrap_or_default());
            run_shell_card(
                context,
                step,
                facts,
                &call_id,
                label,
                &prepared.display,
                &prepared.script,
                &set,
                on_event,
            )
            .await
        }
        SHELL => {
            let command = call
                .arguments
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if command.is_empty() {
                return Some(refusal(
                    &call_id,
                    "shell".to_string(),
                    "No command was given.".to_string(),
                    on_event,
                ));
            }
            if let Some(what) = changes_something(&command) {
                return Some(refusal(
                    &call_id,
                    format!("shell: {}", truncate_chars(&command, 160)),
                    format!(
                        "Refused: the shell tool is for looking at things, and {what}. Changes are made by the \
                         step's own blocks: run the one that applies, or reply with your summary — saying what \
                         went wrong, if a block failed."
                    ),
                    on_event,
                ));
            }
            let script = wrap_script(&command, facts, &assigned_names(&command));
            run_shell_card(
                context,
                step,
                facts,
                &call_id,
                format!("shell: {}", truncate_chars(&command, 160)),
                &command,
                &script,
                &Facts::new(),
                on_event,
            )
            .await
        }
        name => {
            if name == BUILTIN_WRITE_FILE || name == BUILTIN_REPLACE_LINES {
                let target = call
                    .arguments
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let resolved = absolute(target).unwrap_or_else(|| context.working_dir.join(target));
                if !normalize(&resolved).starts_with(normalize(context.working_dir)) {
                    let error = format!("Refused: `{target}` is outside the target directory.");
                    return Some(refusal(
                        &call_id,
                        format!("{name} {target}"),
                        error,
                        on_event,
                    ));
                }
            }
            on_event(TurnEvent::ToolCallStarted {
                call_id: call_id.clone(),
                name: name.to_string(),
                input: call.arguments.clone(),
            });
            let result =
                dispatch_builtin_tool(name, &call.arguments, context.working_dir, context.path)
                    .await;
            let (outcome, text) = match result {
                Ok(text) => (ToolCallOutcome::Success(json!({ "result": text })), text),
                Err(error) => (ToolCallOutcome::Error(error.clone()), error),
            };
            on_event(TurnEvent::ToolCallResult { call_id, outcome });
            let target = call
                .arguments
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some((
                CallRecord {
                    label: format!("{name} {target}"),
                    exit_code: None,
                    output: clip_output(&text),
                },
                clip_output(&text),
            ))
        }
    }
}

/// Runs a prepared shell script as a `Shell` card, so it renders exactly like
/// the harness's other commands, and folds the values it reports into `facts`.
#[allow(clippy::too_many_arguments)]
async fn run_shell_card(
    context: &RunContext<'_>,
    step: &RunbookStep,
    facts: &mut Facts,
    call_id: &str,
    label: String,
    display: &str,
    script: &str,
    set: &Facts,
    on_event: &mut impl FnMut(TurnEvent),
) -> Option<(CallRecord, String)> {
    if let Some(reason) = escapes_root(display, context.working_dir) {
        return Some(refusal(
            call_id,
            label,
            format!("Refused before running: {reason}."),
            on_event,
        ));
    }
    on_event(TurnEvent::ToolCallStarted {
        call_id: call_id.to_string(),
        name: "Shell".to_string(),
        input: json!({ "command": display }),
    });
    let Some(execution) = run_script(context, script).await else {
        on_event(TurnEvent::ToolCallResult {
            call_id: call_id.to_string(),
            outcome: ToolCallOutcome::Cancelled,
        });
        return None;
    };
    let (stdout, captured) = split_captured_facts(&execution.stdout);
    facts.extend(
        set.iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
    facts.extend(captured);
    let shown = ShellExecution {
        stdout,
        stderr: execution.stderr,
        exit_code: execution.exit_code,
    };
    on_event(TurnEvent::ToolCallResult {
        call_id: call_id.to_string(),
        outcome: shown.outcome(),
    });
    let mut output = shown.stdout.clone();
    if !shown.stderr.trim().is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&shown.stderr);
    }
    let output = if output.len() > LARGE_TEXT_CHARS {
        read_output_in_parts(context, step, &label, &output, on_event).await
    } else {
        clip_output(&output)
    };
    // `# must print` lines are checked here, against the real output, so a
    // judge reads a verdict instead of having to match values itself.
    let verdicts = run_print_checks(&print_checks(display), &shown.stdout, facts);
    let output = if verdicts.is_empty() {
        output
    } else {
        format!("{output}\n\n{}", verdicts.join("\n"))
    };
    let reply = format!("exit {}\n{output}", shown.exit_code);
    Some((
        CallRecord {
            label,
            exit_code: Some(shown.exit_code),
            output,
        },
        reply,
    ))
}

/// A block's output too long for the worker and judge to read whole: the
/// helper reads it in parts against the step's checks (see
/// `ollama_work_notes`), and the models get the answer with the output's
/// first and last lines.
async fn read_output_in_parts(
    context: &RunContext<'_>,
    step: &RunbookStep,
    label: &str,
    output: &str,
    on_event: &mut impl FnMut(TurnEvent),
) -> String {
    let checks = step.stated_checks();
    let checks = if checks.is_empty() {
        String::new()
    } else {
        format!(
            " The step's checks: {}.",
            truncate_chars(&checks.join("; "), 1_200)
        )
    };
    let question = format!(
        "This is the output of {label} in the runbook step \"{}\".{checks} What in it bears on those checks, \
         and did anything fail or warn?",
        step.title
    );
    let mut notes = WorkNotes::new(label, &question, output, &context.work_dir.join("notes"));
    // A helper that answers nothing at all ends the pass; the output then
    // goes to the worker clipped, as it would without one.
    let mut failures_in_a_row = 0;
    while let Some(next) = notes.next_call() {
        if context.cancelled() {
            break;
        }
        let think = context.helper_thinks.then_some(false);
        let reply = ask_fresh(
            context,
            context.helper,
            think,
            SHORT_REPLY_MAX_TOKENS,
            next.system,
            &next.user,
            on_event,
        )
        .await
        .ok()
        .flatten();
        if reply.is_some() {
            failures_in_a_row = 0;
        } else {
            failures_in_a_row += 1;
            if failures_in_a_row >= 3 {
                on_event(TurnEvent::Text {
                    text: format!(
                        "_The helper `{}` answered nothing three times running, so this output is shown clipped \
                         instead of read in parts._",
                        context.helper
                    ),
                });
                return clip_output(output);
            }
        }
        notes.accept(reply.as_deref());
    }
    match notes.answer() {
        Some(answer) => format!(
            "[{} lines, read in {} parts by `{}`; notes in {}]\n{answer}\n\n[The output's first and last lines:]\n{}",
            notes.total_lines(),
            notes.part_count(),
            context.helper,
            notes.notes_path().display(),
            clip_output(output)
        ),
        None => clip_output(output),
    }
}

/// One model call in a fresh conversation, holding only `system` and `user`;
/// `None` when it failed or the user stopped the run.
#[allow(clippy::too_many_arguments)]
async fn ask_fresh(
    context: &RunContext<'_>,
    model: &str,
    think: Option<bool>,
    max_tokens: u32,
    system: &str,
    user: &str,
    on_event: &mut impl FnMut(TurnEvent),
) -> std::io::Result<Option<String>> {
    let mut request = json!({
        "model": wire_name(model),
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "stream": true,
        "keep_alive": KEEP_ALIVE,
        "options": {
            "temperature": 0,
            "num_ctx": RUNBOOK_NUM_CTX,
            "num_predict": max_tokens,
        },
    });
    if let Some(think) = think {
        request["think"] = json!(think);
    }
    match chat_call(context, model, &request.to_string(), on_event).await? {
        Ok(response) => Ok(Some(response.content)),
        Err(_) => Ok(None),
    }
}

fn refusal(
    call_id: &str,
    label: String,
    error: String,
    on_event: &mut impl FnMut(TurnEvent),
) -> (CallRecord, String) {
    on_event(TurnEvent::ToolCallStarted {
        call_id: call_id.to_string(),
        name: "Shell".to_string(),
        input: json!({ "command": label }),
    });
    on_event(TurnEvent::ToolCallResult {
        call_id: call_id.to_string(),
        outcome: ToolCallOutcome::Error(error.clone()),
    });
    (
        CallRecord {
            label,
            exit_code: None,
            output: error.clone(),
        },
        error,
    )
}

/// The block numbers `run_block` accepts in `step`.
fn offered_block_numbers(step: &RunbookStep) -> BTreeSet<usize> {
    step.blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| block.offered())
        .map(|(index, _)| index + 1)
        .collect()
}

/// The block a `run_block` call names, if it is one.
fn run_block_number(call: &OllamaToolCall) -> Option<usize> {
    if call.name != RUN_BLOCK {
        return None;
    }
    let block = call.arguments.get("block")?;
    block
        .as_u64()
        .or_else(|| block.as_str().and_then(|text| text.trim().parse().ok()))
        .and_then(|number| usize::try_from(number).ok())
}

/// What a small worker's call means, as a call the driver runs:
/// - `run_block` with its number given some other way (`"number": 2`,
///   `2.0`, `"block 2"`) is that block;
/// - a shell command that is a block's text, pasted (llama3.1:8b does
///   this), is that block, so its values are captured, its checks run and
///   it counts as run.
///
/// A `run_block` with no number at all is not guessed at — the next block
/// may be one the runbook runs only when a check fails (step B's `git tag
/// -d`) — and gets a refusal that says what to send.
fn normalize_call(call: &OllamaToolCall, step: &RunbookStep) -> OllamaToolCall {
    let as_block = |number: usize, set: Option<&Value>| {
        let mut arguments = json!({ "block": number });
        if let Some(set) = set {
            arguments["set"] = set.clone();
        }
        OllamaToolCall {
            name: RUN_BLOCK.to_string(),
            arguments,
        }
    };
    match call.name.as_str() {
        RUN_BLOCK if run_block_number(call).is_none() => {
            match loose_block_number(&call.arguments) {
                Some(number) => as_block(number, call.arguments.get("set")),
                None => call.clone(),
            }
        }
        SHELL => {
            let command = call
                .arguments
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let pasted = step.blocks.iter().position(|block| {
                block.offered() && !block.is_resolve() && same_script(&block.code, command)
            });
            match pasted {
                Some(index) => as_block(index + 1, None),
                None => call.clone(),
            }
        }
        _ => call.clone(),
    }
}

/// A block number given some other way than `"block": 2`: under another
/// name, as `2.0`, or as text like `"block 2"`.
fn loose_block_number(arguments: &Value) -> Option<usize> {
    let parsed: Value;
    let arguments = match arguments {
        // Arguments sent as a JSON string, or as a bare number in one.
        Value::String(text) => {
            parsed = match serde_json::from_str::<Value>(text) {
                Ok(object @ Value::Object(_)) => object,
                _ => json!({ "block": text }),
            };
            &parsed
        }
        Value::Number(_) => {
            parsed = json!({ "block": arguments });
            &parsed
        }
        _ => arguments,
    };
    let object = arguments.as_object()?;
    let value = [
        "block",
        "number",
        "block_number",
        "blockNumber",
        "n",
        "index",
        "id",
    ]
    .iter()
    .find_map(|name| object.get(*name))
    .or_else(|| {
        object
            .iter()
            .filter(|(name, _)| name.as_str() != "set")
            .map(|(_, value)| value)
            .next()
            .filter(|_| object.len() == 1)
    })?;
    let number: u64 = match value {
        Value::Number(number) => number
            .as_f64()
            .filter(|number| number.fract() == 0.0 && *number >= 1.0)
            .map(|number| number as u64),
        Value::String(text) => text
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .ok(),
        _ => None,
    }?;
    usize::try_from(number).ok().filter(|number| *number >= 1)
}

/// Whether a pasted command is a block's script: the same lines, ignoring
/// indentation and blank lines.
fn same_script(code: &str, command: &str) -> bool {
    let lines = |text: &str| {
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let pasted = lines(command);
    !pasted.is_empty() && pasted == lines(code)
}

/// Sent back to a worker that stopped with block `number` still to run:
/// one the step runs in any case, or one whose case a check says holds.
fn run_block_nudge(number: usize, block: &RunbookBlock, checked: bool) -> String {
    let quoted = if block.lead_in.is_empty() {
        String::new()
    } else {
        format!(" (\"{}\")", truncate_chars(&block.lead_in, 200))
    };
    if checked {
        format!(
            "Block {number} of this step has not run yet, and a check of what the step has printed says the case \
             it runs in holds{quoted}: run it with run_block ({{\"block\": {number}}}). Then reply with your summary."
        )
    } else {
        format!(
            "Block {number} of this step has not run yet, and the step runs it in any case{quoted}: run it with \
             run_block ({{\"block\": {number}}}). Then reply with your summary — or reply now, if the step says \
             this doesn't apply here."
        )
    }
}

/// Asks the judge, in a fresh conversation, whether step `index`'s text or
/// output really says the rest of the run isn't needed. `true` too when no
/// answer comes back: the verdict stands.
async fn run_really_ends(
    context: &RunContext<'_>,
    runbook: &Runbook,
    index: usize,
    run: &WorkerRun,
    on_event: &mut impl FnMut(TurnEvent),
) -> std::io::Result<bool> {
    let step = &runbook.steps[index];
    let later = runbook.steps[index + 1..]
        .iter()
        .map(|step| format!("- {}", step.title))
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!(
        "The step — {}:\n\n{}\n\nWhat it ran, and the end of what each call printed:\n{}\n\nThe steps after it:\n{later}",
        step.title,
        truncate_chars(&step.body, 4_000),
        printed_digest(&run.records)
    );
    let reply = ask_judge_yes_or_no(context, DONE_CHECK_SYSTEM_PROMPT, &user, on_event).await?;
    Ok(!reply.is_some_and(|reply| answers_no(&reply)))
}

/// The blocks of a step that ran or that a condition check ruled out (and
/// no block before it has run since).
fn decided_blocks(run: &WorkerRun) -> BTreeSet<usize> {
    run.ran_blocks
        .iter()
        .copied()
        .chain(
            run.rulings
                .keys()
                .copied()
                .filter(|number| ruled_out_now(run, *number).is_some()),
        )
        .collect()
}

/// The ruling on block `number`'s case, while it still stands: no block
/// before it has run since the check.
fn ruling_now(run: &WorkerRun, number: usize) -> Option<&CaseRuling> {
    run.rulings
        .get(&number)
        .filter(|ruling| ruling.ran_before.iter().eq(run.ran_blocks.range(..number)))
}

/// Why block `number` was ruled out, while that still stands.
fn ruled_out_now(run: &WorkerRun, number: usize) -> Option<&str> {
    ruling_now(run, number)
        .filter(|ruling| !ruling.holds)
        .map(|ruling| ruling.why.as_str())
}

/// Whether a check found block `number`'s case to hold, and that still stands.
fn held_now(run: &WorkerRun, number: usize) -> bool {
    ruling_now(run, number).is_some_and(|ruling| ruling.holds)
}

fn record_holds(run: &mut WorkerRun, number: usize) {
    run.rulings.insert(
        number,
        CaseRuling {
            holds: true,
            unclear: false,
            why: String::new(),
            ran_before: run.ran_blocks.range(..number).copied().collect(),
        },
    );
}

/// Records that a check ruled block `number` out — a card, and a record the
/// judge reads — and returns what the worker is told.
fn rule_out(
    run: &mut WorkerRun,
    number: usize,
    block: &RunbookBlock,
    why: &str,
    on_event: &mut impl FnMut(TurnEvent),
) -> String {
    let (record, reply) = refusal(
        &format!("runbook-{}", Uuid::new_v4()),
        format!("block {number}"),
        format!(
            "Not run: block {number} runs only in the case the runbook states before it (\"{}\"), and a check of \
             what the step has printed says that case doesn't hold — {why}",
            truncate_chars(&block.lead_in, 200)
        ),
        on_event,
    );
    run.rulings.insert(
        number,
        CaseRuling {
            holds: false,
            unclear: false,
            why: reply.clone(),
            ran_before: run.ran_blocks.range(..number).copied().collect(),
        },
    );
    run.records.push(record);
    reply
}

/// Whether block `number`'s case holds, by the standing ruling or, failing
/// one, by a check now — recorded either way.
async fn case_holds(
    context: &RunContext<'_>,
    step: &RunbookStep,
    run: &mut WorkerRun,
    number: usize,
    block: &RunbookBlock,
    on_event: &mut impl FnMut(TurnEvent),
) -> std::io::Result<CaseCheck> {
    if held_now(run, number) {
        return Ok(CaseCheck::Holds);
    }
    if let Some(why) = ruled_out_now(run, number) {
        return Ok(CaseCheck::Fails(why.to_string()));
    }
    // "Otherwise" is the alternative to the conditional block before it, so
    // that block's ruling decides this one and no judge is asked. Asked,
    // qwen3.5:4b echoed the NO it had just given the block before.
    if block.is_otherwise() {
        if let Some(previous) = step.blocks[..number - 1]
            .iter()
            .rposition(RunbookBlock::has_condition)
            .map(|index| index + 1)
        {
            if ruling_now(run, previous).is_some_and(|ruling| !ruling.holds && !ruling.unclear) {
                record_holds(run, number);
                return Ok(CaseCheck::Holds);
            }
            if run.ran_blocks.contains(&previous) || held_now(run, previous) {
                let why = format!(
                    "block {previous}, which this block is the alternative to, is the one that applies"
                );
                rule_out(run, number, block, &why, on_event);
                return Ok(CaseCheck::Fails(why));
            }
        }
    }
    let check = check_case(context, step, number, block, &run.records, on_event).await?;
    match &check {
        CaseCheck::Holds => record_holds(run, number),
        CaseCheck::Fails(why) => {
            rule_out(run, number, block, why, on_event);
        }
        CaseCheck::Unknown => {}
    }
    Ok(check)
}

/// Whether the worker's `run_block` call is refused, and what it is told:
/// a block whose runbook text opens with the case it runs in runs only when
/// a check of the output so far says the case holds. qwen3.5:9b ran every
/// block of the merge step in turn, "abort the merge and STOP" among them,
/// whatever it was told. A check that can't say either way lets an ordinary
/// block run on the worker's judgement, not one the runbook stops at — that
/// one aborts or deletes something.
async fn gate(
    context: &RunContext<'_>,
    step: &RunbookStep,
    run: &mut WorkerRun,
    call: &OllamaToolCall,
    on_event: &mut impl FnMut(TurnEvent),
) -> std::io::Result<Option<String>> {
    let Some(number) = run_block_number(call) else {
        return Ok(None);
    };
    let Some(block) = step.blocks.get(number - 1) else {
        return Ok(None);
    };
    if !block.offered() || !block.has_condition() || run.ran_blocks.contains(&number) {
        return Ok(None);
    }
    Ok(
        match case_holds(context, step, run, number, block, on_event).await? {
            CaseCheck::Holds => None,
            CaseCheck::Fails(_) => Some(
                ruled_out_now(run, number)
                    .map(str::to_string)
                    .unwrap_or_default(),
            ),
            CaseCheck::Unknown if block.says_stop() => {
                let reply = rule_out(
                run,
                number,
                block,
                "the check gave no clear answer, and this block ends the step (the runbook says STOP), so it \
                 does not run on a guess. If its case holds, say so in your summary.",
                on_event,
            );
                if let Some(ruling) = run.rulings.get_mut(&number) {
                    ruling.unclear = true;
                }
                Some(reply)
            }
            CaseCheck::Unknown => None,
        },
    )
}

/// The block a worker that stopped should run next, if any: one it skipped
/// that the step runs in any case; else the first after the last that ran
/// that runs in any case, or whose case — put to the judge — holds. Blocks
/// whose case doesn't hold are ruled out on the way. With it, whether a
/// check said so.
async fn next_block_to_run(
    context: &RunContext<'_>,
    step: &RunbookStep,
    run: &mut WorkerRun,
    on_event: &mut impl FnMut(TurnEvent),
) -> std::io::Result<Option<(usize, bool)>> {
    if run.stopped_at.is_some() {
        return Ok(None);
    }
    let last_ran = run.ran_blocks.iter().max().copied().unwrap_or(0);
    let offered = offered_block_numbers(step);
    let skipped = offered.iter().copied().find(|number| {
        *number < last_ran
            && !run.ran_blocks.contains(number)
            && step
                .blocks
                .get(number - 1)
                .is_some_and(|block| !block.has_condition())
    });
    if let Some(number) = skipped {
        return Ok(Some((number, false)));
    }
    for number in offered.into_iter().filter(|number| *number > last_ran) {
        if ruled_out_now(run, number).is_some() {
            continue;
        }
        let Some(block) = step.blocks.get(number - 1) else {
            continue;
        };
        if !block.has_condition() {
            return Ok(Some((number, false)));
        }
        match case_holds(context, step, run, number, block, on_event).await? {
            CaseCheck::Fails(_) => {}
            CaseCheck::Holds => return Ok(Some((number, true))),
            // Not sure: no sending a worker into a block that may be the
            // one that deletes something.
            CaseCheck::Unknown => return Ok(None),
        }
    }
    Ok(None)
}

/// Blocks with `# must print` checks that neither ran nor were ruled out —
/// their case put to the judge now, if it wasn't — by number, with the
/// checks. A step with any can't pass: those checks are the runbook's own
/// measure of it.
async fn unmet_print_checks(
    context: &RunContext<'_>,
    step: &RunbookStep,
    run: &mut WorkerRun,
    on_event: &mut impl FnMut(TurnEvent),
) -> std::io::Result<Vec<(usize, String)>> {
    let mut unmet = Vec::new();
    for number in offered_block_numbers(step) {
        let Some(block) = step.blocks.get(number - 1) else {
            continue;
        };
        let checks = print_checks(&block.code);
        if checks.is_empty()
            || run.ran_blocks.contains(&number)
            || ruled_out_now(run, number).is_some()
        {
            continue;
        }
        if block.has_condition() {
            match case_holds(context, step, run, number, block, on_event).await? {
                CaseCheck::Fails(_) => continue,
                CaseCheck::Holds => {}
                // Overruling the judge takes a clear answer.
                CaseCheck::Unknown => continue,
            }
        }
        let checks = checks
            .iter()
            .map(|check| format!("\"{}\" must print {}", check.label, check.expected))
            .collect::<Vec<_>>()
            .join("; ");
        unmet.push((number, checks));
    }
    Ok(unmet)
}

/// Asks the judge, in a fresh conversation, whether the case a block runs in
/// holds for what the step has printed so far.
async fn check_case(
    context: &RunContext<'_>,
    step: &RunbookStep,
    number: usize,
    block: &RunbookBlock,
    records: &[CallRecord],
    on_event: &mut impl FnMut(TurnEvent),
) -> std::io::Result<CaseCheck> {
    let user = format!(
        "The step — {}:\n\n{}\n\nThe block in question is block {number}. Just before it, the runbook says: \
         \"{}\"\n\nThe block:\n```\n{}\n```\n\nWhat this step has run so far, and what it printed:\n\n{}",
        step.title,
        truncate_chars(&step.body, 5_000),
        block.lead_in,
        truncate_chars(&block.code, 1_500),
        last_chars(&records_text(&records_for_case_check(records)), 6_000),
    );
    let reply = ask_judge_yes_or_no(context, CONDITION_SYSTEM_PROMPT, &user, on_event).await?;
    let said = reply
        .as_deref()
        .map(|reply| truncate_chars(&reply.split_whitespace().collect::<Vec<_>>().join(" "), 240))
        .unwrap_or_default();
    match reply.as_deref().and_then(yes_or_no) {
        Some(true) => on_event(TurnEvent::Text {
            text: format!("↳ Block {number}'s case, checked: it holds — {said}"),
        }),
        None => on_event(TurnEvent::Text {
            text: format!("↳ Block {number}'s case, checked: no clear answer — {said}"),
        }),
        Some(false) => {}
    }
    Ok(match reply.as_deref().and_then(yes_or_no) {
        Some(true) => CaseCheck::Holds,
        Some(false) => CaseCheck::Fails(truncate_chars(
            &reply
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            240,
        )),
        None => CaseCheck::Unknown,
    })
}

/// The step's records as a case check sees them: a block refused on its own
/// case shows as that, without the words of the verdict that refused it —
/// a small judge, shown "NO" in the record, gave the next block the same.
fn records_for_case_check(records: &[CallRecord]) -> Vec<CallRecord> {
    records
        .iter()
        .map(|record| {
            let output = match record.output.split_once(" — ") {
                Some((before, _))
                    if record.exit_code.is_none() && before.starts_with("Not run:") =>
                {
                    format!("{before}.")
                }
                _ => record.output.clone(),
            };
            CallRecord {
                label: record.label.clone(),
                exit_code: record.exit_code,
                output,
            }
        })
        .collect()
}

/// Puts a YES-or-NO question to the judge in a fresh conversation. An answer
/// without its YES or NO — or none at all, a thinking judge having spent its
/// cap thinking (qwen3.5:4b did, twice, on step B's "Otherwise … push") — is
/// asked for once more, without thinking, in one word.
async fn ask_judge_yes_or_no(
    context: &RunContext<'_>,
    system: &str,
    user: &str,
    on_event: &mut impl FnMut(TurnEvent),
) -> std::io::Result<Option<String>> {
    let reply = ask_fresh(
        context,
        context.judge,
        context.judge_thinks.then_some(true),
        CONDITION_MAX_TOKENS,
        system,
        user,
        on_event,
    )
    .await?;
    if reply.as_deref().and_then(yes_or_no).is_some() {
        return Ok(reply);
    }
    ask_fresh(
        context,
        context.judge,
        context.judge_thinks.then_some(false),
        SHORT_REPLY_MAX_TOKENS,
        system,
        &format!(
            "{user}\n\nReply with one short reason, then the one line VERDICT: YES or VERDICT: NO."
        ),
        on_event,
    )
    .await
}

/// A reply's verdict: the word after its last `VERDICT:`, or, without one,
/// its first YES or NO. `Some(true)` for yes.
fn yes_or_no(reply: &str) -> Option<bool> {
    let upper = reply.to_ascii_uppercase();
    let words = |text: &str| {
        text.split(|c: char| !c.is_ascii_alphabetic())
            .filter(|word| !word.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let candidates = match upper.rfind("VERDICT:") {
        Some(at) => words(&upper[at + "VERDICT:".len()..]),
        None => words(&upper),
    };
    candidates.iter().find_map(|word| match word.as_str() {
        "YES" => Some(true),
        "NO" => Some(false),
        _ => None,
    })
}

/// Whether a reply's first YES or NO is a no.
fn answers_no(reply: &str) -> bool {
    yes_or_no(reply) == Some(false)
}

/// Where the step stands, told after every call: the next block after the
/// last that ran, with what the runbook says before it — and when that one
/// runs only in some case, those after it up to one that runs in any case.
/// qwen3.5:9b, three blocks into a merge, looked about with shell, resolved
/// a conflict by hand and ran the "abort and STOP" block meant for when the
/// conflict pass it never ran leaves something over.
fn next_blocks_hint(step: &RunbookStep, ran: &BTreeSet<usize>) -> Option<String> {
    let last_ran = ran.iter().max().copied().unwrap_or(0);
    let mut upcoming: Vec<(usize, &RunbookBlock)> = Vec::new();
    for (index, block) in step.blocks.iter().enumerate() {
        let number = index + 1;
        if number <= last_ran || !block.offered() || ran.contains(&number) {
            continue;
        }
        upcoming.push((number, block));
        if !block.has_condition() {
            break;
        }
    }
    let quoted = |block: &RunbookBlock| {
        if block.lead_in.is_empty() {
            String::new()
        } else {
            format!(" (\"{}\")", truncate_chars(&block.lead_in, 200))
        }
    };
    match upcoming.as_slice() {
        [] => None,
        [(number, block)] if !block.has_condition() => Some(format!(
            "Next in this step: block {number}{} — run_block {{\"block\": {number}}}.",
            quoted(block)
        )),
        _ => Some(format!(
            "What comes next in this step: {}. Each of these runs only if what it says holds for the output so \
             far: run those it holds for, and skip the rest.",
            upcoming
                .iter()
                .map(|(number, block)| {
                    let when = if block.has_condition() {
                        ", only in some cases"
                    } else {
                        ""
                    };
                    format!("block {number}{when}{}", quoted(block))
                })
                .collect::<Vec<_>>()
                .join("; ")
        )),
    }
}

/// `[2, 3, 4]` → "2, 3 and 4".
fn join_and(numbers: &[usize]) -> String {
    let words: Vec<String> = numbers.iter().map(ToString::to_string).collect();
    match words.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        Some((last, _)) => last.clone(),
        None => String::new(),
    }
}

/// What makes two calls the same call: for `run_block`, the block and the
/// values given; for anything else, the tool and its arguments.
fn call_signature(call: &OllamaToolCall) -> String {
    match run_block_number(call) {
        Some(number) => format!(
            "{RUN_BLOCK} {number} {:?}",
            string_map(call.arguments.get("set"))
        ),
        None => format!("{} {}", call.name, call.arguments),
    }
}

fn runnable_block_numbers(step: &RunbookStep) -> String {
    let numbers: Vec<String> = step
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| block.offered())
        .map(|(index, _)| (index + 1).to_string())
        .collect();
    if numbers.is_empty() {
        "none".to_string()
    } else {
        numbers.join(", ")
    }
}

fn string_map(value: Option<&Value>) -> Facts {
    value
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .filter(|(name, _)| is_fact_name(name))
                .filter_map(|(name, value)| {
                    let text = match value {
                        Value::String(text) => text.clone(),
                        Value::Number(number) => number.to_string(),
                        _ => return None,
                    };
                    Some((name.clone(), text))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Runs `script`, killing it if the user stops the run. `None` when stopped.
async fn run_script(context: &RunContext<'_>, script: &str) -> Option<ShellExecution> {
    let execution = execute_shell(script, context.working_dir, context.path);
    let Some(cancel) = context.cancel.clone() else {
        return Some(execution.await);
    };
    futures::pin_mut!(execution);
    match futures::future::select(execution, Box::pin(wait_for_cancel(cancel))).await {
        Either::Left((execution, _)) => Some(execution),
        // Dropping the shell's future kills the child (`kill_on_drop`).
        Either::Right(((), _)) => None,
    }
}

async fn wait_for_cancel(cancel: HarnessCancel) {
    while !cancel.is_cancelled() {
        Timer::after(Duration::from_millis(50)).await;
    }
}

/// Seconds to wait before each retry of a call that failed to load its model.
const LOAD_RETRY_WAITS: [u64; 2] = [5, 20];

/// Whether a failed call is the kind that passes: Ollama taking too long to
/// start a model's runner, or its service restarting. A worker, a judge and a
/// helper rarely fit in memory together, so models swap in and out all run
/// long, and a swap on a busy machine sometimes times out.
fn is_transient_failure(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "timed out waiting",
        "llama-server",
        "runner",
        "unexpectedly stopped",
        "terminated",
        "could not reach ollama",
        "unexpected eof",
    ]
    .iter()
    .any(|needle| error.contains(needle))
}

/// One streamed `/api/chat` call, tried again after a pause when the model
/// fails to load. Thinking streams live as Thinking events; content is
/// buffered. Outer error: curl could not run. Inner: the call failed, with a
/// message worth showing.
async fn chat_call(
    context: &RunContext<'_>,
    model: &str,
    request: &str,
    on_event: &mut impl FnMut(TurnEvent),
) -> std::io::Result<Result<OllamaChatResult, String>> {
    let mut retries = 0;
    loop {
        let result = chat_call_once(context, model, request, on_event).await?;
        let Err(error) = &result else {
            return Ok(result);
        };
        if retries >= LOAD_RETRY_WAITS.len() || context.cancelled() || !is_transient_failure(error)
        {
            return Ok(result);
        }
        let wait = LOAD_RETRY_WAITS[retries];
        retries += 1;
        on_event(TurnEvent::Text {
            text: format!(
                "_Ollama was slow to load `{model}` ({}); trying again in {wait}s._",
                truncate_chars(error.trim(), 120)
            ),
        });
        match context.cancel.clone() {
            Some(cancel) => {
                let pause = Timer::after(Duration::from_secs(wait));
                futures::pin_mut!(pause);
                if let Either::Right(_) =
                    futures::future::select(pause, Box::pin(wait_for_cancel(cancel))).await
                {
                    return Ok(Err("stopped".to_string()));
                }
            }
            None => {
                Timer::after(Duration::from_secs(wait)).await;
            }
        }
    }
}

async fn chat_call_once(
    context: &RunContext<'_>,
    model: &str,
    request: &str,
    on_event: &mut impl FnMut(TurnEvent),
) -> std::io::Result<Result<OllamaChatResult, String>> {
    let accumulator = Arc::new(Mutex::new(OllamaChatAccumulator::default()));
    let parser_accumulator = Arc::clone(&accumulator);
    let status = run_jsonl_stream_cancellable(
        new_ollama_chat_command(context.working_dir, context.path, base_url_of(model)),
        request,
        context.cancel.clone(),
        move |line| {
            parser_accumulator
                .lock()
                .map(|mut accumulator| accumulator.push_line(line))
                .unwrap_or_default()
        },
        &mut *on_event,
    )
    .await?;
    if context.cancelled() {
        return Ok(Err("stopped".to_string()));
    }
    if !status.success() {
        return Ok(Err(format!(
            "could not reach Ollama for `{model}` at {}",
            base_url_of(model)
        )));
    }
    Ok(accumulator
        .lock()
        .map(|accumulator| accumulator.clone().finish(model))
        .unwrap_or_else(|_| Err("Ollama response state became unavailable".to_string())))
}

/// Asks the judge for a verdict on one step, once more if the first answer
/// can't be read. `None` when no verdict came back.
async fn judge_step(
    context: &RunContext<'_>,
    runbook: &Runbook,
    step: &RunbookStep,
    facts_before: &Facts,
    worker_run: &WorkerRun,
    on_event: &mut impl FnMut(TurnEvent),
) -> std::io::Result<Option<Verdict>> {
    let mut messages = vec![
        json!({ "role": "system", "content": JUDGE_SYSTEM_PROMPT }),
        json!({
            "role": "user",
            "content": judge_user_prompt(runbook, step, facts_before, worker_run),
        }),
    ];
    for _ in 0..2 {
        if context.cancelled() {
            return Ok(None);
        }
        let mut request = json!({
            "model": wire_name(context.judge),
            "messages": messages,
            "stream": true,
            "keep_alive": KEEP_ALIVE,
            "options": {
                "temperature": 0,
                "num_ctx": RUNBOOK_NUM_CTX,
                "num_predict": JUDGE_MAX_TOKENS,
            },
        });
        if context.judge_thinks {
            request["think"] = json!(true);
        }
        let Ok(response) =
            chat_call(context, context.judge, &request.to_string(), on_event).await?
        else {
            return Ok(None);
        };
        if let Some(verdict) = parse_verdict(&response.content) {
            return Ok(Some(verdict));
        }
        messages.push(json!({ "role": "assistant", "content": response.content }));
        messages.push(json!({
            "role": "user",
            "content": "Reply with the JSON object only: {\"verdict\": …, \"reason\": …, \"facts\": {…}}.",
        }));
    }
    Ok(None)
}

/// The run's closing report: the runbook's own template filled in by the
/// judge when it has one, else a plain list.
async fn write_report(
    context: &RunContext<'_>,
    runbook: &Runbook,
    results: &[(String, StepResult)],
    facts: &Facts,
    on_event: &mut impl FnMut(TurnEvent),
) -> std::io::Result<String> {
    let record = run_record(results, false);
    let plain = format!("**Run record**\n\n{record}\n\nValues found: {}", {
        let inline = facts_inline(facts);
        if inline.is_empty() {
            "none".to_string()
        } else {
            inline
        }
    });
    let Some(template) = &runbook.report_template else {
        return Ok(plain);
    };
    let mut request = json!({
        "model": wire_name(context.judge),
        "messages": [
            { "role": "system", "content": REPORT_SYSTEM_PROMPT },
            {
                "role": "user",
                "content": format!(
                    "Template:\n{template}\n\nRun record:\n{}\n\nValues found:\n{}",
                    run_record(results, true),
                    facts_lines(facts)
                ),
            },
        ],
        "stream": true,
        "keep_alive": KEEP_ALIVE,
        "options": {
            "temperature": 0,
            "num_ctx": RUNBOOK_NUM_CTX,
            "num_predict": REPORT_MAX_TOKENS,
        },
    });
    // Filling in a template wants no reasoning: qwen3.5:9b, left to think,
    // spent the whole cap on it and wrote nothing.
    if context.judge_thinks {
        request["think"] = json!(false);
    }
    let request = request.to_string();
    match chat_call(context, context.judge, &request, on_event).await? {
        Ok(response) if !response.content.trim().is_empty() => Ok(format!(
            "{plain}\n\n**Report** (the runbook's template)\n\n```text\n{}\n```",
            strip_fence(response.content.trim())
        )),
        _ => Ok(plain),
    }
}

/// Each step's verdict and reason, one line each; with `printed`, what its
/// calls printed under it, for the report writer.
fn run_record(results: &[(String, StepResult)], printed: bool) -> String {
    results
        .iter()
        .map(|(title, result)| match result {
            StepResult::Judged(kind, reason, digest) => {
                let line = format!("- {title}: {} — {reason}", verdict_word(*kind));
                if printed && !digest.is_empty() {
                    format!(
                        "{line}\n  What it ran, and the end of what each call printed:\n{digest}"
                    )
                } else {
                    line
                }
            }
            StepResult::NotReached => format!("- {title}: not reached"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn verdict_word(kind: VerdictKind) -> &'static str {
    match kind {
        VerdictKind::Pass => "passed",
        VerdictKind::Skip => "skipped",
        VerdictKind::Done => "done (nothing more to do)",
        VerdictKind::Stop => "STOPPED",
    }
}

fn strip_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let rest = rest.split_once('\n').map(|(_, rest)| rest).unwrap_or(rest);
    rest.strip_suffix("```").unwrap_or(rest).trim_end()
}

/// Keeps the head and the tail of long output, where context and errors are.
pub(crate) fn clip_output(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= OUTPUT_HEAD_CHARS + OUTPUT_TAIL_CHARS {
        return text.to_string();
    }
    let head: String = chars[..OUTPUT_HEAD_CHARS].iter().collect();
    let tail: String = chars[chars.len() - OUTPUT_TAIL_CHARS..].iter().collect();
    format!(
        "{head}\n… [{} characters omitted] …\n{tail}",
        chars.len() - OUTPUT_HEAD_CHARS - OUTPUT_TAIL_CHARS
    )
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{cut}…")
}

#[path = "ollama_runbook_conflicts.rs"]
mod conflicts;

#[cfg(test)]
#[path = "ollama_runbook_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "ollama_runbook_replay_tests.rs"]
mod replay_tests;
