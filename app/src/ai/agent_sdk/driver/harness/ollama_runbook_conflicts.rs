//! The work-file pass: merge conflicts worked one at a time.
//!
//! Merging a big upstream release can leave dozens of conflicts across files
//! thousands of lines long — far more than a 14B model can hold at once, and a
//! model asked to fix a whole file rewrites it and loses half of it. So the
//! driver slices the work itself, at the conflict markers:
//!
//! - files the runbook's table says to keep or take whole are done by rule,
//!   with no model at all;
//! - every other conflict goes to the analyst (the run's judge model) in a
//!   fresh conversation holding only that conflict, the runbook's rule for the
//!   file, the project's own patch for it and how a reference branch carries
//!   the same code — and comes back as a short note with the merged lines;
//! - the helper (a small model) checks each proposed fix before the driver
//!   splices it over exactly that conflict's lines, so no model ever writes a
//!   whole file;
//! - every note is saved to a small work file per source file, with line
//!   numbers and a progress line at the bottom, so a stopped run picks up where
//!   it left off and the human gets a readable record of what was changed and
//!   what was left for them.
//!
//! A runbook asks for the pass with a fenced `resolve` block, run with
//! `run_block` like any other; its optional `reference:` line names a git ref
//! to consult, and `work:` overrides where the notes go.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use ai::agent::{ToolCallOutcome, TurnEvent};

use super::{
    ask_fresh, clip_output, normalize, run_script, shell_quote, truncate_chars, CallRecord,
    RunContext, RunbookStep,
};

/// Unchanged lines shown above and below a conflict.
const CONTEXT_LINES: usize = 12;
/// A conflict longer than this goes to the human: a model merging it would
/// be guessing.
const MAX_CONFLICT_LINES: usize = 200;
/// Caps on the reference material shown with one conflict.
const MAX_PATCH_LINES: usize = 80;
const MAX_REFERENCE_LINES: usize = 80;
/// A file larger than this is left whole for the human.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const STATE_FILE: &str = "state.json";
const PROGRESS_FILE: &str = "progress.md";

// ---------------------------------------------------------------------------
// Conflicts
// ---------------------------------------------------------------------------

/// One conflict block, as git wrote it into a file.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Conflict {
    /// 1-based lines of the `<<<<<<<` and `>>>>>>>` markers.
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
    pub(crate) ours: Vec<String>,
    /// Present when git wrote the common ancestor too (`--conflict=diff3`).
    pub(crate) base: Option<Vec<String>>,
    pub(crate) theirs: Vec<String>,
}

impl Conflict {
    fn line_count(&self) -> usize {
        self.end_line + 1 - self.start_line
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Section {
    Ours,
    Base,
    Theirs,
}

/// `<<<<<<<`, `|||||||` or `>>>>>>>`: seven of `c`, then the end of the line
/// or a space and a label.
fn is_marker(line: &str, c: char) -> bool {
    let line = line.trim_end_matches(['\n', '\r']);
    let marker: String = std::iter::repeat_n(c, 7).collect();
    line.strip_prefix(marker.as_str())
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
}

fn is_separator(line: &str) -> bool {
    line.trim_end() == "======="
}

/// Whether `text` carries any conflict marker line — used on a proposed fix,
/// which must have none.
pub(crate) fn has_conflict_markers(text: &str) -> bool {
    text.lines().any(|line| {
        is_marker(line, '<') || is_marker(line, '|') || is_marker(line, '>') || is_separator(line)
    })
}

/// Whether a file is still conflicted: any conflict block, or a stray opening
/// or closing marker. A lone `=======` line is not counted, since text files
/// can hold one legitimately.
pub(crate) fn has_open_conflicts(text: &str) -> bool {
    !find_conflicts(text).is_empty()
        || text
            .lines()
            .any(|line| is_marker(line, '<') || is_marker(line, '>'))
}

/// Every well-formed conflict in `text`, in order. A malformed one (a block
/// that never closes) is skipped rather than guessed at.
pub(crate) fn find_conflicts(text: &str) -> Vec<Conflict> {
    let lines: Vec<&str> = text.lines().collect();
    let mut conflicts = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if !is_marker(lines[index], '<') {
            index += 1;
            continue;
        }
        let start = index;
        let mut section = Section::Ours;
        let mut ours = Vec::new();
        let mut base: Option<Vec<String>> = None;
        let mut theirs = Vec::new();
        let mut end = None;
        let mut cursor = start + 1;
        while cursor < lines.len() {
            let line = lines[cursor];
            if section == Section::Ours && is_marker(line, '|') {
                section = Section::Base;
                base = Some(Vec::new());
            } else if section != Section::Theirs && is_separator(line) {
                section = Section::Theirs;
            } else if section == Section::Theirs && is_marker(line, '>') {
                end = Some(cursor);
                break;
            } else if is_marker(line, '<') {
                break;
            } else {
                let line = line.to_string();
                match section {
                    Section::Ours => ours.push(line),
                    Section::Base => base.get_or_insert_with(Vec::new).push(line),
                    Section::Theirs => theirs.push(line),
                }
            }
            cursor += 1;
        }
        match end {
            Some(end) => {
                conflicts.push(Conflict {
                    start_line: start + 1,
                    end_line: end + 1,
                    ours,
                    base,
                    theirs,
                });
                index = end + 1;
            }
            None => index = cursor.max(start + 1),
        }
    }
    conflicts
}

/// Replaces `conflict` (markers included) with `resolution`, keeping the
/// file's line endings.
pub(crate) fn splice(text: &str, conflict: &Conflict, resolution: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let ending = if lines
        .get(conflict.start_line - 1)
        .is_some_and(|line| line.ends_with("\r\n"))
    {
        "\r\n"
    } else {
        "\n"
    };
    let mut out = String::with_capacity(text.len());
    for line in &lines[..conflict.start_line - 1] {
        out.push_str(line);
    }
    for line in resolution.lines() {
        out.push_str(line);
        out.push_str(ending);
    }
    for line in lines.iter().skip(conflict.end_line) {
        out.push_str(line);
    }
    out
}

/// A stable key for one conflict, so a later run recognizes it after line
/// numbers have moved: FNV-1a over the file and the conflict's three sides.
pub(crate) fn conflict_key(file: &str, conflict: &Conflict) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |text: &str| {
        for byte in text.bytes().chain(std::iter::once(0)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
    feed(file);
    feed(&conflict.ours.join("\n"));
    feed(
        &conflict
            .base
            .as_ref()
            .map(|base| base.join("\n"))
            .unwrap_or_default(),
    );
    feed(&conflict.theirs.join("\n"));
    format!("{hash:016x}")
}

// ---------------------------------------------------------------------------
// The runbook's table
// ---------------------------------------------------------------------------

/// What a runbook's table says to do when a file conflicts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RuleAction {
    /// Keep this branch's whole file.
    KeepOurs,
    /// Take the incoming whole file.
    TakeTheirs,
    /// The file must not conflict; if it does, stop.
    Stop,
    /// Merge the two, conflict by conflict.
    Merge,
}

/// One row of the table that tells an operator how to resolve conflicts.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TableRule {
    /// Backticked paths and globs from the row's first cell.
    pub(crate) patterns: Vec<String>,
    /// The row's first cell mentions patches: it covers any file a patch in
    /// one of `patterns` (a directory) touches.
    pub(crate) by_patch: bool,
    pub(crate) change: String,
    pub(crate) on_conflict: String,
    pub(crate) action: RuleAction,
}

