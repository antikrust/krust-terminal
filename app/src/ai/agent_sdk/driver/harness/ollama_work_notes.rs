//! Work notes: how a small local model copes with a text too big for it.
//!
//! A 7–14B model with an 8–16k window can't read a 3,000-line log or a 60 KB
//! command output. Handed one, Ollama quietly drops the start of the
//! conversation — the system prompt, the user's request — to make room, and
//! the model answers about the wrong thing. So a harness never hands it one.
//! A text over [`LARGE_TEXT_CHARS`] is split into line-numbered parts; each
//! part is read in a fresh conversation against the question at hand, and
//! the notes, with line numbers, go into a small work file with a progress
//! line at the bottom. When every part is read, the notes are reduced — in
//! rounds, if even they are too long — into one short answer, which goes back
//! to the model in place of the text, with pointers to the full text and the
//! notes.
//!
//! The pass is a plain state machine ([`WorkNotes::next_call`] /
//! [`WorkNotes::accept`]), so each harness drives it with its own model-call
//! machinery, and a stopped run resumes from the saved notes instead of
//! starting over.

use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Texts longer than this are read in parts rather than handed to a model.
pub(crate) const LARGE_TEXT_CHARS: usize = 12_000;
/// The most text one reading call sees.
const PART_CHARS: usize = 6_000;
/// The most notes one reducing call sees.
const REDUCE_CHARS: usize = 7_000;
/// Reducing rounds before the notes are cut to fit instead.
const MAX_REDUCE_ROUNDS: usize = 4;
/// What a reader replies when a part has nothing on the question.
const NOTHING: &str = "NOTHING";

const READ_SYSTEM_PROMPT: &str = "You read one part of a long text for someone who can't hold all of it at \
once. Each line starts with its label, like L612|. Note only lines that bear directly on the question — a problem, \
a failure, or the thing asked about — citing them by label, like [L612] or [L120-L134], copied exactly. At most \
eight short lines, no preamble. If this part only shows things going normally, or nothing in it bears on the \
question, reply with the single word NOTHING.";

const REDUCE_SYSTEM_PROMPT: &str = "You condense notes taken on one stretch of a long text. Keep everything that \
bears on the question and every line number cited; drop repeats and anything off the question. Reply with the \
condensed notes only.";

const ANSWER_SYSTEM_PROMPT: &str = "You answer a question about a long text from notes taken on it part by part, \
in order. Answer from the notes alone and cite their line numbers. If the notes don't settle it, say what is \
missing. Be brief: at most fifteen lines.";

/// One part of the text: its lines, numbered from 1 in the whole text.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Part {
    pub(crate) first_line: usize,
    pub(crate) last_line: usize,
    /// The part's lines, each prefixed with its label (`L612| …`).
    pub(crate) numbered: String,
    /// The part's lines as they are, with their numbers, for checking what a
    /// reader cites.
    pub(crate) lines: Vec<(usize, String)>,
}

/// Splits `text` into parts of at most `max_chars`, ending a part at a blank
/// line or a diff/section boundary near the limit when there is one, so a
/// paragraph or hunk is not cut in half. A single line longer than the limit
/// is split across parts and keeps its number.
pub(crate) fn split_into_parts(text: &str, max_chars: usize) -> Vec<Part> {
    // (line number, text) with overlong lines cut into pieces.
    let mut lines: Vec<(usize, &str)> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let mut rest = line;
        loop {
            if rest.len() <= max_chars {
                lines.push((index + 1, rest));
                break;
            }
            let mut cut = max_chars;
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            lines.push((index + 1, &rest[..cut]));
            rest = &rest[cut..];
        }
    }
    let width = lines.last().map(|(n, _)| n.to_string().len()).unwrap_or(1) + 3;
    let mut parts = Vec::new();
    let mut start = 0;
    while start < lines.len() {
        let mut end = start;
        let mut size = 0;
        while end < lines.len() {
            let cost = lines[end].1.len() + width;
            if end > start && size + cost > max_chars {
                break;
            }
            size += cost;
            end += 1;
        }
        // Prefer to end at a boundary in the last quarter of the part.
        if end < lines.len() {
            let floor = start + (end - start) * 3 / 4;
            if let Some(boundary) = (floor.max(start + 1)..end)
                .rev()
                .find(|&index| is_boundary(lines[index].1))
            {
                end = boundary;
            }
        }
        let numbered = lines[start..end]
            .iter()
            .map(|(number, line)| format!("L{number}| {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(Part {
            first_line: lines[start].0,
            last_line: lines[end - 1].0,
            numbered,
            lines: lines[start..end]
                .iter()
                .map(|(number, line)| (*number, line.to_string()))
                .collect(),
        });
        start = end;
    }
    parts
}

