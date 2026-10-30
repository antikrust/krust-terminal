//! Opt-in text files of local agent conversations, for scripts and other agents to read.
//!
//! With Settings > Agents > "Save conversations and images to disk" on, every local-harness
//! conversation (Claude Code, Codex, Ollama and runbook runs) is also written, as it happens, to
//! `transcripts/<started>-<id>.md` in Krust's state directory, and `transcripts/latest.md` points
//! at the newest one. The file is plain markdown: what the agent was asked, what it said, each
//! tool call with its result, the run's checklist and how each turn ended. It is append-only, so
//! a reader can poll it, and it stays after the tab is closed. Images attached to a turn are linked
//! from it; with the setting off they are deleted the next time Krust starts.
//!
//! Events are recorded where [`LocalHarnessStream`] turns them into messages, so one place covers
//! every provider. Nothing here touches the conversation itself.
//!
//! [`LocalHarnessStream`]: crate::ai::blocklist::local_harness_stream::LocalHarnessStream

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use ai::agent::{PlanUpdate, ToolCallOutcome, TurnEvent};
use chrono::Local;
use settings::Setting as _;
use krustui::{Entity, ModelContext, SingletonEntity};

use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent_sdk::driver::harness::turn_images::clear_saved_images;
use crate::settings::ConversationTranscriptSettings;

/// What a tool call's input shows, in characters.
const INPUT_CHARS: usize = 1_500;
/// What a tool call's result shows, in characters.
const RESULT_CHARS: usize = 4_000;

/// Mirrors the setting, which the stream (off the main thread) cannot read.
static ENABLED: AtomicBool = AtomicBool::new(false);
static FILES: OnceLock<Mutex<HashMap<AIConversationId, Transcript>>> = OnceLock::new();

struct Transcript {
    path: PathBuf,
    /// The last thing written was streamed text with no line end yet.
    mid_line: bool,
}

/// Keeps [`ENABLED`] in step with the setting.
pub struct ConversationTranscripts;

impl Entity for ConversationTranscripts {
    type Event = ();
}

impl SingletonEntity for ConversationTranscripts {}

impl ConversationTranscripts {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let settings = ConversationTranscriptSettings::handle(ctx);
        ENABLED.store(
            *settings.as_ref(ctx).write_conversation_transcripts.value(),
            Ordering::Relaxed,
        );
        // Off, attached images are kept only for the run of the app they were attached in.
        if !ENABLED.load(Ordering::Relaxed) {
            std::thread::spawn(clear_saved_images);
        }
        ctx.subscribe_to_model(&settings, |_, settings, _, ctx| {
            ENABLED.store(
                *settings.as_ref(ctx).write_conversation_transcripts.value(),
                Ordering::Relaxed,
            );
        });
        Self
    }
}

/// The user's prompt, as a conversation begins or is followed up.
pub(crate) fn record_user_prompt(conversation_id: AIConversationId, prompt: &str) {
    if ENABLED.load(Ordering::Relaxed) {
        append(conversation_id, &entry("## ", "you", prompt), false);
    }
}

/// Links the images attached to the turn that just began.
pub(crate) fn record_images(conversation_id: AIConversationId, paths: &[String]) {
    if ENABLED.load(Ordering::Relaxed) && !paths.is_empty() {
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
    if ENABLED.load(Ordering::Relaxed) && !text.is_empty() {
        append(conversation_id, text, true);
    }
}

/// One event of a run, other than token deltas and thinking.
pub(crate) fn record_event(conversation_id: AIConversationId, event: &TurnEvent) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
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
        id.chars().take(8).collect::<String>()
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
