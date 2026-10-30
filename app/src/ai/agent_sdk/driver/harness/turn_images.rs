//! Images that go with a turn's prompt, for the runners that send them themselves.
//!
//! A turn's images are set for its session before the runner starts, and the first user message
//! of the turn takes them ([`take_turn_images`]). In the history they sit in a field of Krust's
//! own beside the message's plain-text `content`, so everything that reads a history as text keeps
//! working; only what goes on the wire is converted: Ollama's `/api/chat` takes an `images` list,
//! OpenAI-style APIs take content parts. Claude Code and Codex open image files themselves, so
//! they are given paths in the prompt instead.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use base64::Engine as _;
use serde_json::{json, Value};
use uuid::Uuid;

/// Where a user message keeps its images in the history. Never sent as is.
pub(crate) const IMAGES_FIELD: &str = "krust_images";

/// The largest image file a task may name. Hosted APIs refuse much beyond this.
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// One image, base64-encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TurnImage {
    pub(crate) mime_type: String,
    pub(crate) data: String,
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

/// Puts `images` on a user message of the history.
pub(crate) fn attach_images(message: &mut Value, images: &[TurnImage]) {
    if images.is_empty() {
        return;
    }
    message[IMAGES_FIELD] = images
        .iter()
        .map(|image| json!({ "mime_type": image.mime_type, "data": image.data }))
        .collect();
}

fn images_of(message: &Value) -> Vec<TurnImage> {
    message
        .get(IMAGES_FIELD)
        .and_then(Value::as_array)
        .map(|images| {
            images
                .iter()
                .filter_map(|image| {
                    Some(TurnImage {
                        mime_type: image.get("mime_type")?.as_str()?.to_string(),
                        data: image.get("data")?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A history message as Ollama's `/api/chat` wants it: `images` is a list of bare base64.
pub(crate) fn to_ollama_message(message: &mut Value) {
    let images = images_of(message);
    if !images.is_empty() {
        message["images"] = images.into_iter().map(|image| Value::String(image.data)).collect();
    }
}

/// A history message as an OpenAI-style chat API wants it: `content` becomes the text followed by
/// one part per image, as a data URL.
pub(crate) fn to_openai_message(message: &mut Value) {
    let images = images_of(message);
    if images.is_empty() {
        return;
    }
    let text = message.get("content").and_then(Value::as_str).unwrap_or_default();
    let mut parts = vec![json!({ "type": "text", "text": text })];
    parts.extend(images.into_iter().map(|image| {
        json!({
            "type": "image_url",
            "image_url": { "url": format!("data:{};base64,{}", image.mime_type, image.data) },
        })
    }));
    message["content"] = Value::Array(parts);
}

/// How much of a message's serialized size is its images, which do not count as context the way
/// text does: a history's size is measured without them.
pub(crate) fn image_chars(message: &Value) -> usize {
    message
        .get(IMAGES_FIELD)
        .map_or(0, |images| images.to_string().len())
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

/// Reads image files a task names. An error says which file and why, for the task's row.
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
            let bytes = std::fs::read(&resolved)
                .map_err(|error| format!("could not read the image {path}: {error}"))?;
            Ok(TurnImage {
                mime_type: mime_type.to_string(),
                data: base64::engine::general_purpose::STANDARD.encode(bytes),
            })
        })
        .collect()
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

/// Writes images pasted into the input to files, for a local agent app, and returns their paths.
/// One that cannot be written is logged and left out.
pub(crate) fn save_for_local_app(images: &[TurnImage], session_id: &Uuid) -> Vec<String> {
    let dir = krust_core::paths::state_dir().join("turn-images");
    if let Err(error) = std::fs::create_dir_all(&dir) {
        log::warn!("turn_images: could not make {}: {error}", dir.display());
        return Vec::new();
    }
    images
        .iter()
        .enumerate()
        .filter_map(|(index, image)| {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&image.data)
                .ok()?;
            let path = dir.join(format!(
                "{session_id}-{index}.{}",
                extension_for(&image.mime_type)
            ));
            match std::fs::write(&path, bytes) {
                Ok(()) => Some(path.to_string_lossy().into_owned()),
                Err(error) => {
                    log::warn!("turn_images: could not write {}: {error}", path.display());
                    None
                }
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "turn_images_tests.rs"]
mod tests;