/// A line a part may start at: blank, or the head of a diff file or hunk.
fn is_boundary(line: &str) -> bool {
    let line = line.trim_end();
    line.is_empty() || line.starts_with("diff --git ") || line.starts_with("@@")
}

/// FNV-1a, for a key that stays the same across runs and builds.
fn stable_hash(parts: &[&str]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.bytes().chain(std::iter::once(0)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

/// `Output of git log --stat` → `output-of-git-log-stat`, at most 40 chars.
fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 40 {
            break;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "text".to_string()
    } else {
        out
    }
}

/// One call the pass wants made, in a fresh conversation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NextCall {
    pub(crate) system: &'static str,
    pub(crate) user: String,
    /// Shown while the call runs, e.g. "part 3 of 9".
    pub(crate) label: String,
}

/// What is saved between runs: each part's notes and the answer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Saved {
    key: String,
    /// Per part: `None` not read yet, `Some("")` nothing relevant.
    notes: Vec<Option<String>>,
    answer: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum Stage {
    Reading,
    /// Notes being condensed: the current round's items, how they are
    /// batched, and what each batch condensed to so far.
    Reducing {
        round: usize,
        items: Vec<String>,
        batches: Vec<Range<usize>>,
        condensed: Vec<String>,
    },
    Answering {
        items: Vec<String>,
    },
    Done,
}

/// A long text being read in parts against one question.
#[derive(Debug, Clone)]
pub(crate) struct WorkNotes {
    title: String,
    question: String,
    total_lines: usize,
    parts: Vec<Part>,
    saved: Saved,
    stage: Stage,
    reduce_chars: usize,
    dir: PathBuf,
    stem: String,
}

impl WorkNotes {
    /// Starts (or resumes, if `dir` holds notes for the same text and
    /// question) a pass over `text`. The full text is saved beside the notes
    /// so their line numbers can be followed.
    pub(crate) fn new(title: &str, question: &str, text: &str, dir: &Path) -> Self {
        Self::with_limits(title, question, text, dir, PART_CHARS, REDUCE_CHARS)
    }

    pub(crate) fn with_limits(
        title: &str,
        question: &str,
        text: &str,
        dir: &Path,
        part_chars: usize,
        reduce_chars: usize,
    ) -> Self {
        let parts = split_into_parts(text, part_chars);
        let key = stable_hash(&[question, text, &part_chars.to_string()]);
        let stem = format!("{}-{}", slug(title), &key[..8]);
        let mut notes = Self {
            title: title.to_string(),
            question: question.to_string(),
            total_lines: text.lines().count(),
            saved: Saved {
                key: key.clone(),
                notes: vec![None; parts.len()],
                answer: None,
            },
            parts,
            stage: Stage::Reading,
            reduce_chars,
            dir: dir.to_path_buf(),
            stem,
        };
        if let Some(saved) = std::fs::read_to_string(notes.state_path())
            .ok()
            .and_then(|json| serde_json::from_str::<Saved>(&json).ok())
            .filter(|saved| saved.key == key && saved.notes.len() == notes.parts.len())
        {
            notes.saved = saved;
        }
        if notes.saved.answer.is_some() {
            notes.stage = Stage::Done;
        } else if notes.saved.notes.iter().all(Option::is_some) {
            notes.start_reducing(0);
        }
        let _ = std::fs::create_dir_all(dir);
        if !notes.text_path().exists() {
            let _ = std::fs::write(notes.text_path(), text);
        }
        notes
    }

