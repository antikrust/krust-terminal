//! Cloud model providers reached over the OpenAI chat-completions API with
//! the user's own key: OpenAI, Anthropic, Google Gemini, Z.ai (Zhipu's GLM),
//! Groq, OpenRouter, and Venice.
//!
//! The local harness calls these directly (see
//! `agent_sdk::driver::harness::api_stream_exec`); nothing goes through
//! Krust's cloud, and the keys never leave this machine except to the
//! provider they belong to. That is also why they are kept here rather than in
//! `ai::api_keys::ApiKeyManager`, whose keys are forwarded to the Krust server
//! and whose settings are gated on a Krust plan.
//!
//! Each provider's models are a short curated list: free-tier text models
//! where the provider has a free tier, else cheap or recommended ones that
//! can call tools. OpenRouter's free roster changes every few weeks, so its
//! list is also fetched live (see [`ApiProvider::live_free_models_url`]). A
//! model id is `api/<provider>/<model>` — the model part may itself contain
//! slashes (`api/groq/openai/gpt-oss-120b`).

use std::collections::HashMap;

use krust_errors::report_error;
use krustui::{Entity, ModelContext, SingletonEntity};
use krustui_extras::secure_storage::{self, AppContextExt};
use serde::Deserialize;

use crate::ai::harness_availability::HarnessModelInfo;

pub const API_MODEL_ID_PREFIX: &str = "api/";
/// The provider key under which the picker persists the last API model.
pub const API_HARNESS_CONFIG_NAME: &str = "api";

/// The keychain item holding every provider's key, as a JSON object keyed by
/// provider id.
const SECURE_STORAGE_KEY: &str = "ApiProviderKeys";

/// At most this many live-fetched models per provider, so a long free roster
/// doesn't run the model menu off the screen.
const MAX_LIVE_MODELS: usize = 12;

pub struct ApiProvider {
    pub id: &'static str,
    pub display_name: &'static str,
    /// The OpenAI-compatible base; `/chat/completions` is appended.
    pub base_url: &'static str,
    /// Where to get a key — a link on the settings page.
    pub key_url: &'static str,
    /// What using it costs, shown on the settings page.
    pub pricing_note: &'static str,
    /// The curated models, in order of preference.
    pub models: &'static [ApiModel],
    /// A public model list to read the provider's current free models from,
    /// for a provider whose free roster changes too often to hardcode. The
    /// curated `models` still order the result, and stand in when the fetch
    /// fails.
    pub live_free_models_url: Option<&'static str>,
}

pub struct ApiModel {
    pub id: &'static str,
    pub display_name: &'static str,
    /// USD per million (input, output) tokens, for adding up what a turn
    /// cost. Free models are `(0.0, 0.0)`.
    pub price_per_million: (f64, f64),
    /// The model's context window in tokens: what the context meter measures
    /// against, and what sizes the runner's file reads.
    pub context_window: u64,
}

