//! Images that go with a turn's prompt, kept as linked files.
//!
//! An image is a file on disk: one a task names, or one pasted into the input and saved here. The
//! turn's prompt names its path, and the user message opening the turn links to it in a field of
//! Krust's own, so the history stays small text. The bytes are read and sent only for the turn the
//! image came with; a later turn sees the path in the conversation but is not sent the image again.
//! On the wire Ollama's `/api/chat` takes an `images` list and OpenAI-style APIs take content parts.
//! Claude Code and Codex open the files themselves, so the path in the prompt is all they need,
//! including when failover hands them a turn (its recap carries the prompt). Pasted images are kept
//! or deleted at the next start according to Settings > Agents > "Save conversations and images to
//! disk" (see `ai::conversation_transcripts`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use base64::Engine as _;
use serde_json::{json, Value};
use uuid::Uuid;

/// Where a user message keeps its images in the history. Never sent as is.
pub(crate) const IMAGES_FIELD: &str = "krust_images";

/// Where pasted images are saved, in the state directory.
const SAVED_IMAGES_DIR: &str = "turn-images";

/// The largest image file a task may name. Hosted APIs refuse much beyond this.
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// One image file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TurnImage {
    pub(crate) mime_type: String,
    pub(crate) path: String,
}

fn pending() -> &'static Mutex<HashMap<Uuid, Vec<TurnImage>>> {
    static PENDING: OnceLock<Mutex<HashMap<Uuid, Vec<TurnImage>>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Sets the images the next turn of `session_id` opens with.
pub(crate) fn set_turn_images(session_id: &Uuid, images: Vec<TurnImage>) {
    if let Ok(mut pending) = pending().lock() {
        if images.is_empty() {
            pending.remove(session_id);
        } else {
            pending.insert(*session_id, images);
        }
    }
}

/// The images set for `session_id`'s turn, once: the user message opening the turn has them, a
/// later message of the same turn does not.
pub(crate) fn take_turn_images(session_id: &Uuid) -> Vec<TurnImage> {
    pending()
        .lock()
        .ok()
        .and_then(|mut pending| pending.remove(session_id))
        .unwrap_or_default()
}

/// Links `images` from a user message of the history.
pub(crate) fn attach_images(message: &mut Value, images: &[TurnImage]) {
    if images.is_empty() {
        return;
    }
    message[IMAGES_FIELD] = images
        .iter()
        .map(|image| json!({ "mime_type": image.mime_type, "path": image.path }))
        .collect();
}

/// The images a message links, read and base64-encoded as `(MIME type, data)`. One that can no
/// longer be read is logged and left out: the turn goes on with the rest.
fn encoded_images(message: &Value) -> Vec<(String, String)> {
    let Some(images) = message.get(IMAGES_FIELD).and_then(Value::as_array) else {
        return Vec::new();
    };
    images
        .iter()
        .filter_map(|image| {
            let mime_type = image.get("mime_type")?.as_str()?;
            let path = image.get("path")?.as_str()?;
            match std::fs::read(path) {
                Ok(bytes) => Some((
                    mime_type.to_string(),
                    base64::engine::general_purpose::STANDARD.encode(bytes),
                )),
                Err(error) => {
                    log::warn!("turn_images: could not read {path}: {error}");
                    None
                }
            }
        })
        .collect()
}

/// A history message as Ollama's `/api/chat` wants it: `images` is a list of bare base64.
pub(crate) fn to_ollama_message(message: &mut Value) {
    let images = encoded_images(message);
    if !images.is_empty() {
        message["images"] = images.into_iter().map(|(_, data)| Value::String(data)).collect();
    }
}