/// The rows of a step's conflict table: any markdown table with at least three
/// columns whose first column holds backticked paths.
pub(crate) fn table_rules(step_body: &str) -> Vec<TableRule> {
    let mut rules = Vec::new();
    for line in step_body.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() < 3 || cells[0].chars().all(|c| c == '-' || c == ':' || c == ' ') {
            continue;
        }
        let patterns: Vec<String> = backticked(cells[0])
            .into_iter()
            .flat_map(|pattern| expand_braces(&pattern))
            .collect();
        if patterns.is_empty() {
            continue;
        }
        let on_conflict = cells[cells.len() - 1].to_string();
        rules.push(TableRule {
            by_patch: cells[0].to_ascii_lowercase().contains("patch"),
            patterns,
            change: cells[1].to_string(),
            action: rule_action(&on_conflict),
            on_conflict,
        });
    }
    rules
}

fn backticked(cell: &str) -> Vec<String> {
    cell.split('`')
        .enumerate()
        .filter(|(index, _)| index % 2 == 1)
        .map(|(_, text)| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .collect()
}

/// `dom/a.{cpp,h}` → `dom/a.cpp`, `dom/a.h` (one level, which is all a table
/// uses).
fn expand_braces(pattern: &str) -> Vec<String> {
    let (Some(open), Some(close)) = (pattern.find('{'), pattern.find('}')) else {
        return vec![pattern.to_string()];
    };
    if close < open {
        return vec![pattern.to_string()];
    }
    pattern[open + 1..close]
        .split(',')
        .map(|choice| {
            format!(
                "{}{}{}",
                &pattern[..open],
                choice.trim(),
                &pattern[close + 1..]
            )
        })
        .collect()
}

fn rule_action(on_conflict: &str) -> RuleAction {
    // Drop asides like "(the next block)" before reading the instruction.
    let mut plain = String::new();
    let mut depth = 0usize;
    for c in on_conflict.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => plain.push(c),
            _ => {}
        }
    }
    let plain = plain
        .trim()
        .trim_end_matches('.')
        .trim()
        .replace('’', "'")
        .to_ascii_lowercase();
    if plain
        .split(|c: char| !c.is_ascii_alphabetic())
        .any(|word| word == "stop")
    {
        RuleAction::Stop
    } else if plain == "keep ours" {
        RuleAction::KeepOurs
    } else if matches!(
        plain.as_str(),
        "take mozilla's" | "take theirs" | "take mozilla's version"
    ) {
        RuleAction::TakeTheirs
    } else {
        RuleAction::Merge
    }
}

fn pattern_matches(pattern: &str, file: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("**") {
        return file.starts_with(prefix);
    }
    pattern == file
}

/// The table row covering `file`, if any. `patched` is every file a patch in
/// the project's patch directories touches.
pub(crate) fn rule_for<'a>(
    rules: &'a [TableRule],
    file: &str,
    patched: &BTreeMap<String, String>,
) -> Option<&'a TableRule> {
    rules
        .iter()
        .find(|rule| !rule.by_patch && rule.patterns.iter().any(|p| pattern_matches(p, file)))
        .or_else(|| {
            rules
                .iter()
                .find(|rule| rule.by_patch && patched.contains_key(file))
        })
}

// ---------------------------------------------------------------------------
// Context for one conflict
// ---------------------------------------------------------------------------