pub const PROVIDERS: &[ApiProvider] = &[
    ApiProvider {
        id: "openai",
        display_name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        key_url: "https://platform.openai.com/api-keys",
        pricing_note: "Paid per token: GPT-6 Luna from $0.10 per million input tokens, GPT-6.1 \
            Sol $2, GPT-6 Astra $10.",
        models: &[
            ApiModel {
                id: "gpt-6.1-sol",
                display_name: "GPT-6.1 Sol",
                price_per_million: (2.0, 10.0),
                context_window: 1_050_000,
            },
            ApiModel {
                id: "gpt-6-luna",
                display_name: "GPT-6 Luna",
                price_per_million: (0.10, 0.50),
                context_window: 1_050_000,
            },
            ApiModel {
                id: "gpt-6-astra",
                display_name: "GPT-6 Astra",
                price_per_million: (10.0, 50.0),
                context_window: 1_050_000,
            },
        ],
        live_free_models_url: None,
    },
    ApiProvider {
        id: "anthropic",
        display_name: "Anthropic",
        // Anthropic's OpenAI-compatible endpoint. It returns no thinking, and
        // Anthropic describes it as for testing and comparison.
        base_url: "https://api.anthropic.com/v1",
        key_url: "https://platform.claude.com/settings/keys",
        pricing_note: "Paid per token: Claude Haiku 5.5 from $0.10 per million input tokens, \
            Sonnet 5.5 $2, Opus 5.5 $4, Fable 5.1 $10.",
        models: &[
            ApiModel {
                id: "claude-sonnet-5-5",
                display_name: "Claude Sonnet 5.5",
                price_per_million: (2.0, 10.0),
                context_window: 1_000_000,
            },
            ApiModel {
                id: "claude-opus-5-5",
                display_name: "Claude Opus 5.5",
                price_per_million: (4.0, 20.0),
                context_window: 1_000_000,
            },
            ApiModel {
                id: "claude-haiku-5-5",
                display_name: "Claude Haiku 5.5",
                price_per_million: (0.10, 0.50),
                context_window: 1_000_000,
            },
            ApiModel {
                id: "claude-fable-5-1",
                display_name: "Claude Fable 5.1",
                price_per_million: (10.0, 50.0),
                context_window: 1_000_000,
            },
        ],
        live_free_models_url: None,
    },
    ApiProvider {
        id: "gemini",
        display_name: "Google Gemini",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        key_url: "https://aistudio.google.com/apikey",
        pricing_note: "Flash models are free with low daily limits. Google uses free-tier \
            prompts to improve its products.",
        // Google's rolling aliases: they follow the newest Flash models, so
        // the list doesn't go stale every few months.
        models: &[
            ApiModel {
                id: "gemini-flash-latest",
                display_name: "Gemini Flash",
                price_per_million: (0.0, 0.0),
                context_window: 1_048_576,
            },
            ApiModel {
                id: "gemini-flash-lite-latest",
                display_name: "Gemini Flash-Lite",
                price_per_million: (0.0, 0.0),
                context_window: 1_048_576,
            },
        ],
        live_free_models_url: None,
    },
    ApiProvider {
        id: "zai",
        display_name: "Z.ai",
        base_url: "https://api.z.ai/api/paas/v4",
        key_url: "https://z.ai/manage-apikey/apikey-list",
        pricing_note: "GLM-4.7-Flash and GLM-4.5-Flash are free. The free tier runs one \
            request at a time.",
        models: &[
            ApiModel {
                id: "glm-4.7-flash",
                display_name: "GLM-4.7 Flash",
                price_per_million: (0.0, 0.0),
                context_window: 200_000,
            },
            ApiModel {
                id: "glm-4.5-flash",
                display_name: "GLM-4.5 Flash",
                price_per_million: (0.0, 0.0),
                context_window: 128_000,
            },
        ],
        live_free_models_url: None,
    },
    ApiProvider {
        id: "groq",
        display_name: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        key_url: "https://console.groq.com/keys",
        pricing_note: "Free with daily request and token limits. Very fast.",
        models: &[
            ApiModel {
                id: "openai/gpt-oss-120b",
                display_name: "GPT-OSS 120B",
                price_per_million: (0.0, 0.0),
                context_window: 131_072,
            },
            ApiModel {
                id: "qwen/qwen3.8-27b",
                display_name: "Qwen 3.8 27B",
                price_per_million: (0.0, 0.0),
                context_window: 131_072,
            },
            ApiModel {
                id: "openai/gpt-oss-20b",
                display_name: "GPT-OSS 20B",
                price_per_million: (0.0, 0.0),
                context_window: 131_072,
            },
        ],
        live_free_models_url: None,
    },
    ApiProvider {
        id: "openrouter",
        display_name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        key_url: "https://openrouter.ai/settings/keys",
        pricing_note: "Its free models (the ones listed here) allow 50 requests a day in all, \
            or 1,000 a day once you have bought $10 of credit. The free list changes often; \
            Krust keeps it current. Some free models' hosts may log prompts.",
        // Preferred order among the free models, from the big labs whose
        // free variants tend to stay; anything else free that can call tools
        // follows from the live list.
        models: &[
            ApiModel {
                id: "nvidia/nemotron-3-super-120b-a12b:free",
                display_name: "Nemotron 3 Super 120B (NVIDIA)",
                price_per_million: (0.0, 0.0),
                context_window: 1_000_000,
            },
            ApiModel {
                id: "google/gemma-4-31b-it:free",
                display_name: "Gemma 4 31B (Google)",
                price_per_million: (0.0, 0.0),
                context_window: 262_144,
            },
            ApiModel {
                id: "nvidia/nemotron-3-ultra-550b-a55b:free",
                display_name: "Nemotron 3 Ultra 550B (NVIDIA)",
                price_per_million: (0.0, 0.0),
                context_window: 1_000_000,
            },
            ApiModel {
                id: "poolside/laguna-s-2.1:free",
                display_name: "Laguna S 2.1 (Poolside)",
                price_per_million: (0.0, 0.0),
                context_window: 1_048_576,
            },
            ApiModel {
                id: "cohere/north-mini-code:free",
                display_name: "North Mini Code (Cohere)",
                price_per_million: (0.0, 0.0),
                context_window: 256_000,
            },
        ],
        live_free_models_url: Some("https://openrouter.ai/api/v1/models?supported_parameters=tools"),
    },
    ApiProvider {
        id: "venice",
        display_name: "Venice",
        base_url: "https://api.venice.ai/api/v1",
        key_url: "https://venice.ai/settings/api",
        pricing_note: "Paid per token, from about $0.07 per million. These models run on \
            Venice's private hosting, which keeps no prompts.",
        // All "private" (Venice-hosted, no prompt retention) and able to call
        // tools, per Venice's public model list: its own default, the
        // cheapest, two coding models, and its uncensored one.
        models: &[
            ApiModel {
                id: "zai-org-glm-5-2",
                display_name: "GLM 5.2",
                price_per_million: (1.40, 4.40),
                context_window: 1_000_000,
            },
            ApiModel {
                id: "deepseek-v4-flash-0731",
                display_name: "DeepSeek V4 Flash",
                price_per_million: (0.13, 0.26),
                context_window: 1_000_000,
            },
            ApiModel {
                id: "openai-gpt-oss-120b",
                display_name: "GPT-OSS 120B",
                price_per_million: (0.07, 0.30),
                context_window: 128_000,
            },
            ApiModel {
                id: "qwen3-6-35b-a3b",
                display_name: "Qwen 3.6 35B",
                price_per_million: (0.10, 1.00),
                context_window: 256_000,
            },
            ApiModel {
                id: "venice-uncensored-1-2",
                display_name: "Venice Uncensored 1.2",
                price_per_million: (0.20, 0.90),
                context_window: 128_000,
            },
        ],
        live_free_models_url: None,
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

/// The name `model` is listed under on `provider` — the model's own id when
/// it isn't listed (a live-listed free model, say).
pub fn model_display_name(provider: &ApiProvider, model: &str) -> String {
    provider
        .models
        .iter()
        .find(|listed| listed.id == model)
        .map_or_else(|| model.to_string(), |listed| listed.display_name.to_string())
}

/// `model`'s context window in tokens, when it's listed.
pub fn model_context_window(provider: &ApiProvider, model: &str) -> Option<u64> {
    provider
        .models
        .iter()
        .find(|listed| listed.id == model)
        .map(|listed| listed.context_window)
}

/// USD per million (input, output) tokens for `model` on `provider`: its
/// listed price, or free for an OpenRouter `:free` model from the live list.
/// `None` when the price isn't known, so no cost is claimed.
pub fn model_price(provider: &ApiProvider, model: &str) -> Option<(f64, f64)> {
    provider
        .models
        .iter()
        .find(|listed| listed.id == model)
        .map(|listed| listed.price_per_million)
        .or_else(|| model.ends_with(":free").then_some((0.0, 0.0)))
}

/// A model read from a provider's live list: `(id, display name)`.
type LiveModel = (String, String);

/// The keys the user has set, one per provider, and each live-listed
/// provider's current free models. Keys are loaded from the keychain once at
/// startup; every change is written back.
pub struct ApiProviderKeys {
    keys: HashMap<String, String>,
    /// Free models fetched from a provider's `live_free_models_url`.
    live_models: HashMap<&'static str, Vec<LiveModel>>,
    /// Guards the deferred keychain write against an older write landing
    /// after a newer one (see `set_key`).
    write_version: u64,
    /// The stored keys exist but couldn't be read — the keychain prompt was
    /// denied, say. Saving then would replace every stored key with just the
    /// ones typed since, so nothing is written until a read succeeds.
    unreadable: bool,
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
        let mut unreadable = false;
        let keys = match ctx.secure_storage().read_value(SECURE_STORAGE_KEY) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_else(|error| {
                report_error!(
                    anyhow::Error::new(error).context("Failed to parse the API provider keys")
                );
                HashMap::new()
            }),
            Err(secure_storage::Error::NotFound) => HashMap::new(),
            Err(error) => {
                log::warn!("api_providers: could not read the stored keys: {error}");
                unreadable = true;
                HashMap::new()
            }
        };
        let me = Self {
            unreadable,
            keys,
            live_models: HashMap::new(),
            write_version: 0,
        };
        for provider in PROVIDERS {
            if me.key_for(provider.id).is_some() {
                me.fetch_live_models(provider, ctx);
            }
        }
        me
    }

    /// No keys and no keychain — for tests whose app has no secure storage.
    #[cfg(test)]
    pub fn new_empty() -> Self {
        Self {
            keys: HashMap::new(),
            live_models: HashMap::new(),
            write_version: 0,
            unreadable: false,
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
        // Keys that couldn't be read at startup: try again now, so the change
        // lands on top of them rather than replacing them.
        if self.unreadable {
            match ctx.secure_storage().read_value(SECURE_STORAGE_KEY) {
                Ok(json) => {
                    if let Ok(stored) = serde_json::from_str::<HashMap<String, String>>(&json) {
                        self.keys = stored;
                    }
                    self.unreadable = false;
                }
                Err(secure_storage::Error::NotFound) => self.unreadable = false,
                Err(error) => {
                    log::warn!("api_providers: still could not read the stored keys: {error}");
                }
            }
        }
        let key = key
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty());
        if self.keys.get(provider_id) == key.as_ref() {
            return;
        }
        let added = key.is_some();
        match key {
            Some(key) => self.keys.insert(provider_id.to_string(), key),
            None => self.keys.remove(provider_id),
        };
        ctx.emit(ApiProviderKeysEvent::Changed);
        if added {
            if let Some(provider) = provider(provider_id) {
                self.fetch_live_models(provider, ctx);
            }
        }
        if self.unreadable {
            // Usable for this session; not saved, so the stored keys survive.
            log::warn!(
                "api_providers: not saving keys: the stored ones couldn't be read and would be \
                 overwritten"
            );
            return;
        }

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

    /// Reads a live-listed provider's current free models in the background.
    /// The list is public, so no key goes with the request. A failed fetch
    /// leaves the curated list in place.
    fn fetch_live_models(&self, provider: &'static ApiProvider, ctx: &mut ModelContext<Self>) {
        let Some(url) = provider.live_free_models_url else {
            return;
        };
        ctx.spawn(
            async move {
                let body = reqwest::get(url).await?.error_for_status()?.text().await?;
                anyhow::Ok(body)
            },
            move |me, body, ctx| match body {
                Ok(body) => {
                    let models = parse_live_free_models(&body, provider);
                    if !models.is_empty() && me.live_models.get(provider.id) != Some(&models) {
                        me.live_models.insert(provider.id, models);
                        ctx.emit(ApiProviderKeysEvent::Changed);
                    }
                }
                Err(error) => {
                    log::warn!(
                        "api_providers: could not read {}'s free models: {error}",
                        provider.display_name
                    );
                }
            },
        );
    }

    /// One provider's models, best first: its live free list when it has
    /// one, else the curated list.
    fn models_for(&self, provider: &'static ApiProvider) -> Vec<LiveModel> {
        match self.live_models.get(provider.id) {
            Some(live) => live.clone(),
            None => provider
                .models
                .iter()
                .map(|model| (model.id.to_string(), model.display_name.to_string()))
                .collect(),
        }
    }

    /// Picker entries for every provider that has a key.
    pub fn available_models(&self) -> Vec<HarnessModelInfo> {
        PROVIDERS
            .iter()
            .filter(|provider| self.key_for(provider.id).is_some())
            .flat_map(|provider| {
                self.models_for(provider)
                    .into_iter()
                    .map(move |(id, display_name)| HarnessModelInfo {
                        id: model_id_for(provider, &id),
                        display_name,
                        reasoning_level: None,
                    })
            })
            .collect()
    }
}

#[derive(Deserialize)]
struct LiveModelList {
    data: Vec<LiveModelEntry>,
}

#[derive(Deserialize)]
struct LiveModelEntry {
    id: String,
    name: Option<String>,
    context_length: Option<u64>,
}

/// The free models in an OpenRouter-style model list (`{"data": [{id, name,
/// context_length}]}`, already filtered to tool-capable models by the URL).
/// Free means a `:free` id. The curated models come first, in their order;
/// the rest follow by context window, largest first.
fn parse_live_free_models(body: &str, provider: &ApiProvider) -> Vec<LiveModel> {
    let Ok(list) = serde_json::from_str::<LiveModelList>(body) else {
        return Vec::new();
    };
    let mut free: Vec<LiveModelEntry> = list
        .data
        .into_iter()
        .filter(|entry| entry.id.ends_with(":free"))
        .collect();
    let curated_rank = |id: &str| {
        provider
            .models
            .iter()
            .position(|model| model.id == id)
            .unwrap_or(usize::MAX)
    };
    free.sort_by(|a, b| {
        curated_rank(&a.id)
            .cmp(&curated_rank(&b.id))
            .then(b.context_length.cmp(&a.context_length))
    });
    free.into_iter()
        .take(MAX_LIVE_MODELS)
        .map(|entry| {
            // A curated model keeps its curated name; others get the list's,
            // without the "(free)" every one of them carries.
            let display_name = provider
                .models
                .iter()
                .find(|model| model.id == entry.id)
                .map(|model| model.display_name.to_string())
                .unwrap_or_else(|| {
                    entry
                        .name
                        .unwrap_or_else(|| entry.id.clone())
                        .trim_end_matches("(free)")
                        .trim()
                        .to_string()
                });
            (entry.id, display_name)
        })
        .collect()
}

#[cfg(test)]
#[path = "api_providers_tests.rs"]
mod tests;
