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
//! rounds, if even they are too long — into one short answer. Each line the
//! answer rests on is then checked against the text itself; a claim the
//! lines plainly contradict is dropped with the notes behind it and the
//! question answered again. The answer goes back to the model in place of
//! the text, with pointers to the full text and the notes.
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
/// How many of the lines an answer cites are checked against the text.
const MAX_CHECKS: usize = 4;
/// How many findings of one part are checked; findings past these are
/// dropped — a reader that lists more is listing noise.
const MAX_FINDING_CHECKS: usize = 6;
/// Answers given in all, when checks keep rejecting what one rests on.
const MAX_CHECK_ROUNDS: usize = 2;

/// Small readers (llama3.1:8b) answered a 104-part source file with one bare
/// label per part and no words, the broken line's part included; the
/// finding's form and "a label alone says nothing" are for them. The form
/// is shown without a sample finding: given one ("the write fails here
/// because the disk is full"), llama3.1:8b wrote its findings after it ("the
/// write fails here because the numerator is zero").
const READ_SYSTEM_PROMPT: &str = "You read one part of a long text for someone who can't hold all of it at \
once. Each line starts with its label, like L612|. Look at every line of the part, then reply in one of two ways.

If lines bear directly on the question — a problem, a failure, or the thing asked about — write one line for each \
finding: its label in brackets, copied exactly, then in your own words what that line shows. At most eight \
findings, no preamble. A label alone, with no words, says nothing.

If nothing in this part bears on the question, reply with the single word NOTHING.";

/// Asked of each finding a reader makes before it becomes a note: small
/// readers told to look for a problem find one in every part, and checking
/// one claim against the lines it cites is a smaller job than the search.
/// Three small readers in turn called a statement that goes on over the
/// next lines "incomplete", or a name defined a few lines up "undefined",
/// and the same models checking the finding let it through: the lines
/// around a finding are shown so that the checker reads them.
const FINDING_CHECK_SYSTEM_PROMPT: &str = "You check one finding a reader made about a long text, against the \
lines it cites, each shown with its label and the lines around it. Reply YES if those lines show what the finding \
says, or show something that bears it out. Reply NO if they contradict it, or show nothing of what it says — a \
statement that goes on over the next lines is not incomplete, and a name defined on a nearby line is not \
undefined. Then give one short reason.";

const REDUCE_SYSTEM_PROMPT: &str = "You condense notes taken on one stretch of a long text. Keep everything that \
bears on the question and every line number cited; drop repeats and anything off the question. Reply with the \
condensed notes only.";

const ANSWER_SYSTEM_PROMPT: &str = "You answer a question about a long text from notes taken on it part by part, \
in order. Answer from the notes alone and cite their line numbers. If the notes don't settle it, say what is \
missing. Be brief: at most fifteen lines.";

/// llama3.1:8b answered a broken source file with a line "missing a
/// semicolon" that had one; shown the line, a model can tell. The check is
/// framed by the question: bonsai-2:27b, asked only whether "the line that
/// matters is L6197" held, read "matters" as "is correct" and said FALSE of
/// the very typo it had found.
const CHECK_SYSTEM_PROMPT: &str = "You check one line an answer rests on. The answer to the question below says \
this line (with those near it) shows what the question asks about. The lines are shown with their labels. Reply \
FALSE only if they plainly show otherwise — for example, the answer says a line lacks something the line has, or \
has a problem it doesn't; a statement that goes on over the next lines is not incomplete, and a name defined on a \
nearby line is not undefined. Otherwise reply TRUE: when the lines bear the answer out, however it is worded, and \
when they can't settle it. Then give one short reason.";

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

/// What saved notes are for: the question, the text, how it was split and
/// the prompt it was read with — notes taken with an earlier prompt are read
/// again, not resumed.
fn notes_key(question: &str, text: &str, part_chars: usize) -> String {
    stable_hash(&[question, text, &part_chars.to_string(), READ_SYSTEM_PROMPT])
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
    /// Per part, the findings its reader made that the check dropped.
    #[serde(default)]
    dropped: Vec<Vec<String>>,
    answer: Option<String>,
    /// Claims an answer made that the text, checked, did not bear out.
    #[serde(default)]
    rejected: Vec<Rejected>,
}