/// Each file the patches in `dir` touch, mapped to that file's section of the
/// patch (header included), labelled with the patch's name.
pub(crate) fn patch_sections(dir: &Path) -> BTreeMap<String, String> {
    let mut sections = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return sections;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext == "patch" || ext == "diff")
        })
        .collect();
    paths.sort();
    for path in paths {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        for (file, section) in split_patch(&text) {
            sections
                .entry(file)
                .or_insert_with(|| format!("from {name}:\n{section}"));
        }
    }
    sections
}

/// A unified diff split into its per-file sections, keyed by the new path.
pub(crate) fn split_patch(text: &str) -> Vec<(String, String)> {
    let mut sections: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let file = rest
                .split(" b/")
                .nth(1)
                .unwrap_or_default()
                .trim()
                .to_string();
            sections.push((file, String::new()));
        }
        if let Some((_, section)) = sections.last_mut() {
            section.push_str(line);
            section.push('\n');
        }
    }
    sections.retain(|(file, _)| !file.is_empty());
    sections
}

/// The hunks of `diff` that touch the code in `conflict`: those whose removed
/// or unchanged lines include a distinctive line of the incoming side or the
/// ancestor. Capped, since a reference is a hint, not the answer.
pub(crate) fn related_hunks(diff: &str, conflict: &Conflict, max_lines: usize) -> String {
    let anchors: BTreeSet<&str> = conflict
        .theirs
        .iter()
        .chain(conflict.base.iter().flatten())
        .map(|line| line.trim())
        .filter(|line| line.len() >= 12)
        .collect();
    if anchors.is_empty() {
        return String::new();
    }
    let mut hunks: Vec<Vec<&str>> = Vec::new();
    for line in diff.lines() {
        if line.starts_with("@@") {
            hunks.push(vec![line]);
        } else if let Some(hunk) = hunks.last_mut() {
            if !line.starts_with("diff --git") {
                hunk.push(line);
            }
        }
    }
    let mut out: Vec<&str> = Vec::new();
    for hunk in hunks {
        let touches = hunk.iter().skip(1).any(|line| {
            (line.starts_with(' ') || line.starts_with('-')) && anchors.contains(line[1..].trim())
        });
        if touches {
            out.extend(hunk);
        }
        if out.len() >= max_lines {
            break;
        }
    }
    out.truncate(max_lines);
    out.join("\n")
}

fn numbered(lines: &[&str], first_line: usize) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(offset, line)| format!("{:>6}  {line}", first_line + offset))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The conflict as the analyst reads it: a few lines either side, then the
/// three sides, each labelled.
fn conflict_excerpt(text: &str, conflict: &Conflict) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let before_start = conflict.start_line.saturating_sub(1 + CONTEXT_LINES);
    let before = &lines[before_start..conflict.start_line - 1];
    let after_end = (conflict.end_line + CONTEXT_LINES).min(lines.len());
    let after = &lines[conflict.end_line.min(lines.len())..after_end];
    let mut out = String::new();
    if !before.is_empty() {
        out.push_str(&format!(
            "Lines before the conflict:\n{}\n\n",
            numbered(before, before_start + 1)
        ));
    }
    out.push_str(&format!(
        "OURS (this branch):\n{}\n\n",
        side_text(&conflict.ours)
    ));
    if let Some(base) = &conflict.base {
        out.push_str(&format!(
            "BASE (the common ancestor):\n{}\n\n",
            side_text(base)
        ));
    }
    out.push_str(&format!(
        "THEIRS (being merged in):\n{}\n",
        side_text(&conflict.theirs)
    ));
    if !after.is_empty() {
        out.push_str(&format!(
            "\nLines after the conflict:\n{}\n",
            numbered(after, conflict.end_line + 1)
        ));
    }
    out
}

fn side_text(lines: &[String]) -> String {
    if lines.is_empty() {
        "(nothing)".to_string()
    } else {
        format!("```\n{}\n```", lines.join("\n"))
    }
}

// ---------------------------------------------------------------------------
// The models' answers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    fn word(self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Medium => "medium",
            Confidence::Low => "low",
        }
    }
}

/// The analyst's note on one conflict.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Analysis {
    pub(crate) ours: String,
    pub(crate) theirs: String,
    pub(crate) confidence: Confidence,
    pub(crate) why: String,
    /// The merged lines; `None` when the analyst handed it to the human.
    pub(crate) resolution: Option<String>,
}

/// Reads the analyst's reply: `OURS:`, `THEIRS:`, `CONFIDENCE:` and `WHY:`
/// lines, then the merged lines in the last fenced block, or `HUMAN: why`.
pub(crate) fn parse_analysis(content: &str) -> Option<Analysis> {
    let field = |name: &str| -> Option<String> {
        content.lines().find_map(|line| {
            let line = line.trim().trim_start_matches(['*', '-', ' ']);
            let (key, value) = line.split_once(':')?;
            (key.trim_matches('*').trim().eq_ignore_ascii_case(name))
                .then(|| value.trim().trim_matches('*').trim().to_string())
        })
    };
    if let Some(reason) = field("HUMAN") {
        return Some(Analysis {
            ours: field("OURS").unwrap_or_default(),
            theirs: field("THEIRS").unwrap_or_default(),
            confidence: Confidence::Low,
            why: reason,
            resolution: None,
        });
    }
    let resolution = last_fenced_block(content)?;
    let confidence = match field("CONFIDENCE").unwrap_or_default().to_ascii_lowercase() {
        word if word.starts_with("high") => Confidence::High,
        word if word.starts_with("medium") => Confidence::Medium,
        _ => Confidence::Low,
    };
    Some(Analysis {
        ours: field("OURS").unwrap_or_default(),
        theirs: field("THEIRS").unwrap_or_default(),
        confidence,
        why: field("WHY").unwrap_or_default(),
        resolution: Some(resolution),
    })
}

