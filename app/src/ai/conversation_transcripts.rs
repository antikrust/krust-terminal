//! Every local agent conversation on disk, as the record models and scripts can look things up in.
//!
//! Every local-harness conversation (Claude Code, Codex, Ollama and runbook runs) is written, as it
//! happens, to `transcripts/<started>-<id>.md` in Krust's state directory, and
//! `transcripts/latest.md` points at the newest. The file is plain markdown: what the agent was
//! asked, what it said, each tool call with its result, links to the images attached, the run's
//! checklist and how each turn ended. It is append-only, so a reader can poll it, and nothing that
//! happens to a model's history (trimming, compaction) touches it.
//!
//! So a model that lost something from its window can get it back: the Ollama and API runners offer
//! the `search_conversation` tool ([`lookup`]), and every compaction leaves a note saying so
//! ([`lookup_pointer`]). Settings > Agents > "Save conversations and images to disk" decides whether
//! the files outlast the app: off, they and the attached images are deleted at the next start.
//!
//! Events are recorded where [`LocalHarnessStream`] turns them into messages, so one place covers
//! every provider. Nothing here touches the conversation itself.
//!
//! [`LocalHarnessStream`]: crate::ai::blocklist::local_harness_stream::LocalHarnessStream

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use ai::agent::{PlanUpdate, ToolCallOutcome, TurnEvent};
use chrono::Local;
use serde_json::{json, Value};
use settings::Setting as _;
use krustui::{Entity, ModelContext, SingletonEntity};
use uuid::Uuid;

use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent_sdk::driver::harness::ollama_stream_exec::McpToolSpec;
use crate::ai::agent_sdk::driver::harness::turn_images::clear_saved_images;
use crate::settings::ConversationTranscriptSettings;

/// How much of a tool call's input the record keeps, in characters.
const INPUT_CHARS: usize = 50_000;
/// How much of a tool call's result the record keeps, in characters.
const RESULT_CHARS: usize = 200_000;
/// What one `search_conversation` answer may carry, in characters.
const LOOKUP_CHARS: usize = 16_000;
/// Lines a `search_conversation` read returns unless asked for fewer, and matches a search lists.
const LOOKUP_LINES: usize = 200;
const LOOKUP_MATCHES: usize = 40;
/// What one line of a `search_conversation` answer shows, in characters.
const LOOKUP_LINE_CHARS: usize = 400;
/// The tool that looks things up in the record.
pub(crate) const SEARCH_CONVERSATION: &str = "search_conversation";

static FILES: OnceLock<Mutex<HashMap<AIConversationId, Transcript>>> = OnceLock::new();
/// Which conversation each runner session belongs to: runners know only their session.
static SESSIONS: OnceLock<Mutex<HashMap<Uuid, AIConversationId>>> = OnceLock::new();

struct Transcript {
    path: PathBuf,
    /// The last thing written was streamed text with no line end yet.
    mid_line: bool,
}

/// Applies the setting when the app starts.
pub struct ConversationTranscripts;

impl Entity for ConversationTranscripts {
    type Event = ();
}

impl SingletonEntity for ConversationTranscripts {}

impl ConversationTranscripts {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        // Off, a conversation's files and images are kept only for the run of the app they were
        // made in.
        let keep = *ConversationTranscriptSettings::as_ref(ctx)
            .write_conversation_transcripts
            .value();
        if !keep {
            std::thread::spawn(|| {
                clear_saved_images();
                if let Err(error) = fs::remove_dir_all(transcripts_dir()) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        log::warn!("transcripts: could not clear them: {error}");
                    }
                }
            });
        }
        Self
    }
}

/// The user's prompt, as a conversation begins or is followed up.
pub(crate) fn record_user_prompt(conversation_id: AIConversationId, prompt: &str) {
    append(conversation_id, &entry("## ", "you", prompt), false);
}

/// Links the images attached to the turn that just began.
pub(crate) fn record_images(conversation_id: AIConversationId, paths: &[String]) {
    if !paths.is_empty() {
        let links = paths
            .iter()
            .map(|path| format!("![image]({path})"))
            .collect::<Vec<_>>()
            .join("\n");
        append(conversation_id, &entry("### ", "images", &links), false);
    }
}

/// A chunk of streamed answer text.
pub(crate) fn record_text_delta(conversation_id: AIConversationId, text: &str) {
    if !text.is_empty() {
        append(conversation_id, text, true);
    }
}

/// Notes that a runner session belongs to `conversation_id`, so it can look the record up.
pub(crate) fn bind_session(session_id: Uuid, conversation_id: AIConversationId) {
    if let Ok(mut sessions) = SESSIONS.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        sessions.insert(session_id, conversation_id);
    }
}

fn conversation_of(session_id: &Uuid) -> Option<AIConversationId> {
    SESSIONS.get()?.lock().ok()?.get(session_id).copied()
}

