//! What a model can do besides read and write text, and what a task needs from one.
//!
//! A model's [`Caps`] come from, in order: the user's override on its row
//! (`byor_model_caps`), what the provider reported (Ollama's `/api/show`, OpenRouter's model
//! list), a guess from the name for a hosted model, and for Claude Code and Codex a fixed
//! answer. A model nothing is known about has no capabilities: a task that needs one is not sent
//! to it, while text work is unaffected. The chooser itself is untouched; this filters the
//! candidates before [`crate::ai::task_router::choose`] sees them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use krustui::{AppContext, SingletonEntity};
use serde::{Deserialize, Serialize};
use settings::Setting as _;

use crate::ai::api_providers::{api_model_parts, cli_model_parts};
use crate::settings::AISettings;

/// A set of things a model can do, or that a task needs a model to do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps {
    /// Reads images.
    #[serde(default)]
    pub vision: bool,
    /// Makes images.
    #[serde(default)]
    pub image_out: bool,
    /// Turns text or images into embeddings.
    #[serde(default)]
    pub embed: bool,
}

const VISION: &str = "vision";
const IMAGE_OUT: &str = "image_out";
const EMBED: &str = "embed";

impl Caps {
    pub const NONE: Self = Self {
        vision: false,
        image_out: false,
        embed: false,
    };
    pub const VISION: Self = Self {
        vision: true,
        ..Self::NONE
    };

    /// The single capability a label names, e.g. `vision`.
    pub fn from_label(label: &str) -> Option<Self> {
        match label.trim().to_ascii_lowercase().as_str() {
            VISION | "image" | "images" => Some(Self::VISION),
            IMAGE_OUT | "image-out" => Some(Self {
                image_out: true,
                ..Self::NONE
            }),
            EMBED | "embedding" => Some(Self {
                embed: true,
                ..Self::NONE
            }),
            _ => None,
        }
    }

    /// The capabilities a list of labels names; labels nobody knows are ignored.
    pub fn from_labels<'a>(labels: impl IntoIterator<Item = &'a str>) -> Self {
        labels
            .into_iter()
            .filter_map(Self::from_label)
            .fold(Self::NONE, Self::or)
    }

    pub fn labels(self) -> Vec<&'static str> {
        let mut labels = Vec::new();
        if self.vision {
            labels.push(VISION);
        }
        if self.image_out {
            labels.push(IMAGE_OUT);
        }
        if self.embed {
            labels.push(EMBED);
        }
        labels
    }

    pub fn is_none(self) -> bool {
        self == Self::NONE
    }

    pub fn or(self, other: Self) -> Self {
        Self {
            vision: self.vision || other.vision,
            image_out: self.image_out || other.image_out,
            embed: self.embed || other.embed,
        }
    }

    pub fn without(self, other: Self) -> Self {
        Self {
            vision: self.vision && !other.vision,
            image_out: self.image_out && !other.image_out,
            embed: self.embed && !other.embed,
        }
    }

    /// Whether everything `needs` asks for is here.
    pub fn covers(self, needs: Self) -> bool {
        needs.without(self).is_none()
    }

    /// What a model with these capabilities does, in words for a task's status line.
    pub fn describe(self) -> String {
        let mut parts = Vec::new();
        if self.vision {
            parts.push("reads images");
        }
        if self.image_out {
            parts.push("makes images");
        }
        if self.embed {
            parts.push("makes embeddings");
        }
        parts.join(" and ")
    }
}

// ---------------------------------------------------------------------------
// The user's override on a row
// ---------------------------------------------------------------------------

/// What the user forced on and off for `model_id`, from `byor_model_caps` entries
/// (`<model id>=vision,-embed`: a bare label forces it on, a leading `-` forces it off).
pub fn override_for(entries: &[String], model_id: &str) -> (Caps, Caps) {
    let (mut on, mut off) = (Caps::NONE, Caps::NONE);
    for (_, labels) in entries
        .iter()
        .filter_map(|entry| entry.rsplit_once('='))
        .filter(|(id, _)| *id == model_id)
    {
        for token in labels.split(',') {
            match token.trim().strip_prefix('-') {
                Some(label) => off = off.or(Caps::from_label(label).unwrap_or(Caps::NONE)),
                None => on = on.or(Caps::from_label(token).unwrap_or(Caps::NONE)),
            }
        }
    }
    (on, off)
}

/// `entries` with `flag` (one capability) on `model_id` flipped from what it is now. When the
/// flip lands on what `base` already says the override is dropped, so a row carries one only
/// where the user disagrees with the guess.
pub fn flipped_override(entries: &[String], model_id: &str, flag: Caps, base: Caps) -> Vec<String> {
    let (on, off) = override_for(entries, model_id);
    let wanted = !base.or(on).without(off).covers(flag);
    let (mut on, mut off) = (on.without(flag), off.without(flag));
    if wanted != base.covers(flag) {
        if wanted {
            on = on.or(flag);
        } else {
            off = off.or(flag);
        }
    }
    let mut kept: Vec<String> = entries
        .iter()
        .filter(|entry| entry.rsplit_once('=').is_none_or(|(id, _)| id != model_id))
        .cloned()
        .collect();
    let tokens: Vec<String> = on
        .labels()
        .into_iter()
        .map(str::to_string)
        .chain(off.labels().into_iter().map(|label| format!("-{label}")))
        .collect();
    if !tokens.is_empty() {
        kept.push(format!("{model_id}={}", tokens.join(",")));
    }
    kept
}