/// The contents of the last fenced code block in `content`.
pub(crate) fn last_fenced_block(content: &str) -> Option<String> {
    let mut last = None;
    let mut current: Option<Vec<&str>> = None;
    for line in content.lines() {
        if line.trim_start().starts_with("```") {
            match current.take() {
                Some(lines) => last = Some(lines.join("\n")),
                None => current = Some(Vec::new()),
            }
            continue;
        }
        if let Some(lines) = current.as_mut() {
            lines.push(line);
        }
    }
    last
}

/// What the helper made of a proposed fix.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Check {
    /// Write the proposed lines as they are.
    Apply,
    /// Write these lines instead.
    Edit(String),
    /// Leave the conflict for the human, for this reason.
    Human(String),
}

pub(crate) fn parse_check(content: &str) -> Check {
    let first = content
        .lines()
        .map(|line| line.trim().trim_matches('*').trim())
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let upper = first.to_ascii_uppercase();
    if upper.starts_with("APPLY") {
        Check::Apply
    } else if upper.starts_with("EDIT") {
        match last_fenced_block(content) {
            Some(lines) => Check::Edit(lines),
            None => Check::Human("the helper asked to edit the fix but gave no lines".to_string()),
        }
    } else if upper.starts_with("HUMAN") {
        let reason = first
            .split_once(':')
            .map(|(_, reason)| reason.trim().to_string())
            .filter(|reason| !reason.is_empty())
            .unwrap_or_else(|| "the helper wasn't sure".to_string());
        Check::Human(reason)
    } else {
        Check::Human("the helper's answer was unreadable".to_string())
    }
}

// ---------------------------------------------------------------------------
// The work notes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EntryStatus {
    /// Written into the file.
    Applied,
    /// A fix is in the notes but was not written: outside what the runbook
    /// lets the pass change.
    Proposed,
    /// Left for the human.
    LeftForYou,
}

/// One conflict's record in the work notes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkEntry {
    pub(crate) key: String,
    pub(crate) file: String,
    pub(crate) conflict: usize,
    pub(crate) of: usize,
    pub(crate) lines: (usize, usize),
    #[serde(default)]
    pub(crate) ours: String,
    #[serde(default)]
    pub(crate) theirs: String,
    pub(crate) confidence: Option<Confidence>,
    #[serde(default)]
    pub(crate) why: String,
    pub(crate) resolution: Option<String>,
    pub(crate) status: EntryStatus,
    /// Who wrote it, or why it was left.
    #[serde(default)]
    pub(crate) note: String,
}

/// Everything the pass has learned, saved after every conflict.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkState {
    pub(crate) entries: Vec<WorkEntry>,
}

impl WorkState {
    pub(crate) fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join(STATE_FILE))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(dir.join(STATE_FILE), text)
    }

    pub(crate) fn find(&self, key: &str) -> Option<&WorkEntry> {
        self.entries.iter().find(|entry| entry.key == key)
    }

    pub(crate) fn upsert(&mut self, entry: WorkEntry) {
        match self.entries.iter_mut().find(|e| e.key == entry.key) {
            Some(existing) => *existing = entry,
            None => self.entries.push(entry),
        }
    }
}

/// `intl/l10n/Localization.cpp` → `intl__l10n__Localization.cpp.md`.
pub(crate) fn notes_file_name(file: &str) -> String {
    format!("{}.md", file.replace('/', "__"))
}

fn status_words(entry: &WorkEntry) -> String {
    match entry.status {
        EntryStatus::Applied => format!("applied — {}", entry.note),
        EntryStatus::Proposed => format!("proposed, not applied — {}", entry.note),
        EntryStatus::LeftForYou => format!("left for you — {}", entry.note),
    }
}

/// The work file for one source file: each conflict's note in order, then the
/// progress line at the bottom.
pub(crate) fn render_notes(file: &str, entries: &[&WorkEntry], updated: &str) -> String {
    let mut out = format!(
        "# {file}\n\nMerge conflicts in this file, worked one at a time. Line numbers are where each \
         conflict sat in the conflicted file.\n"
    );
    for entry in entries {
        out.push_str(&format!(
            "\n## Conflict {} of {} — lines {}–{}\n\n",
            entry.conflict, entry.of, entry.lines.0, entry.lines.1
        ));
        if !entry.ours.is_empty() {
            out.push_str(&format!("- Ours: {}\n", entry.ours));
        }
        if !entry.theirs.is_empty() {
            out.push_str(&format!("- Theirs: {}\n", entry.theirs));
        }
        if let Some(confidence) = entry.confidence {
            let why = if entry.why.is_empty() {
                String::new()
            } else {
                format!(" — {}", entry.why)
            };
            out.push_str(&format!("- Confidence: {}{why}\n", confidence.word()));
        }
        out.push_str(&format!("- Status: {}\n", status_words(entry)));
        if let Some(resolution) = &entry.resolution {
            out.push_str(&format!("\nFix:\n\n```\n{resolution}\n```\n"));
        }
    }
    let applied = entries
        .iter()
        .filter(|e| e.status == EntryStatus::Applied)
        .count();
    let left = entries.len() - applied;
    let of = entries.first().map(|e| e.of).unwrap_or_default();
    out.push_str(&format!(
        "\n---\nProgress: {} of {of} conflicts worked · {applied} applied · {left} for you · updated {updated}\n",
        entries.len()
    ));
    out
}