/// A claim an answer made about some lines, which those lines contradicted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Rejected {
    first: usize,
    last: usize,
    claim: String,
    reason: String,
}

/// A reading reply being checked finding by finding before it becomes its
/// part's note.
#[derive(Debug, Clone, PartialEq)]
struct PendingNote {
    part: usize,
    /// The reply's findings: each a line that cites the text, with any
    /// uncited lines after it.
    findings: Vec<String>,
    /// The findings checked, by index: those that cite the text, up to
    /// [`MAX_FINDING_CHECKS`].
    to_check: Vec<usize>,
    /// Per finding in `to_check` checked so far: whether it held.
    held: Vec<bool>,
}

/// A line (or lines) an answer cites, with the answer's line that cites it.
#[derive(Debug, Clone, PartialEq)]
struct Claim {
    first: usize,
    last: usize,
    text: String,
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
    /// The answer's claims being checked against the text: per claim checked
    /// so far, `None` if it held, else why not.
    Checking {
        answer: String,
        claims: Vec<Claim>,
        verdicts: Vec<Option<String>>,
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
    /// Answers given so far this run, for [`MAX_CHECK_ROUNDS`].
    answers_given: usize,
    /// The part whose reply is being checked, finding by finding.
    pending: Option<PendingNote>,
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
        let key = notes_key(question, text, part_chars);
        let stem = format!("{}-{}", slug(title), &key[..8]);
        let mut notes = Self {
            title: title.to_string(),
            question: question.to_string(),
            total_lines: text.lines().count(),
            saved: Saved {
                key: key.clone(),
                notes: vec![None; parts.len()],
                dropped: vec![Vec::new(); parts.len()],
                answer: None,
                rejected: Vec::new(),
            },
            parts,
            stage: Stage::Reading,
            reduce_chars,
            answers_given: 0,
            pending: None,
            dir: dir.to_path_buf(),
            stem,
        };
        if let Some(saved) = std::fs::read_to_string(notes.state_path())
            .ok()
            .and_then(|json| serde_json::from_str::<Saved>(&json).ok())
            .filter(|saved| saved.key == key && saved.notes.len() == notes.parts.len())
        {
            notes.saved = saved;
            notes.saved.dropped.resize(notes.parts.len(), Vec::new());
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
                if let Some(pending) = &self.pending {
                    let finding = &pending.findings[pending.to_check[pending.held.len()]];
                    let (which, excerpt) = self.excerpt_around(&cited_lines(finding));
                    return Some(NextCall {
                        system: FINDING_CHECK_SYSTEM_PROMPT,
                        user: format!(
                            "Question: {}\n\nA reader's finding: {}\n\n{which} of {}:\n```\n{excerpt}\n```",
                            self.question,
                            finding.trim(),
                            self.title,
                        ),
                        label: format!(
                            "part {} of {}: checking finding {} of {}",
                            pending.part + 1,
                            self.parts.len(),
                            pending.held.len() + 1,
                            pending.to_check.len()
                        ),
                    });
                }
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
            Stage::Checking {
                claims, verdicts, ..
            } => {
                let claim = claims.get(verdicts.len())?;
                // The lines around every line the answer cites, not just
                // this claim's: a claim about one of them may speak of
                // another (the misspelled name at L6197, defined at L6170).
                let cited: Vec<(usize, usize)> = claims
                    .iter()
                    .map(|claim| (claim.first, claim.last))
                    .collect();
                let (which, excerpt) = self.excerpt_around(&cited);
                Some(NextCall {
                    system: CHECK_SYSTEM_PROMPT,
                    user: format!(
                        "Question: {}\n\nThe answer's claim: {}\n\n{which} of {}:\n```\n{excerpt}\n```",
                        self.question, claim.text, self.title,
                    ),
                    label: format!(
                        "checking the answer against the text ({} of {})",
                        verdicts.len() + 1,
                        claims.len()
                    ),
                })
            }
            Stage::Done => None,
        }
    }