    pub(crate) fn part_count(&self) -> usize {
        self.parts.len()
    }

    pub(crate) fn total_lines(&self) -> usize {
        self.total_lines
    }

    pub(crate) fn answer(&self) -> Option<&str> {
        self.saved.answer.as_deref()
    }

    pub(crate) fn notes_path(&self) -> PathBuf {
        self.dir.join(format!("{}.md", self.stem))
    }

    pub(crate) fn text_path(&self) -> PathBuf {
        self.dir.join(format!("{}.txt", self.stem))
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join(format!("{}.json", self.stem))
    }

    /// The next call to make, or `None` when the answer is in.
    pub(crate) fn next_call(&self) -> Option<NextCall> {
        match &self.stage {
            Stage::Reading => {
                let index = self.saved.notes.iter().position(Option::is_none)?;
                let part = &self.parts[index];
                Some(NextCall {
                    system: READ_SYSTEM_PROMPT,
                    user: format!(
                        "Question: {}\n\nText: {} — part {} of {}, lines {}–{}:\n```\n{}\n```",
                        self.question,
                        self.title,
                        index + 1,
                        self.parts.len(),
                        part.first_line,
                        part.last_line,
                        part.numbered
                    ),
                    label: format!("part {} of {}", index + 1, self.parts.len()),
                })
            }
            Stage::Reducing {
                round,
                items,
                batches,
                condensed,
            } => {
                let batch = batches.get(condensed.len())?;
                Some(NextCall {
                    system: REDUCE_SYSTEM_PROMPT,
                    user: format!(
                        "Question: {}\n\nNotes on {}, in order:\n\n{}",
                        self.question,
                        self.title,
                        items[batch.clone()].join("\n\n")
                    ),
                    label: format!(
                        "condensing notes, round {} ({} of {})",
                        round + 1,
                        condensed.len() + 1,
                        batches.len()
                    ),
                })
            }
            Stage::Answering { items } => Some(NextCall {
                system: ANSWER_SYSTEM_PROMPT,
                user: format!(
                    "Question: {}\n\nNotes on {} ({} lines), in order:\n\n{}",
                    self.question,
                    self.title,
                    self.total_lines,
                    items.join("\n\n")
                ),
                label: "answering from the notes".to_string(),
            }),
            Stage::Done => None,
        }
    }

    /// Records the reply to the call `next_call` gave; `None` when that call
    /// failed. Saves the notes after every call.
    pub(crate) fn accept(&mut self, reply: Option<&str>) {
        let reply = reply.map(str::trim).filter(|reply| !reply.is_empty());
        match &mut self.stage {
            Stage::Reading => {
                if let Some(index) = self.saved.notes.iter().position(Option::is_none) {
                    let note = match reply {
                        Some(reply) if is_nothing(reply) || only_citations(reply) => String::new(),
                        Some(reply) => ground_citations(reply, &self.parts[index].lines),
                        None => "(this part could not be read)".to_string(),
                    };
                    self.saved.notes[index] = Some(note);
                }
                if self.saved.notes.iter().all(Option::is_some) {
                    self.start_reducing(0);
                }
            }
            Stage::Reducing {
                round,
                items,
                batches,
                condensed,
            } => {
                let index = condensed.len();
                // A failed condensing call keeps its batch as it was.
                let text = match reply {
                    Some(reply) => reply.to_string(),
                    None => items[batches[index].clone()].join("\n\n"),
                };
                condensed.push(text);
                if condensed.len() == batches.len() {
                    let (round, next) = (*round, std::mem::take(condensed));
                    self.reduce(round + 1, next);
                }
            }
            Stage::Answering { items } => {
                let answer = match reply {
                    Some(reply) => reply.to_string(),
                    // No answer came back: the notes themselves are the answer.
                    None => truncate(&items.join("\n\n"), self.reduce_chars),
                };
                self.saved.answer = Some(answer);
                self.stage = Stage::Done;
            }
            Stage::Done => {}
        }
        self.save();
    }