// ---------------------------------------------------------------------------
// The pass
// ---------------------------------------------------------------------------

/// A `resolve` block's settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PassSettings {
    /// A git ref whose version of each file is shown as a reference.
    pub(crate) reference: Option<String>,
    /// Where the notes go, relative to the target directory.
    pub(crate) work: Option<String>,
}

pub(crate) fn parse_pass_settings(code: &str) -> PassSettings {
    let mut settings = PassSettings::default();
    for line in code.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().to_string();
        if value.is_empty() {
            continue;
        }
        match key.trim().to_ascii_lowercase().as_str() {
            "reference" => settings.reference = Some(value),
            "work" => settings.work = Some(value),
            _ => {}
        }
    }
    settings
}

const ANALYST_SYSTEM_PROMPT: &str = "You merge ONE git merge conflict. OURS is this branch's version, THEIRS is \
the version being merged in, BASE (when shown) is their common ancestor. Unless the rule below says otherwise: keep \
what this branch changed (OURS against BASE) and take everything the incoming side changed (THEIRS against BASE) \
around it, so both changes survive. Write the merged lines that replace the whole conflict block.

Reply in exactly this form, nothing else:
OURS: one line — what this branch changed here
THEIRS: one line — what the incoming side changed here
CONFIDENCE: high, medium or low
WHY: one or two sentences
RESOLUTION:
```
the merged lines, with no conflict markers and no line numbers
```

If the merge needs a decision you cannot make from what is shown, reply with one line instead:
HUMAN: why a person has to decide this one";

const HELPER_SYSTEM_PROMPT: &str = "You check one proposed fix for a git merge conflict before it is written into \
the file. Reply with one of these, nothing else:
APPLY — the fix keeps this branch's change and the incoming update, and has no conflict markers.
EDIT — then the corrected lines in one fenced block, if it needs a small fix (a dropped line, a stray marker).
HUMAN: why — if it is wrong in a way a small edit can't fix.";

#[allow(clippy::too_many_arguments)]
fn analyst_prompt(
    file: &str,
    number: usize,
    of: usize,
    text: &str,
    conflict: &Conflict,
    rule: Option<&TableRule>,
    patch: Option<&String>,
    reference: Option<(&str, String)>,
) -> String {
    let rule = match rule {
        Some(rule) => format!(
            "The runbook's rule for this file: our change is \"{}\"; on conflict: \"{}\".\n\n",
            rule.change, rule.on_conflict
        ),
        None => String::new(),
    };
    let patch = match patch {
        Some(section) => format!(
            "This branch's own patch for the file (what OURS adds):\n```diff\n{}\n```\n\n",
            truncate_lines(section, MAX_PATCH_LINES)
        ),
        None => String::new(),
    };
    let reference = match reference {
        Some((name, hunks)) if !hunks.is_empty() => format!(
            "For reference only, how `{name}` changes the same code on top of the incoming version:\n```diff\n{hunks}\n```\n\n"
        ),
        _ => String::new(),
    };
    format!(
        "File: {file} — conflict {number} of {of}, lines {}–{}.\n\n{rule}{patch}{reference}{}",
        conflict.start_line,
        conflict.end_line,
        conflict_excerpt(text, conflict)
    )
}

fn helper_prompt(file: &str, text: &str, conflict: &Conflict, analysis: &Analysis) -> String {
    format!(
        "File: {file}, lines {}–{}.\n\n{}\nThe analyst's note — ours: {}; theirs: {}; why: {}\n\nProposed fix:\n```\n{}\n```",
        conflict.start_line,
        conflict.end_line,
        conflict_excerpt(text, conflict),
        analysis.ours,
        analysis.theirs,
        analysis.why,
        analysis.resolution.as_deref().unwrap_or_default()
    )
}

fn truncate_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max {
        return text.trim_end().to_string();
    }
    format!(
        "{}\n… ({} more lines)",
        lines[..max].join("\n"),
        lines.len() - max
    )
}

/// Runs a git command in the target directory; `None` when the user stopped
/// the run, else whether it succeeded and what it printed.
async fn git(context: &RunContext<'_>, args: &str) -> Option<(bool, String)> {
    let execution = run_script(context, &format!("git {args}")).await?;
    let mut output = execution.stdout;
    if !execution.stderr.trim().is_empty() {
        output.push_str(&execution.stderr);
    }
    Some((execution.exit_code == 0, output))
}

/// Shows one unit of the pass as a card.
fn card(
    on_event: &mut impl FnMut(TurnEvent),
    input: serde_json::Value,
    outcome: Result<String, String>,
) {
    let call_id = format!("runbook-resolve-{}", Uuid::new_v4());
    on_event(TurnEvent::ToolCallStarted {
        call_id: call_id.clone(),
        name: "resolve".to_string(),
        input,
    });
    on_event(TurnEvent::ToolCallResult {
        call_id,
        outcome: match outcome {
            Ok(text) => ToolCallOutcome::Success(json!({ "result": text })),
            Err(text) => ToolCallOutcome::Error(text),
        },
    });
}

/// How the pass went, for the worker and the judge.
#[derive(Debug, Default)]
struct PassTally {
    applied: usize,
    by_rule: Vec<String>,
    staged: Vec<String>,
    left: Vec<String>,
    still_conflicted: Vec<String>,
}

