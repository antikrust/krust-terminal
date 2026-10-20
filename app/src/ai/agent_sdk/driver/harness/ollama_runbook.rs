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

use std::collections::BTreeMap;
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

/// Model calls a worker may make on one step before the step is called off.
const MAX_WORKER_CALLS: usize = 16;
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
    let mut current: Option<(String, Vec<&str>)> = None;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if is_fence(trimmed) {
            match current.take() {
                Some((lang, lines)) => blocks.push(RunbookBlock {
                    lang,
                    code: lines.join("\n"),
                }),
                None => current = Some((fence_lang(trimmed), Vec::new())),
            }
            continue;
        }
        if let Some((_, lines)) = current.as_mut() {
            lines.push(line);
        }
    }
    blocks
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

fn simple_commands(script: &str) -> Vec<String> {
    script
        .replace("&&", "\n")
        .replace("||", "\n")
        .split(['\n', ';', '|', '(', ')', '{', '}'])
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
- When you are done, reply in plain text: what you ran and what the output showed. Do not start another step.";

const JUDGE_SYSTEM_PROMPT: &str = "You check one step of a runbook run. Another model ran it; you only read \
what happened. Decide from the step's own success criteria and the actual command output below — never from what \
the worker says it did.

Reply with one JSON object and nothing else:
{\"verdict\": \"pass\" | \"skip\" | \"done\" | \"stop\", \"reason\": \"one or two sentences quoting the output \
that decided it\", \"facts\": {\"NAME\": \"value\"}}

- pass: every check this step states holds in the output.
- skip: the step says it does not apply in this situation, and the output proves that.
- done: the step concluded there is nothing left to do (for example, already up to date). The whole run ends here, \
successfully.
- stop: a check failed, the step says to STOP, a command failed in a way the step does not expect, or the output \
you would need is missing. When in doubt, stop.
- facts: UPPERCASE values that a later step will need and that this step established (for example, the version it \
found). Leave out values already known.";

const REPORT_SYSTEM_PROMPT: &str = "You write the final report of a runbook run by filling in the runbook's own \
reporting template from the run record. Use only what the record shows; write n/a for anything the run did not \
reach. Reply with the filled-in template only.";

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
    records: &[CallRecord],
    summary: &str,
) -> String {
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
         What was run, in order:\n\n{}\n\nThe worker's summary:\n{}",
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
    summary: String,
    usage: Option<TurnUsage>,
    cancelled: bool,
    /// The turn broke down (service unreachable, call limit) before the step
    /// could be judged.
    failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum StepResult {
    Judged(VerdictKind, String),
    NotReached,
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
        finish_with_text(
            &mut on_event,
            format!(
                "**Runbook mode** — give me the runbook file to follow, as a path (absolute, `~/…`, or relative to \
                 `{}`). The runbook runs in the folder this terminal is in, so `cd` into the sandbox first.",
                working_dir.display()
            ),
        );
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

    if let Err(error) = ensure_ollama_service(path).await {
        emit_failed_turn(
            &mut on_event,
            format!("Could not start the local Ollama service: {error}"),
        );
        return Err(error);
    }
    match probe_model_caps_checked(worker, path).await {
        Some(caps) if caps.tools => {}
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
    }
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
        let worker_run = run_worker(
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

        let new_facts: Facts = verdict
            .facts
            .iter()
            .filter(|(name, value)| facts.get(*name) != Some(*value))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let reason = if verdict.reason.is_empty() {
            "no reason given".to_string()
        } else {
            verdict.reason.clone()
        };
        results[index].1 = StepResult::Judged(verdict.kind, reason.clone());
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

    for call_index in 0..=MAX_WORKER_CALLS {
        if context.cancelled() {
            run.cancelled = true;
            return Ok(run);
        }
        let request = json!({
            "model": wire_name(context.worker),
            "messages": messages,
            "tools": tool_defs,
            "stream": true,
            "keep_alive": KEEP_ALIVE,
            "options": { "temperature": 0, "num_ctx": RUNBOOK_NUM_CTX },
        })
        .to_string();
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
            run.summary = response.content;
            return Ok(run);
        };
        if call_index == MAX_WORKER_CALLS {
            run.failure = Some(format!(
                "the worker made {MAX_WORKER_CALLS} calls on this step without finishing it"
            ));
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
            let Some((record, reply)) = execute_call(context, step, facts, call, on_event).await
            else {
                run.cancelled = true;
                return Ok(run);
            };
            run.records.push(record);
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
            .filter(|tool| tool.name != BUILTIN_RUN_CODE),
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
                let error = format!(
                    "There is no runnable block {} in this step. Its runnable blocks are: {}.",
                    number
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "(missing number)".to_string()),
                    runnable_block_numbers(step)
                );
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
            let script = wrap_script(&command, facts, &assigned_names(&command));
            run_shell_card(
                context,
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
            if name == BUILTIN_WRITE_FILE {
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
                        format!("write_file {target}"),
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
    let output = clip_output(&output);
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

/// One streamed `/api/chat` call. Thinking streams live as Thinking events;
/// content is buffered. Outer error: curl could not run. Inner: the call
/// failed, with a message worth showing.
async fn chat_call(
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
            "content": judge_user_prompt(runbook, step, facts_before, &worker_run.records, &worker_run.summary),
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
            "options": { "temperature": 0, "num_ctx": RUNBOOK_NUM_CTX },
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
    let record = results
        .iter()
        .map(|(title, result)| match result {
            StepResult::Judged(kind, reason) => {
                format!("- {title}: {} — {reason}", verdict_word(*kind))
            }
            StepResult::NotReached => format!("- {title}: not reached"),
        })
        .collect::<Vec<_>>()
        .join("\n");
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
    let request = json!({
        "model": wire_name(context.judge),
        "messages": [
            { "role": "system", "content": REPORT_SYSTEM_PROMPT },
            {
                "role": "user",
                "content": format!(
                    "Template:\n{template}\n\nRun record:\n{record}\n\nValues found:\n{}",
                    facts_lines(facts)
                ),
            },
        ],
        "stream": true,
        "keep_alive": KEEP_ALIVE,
        "options": { "temperature": 0, "num_ctx": RUNBOOK_NUM_CTX },
    })
    .to_string();
    match chat_call(context, context.judge, &request, on_event).await? {
        Ok(response) if !response.content.trim().is_empty() => Ok(format!(
            "{plain}\n\n**Report** (the runbook's template)\n\n```text\n{}\n```",
            strip_fence(response.content.trim())
        )),
        _ => Ok(plain),
    }
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
