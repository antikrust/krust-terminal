//! Keeping a local model's window from overflowing, for the Ollama tool chat.
//!
//! A local model's window is small, and when a conversation outgrows it Ollama
//! quietly drops the start — the system prompt and the user's request — so the
//! model forgets the task. The cloud-model loop copes by asking the model for
//! a summary; a 9B model's summary is lossy and costs a request, so this loop
//! does not use one. It works in three ways, none of which prints anything:
//!
//! - **Sizes to the model**: the window asked of Ollama comes from the model's
//!   own maximum and size ([`num_ctx_for`]), and everything below scales with
//!   it: how much one tool result may carry, how much one read returns, when
//!   the history is trimmed or rebuilt.
//! - **Spills long output to a file**: a result over the limit is saved whole
//!   and the model gets its head and tail with the path ([`clip_with_spill`]),
//!   so the exact text is still one `read_file` away.
//! - **Rebuilds from notes**: the model writes what it will need to a notes
//!   file with the `remember` tool, and before the window fills it is told to
//!   do that once more ([`SAVE_NOTES_NUDGE`]). Then the history is rebuilt
//!   from the system prompt, the request, the checklist, the notes and the
//!   files touched ([`compact`]) — the task's state, not a narrative.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde_json::{json, Value};
use uuid::Uuid;

use super::ollama_stream_exec::{turn_user_message, McpToolSpec};
use super::turn_images::image_chars;

/// Window asked of a model whose size or maximum isn't known, and of any
/// model over [`SMALL_MODEL_BILLIONS`], whose cache costs the most memory.
const DEFAULT_NUM_CTX: usize = 16_384;
/// Window asked of a small model: roomy enough for a runbook step with its
/// reads, and a 9B's cache at this size still fits beside it on a 24GB Mac.
const SMALL_MODEL_NUM_CTX: usize = 32_768;
const SMALL_MODEL_BILLIONS: f64 = 12.0;
/// Overrides the window for every local model, in tokens.
const NUM_CTX_ENV: &str = "KRUST_OLLAMA_NUM_CTX";
/// What a token costs in characters, for counting without a tokenizer. Code,
/// hex and disassembly run lower than prose, so this errs towards full.
const CHARS_PER_TOKEN: usize = 3;

/// Share of the window at which stale tool results are dropped, and the share
/// that drops them until the history is back under.
const TRIM_AT: f64 = 0.40;
const TRIM_TO: f64 = 0.30;
/// Share at which the history is rebuilt from the notes.
const COMPACT_AT: f64 = 0.55;
/// Share at which a finished checklist item also rebuilds it: its work is
/// done with, so even a modest history is worth clearing.
const TASK_DONE_COMPACT_AT: f64 = 0.20;
/// Tool results shorter than this are never dropped as stale.
const STALE_RESULT_MIN_CHARS: usize = 1_500;
/// Earlier turns keep only their text, each cut to this, the last few only.
const PRIOR_MESSAGE_CHARS: usize = 1_200;
const PRIOR_MESSAGES_KEPT: usize = 6;
/// The files named in the carried-over state.
const FILES_NAMED: usize = 16;

/// Marks the carried-over state message, so a later rebuild leaves it out.
const STATE_MARKER: &str = "[Carried over";

pub(crate) const REMEMBER: &str = "remember";

/// What the model is told about its notes, on top of the usual tool prompt.
pub(crate) const MEMORY_PROMPT: &str = "Your memory is poor, and your context is small and is \
    cleared and rebuilt, often, during a long task. What survives is your checklist, your request \
    and your notes, and nothing else. So for anything complicated:\n\
    - When you are asked to follow a document or procedure, or the work has more than two steps, \
    your first action is todo_list; then work through it, sending the whole list again each time \
    you finish a step.\n\
    - Write things down the moment you establish them, don't trust yourself to recall them later. \
    The remember tool saves notes to a file of your own: an exact path, address, constant, \
    result or decision, or what the next step is, as short lines, one fact each. If the request \
    names a notes file of its own, keep that one up too.\n\
    - Read a long document a section at a time, not from the top: read_file with headings=true \
    lists its headings, then read_file path#Heading reads the one you need.\n\
    - Never write out notes or a summary of your progress as text in your answers: that spends \
    the context twice. The notes are for you, not the user.";

/// Added to the `todo_list` reply when the checklist is first made: a task
/// with a checklist is a complicated one.
pub(crate) fn checklist_started_reminder(session_id: &Uuid) -> String {
    format!(
        "\nThis is a multi-step task, so write things down as you go: call remember for each \
         fact you establish (path, address, constant, result, decision) before you move to the \
         next thing. Your notes are saved in {} and survive when your context is cleared.",
        notes_path(session_id).display()
    )
}