/// Works every conflicted file in the target directory, as a `resolve` block
/// asks. `None` when the user stopped the run.
pub(crate) async fn run_conflict_pass(
    context: &RunContext<'_>,
    step: &RunbookStep,
    code: &str,
    on_event: &mut impl FnMut(TurnEvent),
) -> Option<(CallRecord, String)> {
    let settings = parse_pass_settings(code);
    let work_dir = match &settings.work {
        Some(work) => {
            let path = Path::new(work);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                normalize(&context.working_dir.join(path))
            }
        }
        None => context.work_dir.to_path_buf(),
    };
    let label = "resolve (work-file pass)".to_string();

    let (ok, listing) = git(context, "diff --name-only --diff-filter=U").await?;
    if !ok {
        let reply = format!("Could not list the conflicted files:\n{listing}");
        return Some(record(label, 1, reply));
    }
    let files: Vec<String> = listing
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    if files.is_empty() {
        return Some(record(
            label,
            0,
            "Nothing is conflicted: there is no work for the pass.".to_string(),
        ));
    }

    let rules = table_rules(&step.body);
    let patched: BTreeMap<String, String> = rules
        .iter()
        .filter(|rule| rule.by_patch)
        .flat_map(|rule| rule.patterns.iter())
        .flat_map(|dir| patch_sections(&context.working_dir.join(dir.trim_end_matches("**"))))
        .collect();
    let reference = match &settings.reference {
        Some(reference) => {
            let (exists, _) = git(
                context,
                &format!("rev-parse --verify --quiet {}", shell_quote(reference)),
            )
            .await?;
            exists.then(|| reference.clone())
        }
        None => None,
    };
    let mut state = WorkState::load(&work_dir);
    let mut tally = PassTally::default();
    on_event(TurnEvent::Text {
        text: format!(
            "**Work-file pass** over {} conflicted file{}. Notes: `{}`. Analyst **`{}`**, helper **`{}`**{}.",
            files.len(),
            if files.len() == 1 { "" } else { "s" },
            work_dir.display(),
            context.judge,
            context.helper,
            match &reference {
                Some(reference) => format!(", reference `{reference}`"),
                None => String::new(),
            }
        ),
    });

    for file in &files {
        if context.cancelled() {
            return None;
        }
        let rule = rule_for(&rules, file, &patched);
        match rule.map(|rule| rule.action) {
            Some(RuleAction::KeepOurs) | Some(RuleAction::TakeTheirs) => {
                let side = if rule.map(|rule| rule.action) == Some(RuleAction::KeepOurs) {
                    "--ours"
                } else {
                    "--theirs"
                };
                let quoted = shell_quote(file);
                let (ok, output) = git(
                    context,
                    &format!("checkout {side} -- {quoted} && git add -- {quoted}"),
                )
                .await?;
                let words = if side == "--ours" {
                    "kept ours whole"
                } else {
                    "took theirs whole"
                };
                if ok {
                    tally
                        .by_rule
                        .push(format!("{file} ({words}, by the runbook's rule)"));
                    tally.staged.push(file.clone());
                    card(
                        on_event,
                        json!({ "file": file, "rule": words }),
                        Ok(format!("{words}, by the runbook's rule")),
                    );
                } else {
                    tally.left.push(format!(
                        "{file}: could not apply the rule ({words}): {}",
                        output.trim()
                    ));
                    tally.still_conflicted.push(file.clone());
                    card(
                        on_event,
                        json!({ "file": file, "rule": words }),
                        Err(output.trim().to_string()),
                    );
                }
                continue;
            }
            Some(RuleAction::Stop) => {
                let reason = "the runbook says to stop if this file conflicts".to_string();
                tally.left.push(format!("{file}: {reason}"));
                tally.still_conflicted.push(file.clone());
                card(on_event, json!({ "file": file }), Err(reason));
                continue;
            }
            Some(RuleAction::Merge) | None => {}
        }
        // Outside the table, the runbook doesn't allow a merge here: the pass
        // still works each conflict, but leaves the fix in the notes.
        let may_apply = rule.is_some();

        let quoted = shell_quote(file);
        let (ok, output) = git(context, &format!("checkout --conflict=diff3 -- {quoted}")).await?;
        if !ok {
            tally.left.push(format!(
                "{file}: could not re-create its conflicts: {}",
                output.trim()
            ));
            tally.still_conflicted.push(file.clone());
            continue;
        }
        let path = context.working_dir.join(file);
        let too_big = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_FILE_BYTES);
        let Some(mut text) = (!too_big)
            .then(|| std::fs::read_to_string(&path).ok())
            .flatten()
        else {
            tally.left.push(format!(
                "{file}: too large or not text — left whole for you"
            ));
            tally.still_conflicted.push(file.clone());
            continue;
        };
        let first_pass = find_conflicts(&text);
        let of = first_pass.len();
        let numbers: BTreeMap<String, usize> = first_pass
            .iter()
            .enumerate()
            .map(|(index, conflict)| (conflict_key(file, conflict), index + 1))
            .collect();
        let reference_diff = match &reference {
            Some(reference) => {
                git(
                    context,
                    &format!("diff MERGE_HEAD {} -- {quoted}", shell_quote(reference)),
                )
                .await?
                .1
            }
            None => String::new(),
        };
        let mut handled: BTreeSet<String> = BTreeSet::new();

        loop {
            if context.cancelled() {
                return None;
            }
            let conflicts = find_conflicts(&text);
            let Some(conflict) = conflicts
                .into_iter()
                .find(|conflict| !handled.contains(&conflict_key(file, conflict)))
            else {
                break;
            };
            let key = conflict_key(file, &conflict);
            handled.insert(key.clone());
            let number = numbers.get(&key).copied().unwrap_or(handled.len());
            let lines = (conflict.start_line, conflict.end_line);
            let card_input = json!({
                "file": file,
                "conflict": format!("{number} of {of}"),
                "lines": format!("{}–{}", lines.0, lines.1),
            });

            let mut entry = WorkEntry {
                key: key.clone(),
                file: file.clone(),
                conflict: number,
                of,
                lines,
                ours: String::new(),
                theirs: String::new(),
                confidence: None,
                why: String::new(),
                resolution: None,
                status: EntryStatus::LeftForYou,
                note: String::new(),
            };

            // A conflict an earlier run already settled keeps its fix.
            let earlier = state.find(&key).cloned();
            let analysis = match earlier
                .as_ref()
                .and_then(|e| e.resolution.clone().map(|r| (e, r)))
            {
                Some((earlier, resolution)) => Some(Analysis {
                    ours: earlier.ours.clone(),
                    theirs: earlier.theirs.clone(),
                    confidence: earlier.confidence.unwrap_or(Confidence::Low),
                    why: earlier.why.clone(),
                    resolution: Some(resolution),
                }),
                None if conflict.line_count() > MAX_CONFLICT_LINES => {
                    entry.note = format!(
                        "{} lines is too big to merge by model",
                        conflict.line_count()
                    );
                    None
                }
                None => {
                    let prompt = analyst_prompt(
                        file,
                        number,
                        of,
                        &text,
                        &conflict,
                        rule,
                        patched.get(file.as_str()),
                        reference.as_deref().map(|name| {
                            (
                                name,
                                related_hunks(&reference_diff, &conflict, MAX_REFERENCE_LINES),
                            )
                        }),
                    );
                    let mut analysis = None;
                    entry.note = format!("`{}` gave no fix I could read", context.judge);
                    for _ in 0..2 {
                        let content = match ask_fresh(
                            context,
                            context.judge,
                            context.judge_thinks.then_some(true),
                            ANALYST_SYSTEM_PROMPT,
                            &prompt,
                            on_event,
                        )
                        .await
                        {
                            Ok(Some(content)) => content,
                            Ok(None) if context.cancelled() => return None,
                            Ok(None) => {
                                entry.note = format!("`{}` didn't answer", context.judge);
                                break;
                            }
                            Err(error) => {
                                entry.note = format!("could not call `{}`: {error}", context.judge);
                                break;
                            }
                        };
                        analysis = parse_analysis(&content);
                        if analysis.is_some() {
                            break;
                        }
                    }
                    analysis
                }
            };

            if let Some(analysis) = &analysis {
                entry.ours = analysis.ours.clone();
                entry.theirs = analysis.theirs.clone();
                entry.confidence = Some(analysis.confidence);
                entry.why = analysis.why.clone();
                entry.resolution = analysis.resolution.clone();
            }

            let mut outcome: Result<String, String> = Err(entry.note.clone());
            match &analysis {
                None => {}
                Some(analysis) if analysis.resolution.is_none() => {
                    entry.note = format!("the analyst handed it to you: {}", analysis.why);
                    outcome = Err(entry.note.clone());
                }
                Some(analysis) if analysis.confidence == Confidence::Low => {
                    entry.note =
                        "low confidence — the fix is in the notes, not applied".to_string();
                    outcome = Err(entry.note.clone());
                }
                Some(_) if !may_apply => {
                    entry.status = EntryStatus::Proposed;
                    entry.note =
                        "outside the runbook's table — the fix is in the notes, not applied"
                            .to_string();
                    outcome = Err(entry.note.clone());
                }
                Some(analysis) => {
                    let checked = if earlier
                        .as_ref()
                        .is_some_and(|e| e.status == EntryStatus::Applied)
                    {
                        Check::Apply
                    } else {
                        let prompt = helper_prompt(file, &text, &conflict, analysis);
                        // A helper that thinks by default is told not to: its job is a
                        // quick check, and the analyst already did the reasoning.
                        let think = context.helper_thinks.then_some(false);
                        match ask_fresh(
                            context,
                            context.helper,
                            think,
                            HELPER_SYSTEM_PROMPT,
                            &prompt,
                            on_event,
                        )
                        .await
                        {
                            Ok(Some(content)) => parse_check(&content),
                            Ok(None) if context.cancelled() => return None,
                            Ok(None) => Check::Human(format!("`{}` didn't answer", context.helper)),
                            Err(error) => Check::Human(format!(
                                "could not call `{}`: {error}",
                                context.helper
                            )),
                        }
                    };
                    let fix = match checked {
                        Check::Apply => analysis.resolution.clone(),
                        Check::Edit(lines) => {
                            entry.resolution = Some(lines.clone());
                            Some(lines)
                        }
                        Check::Human(reason) => {
                            entry.note = format!("the helper sent it to you: {reason}");
                            outcome = Err(entry.note.clone());
                            None
                        }
                    };
                    if let Some(fix) = fix {
                        if has_conflict_markers(&fix) {
                            entry.note =
                                "the fix still had conflict markers — not applied".to_string();
                            outcome = Err(entry.note.clone());
                        } else {
                            let spliced = splice(&text, &conflict, &fix);
                            let remaining = find_conflicts(&spliced).len();
                            if remaining + 1 != find_conflicts(&text).len() {
                                entry.note = "the splice didn't line up — not applied".to_string();
                                outcome = Err(entry.note.clone());
                            } else if let Err(error) = std::fs::write(&path, &spliced) {
                                entry.note = format!("could not write the file: {error}");
                                outcome = Err(entry.note.clone());
                            } else {
                                text = spliced;
                                entry.status = EntryStatus::Applied;
                                entry.note = format!(
                                    "{} confidence, checked by `{}`",
                                    analysis.confidence.word(),
                                    context.helper
                                );
                                tally.applied += 1;
                                outcome = Ok(format!("applied — {}", entry.note));
                            }
                        }
                    }
                }
            }
            if entry.status != EntryStatus::Applied {
                tally.left.push(format!(
                    "{file} lines {}–{}: {}",
                    lines.0, lines.1, entry.note
                ));
            }
            card(on_event, card_input, outcome);
            state.upsert(entry);
            save_notes(&work_dir, &state, file);
        }

        if !has_open_conflicts(&text) {
            match git(context, &format!("add -- {quoted}")).await? {
                (true, _) => tally.staged.push(file.clone()),
                (false, output) => {
                    tally.left.push(format!(
                        "{file}: resolved but `git add` failed: {}",
                        output.trim()
                    ));
                    tally.still_conflicted.push(file.clone());
                }
            }
        } else {
            tally.still_conflicted.push(file.clone());
        }
    }

    save_progress(&work_dir, &state, &tally);
    let summary = pass_summary(&tally, &work_dir);
    let exit = if tally.still_conflicted.is_empty() {
        0
    } else {
        1
    };
    Some(record(label, exit, summary))
}

