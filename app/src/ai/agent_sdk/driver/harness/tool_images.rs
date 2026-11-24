//! Pictures a tool call produced, so the conversation can show them under the call that made them
//! (see [`TurnEvent::Image`]) and the session's images panel can list them.
//!
//! A picture reaches a run in one of three ways:
//! - inline in a tool's answer, as base64 image content: Claude Code's screenshot, computer-use and
//!   Read tools, or an MCP server's image content. It is saved as a file with the pasted images
//!   (`turn_images`), so it is kept or cleared by the same setting, and the answer keeps only its
//!   path.
//! - named in the text answer of an MCP tool called by the Ollama or API runner, as a
//!   [`marker`] line [`ConfiguredMcpTools`](super::ollama_stream_exec::ConfiguredMcpTools) writes
//!   when it saves the tool's image content.
//! - written by a shell command or a `run_code` program, as an image file it names that was
//!   changed while it ran (`screencapture -x shot.png`).

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

use ai::agent::{ToolCallOutcome, TurnEvent};
use serde_json::{json, Value};

use super::ollama_stream_exec::BUILTIN_RUN_CODE;
use super::turn_events_to_conversation::{is_shell_tool, shell_command_from_input};
use super::turn_images::{mime_type_for, resolved_path, save_pasted_images};

/// The most pictures one tool call shows.
const MAX_IMAGES_PER_CALL: usize = 8;

/// Slack for a file system whose modification times are coarser than the clock.
const MODIFIED_SLACK: Duration = Duration::from_secs(2);

/// What starts a [`marker`] line.
const MARKER_PREFIX: &str = "[image saved: ";

/// The line a text answer names a saved picture with. The model reads it too, so it knows where
/// the picture is.
pub(crate) fn marker(path: &str) -> String {
    format!("{MARKER_PREFIX}{path}]")
}

/// Saves `images` (as `(MIME type, base64 data)`) as files named after `name`, returning their
/// paths in order. One that cannot be decoded or written is logged and left out.
pub(crate) fn save_images(images: &[(String, String)], name: &str) -> Vec<String> {
    let name: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    save_pasted_images(
        images
            .iter()
            .take(MAX_IMAGES_PER_CALL)
            .map(|(mime_type, data)| (mime_type.as_str(), data.as_str())),
        &format!("tool-{name}"),
    )
    .into_iter()
    .map(|image| image.path)
    .collect()
}

/// The picture `value` is, as `(MIME type, base64 data)`, in any of the shapes tools answer with:
/// the Messages API's `{"type":"image","source":{"media_type","data"}}`, MCP's
/// `{"type":"image","mimeType","data"}`, and Claude Code's Read result
/// `{"type":"image","file":{"type","base64"}}`.
fn image_in(value: &Value) -> Option<(String, String)> {
    if value.get("type")?.as_str()? != "image" {
        return None;
    }
    let text = |holder: &Value, keys: &[&str]| {
        keys.iter()
            .find_map(|key| holder.get(*key).and_then(Value::as_str))
            .map(str::to_string)
    };
    let (holder, data_keys, type_keys) =
        if let Some(source) = value.get("source").filter(|source| source.is_object()) {
            (source, &["data"][..], &["media_type", "mediaType"][..])
        } else if let Some(file) = value.get("file").filter(|file| file.is_object()) {
            (file, &["base64", "data"][..], &["type", "media_type"][..])
        } else {
            (value, &["data"][..], &["mimeType", "mime_type", "media_type"][..])
        };
    let data = text(holder, data_keys)?;
    let mime_type = text(holder, type_keys)
        .filter(|mime_type| mime_type.starts_with("image/"))
        .unwrap_or_else(|| "image/png".to_string());
    Some((mime_type, data))
}

/// Every picture inside `value`, in order.
pub(crate) fn images_in(value: &Value) -> Vec<(String, String)> {
    let mut images = Vec::new();
    collect_images(value, &mut images);
    images
}