    /// The lines around each of `cited` — four either side of each, at most
    /// twelve per citation and thirty in all — with a `…` where they skip,
    /// and which lines they are ("Lines 6170–6178 and 6193–6201").
    fn excerpt_around(&self, cited: &[(usize, usize)]) -> (String, String) {
        let mut ranges: Vec<(usize, usize)> = cited
            .iter()
            .map(|&(first, last)| excerpt_range(first, last.min(first + 3), self.total_lines))
            .collect();
        ranges.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (from, to) in ranges {
            match merged.last_mut() {
                Some((_, end)) if from <= *end + 1 => *end = (*end).max(to),
                _ => merged.push((from, to)),
            }
        }
        let mut shown = 0;
        merged.retain_mut(|(from, to)| {
            if shown >= 30 {
                return false;
            }
            *to = (*to).min(*from + (30 - shown) - 1);
            shown += *to - *from + 1;
            true
        });
        let which = merged
            .iter()
            .map(|(from, to)| format!("{from}–{to}"))
            .collect::<Vec<_>>();
        let which = match which.split_last() {
            Some((last, rest)) if !rest.is_empty() => {
                format!("Lines {} and {last}", rest.join(", "))
            }
            Some((last, _)) => format!("Lines {last}"),
            None => "Lines".to_string(),
        };
        let excerpt = merged
            .iter()
            .map(|(from, to)| self.excerpt(*from, *to))
            .collect::<Vec<_>>()
            .join("\n…\n");
        (which, excerpt)
    }