/// Added to the `todo_list` reply when a step has just been ticked off.
pub(crate) const STEP_DONE_REMINDER: &str = "\nBefore you start the next step, call remember with \
    what it needs from this one: exact paths, addresses, values and decisions.";

/// Told to the model when the window is nearly full, in place of asking it for
/// a summary: one round to save what it needs before the history is rebuilt.
pub(crate) const SAVE_NOTES_NUDGE: &str = "[Your context is nearly full and will be cleared after \
    your next reply. Before anything else, call remember with everything you will need to carry \
    on: exact paths, addresses, constants, results and decisions, and what the next step is. Then \
    carry on with the task. Don't write the notes out as text.]";

/// Told to the model after a rebuild.
const CARRY_ON: &str = "[The context was cleared to make room. The task is not finished. Carry on \
    with the next unfinished step now, with your tools: don't repeat steps that are done, and \
    don't write out a summary.]";

/// The `remember` tool, offered to this loop only.
pub(crate) fn remember_tool_spec() -> McpToolSpec {
    McpToolSpec {
        name: REMEMBER.to_string(),
        description: "Save notes that survive when your context is cleared, which happens often \
            in a long task. Anything not saved here or in a file is lost. Write exact paths, \
            addresses, constants, results, decisions and what to do next, as short lines."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The notes to save, one fact per line.",
                },
            },
            "required": ["text"],
        }),
    }
}

/// The window to ask Ollama for, from the model's maximum context and its
/// size in billions of parameters, when known. [`NUM_CTX_ENV`] wins.
pub(crate) fn num_ctx_for(context_length: Option<u64>, billions: Option<f64>) -> usize {
    if let Some(tokens) = std::env::var(NUM_CTX_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
    {
        return tokens.clamp(2_048, 262_144);
    }
    let wanted = match billions {
        Some(size) if size <= SMALL_MODEL_BILLIONS => SMALL_MODEL_NUM_CTX,
        _ => DEFAULT_NUM_CTX,
    };
    match context_length {
        Some(max) if (max as usize) < wanted => (max as usize).max(2_048),
        _ => wanted,
    }
}

/// How much of a window each kind of text may take.
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) num_ctx: usize,
}

/// How full the window is.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Pressure {
    Fine,
    /// Drop tool results the model can run again.
    Trim,
    /// Rebuild the history from the notes.
    Compact,
}

impl Limits {
    pub(crate) fn new(num_ctx: usize) -> Self {
        Self { num_ctx }
    }

    /// The most one tool result may carry before it is spilled to a file: an
    /// eighth of the window.
    pub(crate) fn result_chars(&self) -> usize {
        (self.num_ctx * CHARS_PER_TOKEN / 8).max(4_000)
    }

    /// How much of a file one `read_file` returns.
    pub(crate) fn read_window(&self) -> usize {
        self.result_chars().saturating_sub(1_000).max(3_000)
    }

    /// Results this long are too big to skim even head-and-tail, so they are
    /// read in parts against the request instead.
    pub(crate) fn read_in_parts_over(&self) -> usize {
        self.result_chars() * 4
    }

    /// The most the carried-over notes take: a fifth of the window.
    fn notes_chars(&self) -> usize {
        self.num_ctx * CHARS_PER_TOKEN / 5
    }

    /// A history's size in tokens, with the tool definitions sent beside it.
    pub(crate) fn tokens(&self, messages: &[Value], tools_chars: usize) -> usize {
        let chars: usize = messages
            .iter()
            .map(|message| message.to_string().len() - image_chars(message))
            .sum();
        (chars + tools_chars) / CHARS_PER_TOKEN
    }

    pub(crate) fn pressure(&self, tokens: usize, task_done: bool) -> Pressure {
        let fill = tokens as f64 / self.num_ctx as f64;
        if fill >= COMPACT_AT || (task_done && fill >= TASK_DONE_COMPACT_AT) {
            Pressure::Compact
        } else if fill >= TRIM_AT {
            Pressure::Trim
        } else {
            Pressure::Fine
        }
    }

    /// The history size a rebuild must be left under for another to wait:
    /// a rebuild that can't shrink the request (a huge one) must not repeat
    /// every round.
    pub(crate) fn next_compact_floor(&self, tokens_after: usize) -> usize {
        tokens_after + self.num_ctx / 10
    }
}

fn role_of(message: &Value) -> &str {
    message.get("role").and_then(Value::as_str).unwrap_or_default()
}

fn content_of(message: &Value) -> &str {
    message.get("content").and_then(Value::as_str).unwrap_or_default()
}