/// What a model reads of the pass: a run with hundreds of conflicts keeps its
/// head (the totals) and tail (what is still conflicted); the notes files hold
/// the rest.
fn record(label: String, exit_code: i32, reply: String) -> (CallRecord, String) {
    let reply = clip_output(&reply);
    (
        CallRecord {
            label,
            exit_code: Some(exit_code),
            output: reply.clone(),
        },
        format!("exit {exit_code}\n{reply}"),
    )
}

/// `items` joined by `separator`, the first `max` of them and a count of the
/// rest.
fn join_capped(items: &[String], separator: &str, max: usize) -> String {
    if items.len() <= max {
        return items.join(separator);
    }
    format!(
        "{}{separator}… and {} more",
        items[..max].join(separator),
        items.len() - max
    )
}

fn now_hhmm() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

fn save_notes(work_dir: &Path, state: &WorkState, file: &str) {
    let mut entries: Vec<&WorkEntry> = state.entries.iter().filter(|e| e.file == file).collect();
    entries.sort_by_key(|entry| entry.conflict);
    let notes = render_notes(file, &entries, &now_hhmm());
    let saved = state
        .save(work_dir)
        .and_then(|()| std::fs::write(work_dir.join(notes_file_name(file)), notes));
    if let Err(error) = saved {
        log::warn!(
            "runbook: could not save the work notes in {}: {error}",
            work_dir.display()
        );
    }
}