    /// Lines `from` to `to` of the text, labelled as the readers saw them.
    fn excerpt(&self, from: usize, to: usize) -> String {
        self.parts
            .iter()
            .flat_map(|part| &part.lines)
            .filter(|(number, _)| (from..=to).contains(number))
            .map(|(number, line)| format!("L{number}| {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Records the reply to the call `next_call` gave; `None` when that call
    /// failed. Saves the notes after every call.
    pub(crate) fn accept(&mut self, reply: Option<&str>) {
        let reply = reply.map(str::trim).filter(|reply| !reply.is_empty());
        match &mut self.stage {
            Stage::Reading => {
                if let Some(mut pending) = self.pending.take() {
                    // A check that failed to come back drops nothing.
                    pending.held.push(!reply.is_some_and(says_no));
                    if pending.held.len() < pending.to_check.len() {
                        self.pending = Some(pending);
                    } else {
                        self.keep_checked(pending);
                    }
                } else if let Some(index) = self.saved.notes.iter().position(Option::is_none) {
                    let note = match reply {
                        Some(reply) if is_nothing(reply) || only_citations(reply) => String::new(),
                        Some(reply) => {
                            let lines = &self.parts[index].lines;
                            cite_quoted_lines(&ground_citations(reply, lines), lines)
                        }
                        None => "(this part could not be read)".to_string(),
                    };
                    let findings = findings_in(&note);
                    let to_check: Vec<usize> = findings
                        .iter()
                        .enumerate()
                        .filter(|(_, finding)| !cited_lines(finding).is_empty())
                        .map(|(index, _)| index)
                        .take(MAX_FINDING_CHECKS)
                        .collect();
                    if reply.is_none() || to_check.is_empty() {
                        self.saved.notes[index] = Some(note);
                    } else {
                        self.pending = Some(PendingNote {
                            part: index,
                            findings,
                            to_check,
                            held: Vec::new(),
                        });
                    }
                }
                if self.pending.is_none() && self.saved.notes.iter().all(Option::is_some) {
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
                self.answers_given += 1;
                match reply {
                    Some(reply) => {
                        let claims = answer_claims(reply, self.total_lines);
                        if claims.is_empty() {
                            self.saved.answer = Some(reply.to_string());
                            self.stage = Stage::Done;
                        } else {
                            self.stage = Stage::Checking {
                                answer: reply.to_string(),
                                claims,
                                verdicts: Vec::new(),
                            };
                        }
                    }
                    // No answer came back: the notes themselves are the answer.
                    None => {
                        self.saved.answer = Some(truncate(&items.join("\n\n"), self.reduce_chars));
                        self.stage = Stage::Done;
                    }
                }
            }
            Stage::Checking {
                answer,
                claims,
                verdicts,
            } => {
                // A check that failed to come back rejects nothing.
                verdicts.push(reply.filter(|reply| rejects(reply)).map(|reply| {
                    truncate(&reply.split_whitespace().collect::<Vec<_>>().join(" "), 200)
                }));
                if verdicts.len() == claims.len() {
                    let answer = std::mem::take(answer);
                    let failed: Vec<Rejected> = claims
                        .iter()
                        .zip(verdicts.iter())
                        .filter_map(|(claim, verdict)| {
                            Some(Rejected {
                                first: claim.first,
                                last: claim.last,
                                claim: claim.text.clone(),
                                reason: verdict.clone()?,
                            })
                        })
                        .collect();
                    self.finish_checking(answer, failed);
                }
            }
            Stage::Done => {}
        }
        self.save();
    }

    /// A part's note from its checked reply: the findings that held, and
    /// lines that cite nothing. Cited findings past [`MAX_FINDING_CHECKS`]
    /// go unchecked and are dropped.
    fn keep_checked(&mut self, pending: PendingNote) {
        let mut kept = Vec::new();
        let mut dropped = Vec::new();
        for (index, finding) in pending.findings.iter().enumerate() {
            let keep = match pending
                .to_check
                .iter()
                .position(|&checked| checked == index)
            {
                Some(at) => pending.held.get(at).copied().unwrap_or(true),
                None => cited_lines(finding).is_empty(),
            };
            if keep {
                kept.push(finding.clone());
            } else {
                dropped.push(finding.clone());
            }
        }
        self.saved.notes[pending.part] = Some(kept.join("\n"));
        if let Some(slot) = self.saved.dropped.get_mut(pending.part) {
            *slot = dropped;
        }
    }

    /// An answer whose claims held stands. One with a claim the text
    /// contradicted is given again without the notes behind that claim —
    /// once; after that it stands with what failed said.
    fn finish_checking(&mut self, answer: String, failed: Vec<Rejected>) {
        if failed.is_empty() {
            self.saved.answer = Some(answer);
            self.stage = Stage::Done;
            return;
        }
        self.saved.rejected.extend(failed.iter().cloned());
        let failed = failed
            .iter()
            .map(|rejected| format!("L{} — {}", rejected.first, rejected.reason))
            .collect::<Vec<_>>()
            .join("; ");
        if self.answers_given < MAX_CHECK_ROUNDS {
            if self.note_items().is_empty() {
                self.saved.answer = Some(format!(
                    "Nothing found in the text bears on the question. The notes pointed elsewhere, but checked \
                     against the text that did not hold up: {failed}"
                ));
                self.stage = Stage::Done;
            } else {
                self.start_reducing(0);
            }
            return;
        }
        self.saved.answer = Some(format!(
            "{answer}\n\n(Checked against the text, this did not hold up: {failed})"
        ));
        self.stage = Stage::Done;
    }

    /// The notes worth reducing, each labelled with its lines.
    fn note_items(&self) -> Vec<String> {
        self.parts
            .iter()
            .zip(&self.saved.notes)
            .filter_map(|(part, note)| {
                let note = self.without_rejected(note.as_deref()?.trim());
                (!note.is_empty())
                    .then(|| format!("Lines {}–{}:\n{note}", part.first_line, part.last_line))
            })
            .collect()
    }

    /// A note without its findings on lines a check found the answer wrong
    /// about — each finding whole, with the lines that go on from its label.
    fn without_rejected(&self, note: &str) -> String {
        if self.saved.rejected.is_empty() {
            return note.to_string();
        }
        findings_in(note)
            .into_iter()
            .filter(|finding| {
                !cited_lines(finding).iter().any(|(first, last)| {
                    self.saved
                        .rejected
                        .iter()
                        .any(|rejected| rejected.first <= *last && *first <= rejected.last)
                })
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
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
        for (index, (part, note)) in self.parts.iter().zip(&self.saved.notes).enumerate() {
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
            let dropped = self.saved.dropped.get(index).map_or(&[][..], Vec::as_slice);
            if !dropped.is_empty() {
                out.push_str("\nDropped, as the lines didn't bear them out:\n");
                for finding in dropped {
                    out.push_str(&format!("- {}\n", finding.trim()));
                }
            }
        }
        if !self.saved.rejected.is_empty() {
            out.push_str("\n## Checked against the text, and dropped\n\n");
            for rejected in &self.saved.rejected {
                out.push_str(&format!(
                    "- L{}: \"{}\" — {}\n",
                    rejected.first, rejected.claim, rejected.reason
                ));
            }
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

/// A note that quotes a token — `shaepd`, "kMaxVoices" — found on just one
/// line of its part, and doesn't cite that line, gets the citation added
/// after the quote. granite4 named the misspelled identifier in a broken
/// file and cited a line 23 lines from the one that has it.
fn cite_quoted_lines(note: &str, lines: &[(usize, String)]) -> String {
    let cited = cited_lines(note);
    let mut additions: Vec<(usize, usize)> = Vec::new();
    for (end, token) in quoted_tokens(note) {
        let mut holders: Vec<usize> = lines
            .iter()
            .filter(|(_, line)| line.contains(token))
            .map(|(number, _)| *number)
            .collect();
        holders.dedup();
        let [line] = holders[..] else {
            continue;
        };
        let already = cited
            .iter()
            .any(|(first, last)| (*first..=*last).contains(&line))
            || additions.iter().any(|(_, added)| *added == line);
        if !already {
            additions.push((end, line));
        }
    }
    let mut out = note.to_string();
    for (end, line) in additions.into_iter().rev() {
        out.insert_str(end, &format!(" [L{line}]"));
    }
    out
}

/// The tokens a note quotes in backticks or quotes — three to sixty
/// characters, no spaces — each with where its closing quote ends.
fn quoted_tokens(note: &str) -> Vec<(usize, &str)> {
    let mut found = Vec::new();
    for quote in ['`', '"', '\''] {
        let mut from = 0;
        while let Some(open) = note[from..].find(quote).map(|at| from + at) {
            let start = open + quote.len_utf8();
            // "it's", "doesn't": an apostrophe inside a word opens nothing.
            if quote == '\''
                && note[..open]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
            {
                from = start;
                continue;
            }
            let Some(close) = note[start..].find(quote).map(|at| start + at) else {
                break;
            };
            let token = &note[start..close];
            if (3..=60).contains(&token.chars().count()) && !token.contains(char::is_whitespace) {
                found.push((close + quote.len_utf8(), token));
            }
            from = close + quote.len_utf8();
        }
    }
    found.sort_unstable_by_key(|(end, _)| *end);
    found
}

/// The lines an answer rests on: each it cites — `L7550`, `[L610-L615]`,
/// "line 7550" — with the answer's line that cites it, at most
/// [`MAX_CHECKS`], in the order cited.
fn answer_claims(answer: &str, total_lines: usize) -> Vec<Claim> {
    let mut claims: Vec<Claim> = Vec::new();
    for text in answer
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        for (first, last) in cited_lines(text) {
            if first == 0 || first > total_lines || claims.iter().any(|claim| claim.first == first)
            {
                continue;
            }
            claims.push(Claim {
                first,
                last: last.clamp(first, total_lines),
                text: text.to_string(),
            });
            if claims.len() == MAX_CHECKS {
                return claims;
            }
        }
    }
    claims
}

/// The lines `text` cites, as ranges: `L12`, `L12-L14`, "line 12",
/// "lines 12–14".
fn cited_lines(text: &str) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let cited = citations(text);
    let mut index = 0;
    while index < cited.len() {
        let (range, first) = cited[index].clone();
        let paired = cited.get(index + 1).filter(|(next, _)| {
            matches!(
                text.get(range.end..next.start.saturating_sub(1)),
                Some("-" | "–")
            )
        });
        match paired {
            Some((_, last)) => {
                found.push((first.min(*last), first.max(*last)));
                index += 2;
            }
            None => {
                found.push((first, first));
                index += 1;
            }
        }
    }
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find("line") {
        let start = from + at;
        from = start + "line".len();
        if start > 0 && lower.as_bytes()[start - 1].is_ascii_alphanumeric() {
            continue;
        }
        let rest = lower[from..].strip_prefix('s').unwrap_or(&lower[from..]);
        let rest = rest.trim_start();
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        let Ok(first) = digits.parse::<usize>() else {
            continue;
        };
        let after = rest[digits.len()..].trim_start();
        let last = ["-", "–", "to "]
            .iter()
            .find_map(|separator| after.strip_prefix(separator))
            .and_then(|tail| {
                let tail = tail.trim_start().trim_start_matches('l');
                tail.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<usize>()
                    .ok()
            })
            .unwrap_or(first);
        found.push((first.min(last), first.max(last)));
    }
    found
}

/// The lines a claim about `first`–`last` is checked against: those, with
/// four either side, at most sixteen.
fn excerpt_range(first: usize, last: usize, total_lines: usize) -> (usize, usize) {
    let from = first.saturating_sub(4).max(1);
    let to = (last + 4).min(from + 15).min(total_lines.max(1));
    (from, to)
}

/// A note's findings: each line that opens with a label starts one; the
/// lines after it, whatever they cite, belong to it.
fn findings_in(note: &str) -> Vec<String> {
    let mut findings: Vec<String> = Vec::new();
    for line in note.lines().filter(|line| !line.trim().is_empty()) {
        match findings.last_mut() {
            Some(finding) if !starts_with_label(line) => {
                finding.push('\n');
                finding.push_str(line);
            }
            _ => findings.push(line.to_string()),
        }
    }
    findings
}

/// Whether a line opens with a label, past a list marker: `[L612] …`,
/// `L612| …`, `- [L120-L134] …`, `1. [L8] …`. Only such a line starts a
/// finding: bonsai-2:27b wrote the right one over two lines, the second
/// naming the line the misspelled name is defined on, and cut at every line
/// that cited something it became two findings that each failed its check.
fn starts_with_label(line: &str) -> bool {
    let rest = line
        .trim_start()
        .trim_start_matches(['-', '*', '•', '>', ' ', '`']);
    let rest = match rest.find(|c: char| !c.is_ascii_digit()) {
        Some(at) if at > 0 && rest[at..].starts_with(['.', ')']) => rest[at + 1..].trim_start(),
        _ => rest,
    };
    let rest = rest.trim_start_matches(['[', '(', '*', '`']);
    if rest.starts_with('L') && rest[1..].starts_with(|c: char| c.is_ascii_digit()) {
        return true;
    }
    let lower = rest.to_ascii_lowercase();
    lower
        .strip_prefix("lines")
        .or_else(|| lower.strip_prefix("line"))
        .is_some_and(|after| after.trim_start().starts_with(|c: char| c.is_ascii_digit()))
}

/// Whether a finding check's reply says no: its first YES or NO decides.
fn says_no(reply: &str) -> bool {
    reply
        .split(|c: char| !c.is_ascii_alphabetic())
        .take(12)
        .find_map(|word| match word.to_ascii_uppercase().as_str() {
            "YES" => Some(false),
            "NO" => Some(true),
            _ => None,
        })
        .unwrap_or(false)
}

/// Whether a check's reply says the claim doesn't hold.
fn rejects(reply: &str) -> bool {
    let head = reply
        .chars()
        .take(80)
        .collect::<String>()
        .to_ascii_uppercase();
    match (head.find("FALSE"), head.find("TRUE")) {
        (Some(false_at), Some(true_at)) => false_at < true_at,
        (Some(_), None) => true,
        _ => false,
    }
}

/// A reply that is NOTHING, or opens with it and cites no line ("Nothing
/// in this part bears on the question, which asks about …").
fn is_nothing(reply: &str) -> bool {
    let first = reply
        .trim()
        .trim_matches(|c: char| !c.is_ascii_alphabetic())
        .to_ascii_uppercase();
    first == NOTHING || (first.starts_with(NOTHING) && citations(reply).is_empty())
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