/// Drops the tool results the model can run again, oldest first, until the
/// history is back under [`TRIM_TO`] of the window. The latest round — the
/// results the model has yet to act on — is never touched. Returns whether
/// anything was dropped.
pub(crate) fn drop_stale_results(
    messages: &mut [Value],
    limits: &Limits,
    tools_chars: usize,
) -> bool {
    let Some(latest_round) = messages
        .iter()
        .rposition(|message| role_of(message) == "assistant")
    else {
        return false;
    };
    let mut dropped = false;
    for index in 0..latest_round {
        let fill = limits.tokens(messages, tools_chars) as f64 / limits.num_ctx as f64;
        if fill <= TRIM_TO {
            break;
        }
        let message = &mut messages[index];
        if role_of(message) != "tool" || content_of(message).len() < STALE_RESULT_MIN_CHARS {
            continue;
        }
        let chars = content_of(message).len();
        message["content"] = json!(format!(
            "[An earlier tool result ({chars} characters) was dropped to keep room. Run the tool \
             again if you need it.]"
        ));
        dropped = true;
    }
    dropped
}

fn spill_dir(session_id: &Uuid) -> PathBuf {
    krust_core::paths::state_dir()
        .join("ollama-spill")
        .join(session_id.to_string())
}

/// `text` as the model sees it when it is over `max_chars`: the head and the
/// tail, by whole lines, with the line numbers left out and where the whole
/// output is saved. Text that fits comes back as it is.
pub(crate) fn clip_with_spill(
    session_id: &Uuid,
    tool: &str,
    text: &str,
    max_chars: usize,
) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let dir = spill_dir(session_id);
    let file = dir.join(format!(
        "{}-{}.txt",
        tool,
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    let saved = fs::create_dir_all(&dir)
        .and_then(|()| fs::write(&file, text))
        .is_ok();

    let head = head_lines(text, max_chars * 2 / 5);
    let tail = tail_lines(text, max_chars * 2 / 5);
    let total = text.lines().count();
    let head_count = head.lines().count();
    let tail_count = tail.lines().count();
    let omitted = total.saturating_sub(head_count + tail_count);
    let where_to = if saved {
        format!(
            "The whole output is saved at {}: read_file it from a line (offset/limit), or search \
             it with run_code (grep -n).",
            file.display()
        )
    } else {
        "The rest could not be saved; run the command again with its output narrowed.".to_string()
    };
    format!(
        "{head}\n[… lines {}-{} of {total} left out ({omitted} lines). {where_to}]\n{tail}",
        head_count + 1,
        head_count + omitted,
    )
}

fn head_lines(text: &str, budget: usize) -> &str {
    let mut end = 0;
    for line in text.split_inclusive('\n') {
        if end + line.len() > budget {
            break;
        }
        end += line.len();
    }
    if end == 0 {
        end = budget.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
    }
    &text[..end]
}

fn tail_lines(text: &str, budget: usize) -> &str {
    let mut start = text.len().saturating_sub(budget);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    // Start on a whole line when there is one in reach.
    if start > 0 && !text[..start].ends_with('\n') {
        if let Some(newline) = text[start..].find('\n') {
            if start + newline + 1 < text.len() {
                start += newline + 1;
            }
        }
    }
    &text[start..]
}

fn notes_dir() -> PathBuf {
    krust_core::paths::state_dir().join("ollama-notes")
}

/// Where a session's notes are kept.
pub(crate) fn notes_path(session_id: &Uuid) -> PathBuf {
    notes_dir().join(format!("{session_id}.md"))
}

/// Adds `text` to the session's notes, a line at a time, leaving out lines
/// already there. Returns how many notes there are in all.
pub(crate) fn remember(session_id: &Uuid, text: &str) -> Result<usize, String> {
    let path = notes_path(session_id);
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let mut known: HashSet<String> = existing.lines().map(str::to_string).collect();
    let mut added = String::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches(['-', '*']).trim();
        if line.is_empty() {
            continue;
        }
        let note = format!("- {line}");
        if known.insert(note.clone()) {
            added.push_str(&note);
            added.push('\n');
        }
    }
    if !added.is_empty() {
        fs::create_dir_all(notes_dir()).map_err(|error| error.to_string())?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| error.to_string())?;
        file.write_all(added.as_bytes())
            .map_err(|error| error.to_string())?;
    }
    Ok(known.len())
}

/// The newest notes that fit in `max_chars`, oldest first.
fn recent_notes(session_id: &Uuid, max_chars: usize) -> Option<String> {
    let notes = fs::read_to_string(notes_path(session_id)).ok()?;
    let mut kept: Vec<&str> = Vec::new();
    let mut size = 0;
    for line in notes.lines().rev().filter(|line| !line.trim().is_empty()) {
        size += line.len() + 1;
        if size > max_chars && !kept.is_empty() {
            break;
        }
        kept.push(line);
    }
    kept.reverse();
    (!kept.is_empty()).then(|| kept.join("\n"))
}

