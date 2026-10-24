//! Cloud model providers reached over the OpenAI chat-completions API with
//! the user's own key: Z.ai (Zhipu's GLM), Groq, and Gemini.
//!
//! The local harness calls these directly (see
//! `agent_sdk::driver::harness::api_stream_exec`); nothing goes through
//! Krust's cloud, and the keys never leave this machine except to the
//! provider they belong to. That is also why they are kept here rather than in
//! `ai::api_keys::ApiKeyManager`, whose keys are forwarded to the Krust server
//! and whose settings are gated on a Krust plan.
//!
//! Each provider's models are a short curated list of its free-tier text
//! models. A model id is `api/<provider>/<model>` — the model part may itself
//! contain slashes (`api/groq/openai/gpt-oss-120b`).

use std::collections::HashMap;

use krust_errors::report_error;
use krustui::{Entity, ModelContext, SingletonEntity};
use krustui_extras::secure_storage::{self, AppContextExt};

use crate::ai::harness_availability::HarnessModelInfo;

pub const API_MODEL_ID_PREFIX: &str = "api/";
/// The provider key under which the picker persists the last API model.
pub const API_HARNESS_CONFIG_NAME: &str = "api";

/// The keychain item holding every provider's key, as a JSON object keyed by
/// provider id.
const SECURE_STORAGE_KEY: &str = "ApiProviderKeys";

pub struct ApiProvider {
    pub id: &'static str,
    pub display_name: &'static str,
    /// The OpenAI-compatible base; `/chat/completions` is appended.
    pub base_url: &'static str,
    /// Where to get a key, shown on the settings page.
    pub key_url: &'static str,
    /// What the free tier is and costs, shown on the settings page.
    pub free_tier_note: &'static str,
    pub models: &'static [ApiModel],
}

pub struct ApiModel {
    pub id: &'static str,
    pub display_name: &'static str,
}

pub const PROVIDERS: &[ApiProvider] = &[
    ApiProvider {
        id: "zai",
        display_name: "Z.ai",
        base_url: "https://api.z.ai/api/paas/v4",
        key_url: "z.ai/manage-apikey/apikey-list",
        free_tier_note: "GLM-4.7-Flash and GLM-4.5-Flash are free. The free tier runs one \
            request at a time.",
        models: &[
            ApiModel {
                id: "glm-4.7-flash",
                display_name: "GLM-4.7 Flash",
            },
            ApiModel {
                id: "glm-4.5-flash",
                display_name: "GLM-4.5 Flash",
            },
        ],
    },
    ApiProvider {
        id: "groq",
        display_name: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        key_url: "console.groq.com/keys",
        free_tier_note: "Free with daily request and token limits. Very fast.",
        models: &[
            ApiModel {
                id: "openai/gpt-oss-120b",
                display_name: "GPT-OSS 120B",
            },
            ApiModel {
                id: "llama-3.3-70b-versatile",
                display_name: "Llama 3.3 70B",
            },
            ApiModel {
                id: "openai/gpt-oss-20b",
                display_name: "GPT-OSS 20B",
            },
            ApiModel {
                id: "llama-3.1-8b-instant",
                display_name: "Llama 3.1 8B",
            },
        ],
    },
    ApiProvider {
        id: "gemini",
        display_name: "Gemini",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        key_url: "aistudio.google.com/apikey",
        free_tier_note: "Flash models are free with low daily limits. Google uses free-tier \
            prompts to improve its products.",
        // Google's rolling aliases: they follow the newest Flash models, so
        // the list doesn't go stale every few months.
        models: &[
            ApiModel {
                id: "gemini-flash-latest",
                display_name: "Gemini Flash",
            },
            ApiModel {
                id: "gemini-flash-lite-latest",
                display_name: "Gemini Flash-Lite",
            },
        ],
    },
];

pub fn provider(id: &str) -> Option<&'static ApiProvider> {
    PROVIDERS.iter().find(|provider| provider.id == id)
}