fn collect_images(value: &Value, images: &mut Vec<(String, String)>) {
    if let Some(image) = image_in(value) {
        images.push(image);
        return;
    }
    match value {
        Value::Array(items) => items.iter().for_each(|item| collect_images(item, images)),
        Value::Object(fields) => fields.values().for_each(|field| collect_images(field, images)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// `value` with each picture inside it replaced by the path it was saved to (in order), so a
/// tool card holds a path instead of megabytes of base64.
pub(crate) fn replace_images(value: &mut Value, paths: &[String]) {
    let mut next = 0;
    replace_images_from(value, paths, &mut next);
}

fn replace_images_from(value: &mut Value, paths: &[String], next: &mut usize) {
    if image_in(value).is_some() {
        let path = paths.get(*next).cloned().unwrap_or_default();
        *next += 1;
        *value = json!({ "type": "image", "path": path });
        return;
    }
    match value {
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| replace_images_from(item, paths, next)),
        Value::Object(fields) => fields
            .values_mut()
            .for_each(|field| replace_images_from(field, paths, next)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// The saved pictures a text answer names with [`marker`] lines, when the files are there.
fn marked_paths(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix(MARKER_PREFIX)?.strip_suffix(']'))
        .filter(|path| Path::new(path).is_file())
        .map(str::to_string)
        .take(MAX_IMAGES_PER_CALL)
        .collect()
}

/// The text of a tool's answer, when it is text: a string, or a string under `result`.
fn outcome_text(outcome: &ToolCallOutcome) -> Option<&str> {
    match outcome {
        ToolCallOutcome::Success(value) => value
            .as_str()
            .or_else(|| value.get("result").and_then(Value::as_str)),
        ToolCallOutcome::Error(text) => Some(text),
        ToolCallOutcome::Cancelled => None,
    }
}

/// The image files `command` names that were changed since `started`, as absolute paths.
fn images_written_by(command: &str, started: SystemTime, working_dir: &Path) -> Vec<String> {
    let since = started.checked_sub(MODIFIED_SLACK).unwrap_or(started);
    let mut paths: Vec<String> = Vec::new();
    let separators = |c: char| {
        c.is_whitespace()
            || matches!(c, '"' | '\'' | '`' | ';' | '|' | '&' | '(' | ')' | '<' | '>' | ',')
    };
    for word in command.split(separators) {
        // `--output=shot.png`
        let word = word.rsplit('=').next().unwrap_or(word);
        if word.is_empty() || mime_type_for(Path::new(word)).is_none() {
            continue;
        }
        let path = match word.strip_prefix("~/") {
            Some(rest) => match dirs::home_dir() {
                Some(home) => home.join(rest),
                None => continue,
            },
            None => resolved_path(word, working_dir),
        };
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        let changed = metadata
            .modified()
            .map(|modified| modified >= since)
            .unwrap_or(false);
        if !metadata.is_file() || !changed {
            continue;
        }
        let path = path
            .canonicalize()
            .unwrap_or_else(|_| path.clone())
            .to_string_lossy()
            .into_owned();
        if !paths.contains(&path) {
            paths.push(path);
        }
        if paths.len() >= MAX_IMAGES_PER_CALL {
            break;
        }
    }
    paths
}

/// Watches a run's events for pictures its tool calls produced that no event of their own
/// reports: an MCP answer's [`marker`] lines, and image files a shell command wrote. It reads
/// the file system, so it runs off the main thread.
pub(crate) struct ToolImageWatch {
    /// Shell calls still running: their command and the time they cannot have started before.
    shell_calls: HashMap<String, (String, SystemTime)>,
    /// When the event before this one came. A call can't have started before it, and Codex
    /// reports a command only once it has finished, start and result together.
    previous_event: SystemTime,
}

impl Default for ToolImageWatch {
    fn default() -> Self {
        Self {
            shell_calls: HashMap::new(),
            previous_event: SystemTime::now(),
        }
    }
}

impl ToolImageWatch {
    /// The pictures to show after `event`, in order.
    pub(crate) fn images_after(&mut self, event: &TurnEvent, working_dir: &Path) -> Vec<String> {
        let previous_event = std::mem::replace(&mut self.previous_event, SystemTime::now());
        match event {
            TurnEvent::ToolCallStarted {
                call_id,
                name,
                input,
            } => {
                // A shell command, or a program the Ollama and API runners'
                // `run_code` ran (`plt.savefig("chart.png")` names its file too).
                let command = if is_shell_tool(name) {
                    Some(shell_command_from_input(input))
                } else if name == BUILTIN_RUN_CODE {
                    input.get("code").and_then(Value::as_str).map(str::to_string)
                } else {
                    None
                };
                if let Some(command) = command {
                    self.shell_calls
                        .insert(call_id.clone(), (command, previous_event));
                }
                Vec::new()
            }
            TurnEvent::ToolCallResult { call_id, outcome } => {
                let mut paths = outcome_text(outcome).map(marked_paths).unwrap_or_default();
                if let Some((command, started)) = self.shell_calls.remove(call_id) {
                    if matches!(outcome, ToolCallOutcome::Success(_)) {
                        for path in images_written_by(&command, started, working_dir) {
                            if !paths.contains(&path) {
                                paths.push(path);
                            }
                        }
                    }
                }
                paths.truncate(MAX_IMAGES_PER_CALL);
                paths
            }
            TurnEvent::Text { .. }
            | TurnEvent::TextDelta { .. }
            | TurnEvent::Thinking { .. }
            | TurnEvent::Image { .. }
            | TurnEvent::TurnCompleted { .. }
            | TurnEvent::SessionInit { .. }
            | TurnEvent::PlanUpdated { .. } => Vec::new(),
        }
    }
}

#[cfg(test)]
#[path = "tool_images_tests.rs"]
mod tests;