    /// The notes worth reducing, each labelled with its lines.
    fn note_items(&self) -> Vec<String> {
        self.parts
            .iter()
            .zip(&self.saved.notes)
            .filter_map(|(part, note)| {
                let note = note.as_deref()?.trim();
                (!note.is_empty())
                    .then(|| format!("Lines {}–{}:\n{note}", part.first_line, part.last_line))
            })
            .collect()
    }

    fn start_reducing(&mut self, round: usize) {
        let items = self.note_items();
        if items.is_empty() {
            self.saved.answer = Some("Nothing in the text bears on the question.".to_string());
            self.stage = Stage::Done;
            return;
        }
        self.reduce(round, items);
    }

    /// Answers from `items` when they fit, else batches them for another round
    /// of condensing — or, past the last round, cuts them to fit.
    fn reduce(&mut self, round: usize, items: Vec<String>) {
        let total: usize = items.iter().map(|item| item.len() + 2).sum();
        if total <= self.reduce_chars || items.len() == 1 {
            let items = items
                .into_iter()
                .map(|item| truncate(&item, self.reduce_chars))
                .collect();
            self.stage = Stage::Answering { items };
            return;
        }
        if round >= MAX_REDUCE_ROUNDS {
            let share = (self.reduce_chars / items.len()).max(200);
            let items = items.iter().map(|item| truncate(item, share)).collect();
            self.stage = Stage::Answering { items };
            return;
        }
        let mut batches = Vec::new();
        let mut start = 0;
        let mut size = 0;
        for (index, item) in items.iter().enumerate() {
            if index > start && size + item.len() + 2 > self.reduce_chars {
                batches.push(start..index);
                start = index;
                size = 0;
            }
            size += item.len() + 2;
        }
        batches.push(start..items.len());
        if batches.len() == items.len() {
            // Every note is a batch of its own: condensing can't shrink the
            // list, so answer from cut-down notes instead.
            self.reduce(MAX_REDUCE_ROUNDS, items);
            return;
        }
        self.stage = Stage::Reducing {
            round,
            items,
            batches,
            condensed: Vec::new(),
        };
    }

    fn save(&self) {
        let saved = serde_json::to_string_pretty(&self.saved)
            .map_err(std::io::Error::other)
            .and_then(|json| std::fs::write(self.state_path(), json))
            .and_then(|()| std::fs::write(self.notes_path(), self.render()));
        if let Err(error) = saved {
            log::warn!(
                "work notes: could not save the notes in {}: {error}",
                self.dir.display()
            );
        }
    }

    /// The work file: the question, each part's notes with its lines, the
    /// answer once there is one, and the progress line at the bottom.
    pub(crate) fn render(&self) -> String {
        let mut out = format!(
            "# {}\n\nQuestion: {}\n\nThe full text ({} lines) is `{}`. Read in {} parts; line numbers refer to it.\n",
            self.title,
            self.question,
            self.total_lines,
            self.text_path()
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            self.parts.len()
        );
        for (part, note) in self.parts.iter().zip(&self.saved.notes) {
            let Some(note) = note else {
                continue;
            };
            out.push_str(&format!(
                "\n## Lines {}–{}\n\n{}\n",
                part.first_line,
                part.last_line,
                if note.trim().is_empty() {
                    "(nothing on the question)"
                } else {
                    note.trim()
                }
            ));
        }
        if let Some(answer) = &self.saved.answer {
            out.push_str(&format!("\n## Answer\n\n{}\n", answer.trim()));
        }
        let read = self
            .saved
            .notes
            .iter()
            .filter(|note| note.is_some())
            .count();
        out.push_str(&format!(
            "\n---\nProgress: {read} of {} parts read{}\n",
            self.parts.len(),
            if self.saved.answer.is_some() {
                " · answered"
            } else {
                ""
            }
        ));
        out
    }
}