// ---------------------------------------------------------------------------
// What providers reported
// ---------------------------------------------------------------------------

fn cache_file() -> PathBuf {
    krust_core::paths::state_dir().join("model_caps.json")
}

/// Written from background probes, so it lives outside the models, like the failover ledger.
fn cache() -> &'static Mutex<HashMap<String, Caps>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Caps>>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let saved = std::fs::read_to_string(cache_file())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Mutex::new(saved)
    })
}

/// Remembers what a provider reported for `model_id`, saving it when it changed.
pub fn record(model_id: &str, caps: Caps) {
    let Ok(mut cache) = cache().lock() else {
        return;
    };
    if cache.get(model_id) == Some(&caps) {
        return;
    }
    cache.insert(model_id.to_string(), caps);
    match serde_json::to_string(&*cache) {
        Ok(text) => {
            if let Err(error) = std::fs::write(cache_file(), text) {
                log::warn!("model_caps: could not save the capabilities: {error}");
            }
        }
        Err(error) => log::warn!("model_caps: could not write the capabilities: {error}"),
    }
}

/// [`record`] for many models at once: one write instead of one per model.
pub fn record_many(found: Vec<(String, Caps)>) {
    let Ok(mut cache) = cache().lock() else {
        return;
    };
    let mut changed = false;
    for (model_id, caps) in found {
        if cache.get(&model_id) != Some(&caps) {
            cache.insert(model_id, caps);
            changed = true;
        }
    }
    if !changed {
        return;
    }
    match serde_json::to_string(&*cache) {
        Ok(text) => {
            if let Err(error) = std::fs::write(cache_file(), text) {
                log::warn!("model_caps: could not save the capabilities: {error}");
            }
        }
        Err(error) => log::warn!("model_caps: could not write the capabilities: {error}"),
    }
}

/// What a provider reported for `model_id`, if it ever did.
pub fn recorded(model_id: &str) -> Option<Caps> {
    cache().lock().ok()?.get(model_id).copied()
}

/// Asks the Ollama that holds `tag` (a model name, host-qualified for another machine) what it
/// can do. `None` when it could not be asked, which is not the same as "nothing".
pub async fn probe_ollama(tag: &str) -> Option<Caps> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()?;
    crate::ai::ollama::service::fetch_capabilities(&client, tag)
        .await
        .map(|found| found.caps)
}

/// The capabilities in Ollama's `capabilities` list (`completion`, `vision`, `tools`, …).
pub fn from_ollama_capabilities(labels: &[String]) -> Caps {
    Caps {
        vision: labels.iter().any(|label| label == "vision"),
        embed: labels.iter().any(|label| label == "embedding"),
        // Ollama's image-generation models (x/flux2-klein, x/z-image-turbo) list "image".
        image_out: labels.iter().any(|label| label == "image"),
    }
}

/// The capabilities in an OpenRouter model's `architecture.input_modalities` and
/// `output_modalities`.
pub fn from_modalities(input: &[String], output: &[String]) -> Caps {
    Caps {
        vision: input.iter().any(|modality| modality == "image"),
        image_out: output.iter().any(|modality| modality == "image"),
        embed: output.iter().any(|modality| modality == "embeddings"),
    }
}

/// A guess for a hosted model that no list described, from its name. It only ever says yes, for
/// families known to read images; a model it misses can be corrected with the row's vision
/// button.
fn caps_from_name(model: &str) -> Caps {
    let lower = model.to_ascii_lowercase();
    const VISION_NAMES: &[&str] = &[
        "claude", "gemini", "gemma-3", "gemma3", "pixtral", "llava", "vision", "gpt-4o", "gpt-4.1",
        "gpt-5", "gpt-6", "llama-4", "grok-4", "-vl", "vl-", "_vl",
    ];
    if VISION_NAMES.iter().any(|name| lower.contains(name)) {
        Caps::VISION
    } else {
        Caps::NONE
    }
}

// ---------------------------------------------------------------------------
// Resolving a row
// ---------------------------------------------------------------------------

/// What `model_id` can do before the user's override: a local agent app reads image files by
/// itself; otherwise what its provider reported; otherwise, for a hosted model, the name's guess.
/// An Ollama or custom model nothing was reported for has none.
pub fn base_caps(model_id: &str) -> Caps {
    if cli_model_parts(model_id).is_some() {
        return Caps::VISION;
    }
    if let Some(known) = recorded(model_id) {
        return known;
    }
    match api_model_parts(model_id) {
        Some((_, model)) => caps_from_name(model),
        None => Caps::NONE,
    }
}

/// What `model_id` can do, with the user's override applied.
pub fn caps_of(model_id: &str, app: &AppContext) -> Caps {
    let (on, off) = override_for(AISettings::as_ref(app).byor_model_caps.value(), model_id);
    base_caps(model_id).or(on).without(off)
}

/// Whether `model_id` can take a task that needs `needs`. A task that needs nothing goes anywhere.
pub fn can_serve(model_id: &str, needs: Caps, app: &AppContext) -> bool {
    needs.is_none() || caps_of(model_id, app).covers(needs)
}

/// Why a task that needs `needs` is waiting when no model on offer has it.
pub fn unmet_message(needs: Caps) -> String {
    format!(
        "needs a model that {}; none that is on and reachable does (is Ollama running, or a \
         model with this switched on in Settings → Agents?)",
        needs.describe()
    )
}

#[cfg(test)]
#[path = "model_caps_tests.rs"]
mod tests;