fn save_progress(work_dir: &Path, state: &WorkState, tally: &PassTally) {
    let files: BTreeSet<&str> = state.entries.iter().map(|e| e.file.as_str()).collect();
    let mut out = String::from(
        "# Work-file pass\n\nOne notes file per conflicted source file, beside this one.\n\n",
    );
    for file in files {
        let entries: Vec<&WorkEntry> = state.entries.iter().filter(|e| e.file == file).collect();
        let applied = entries
            .iter()
            .filter(|e| e.status == EntryStatus::Applied)
            .count();
        out.push_str(&format!(
            "- `{file}` — {applied} of {} applied → {}\n",
            entries.first().map(|e| e.of).unwrap_or_default(),
            notes_file_name(file)
        ));
    }
    for line in &tally.by_rule {
        out.push_str(&format!("- {line}\n"));
    }
    out.push_str(&format!(
        "\n---\nProgress: {} applied · {} left for you · {} file(s) still conflicted · updated {}\n",
        tally.applied,
        tally.left.len(),
        tally.still_conflicted.len(),
        now_hhmm()
    ));
    if let Err(error) = std::fs::create_dir_all(work_dir)
        .and_then(|()| std::fs::write(work_dir.join(PROGRESS_FILE), out))
    {
        log::warn!(
            "runbook: could not save the pass's progress in {}: {error}",
            work_dir.display()
        );
    }
}

fn pass_summary(tally: &PassTally, work_dir: &Path) -> String {
    let mut out = format!(
        "Work-file pass: {} conflict fix(es) applied, {} left for you. Notes in {}.\n",
        tally.applied,
        tally.left.len(),
        work_dir.display()
    );
    if !tally.by_rule.is_empty() {
        out.push_str(&format!(
            "Done by the runbook's rules: {}.\n",
            join_capped(&tally.by_rule, "; ", 12)
        ));
    }
    if !tally.staged.is_empty() {
        out.push_str(&format!(
            "Resolved and staged: {}.\n",
            join_capped(&tally.staged, ", ", 15)
        ));
    }
    if !tally.left.is_empty() {
        out.push_str("Left for you:\n");
        let shown: Vec<String> = tally
            .left
            .iter()
            .map(|line| format!("- {}", truncate_chars(line, 240)))
            .collect();
        out.push_str(&join_capped(&shown, "\n", 20));
        out.push('\n');
    }
    if tally.still_conflicted.is_empty() {
        out.push_str("Nothing is conflicted any more.\n");
    } else {
        out.push_str(&format!(
            "Still conflicted: {}.\n",
            join_capped(&tally.still_conflicted, ", ", 25)
        ));
    }
    out
}

#[cfg(test)]
#[path = "ollama_runbook_conflicts_tests.rs"]
mod tests;