/// The provider and provider-side model name an `api/…` id refers to.
pub fn api_model_parts(model_id: &str) -> Option<(&'static ApiProvider, &str)> {
    let rest = model_id.strip_prefix(API_MODEL_ID_PREFIX)?;
    let (provider_id, model) = rest.split_once('/')?;
    if model.is_empty() {
        return None;
    }
    Some((provider(provider_id)?, model))
}

pub fn model_id_for(provider: &ApiProvider, model: &str) -> String {
    format!("{API_MODEL_ID_PREFIX}{}/{model}", provider.id)
}

/// The keys the user has set, one per provider. Loaded from the keychain once
/// at startup; every change is written back.
pub struct ApiProviderKeys {
    keys: HashMap<String, String>,
    /// Guards the deferred keychain write against an older write landing
    /// after a newer one (see `set_key`).
    write_version: u64,
}

pub enum ApiProviderKeysEvent {
    Changed,
}

impl Entity for ApiProviderKeys {
    type Event = ApiProviderKeysEvent;
}

impl SingletonEntity for ApiProviderKeys {}

impl ApiProviderKeys {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let keys = match ctx.secure_storage().read_value(SECURE_STORAGE_KEY) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_else(|error| {
                report_error!(
                    anyhow::Error::new(error).context("Failed to parse the API provider keys")
                );
                HashMap::new()
            }),
            Err(secure_storage::Error::NotFound) => HashMap::new(),
            Err(error) => {
                report_error!(
                    anyhow::Error::new(error).context("Failed to read the API provider keys")
                );
                HashMap::new()
            }
        };
        Self {
            keys,
            write_version: 0,
        }
    }

    /// No keys and no keychain — for tests whose app has no secure storage.
    #[cfg(test)]
    pub fn new_empty() -> Self {
        Self {
            keys: HashMap::new(),
            write_version: 0,
        }
    }

    pub fn key_for(&self, provider_id: &str) -> Option<&str> {
        self.keys
            .get(provider_id)
            .map(String::as_str)
            .filter(|key| !key.is_empty())
    }

    /// Sets (or, with `None`, clears) one provider's key.
    pub fn set_key(&mut self, provider_id: &str, key: Option<String>, ctx: &mut ModelContext<Self>) {
        let key = key
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty());
        if self.keys.get(provider_id) == key.as_ref() {
            return;
        }
        match key {
            Some(key) => self.keys.insert(provider_id.to_string(), key),
            None => self.keys.remove(provider_id),
        };
        ctx.emit(ApiProviderKeysEvent::Changed);

        let json = match serde_json::to_string(&self.keys) {
            Ok(json) => json,
            Err(error) => {
                report_error!(
                    anyhow::Error::new(error).context("Failed to serialize the API provider keys")
                );
                return;
            }
        };
        self.write_version += 1;
        let write_version = self.write_version;
        // Deferred, as `ApiKeyManager` does it: the keychain call can be slow,
        // and the in-memory key is already live.
        ctx.spawn(async move { json }, move |me, json, ctx| {
            if write_version != me.write_version {
                return;
            }
            if let Err(error) = ctx
                .secure_storage()
                .write_value_with_owner_only_fallback(SECURE_STORAGE_KEY, &json)
            {
                report_error!(
                    anyhow::Error::new(error).context("Failed to save the API provider keys")
                );
            }
        });
    }

    /// Picker entries for every provider that has a key.
    pub fn available_models(&self) -> Vec<HarnessModelInfo> {
        PROVIDERS
            .iter()
            .filter(|provider| self.key_for(provider.id).is_some())
            .flat_map(|provider| {
                provider.models.iter().map(move |model| HarnessModelInfo {
                    id: model_id_for(provider, model.id),
                    display_name: model.display_name.to_string(),
                    reasoning_level: None,
                })
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "api_providers_tests.rs"]
mod tests;
