//! The suppliers of picture-making models, for Settings → AI → Images and for
//! [`crate::ai::image_gen`]: where to get a key, which models they offer, and how each one wants
//! its request written.
//!
//! Keys live with the chat providers' keys ([`ApiProviderKeys`]) under the provider's id, so a key
//! typed for OpenAI under "Bring your own key" is the one the Images page uses too. A picture model
//! is named `image/<provider>/<model>`. The failover list orders them like the chat list does:
//! `image_failover_order` is the user's order and `image_failover_disabled` what is switched off;
//! the models of a provider with a key that are in neither are added after the ordered ones.

use serde_json::{json, Value};

use krustui::{AppContext, SingletonEntity};
use settings::Setting as _;

use crate::ai::api_providers::ApiProviderKeys;
use crate::settings::AISettings;

pub const MODEL_ID_PREFIX: &str = "image/";

/// How a supplier wants a picture request written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wire {
    /// OpenAI's `/images/generations`: a `size` string; `gpt-image-*` models answer base64 and
    /// reject `response_format`.
    OpenAi,
    /// Together's: `width` and `height` as numbers, `response_format` base64.
    Together,
    /// xAI's: no size at all, `response_format` base64.
    Xai,
}

pub struct ImageModel {
    pub id: &'static str,
    pub display_name: &'static str,
    /// Costs nothing to use (within the supplier's limits).
    pub free: bool,
}

pub struct ImageProvider {
    pub id: &'static str,
    pub display_name: &'static str,
    /// `/images/generations` is appended.
    pub base_url: &'static str,
    /// Where to get a key, a link on the settings page.
    pub key_url: &'static str,
    pub note: &'static str,
    pub wire: Wire,
    pub models: &'static [ImageModel],
}

pub const IMAGE_PROVIDERS: &[ImageProvider] = &[
    ImageProvider {
        id: "openai",
        display_name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        key_url: "https://platform.openai.com/api-keys",
        note: "Paid per picture. The only one here that can draw on a transparent background or copy a reference picture.",
        wire: Wire::OpenAi,
        models: &[ImageModel {
            id: "gpt-image-1",
            display_name: "GPT Image 1",
            free: false,
        }],
    },
    ImageProvider {
        id: "together",
        display_name: "Together AI",
        base_url: "https://api.together.xyz/v1",
        key_url: "https://api.together.ai/settings/api-keys",
        note: "FLUX.1 schnell Free is rate-limited and costs nothing; the paid FLUX models are cheap.",
        wire: Wire::Together,
        models: &[
            ImageModel {
                id: "black-forest-labs/FLUX.1-schnell-Free",
                display_name: "FLUX.1 schnell (free)",
                free: true,
            },
            ImageModel {
                id: "black-forest-labs/FLUX.1-schnell",
                display_name: "FLUX.1 schnell",
                free: false,
            },
        ],
    },
    ImageProvider {
        id: "xai",
        display_name: "xAI",
        base_url: "https://api.x.ai/v1",
        key_url: "https://console.x.ai",
        note: "Paid per picture. It takes no size: pictures come out at its own size.",
        wire: Wire::Xai,
        models: &[ImageModel {
            id: "grok-2-image",
            display_name: "Grok 2 Image",
            free: false,
        }],
    },
];

pub fn provider(id: &str) -> Option<&'static ImageProvider> {
    IMAGE_PROVIDERS.iter().find(|provider| provider.id == id)
}

pub fn model_id(provider: &ImageProvider, model: &str) -> String {
    format!("{MODEL_ID_PREFIX}{}/{model}", provider.id)
}

/// The supplier and model an `image/<provider>/<model>` id names.
pub fn parse_model_id(model_id: &str) -> Option<(&'static ImageProvider, &'static ImageModel)> {
    let rest = model_id.strip_prefix(MODEL_ID_PREFIX)?;
    let (provider_id, model) = rest.split_once('/')?;
    let provider = provider(provider_id)?;
    let model = provider.models.iter().find(|listed| listed.id == model)?;
    Some((provider, model))
}

/// What a row is called on the page: "Together AI · FLUX.1 schnell (free)".
pub fn label(model_id: &str) -> String {
    match parse_model_id(model_id) {
        Some((provider, model)) => format!("{} · {}", provider.display_name, model.display_name),
        None => model_id.to_string(),
    }
}

/// The body of an `/images/generations` request for `provider`'s wire.
pub fn request_body(
    provider: &ImageProvider,
    model: &str,
    prompt: &str,
    width: u32,
    height: u32,
    transparent: bool,
) -> Value {
    let mut body = json!({ "model": model, "prompt": prompt, "n": 1 });
    match provider.wire {
        Wire::OpenAi => {
            body["size"] = json!(format!("{width}x{height}"));
            if !model.starts_with("gpt-image") {
                body["response_format"] = json!("b64_json");
            }
            if transparent {
                body["background"] = json!("transparent");
            }
        }
        Wire::Together => {
            body["width"] = json!(width);
            body["height"] = json!(height);
            body["response_format"] = json!("b64_json");
        }
        Wire::Xai => {
            body["response_format"] = json!("b64_json");
        }
    }
    body
}

/// Every model of a supplier that has a key, in the user's order: the saved order first, then
/// whatever else has a key, each once. Switched-off rows are kept (greyed on the page).
pub fn failover_rows(app: &AppContext) -> Vec<String> {
    let keys = ApiProviderKeys::as_ref(app);
    let usable = |id: &str| {
        parse_model_id(id).is_some_and(|(provider, _)| keys.key_for(provider.id).is_some())
    };
    let mut rows: Vec<String> = Vec::new();
    for id in AISettings::as_ref(app).image_failover_order.value() {
        if usable(id) && !rows.contains(id) {
            rows.push(id.clone());
        }
    }
    for provider in IMAGE_PROVIDERS {
        for model in provider.models {
            let id = model_id(provider, model.id);
            if usable(&id) && !rows.contains(&id) {
                rows.push(id);
            }
        }
    }
    rows
}

pub fn is_disabled(model_id: &str, app: &AppContext) -> bool {
    AISettings::as_ref(app)
        .image_failover_disabled
        .value()
        .iter()
        .any(|disabled| disabled == model_id)
}

/// `rows` with the one at `index` moved by `delta` places (up is negative), as the saved order.
pub fn moved(rows: &[String], index: usize, delta: isize) -> Vec<String> {
    let mut rows = rows.to_vec();
    let target = index as isize + delta;
    if index < rows.len() && target >= 0 && (target as usize) < rows.len() {
        rows.swap(index, target as usize);
    }
    rows
}

#[cfg(test)]
#[path = "image_providers_tests.rs"]
mod tests;