/// The paths the model's file tools were called on, for the state it carries:
/// what it changed and what it read, each in order of last use.
fn files_touched(messages: &[Value]) -> (Vec<String>, Vec<String>) {
    let mut changed: Vec<String> = Vec::new();
    let mut read: Vec<String> = Vec::new();
    for message in messages.iter().filter(|message| role_of(message) == "assistant") {
        let Some(calls) = message.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        for call in calls {
            let function = &call["function"];
            let Some(path) = function["arguments"]["path"].as_str() else {
                continue;
            };
            let list = match function["name"].as_str() {
                Some("write_file" | "replace_lines") => &mut changed,
                Some("read_file") => &mut read,
                _ => continue,
            };
            list.retain(|known| known != path);
            list.push(path.to_string());
        }
    }
    let newest = |mut list: Vec<String>| {
        list.reverse();
        list.truncate(FILES_NAMED);
        list.reverse();
        list
    };
    (newest(changed), newest(read))
}

/// What the carried-over state says of the checklist.
pub(crate) struct Checklist<'a> {
    pub(crate) done: &'a [String],
    pub(crate) remaining: &'a [String],
}

fn state_text(
    session_id: &Uuid,
    checklist: &Checklist,
    messages: &[Value],
    limits: &Limits,
) -> Option<String> {
    let mut sections = Vec::new();
    if !checklist.done.is_empty() || !checklist.remaining.is_empty() {
        let mut text = String::from("Checklist:");
        for item in checklist.done {
            text.push_str(&format!("\n- [done] {item}"));
        }
        for item in checklist.remaining {
            text.push_str(&format!("\n- [to do] {item}"));
        }
        sections.push(text);
    }
    if let Some(notes) = recent_notes(session_id, limits.notes_chars()) {
        sections.push(format!(
            "Your notes (all of them are in {}):\n{notes}",
            notes_path(session_id).display()
        ));
    }
    let (changed, read) = files_touched(messages);
    if !changed.is_empty() {
        sections.push(format!("Files you changed: {}", changed.join(", ")));
    }
    if !read.is_empty() {
        sections.push(format!("Files you read: {}", read.join(", ")));
    }
    if spill_dir(session_id).is_dir() {
        sections.push(format!(
            "Long outputs you saw cut short are saved whole in {}.",
            spill_dir(session_id).display()
        ));
    }
    (!sections.is_empty()).then(|| {
        format!(
            "{STATE_MARKER} from your earlier work, which is no longer in your context. This is \
             for you; the user doesn't see it.]\n\n{}",
            sections.join("\n\n")
        )
    })
}

fn cut_to(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Rebuilds a history from what the task needs, not from what was said: the
/// system prompt, the text of earlier turns, the request that opened this
/// one (at `turn_start`, if the turn has begun), and one message carrying the
/// checklist, the notes and the files touched. Returns it with where the
/// turn's opening message now is (the history's length when there is none).
pub(crate) fn compact(
    messages: &[Value],
    turn_start: usize,
    checklist: &Checklist,
    session_id: &Uuid,
    limits: &Limits,
) -> (Vec<Value>, usize) {
    let mut rebuilt = Vec::new();
    if messages.first().is_some_and(|message| role_of(message) == "system") {
        rebuilt.push(messages[0].clone());
    }
    let end = turn_start.min(messages.len());
    let before = &messages[rebuilt.len().min(end)..end];
    let mut prior: Vec<Value> = before
        .iter()
        .filter(|message| {
            matches!(role_of(message), "user" | "assistant")
                && message.get("tool_calls").is_none()
                && !content_of(message).trim().is_empty()
                && !content_of(message).starts_with(STATE_MARKER)
        })
        .cloned()
        .collect();
    let skip = prior.len().saturating_sub(PRIOR_MESSAGES_KEPT);
    prior.drain(..skip);
    for message in &mut prior {
        message["content"] = json!(cut_to(content_of(message), PRIOR_MESSAGE_CHARS));
    }
    rebuilt.extend(prior);

    let started = turn_start < messages.len();
    let new_turn_start = rebuilt.len();
    if started {
        rebuilt.push(messages[turn_start].clone());
    }
    let this_turn = &messages[turn_start.min(messages.len())..];
    if let Some(state) = state_text(session_id, checklist, this_turn, limits) {
        rebuilt.push(json!({ "role": "assistant", "content": state }));
        if started {
            rebuilt.push(turn_user_message(session_id, CARRY_ON.to_string()));
        }
    }
    (rebuilt, new_turn_start)
}