/// Each `L<n>` a note cites: where the number sits in the note, and the number.
fn citations(note: &str) -> Vec<(std::ops::Range<usize>, usize)> {
    let bytes = note.as_bytes();
    let mut found = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let starts = bytes[index] == b'L'
            && (index == 0 || !bytes[index - 1].is_ascii_alphanumeric())
            && bytes.get(index + 1).is_some_and(u8::is_ascii_digit);
        if !starts {
            index += 1;
            continue;
        }
        let digits_start = index + 1;
        let mut end = digits_start;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if let Ok(number) = note[digits_start..end].parse() {
            found.push((digits_start..end, number));
        }
        index = end;
    }
    found
}

/// The words in `text` that say something: four or more letters or digits,
/// lowercased.
fn telling_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| word.len() >= 4 && word.chars().any(|c| c.is_ascii_alphabetic()))
        .map(str::to_ascii_lowercase)
        .collect()
}

/// A note that is only citations (`[L120-L173]`) says nothing.
fn only_citations(note: &str) -> bool {
    let mut rest = note.to_string();
    for (range, _) in citations(note).into_iter().rev() {
        rest.replace_range(range.start - 1..range.end, "");
    }
    rest.chars().filter(|c| c.is_ascii_alphabetic()).count() < 3
}

/// Checks each line a note cites against the text: a cited line that shares
/// no telling word with the note, when a line within three of it does, is
/// moved to that line. Small readers cite the line before or after the one
/// they mean; the text itself says which one it was.
pub(crate) fn ground_citations(note: &str, lines: &[(usize, String)]) -> String {
    let words = telling_words(&note.replace(['[', ']'], " "));
    let shared = |line: &str| {
        let line = line.to_ascii_lowercase();
        words
            .iter()
            .filter(|word| line.contains(word.as_str()))
            .count()
    };
    let score = |number: usize| {
        lines
            .iter()
            .find(|(n, _)| *n == number)
            .map_or(0, |(_, line)| shared(line))
    };
    // Citations, with `L610-L613` taken as one range.
    let cited = citations(note);
    let mut spans: Vec<(
        std::ops::Range<usize>,
        usize,
        Option<(std::ops::Range<usize>, usize)>,
    )> = Vec::new();
    let mut index = 0;
    while index < cited.len() {
        let (range, number) = cited[index].clone();
        let pair = cited
            .get(index + 1)
            .filter(|(next, _)| note.get(range.end..next.start.saturating_sub(1)) == Some("-"));
        match pair {
            Some(end) => {
                spans.push((range, number, Some(end.clone())));
                index += 2;
            }
            None => {
                spans.push((range, number, None));
                index += 1;
            }
        }
    }
    let mut grounded = note.to_string();
    for (range, first, end) in spans.into_iter().rev() {
        let last = end.as_ref().map_or(first, |(_, last)| *last);
        let (low, high) = (first.min(last), first.max(last));
        if (low..=high).any(|number| score(number) > 0) {
            continue;
        }
        let best = (low.saturating_sub(3)..=high + 3)
            .map(|candidate| (candidate, score(candidate)))
            .filter(|(_, points)| *points > 0)
            .max_by_key(|(candidate, points)| {
                let distance = if *candidate < low {
                    low - candidate
                } else {
                    candidate.saturating_sub(high)
                };
                (*points, std::cmp::Reverse(distance))
            });
        let Some((best, _)) = best else {
            continue;
        };
        match end {
            // A range that misses the line it means is widened to reach it.
            Some((end_range, _)) if best > high => {
                grounded.replace_range(end_range, &best.to_string());
            }
            Some(_) => grounded.replace_range(range, &best.to_string()),
            None => grounded.replace_range(range, &best.to_string()),
        }
    }
    grounded
}

fn is_nothing(reply: &str) -> bool {
    let first = reply
        .trim()
        .trim_matches(|c: char| !c.is_ascii_alphabetic())
        .to_ascii_uppercase();
    first == NOTHING || (first.starts_with(NOTHING) && reply.len() < 40)
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &text[..cut])
}

#[cfg(test)]
#[path = "ollama_work_notes_tests.rs"]
mod tests;