/// One event of a run, other than token deltas and thinking.
pub(crate) fn record_event(conversation_id: AIConversationId, event: &TurnEvent) {
    let text = match event {
        TurnEvent::Text { text } if !text.trim().is_empty() => entry("## ", "agent", text),
        TurnEvent::ToolCallStarted { name, input, .. } => entry(
            "### ",
            &format!("tool `{name}`"),
            &fenced("json", &cut(&input.to_string(), INPUT_CHARS)),
        ),
        TurnEvent::ToolCallResult { outcome, .. } => match outcome {
            ToolCallOutcome::Success(value) => {
                let text = value
                    .get("result")
                    .and_then(|result| result.as_str())
                    .map_or_else(|| value.to_string(), str::to_string);
                entry("### ", "result", &fenced("text", &cut(&text, RESULT_CHARS)))
            }
            ToolCallOutcome::Error(message) => entry("### ", "tool error", &cut(message, RESULT_CHARS)),
            ToolCallOutcome::Cancelled => entry("### ", "tool cancelled", ""),
        },
        TurnEvent::TurnCompleted {
            is_error,
            error_message,
            ..
        } => {
            if *is_error {
                let message = error_message.as_deref().unwrap_or("the turn failed");
                entry("--- ", "turn failed", message)
            } else {
                entry("--- ", "turn finished", "")
            }
        }
        TurnEvent::SessionInit { model, cwd, .. } => entry(
            "### ",
            "run started",
            &format!(
                "model: {}\ncwd: {}",
                model.as_deref().unwrap_or("unknown"),
                cwd.as_deref().unwrap_or("unknown")
            ),
        ),
        TurnEvent::PlanUpdated { update } => entry("### ", "checklist", &plan_text(update)),
        // Token deltas and thinking are not part of the file; the rest has no text.
        TurnEvent::Text { .. }
        | TurnEvent::TextDelta { .. }
        | TurnEvent::Thinking { .. } => return,
    };
    append(conversation_id, &text, false);
}

fn plan_text(update: &PlanUpdate) -> String {
    match update {
        PlanUpdate::Create(items) | PlanUpdate::UpdatePending(items) => items
            .iter()
            .map(|item| format!("- [ ] {} ({})", item.title, item.id))
            .collect::<Vec<_>>()
            .join("\n"),
        PlanUpdate::Complete(ids) => format!("done: {}", ids.join(", ")),
        PlanUpdate::Snapshot(entries) => entries
            .iter()
            .map(|entry| format!("- [{}] {}", if entry.done { "x" } else { " " }, entry.title))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// `prefix` + time + label, then the body.
fn entry(prefix: &str, label: &str, body: &str) -> String {
    let time = Local::now().format("%H:%M:%S");
    if body.trim().is_empty() {
        format!("{prefix}{time} · {label}\n\n")
    } else {
        format!("{prefix}{time} · {label}\n{}\n\n", body.trim_end())
    }
}

fn fenced(language: &str, body: &str) -> String {
    format!("```{language}\n{}\n```", body.trim_end())
}

fn cut(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars).collect();
    format!("{kept}\n… ({} more characters)", text.chars().count() - max_chars)
}

/// The `search_conversation` tool, for the runners that run their own tools.
pub(crate) fn search_conversation_tool_spec() -> McpToolSpec {
    McpToolSpec {
        name: SEARCH_CONVERSATION.to_string(),
        description: "Look something up in the whole record of this conversation, which is kept \
            on disk even when parts of it have been trimmed or summarised out of what you can \
            see: every request, answer and tool result. With `query`, lists the lines containing \
            that text (any case) with their line numbers. With `offset`, reads on from that \
            line. Use it rather than guessing at something said or found earlier."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Text to find." },
                "offset": { "type": "integer", "description": "Lines to skip before reading: offset 125 starts at L126." },
                "limit": { "type": "integer", "description": "How many lines to read (optional)." }
            }
        }),
    }
}

/// The note a compaction leaves: what was shortened is not gone. `None` for a session with no
/// record (a queue task, say).
pub(crate) fn lookup_pointer(session_id: &Uuid) -> Option<String> {
    conversation_of(session_id)?;
    Some(format!(
        "Earlier parts of this conversation were shortened here, but all of it is saved. If you \
         need something from them, look it up with the {SEARCH_CONVERSATION} tool rather than \
         guessing."
    ))
}

/// Answers a `search_conversation` call from the conversation's record: every file it has, oldest
/// first, since a conversation resumed after a restart continues in a new one.
pub(crate) fn lookup(session_id: &Uuid, arguments: &Value) -> Result<String, String> {
    let conversation_id = conversation_of(session_id)
        .ok_or("This run has no saved conversation to search.")?;
    let text = record_files(conversation_id)
        .iter()
        .filter_map(|path| fs::read(path).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        return Err("Nothing has been saved for this conversation yet.".to_string());
    }
    // Small models often send numbers as strings ("126").
    let count = |key: &str| {
        arguments.get(key).and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
                .map(|n| n as usize)
        })
    };
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty());
    Ok(match query {
        Some(query) => search_text(&text, query),
        None => read_text(
            &text,
            count("offset").unwrap_or(0),
            count("limit").unwrap_or(LOOKUP_LINES),
        ),
    })
}