/// A history message as an OpenAI-style chat API wants it: `content` becomes the text followed by
/// one part per image, as a data URL.
pub(crate) fn to_openai_message(message: &mut Value) {
    let images = encoded_images(message);
    if images.is_empty() {
        return;
    }
    let text = message.get("content").and_then(Value::as_str).unwrap_or_default();
    let mut parts = vec![json!({ "type": "text", "text": text })];
    parts.extend(images.into_iter().map(|(mime_type, data)| {
        json!({
            "type": "image_url",
            "image_url": { "url": format!("data:{mime_type};base64,{data}") },
        })
    }));
    message["content"] = Value::Array(parts);
}

/// The MIME type of an image file, from its name. `None` for anything else.
pub(crate) fn mime_type_for(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

/// `path`, relative to `working_dir` unless it is absolute.
pub(crate) fn resolved_path(path: &str, working_dir: &Path) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        working_dir.join(path)
    }
}

/// Checks image files a task names and links them. An error says which file and why, for the
/// task's row.
pub(crate) fn load_image_files(
    paths: &[String],
    working_dir: &Path,
) -> Result<Vec<TurnImage>, String> {
    paths
        .iter()
        .map(|path| -> Result<TurnImage, String> {
            let resolved = resolved_path(path, working_dir);
            let mime_type = mime_type_for(&resolved)
                .ok_or_else(|| format!("{path} is not a PNG, JPEG, WebP or GIF image"))?;
            let size = std::fs::metadata(&resolved)
                .map_err(|error| format!("could not read the image {path}: {error}"))?
                .len();
            if size > MAX_IMAGE_BYTES {
                return Err(format!("the image {path} is over 20 MB"));
            }
            Ok(TurnImage {
                mime_type: mime_type.to_string(),
                path: resolved.to_string_lossy().into_owned(),
            })
        })
        .collect()
}

/// `images`' paths.
pub(crate) fn image_paths(images: &[TurnImage]) -> Vec<String> {
    images.iter().map(|image| image.path.clone()).collect()
}

/// `prompt` with the image files a local agent app should open named after it.
pub(crate) fn prompt_with_image_paths(prompt: &str, paths: &[String]) -> String {
    if paths.is_empty() {
        return prompt.to_string();
    }
    format!("{prompt}\n\nImages to look at:\n{}", paths.join("\n"))
}

fn extension_for(mime_type: &str) -> &'static str {
    match mime_type {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "png",
    }
}

/// Saves images pasted into the input (as `(MIME type, base64 data)`) to files named after the
/// turn, and links them. They are kept, since the conversation goes on naming them. One that cannot
/// be written is logged and left out.
pub(crate) fn save_pasted_images<'a>(
    images: impl IntoIterator<Item = (&'a str, &'a str)>,
    turn_id: &str,
) -> Vec<TurnImage> {
    let dir = krust_core::paths::state_dir().join(SAVED_IMAGES_DIR);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        log::warn!("turn_images: could not make {}: {error}", dir.display());
        return Vec::new();
    }
    images
        .into_iter()
        .enumerate()
        .filter_map(|(index, (mime_type, data))| {
            let bytes = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
            let path = dir.join(format!("{turn_id}-{index}.{}", extension_for(mime_type)));
            match std::fs::write(&path, bytes) {
                Ok(()) => Some(TurnImage {
                    mime_type: mime_type.to_string(),
                    path: path.to_string_lossy().into_owned(),
                }),
                Err(error) => {
                    log::warn!("turn_images: could not write {}: {error}", path.display());
                    None
                }
            }
        })
        .collect()
}

/// Deletes the saved pasted images. Only what [`save_pasted_images`] wrote: a task's own files are
/// never touched.
pub(crate) fn clear_saved_images() {
    let dir = krust_core::paths::state_dir().join(SAVED_IMAGES_DIR);
    if let Err(error) = std::fs::remove_dir_all(&dir) {
        if error.kind() != std::io::ErrorKind::NotFound {
            log::warn!("turn_images: could not clear {}: {error}", dir.display());
        }
    }
}

#[cfg(test)]
#[path = "turn_images_tests.rs"]
mod tests;