/// The lines of `text` containing `query` in any case, numbered, up to what one answer may carry.
fn search_text(text: &str, query: &str) -> String {
    let needle = query.to_lowercase();
    let matches: Vec<(usize, &str)> = text
        .lines()
        .enumerate()
        .filter(|(_, line)| line.to_lowercase().contains(&needle))
        .collect();
    if matches.is_empty() {
        return format!("No line of the conversation contains {query:?}.");
    }
    let mut out = String::new();
    let mut shown = 0;
    for (index, line) in matches.iter().take(LOOKUP_MATCHES) {
        let numbered = numbered_line(*index, line);
        if out.len() + numbered.len() > LOOKUP_CHARS {
            break;
        }
        out.push_str(&numbered);
        shown += 1;
    }
    out.push_str(&format!(
        "\n{shown} of {} matching lines shown. Read around one with `offset`.",
        matches.len()
    ));
    out
}

/// `limit` numbered lines of `text` after the first `offset`, up to what one answer may carry.
fn read_text(text: &str, offset: usize, limit: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if offset >= lines.len() {
        return format!("The conversation has {} lines, so nothing after {offset}.", lines.len());
    }
    let mut out = String::new();
    let mut end = offset;
    for line in lines.iter().skip(offset).take(limit.max(1)) {
        let numbered = numbered_line(end, line);
        if end > offset && out.len() + numbered.len() > LOOKUP_CHARS {
            break;
        }
        out.push_str(&numbered);
        end += 1;
    }
    if end < lines.len() {
        out.push_str(&format!(
            "\nLines {} to {end} of {}. offset {end} reads on.",
            offset + 1,
            lines.len()
        ));
    }
    out
}

fn numbered_line(index: usize, line: &str) -> String {
    let shown: String = line.chars().take(LOOKUP_LINE_CHARS).collect();
    format!("L{}| {shown}\n", index + 1)
}

/// The conversation's files, oldest first (their names begin with when they were started).
fn record_files(conversation_id: AIConversationId) -> Vec<PathBuf> {
    let suffix = format!("-{}.md", file_id(conversation_id));
    let mut files: Vec<PathBuf> = fs::read_dir(transcripts_dir())
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.ends_with(&suffix))
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// The part of a conversation's id its file names carry.
fn file_id(conversation_id: AIConversationId) -> String {
    conversation_id.to_string().chars().take(8).collect()
}

fn transcripts_dir() -> PathBuf {
    krust_core::paths::state_dir().join("transcripts")
}

/// Appends `text` to the conversation's file, making the file on first use. `delta` text is
/// streamed answer text: it gets a heading when a new run of it starts and no line end.
fn append(conversation_id: AIConversationId, text: &str, delta: bool) {
    let files = FILES.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut files) = files.lock() else { return };
    let transcript = match files.entry(conversation_id) {
        std::collections::hash_map::Entry::Occupied(occupied) => occupied.into_mut(),
        std::collections::hash_map::Entry::Vacant(vacant) => {
            let Some(transcript) = create(conversation_id) else { return };
            vacant.insert(transcript)
        }
    };
    let mut out = String::new();
    if delta {
        if !transcript.mid_line {
            out.push_str(&entry("## ", "agent", "").trim_end_matches('\n').to_string());
            out.push('\n');
        }
        out.push_str(text);
    } else {
        if transcript.mid_line {
            out.push_str("\n\n");
        }
        out.push_str(text);
    }
    transcript.mid_line = delta;
    let written = OpenOptions::new()
        .append(true)
        .open(&transcript.path)
        .and_then(|mut file| file.write_all(out.as_bytes()));
    if let Err(error) = written {
        log::warn!("transcript write to {} failed: {error}", transcript.path.display());
    }
}

/// Makes the conversation's file and points `latest.md` at it.
fn create(conversation_id: AIConversationId) -> Option<Transcript> {
    let dir = transcripts_dir();
    if let Err(error) = fs::create_dir_all(&dir) {
        log::warn!("transcripts: could not make {}: {error}", dir.display());
        return None;
    }
    let started = Local::now();
    let id = conversation_id.to_string();
    let name = format!(
        "{}-{}.md",
        started.format("%Y%m%d-%H%M%S"),
        file_id(conversation_id)
    );
    let path = dir.join(name);
    let header = format!(
        "# Conversation {id}\n\nstarted: {}\n\n",
        started.format("%Y-%m-%d %H:%M:%S")
    );
    if let Err(error) = fs::write(&path, header) {
        log::warn!("transcripts: could not write {}: {error}", path.display());
        return None;
    }
    #[cfg(unix)]
    {
        let latest = dir.join("latest.md");
        let _ = fs::remove_file(&latest);
        let _ = std::os::unix::fs::symlink(&path, &latest);
    }
    Some(Transcript {
        path,
        mid_line: false,
    })
}

#[cfg(test)]
#[path = "conversation_transcripts_tests.rs"]
mod tests;
