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

use ai::api_keys::{ApiKeyManager, CustomEndpoint};
use krust_cli::agent::Harness;
use krust_errors::report_error;
use krustui::{AppContext, Entity, ModelContext, SingletonEntity};
use krustui_extras::secure_storage::{self, AppContextExt};
use serde::Deserialize;
use settings::Setting as _;

use crate::ai::agent_sdk::driver::harness::ollama_committee::COMMITTEE_MODEL_NAME;
use crate::ai::agent_sdk::driver::harness::ollama_stream_exec::{
    base_url_of, ollama_model_name, wire_name,
};
use crate::ai::failover_health::FailoverHealth;
use crate::ai::harness_availability::{HarnessAvailabilityModel, HarnessModelInfo};
use crate::ai::task_router::{
    choose, decide, failover_order, speed, strength_with_tier, Candidate, Difficulty, Policy,
};
use crate::settings::AISettings;

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

/// Local Ollama as the API runner reaches it: over Ollama's OpenAI-compatible
/// endpoint, with no key. Not in [`PROVIDERS`], so it never shows on the key
/// page or among the cloud models; BYOR failover can still move to it.
pub static LOCAL_OLLAMA: ApiProvider = ApiProvider {
    id: "ollama",
    display_name: "Ollama (local)",
    base_url: "http://127.0.0.1:11434/v1",
    key_url: "",
    pricing_note: "",
    models: &[],
    live_free_models_url: None,
};

/// What goes in the Authorization header for Ollama, which ignores it.
const LOCAL_OLLAMA_KEY: &str = "ollama";

/// A model on one of the user's custom endpoints (Settings → AI → Bring your
/// own key → Custom endpoints): `custom/<config_key>`.
pub const CUSTOM_MODEL_ID_PREFIX: &str = "custom/";

/// Custom endpoints as the API runner sees them: OpenAI-compatible servers
/// the user added, each with its own URL and key (carried on the route).
pub static CUSTOM_ENDPOINT: ApiProvider = ApiProvider {
    id: "custom",
    display_name: "Custom endpoint",
    base_url: "",
    key_url: "",
    pricing_note: "",
    models: &[],
    live_free_models_url: None,
};

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

/// Whether `model_id` costs nothing to run: a local Ollama model, a model
/// listed at no cost, an OpenRouter `:free` one, or a local Claude Code or
/// Codex app (which runs on the user's own subscription, not per token).
pub fn is_free_model(model_id: &str) -> bool {
    if ollama_model_name(model_id).is_some() || cli_model_parts(model_id).is_some() {
        return true;
    }
    api_model_parts(model_id).and_then(|(provider, model)| model_price(provider, model))
        == Some((0.0, 0.0))
}

/// The local Ollama models BYOR failover can use, as model ids: the ones the
/// model menu offers (models switched off on the Ollama page are already left
/// out). None while local models are switched off.
pub fn local_failover_models(app: &AppContext) -> Vec<String> {
    if !*AISettings::as_ref(app).ollama_enabled.value() {
        return Vec::new();
    }
    HarnessAvailabilityModel::as_ref(app)
        .models_for(Harness::Codex)
        .unwrap_or_default()
        .iter()
        .filter(|model| {
            ollama_model_name(&model.id).is_some_and(|tag| tag != COMMITTEE_MODEL_NAME)
        })
        .map(|model| model.id.clone())
        .collect()
}

/// A BYOR failover row that hands the turn to a local agent app — Claude
/// Code or Codex, run as their own CLIs with no API key:
/// `cli/<harness>/<model>`, the model as that harness's model menu names it.
pub const CLI_MODEL_ID_PREFIX: &str = "cli/";

/// The local agent apps BYOR failover can hand a turn to.
const FAILOVER_CLIS: [Harness; 2] = [Harness::Claude, Harness::Codex];

pub fn cli_model_id_for(harness: Harness, model: &str) -> String {
    format!("{CLI_MODEL_ID_PREFIX}{}/{model}", harness.config_name())
}

/// The local app and its model a `cli/…` id refers to.
pub fn cli_model_parts(model_id: &str) -> Option<(Harness, &str)> {
    let rest = model_id.strip_prefix(CLI_MODEL_ID_PREFIX)?;
    let (name, model) = rest.split_once('/')?;
    let harness = FAILOVER_CLIS
        .into_iter()
        .find(|harness| harness.config_name() == name)?;
    (!model.is_empty()).then_some((harness, model))
}

/// A local app's models for the failover list, as `(model, label)`: its own
/// models from the model menu (the Codex row's Ollama and API models are
/// other runners'). None while the app isn't installed.
fn cli_failover_models(harness: Harness, app: &AppContext) -> Vec<(String, String)> {
    let availability = HarnessAvailabilityModel::as_ref(app);
    if !availability.is_harness_enabled(harness) {
        return Vec::new();
    }
    let mut models: Vec<(String, String)> = Vec::new();
    for model in availability.models_for(harness).unwrap_or_default() {
        if is_api_runner_model(&model.id)
            || ollama_model_name(&model.id).is_some()
            || models.iter().any(|(listed, _)| *listed == model.id)
        {
            continue;
        }
        models.push((model.id.clone(), model.display_name.clone()));
    }
    models
}

/// A local agent app BYOR failover can hand a turn to, with the model to run.
#[derive(Clone, Debug)]
pub struct CliRoute {
    /// The failover row's id: `cli/<harness>/<model>`.
    pub model_id: String,
    pub harness: Harness,
    /// The model as the app (and its model menu) names it.
    pub model: String,
    /// The model as the user knows it.
    pub model_name: String,
    /// "Claude Code", "Codex".
    pub app_name: String,
}

/// The route for a `cli/…` row while its app is installed. A model the menu
/// doesn't list (yet — Codex's catalog loads late) is still run by its id.
pub fn cli_route_for(model_id: &str, app: &AppContext) -> Option<CliRoute> {
    let (harness, model) = cli_model_parts(model_id)?;
    if !HarnessAvailabilityModel::as_ref(app).is_harness_enabled(harness) {
        return None;
    }
    let model_name = cli_failover_models(harness, app)
        .into_iter()
        .find(|(listed, _)| listed == model)
        .map_or_else(|| model.to_owned(), |(_, label)| label);
    Some(CliRoute {
        model_id: model_id.to_owned(),
        harness,
        model: model.to_owned(),
        model_name,
        app_name: harness.display_name().to_owned(),
    })
}

/// Where BYOR failover can move a turn: another model on the API runner, or
/// a local agent app that takes the turn over.
#[derive(Clone)]
pub enum FailoverTarget {
    Api(ApiRoute),
    Cli(CliRoute),
}

impl FailoverTarget {
    pub fn model_id(&self) -> &str {
        match self {
            Self::Api(route) => &route.model_id,
            Self::Cli(route) => &route.model_id,
        }
    }
}

/// Whether `model_id` is one the API runner calls: a cloud provider's model
/// or a custom endpoint's.
pub fn is_api_runner_model(model_id: &str) -> bool {
    api_model_parts(model_id).is_some()
        || model_id.starts_with(CUSTOM_MODEL_ID_PREFIX)
        || is_auto_model_id(model_id)
}

/// The picker's "Auto" entries: `api/auto/<policy>`. Not a model: each turn resolves to one of the
/// failover rows (see [`resolve_auto_model`]), so the conversation shows the model that ran.
pub const AUTO_MODEL_ID_PREFIX: &str = "api/auto/";

pub fn is_auto_model_id(model_id: &str) -> bool {
    model_id.starts_with(AUTO_MODEL_ID_PREFIX)
}

/// The three Auto entries, offered while at least one failover row can run.
pub fn auto_models(app: &AppContext) -> Vec<HarnessModelInfo> {
    let any_row = failover_routes("", app)
        .iter()
        .any(|target| matches!(target, FailoverTarget::Api(_)));
    if !any_row {
        return Vec::new();
    }
    [
        (Policy::CostEfficient, "Auto (cost-efficient)"),
        (Policy::Responsive, "Auto (responsive)"),
        (Policy::Genius, "Auto (genius)"),
    ]
    .into_iter()
    .map(|(policy, name)| HarnessModelInfo {
        id: format!("{AUTO_MODEL_ID_PREFIX}{}", policy.label()),
        display_name: name.to_string(),
        reasoning_level: None,
    })
    .collect()
}

/// The failover row an Auto entry sends this prompt to, or `None` when no row can run: the
/// decider rates the prompt (never below `floor`: a short follow-up in a working conversation is
/// not easy work), and the entry's policy picks among the rows that are on, not known to be down
/// and, with "Free models only", free.
pub fn resolve_auto_model(
    model_id: &str,
    prompt: &str,
    floor: Difficulty,
    app: &AppContext,
) -> Option<String> {
    let policy = Policy::from_label(model_id.strip_prefix(AUTO_MODEL_ID_PREFIX)?)?;
    let difficulty = decide(prompt).difficulty.max(floor);
    let routes: Vec<ApiRoute> = failover_routes("", app)
        .into_iter()
        .filter_map(|target| match target {
            FailoverTarget::Api(route) => Some(route),
            FailoverTarget::Cli(_) => None,
        })
        .collect();
    let candidates: Vec<Candidate> = routes
        .iter()
        .map(|route| Candidate {
            strength: route_strength(route, app),
            speed: speed(&route.model),
            free: is_free_model(&route.model_id),
        })
        .collect();
    choose(policy, difficulty, &candidates).map(|index| routes[index].model_id.clone())
}

/// One model the API runner can call: where the request goes, the model it
/// names, and the key. Not `Debug`, so the key can't end up in a log.
#[derive(Clone)]
pub struct ApiRoute {
    /// The picker's id: `api/<provider>/<model>`, `custom/<config_key>` or
    /// `ollama/<tag>`.
    pub model_id: String,
    /// [`LOCAL_OLLAMA`] for a local model, [`CUSTOM_ENDPOINT`] for a custom
    /// endpoint's.
    pub provider: &'static ApiProvider,
    /// Who serves it, as the user knows them: the provider, or the custom
    /// endpoint's name.
    pub provider_name: String,
    /// What a failure of the whole account rules out: a provider, or one
    /// custom endpoint.
    pub account: String,
    /// The model as the provider names it in a request.
    pub model: String,
    /// The model as the user knows it.
    pub model_name: String,
    /// The OpenAI-compatible base; `/chat/completions` is appended.
    pub base_url: String,
    pub api_key: String,
}

impl ApiRoute {
    pub fn is_local(&self) -> bool {
        std::ptr::eq(self.provider, &LOCAL_OLLAMA)
    }
}

/// The route for `model_id`: a cloud model with its provider's key, a custom
/// endpoint's model, or a local Ollama model. `None` for an unknown model, a
/// provider with no key, or the Ollama committee (which isn't one model).
pub fn route_for(model_id: &str, app: &AppContext) -> Option<ApiRoute> {
    if let Some(config_key) = model_id.strip_prefix(CUSTOM_MODEL_ID_PREFIX) {
        return usable_custom_endpoints(app).find_map(|endpoint| {
            let model = endpoint
                .models
                .iter()
                .find(|model| model.config_key == config_key)?;
            Some(ApiRoute {
                model_id: model_id.to_owned(),
                provider: &CUSTOM_ENDPOINT,
                provider_name: endpoint.name.clone(),
                account: format!("{CUSTOM_MODEL_ID_PREFIX}{}", endpoint.url),
                model: model.name.clone(),
                model_name: model.display_label().to_owned(),
                base_url: custom_base_url(&endpoint.url),
                api_key: endpoint.api_key.clone(),
            })
        });
    }
    if let Some(tag) = ollama_model_name(model_id) {
        if tag == COMMITTEE_MODEL_NAME {
            return None;
        }
        return Some(ApiRoute {
            model_id: model_id.to_owned(),
            provider: &LOCAL_OLLAMA,
            provider_name: LOCAL_OLLAMA.display_name.to_owned(),
            account: LOCAL_OLLAMA.id.to_owned(),
            model: wire_name(tag).to_owned(),
            model_name: tag.to_owned(),
            base_url: format!("{}/v1", base_url_of(tag)),
            api_key: LOCAL_OLLAMA_KEY.to_owned(),
        });
    }
    let (provider, model) = api_model_parts(model_id)?;
    Some(ApiRoute {
        model_id: model_id.to_owned(),
        provider,
        provider_name: provider.display_name.to_owned(),
        account: provider.id.to_owned(),
        model: model.to_owned(),
        model_name: model_display_name(provider, model),
        base_url: provider.base_url.to_owned(),
        api_key: ApiProviderKeys::as_ref(app).key_for(provider.id)?.to_owned(),
    })
}

/// The custom endpoints set up enough to call: a URL and a key.
fn usable_custom_endpoints(app: &AppContext) -> impl Iterator<Item = &CustomEndpoint> {
    ApiKeyManager::as_ref(app)
        .keys()
        .custom_endpoints
        .iter()
        .filter(|endpoint| !endpoint.url.trim().is_empty() && !endpoint.api_key.is_empty())
}

/// The custom endpoints by name, each with its models for the model menu
/// (`custom/<config_key>` ids, labelled with the alias or model name). An
/// endpoint with no usable model is left out.
pub fn custom_endpoint_groups(app: &AppContext) -> Vec<(String, Vec<HarnessModelInfo>)> {
    usable_custom_endpoints(app)
        .map(|endpoint| {
            let models = endpoint
                .models
                .iter()
                .filter(|model| !model.name.trim().is_empty() && !model.config_key.is_empty())
                .map(|model| HarnessModelInfo {
                    id: format!("{CUSTOM_MODEL_ID_PREFIX}{}", model.config_key),
                    display_name: model.display_label().to_owned(),
                    reasoning_level: None,
                })
                .collect::<Vec<_>>();
            (endpoint.name.clone(), models)
        })
        .filter(|(_, models)| !models.is_empty())
        .collect()
}

/// Who serves an API-runner model, by the name the user knows: the cloud
/// provider, or the custom endpoint.
pub fn api_runner_provider_name(model_id: &str, app: &AppContext) -> Option<String> {
    if let Some((provider, _)) = api_model_parts(model_id) {
        return Some(provider.display_name.to_owned());
    }
    custom_endpoint_groups(app)
        .into_iter()
        .find(|(_, models)| models.iter().any(|model| model.id == model_id))
        .map(|(name, _)| name)
}

/// A custom endpoint's URL as an OpenAI-compatible base: as entered, less a
/// trailing slash or a pasted `/chat/completions`.
fn custom_base_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    url.strip_suffix("/chat/completions").unwrap_or(url).to_owned()
}

/// A group of models the failover list's provider menu offers: a provider
/// with a key, a custom endpoint, local Ollama, or a local agent app.
#[derive(Clone, Debug, PartialEq)]
pub struct FailoverSource {
    /// Stable while the list is shown: a provider id, `custom:<n>`,
    /// `ollama`, or `cli:<harness>`.
    pub key: String,
    pub name: String,
    /// `(model id, label)`, best first.
    pub models: Vec<(String, String)>,
}

/// Everything the failover list can pick from, in menu order: providers with
/// a key, then custom endpoints, then local Ollama models, then the local
/// Claude Code and Codex apps.
pub fn failover_sources(app: &AppContext) -> Vec<FailoverSource> {
    let keys = ApiProviderKeys::as_ref(app);
    let mut sources: Vec<FailoverSource> = PROVIDERS
        .iter()
        .filter(|provider| keys.key_for(provider.id).is_some())
        .map(|provider| FailoverSource {
            key: provider.id.to_owned(),
            name: provider.display_name.to_owned(),
            models: keys
                .models_for(provider)
                .into_iter()
                .map(|(model, label)| (model_id_for(provider, &model), label))
                .collect(),
        })
        .collect();
    for (index, endpoint) in usable_custom_endpoints(app).enumerate() {
        let models: Vec<(String, String)> = endpoint
            .models
            .iter()
            .filter(|model| !model.name.trim().is_empty() && !model.config_key.is_empty())
            .map(|model| {
                (
                    format!("{CUSTOM_MODEL_ID_PREFIX}{}", model.config_key),
                    model.display_label().to_owned(),
                )
            })
            .collect();
        if !models.is_empty() {
            sources.push(FailoverSource {
                key: format!("custom:{index}"),
                name: endpoint.name.clone(),
                models,
            });
        }
    }
    let local_models = local_failover_models(app);
    if !local_models.is_empty() {
        sources.push(FailoverSource {
            key: LOCAL_OLLAMA.id.to_owned(),
            name: LOCAL_OLLAMA.display_name.to_owned(),
            models: local_models
                .into_iter()
                .map(|model_id| {
                    let label = ollama_model_name(&model_id).unwrap_or_default().to_owned();
                    (model_id, label)
                })
                .collect(),
        });
    }
    for harness in FAILOVER_CLIS {
        let models = cli_failover_models(harness, app);
        if !models.is_empty() {
            sources.push(FailoverSource {
                key: format!("cli:{}", harness.config_name()),
                name: format!("{} (local app)", harness.display_name()),
                models: models
                    .into_iter()
                    .map(|(model, label)| (cli_model_id_for(harness, &model), label))
                    .collect(),
            });
        }
    }
    sources
}

/// The BYOR failover list as model ids, first choice first: the saved order,
/// with a bare provider id (saved by an earlier build) read as that
/// provider's first model, leaving out models that can't be called now (no
/// key, a removed endpoint, local models switched off or gone). With nothing
/// saved: each provider with a key, on its first model.
pub fn failover_rows(app: &AppContext) -> Vec<String> {
    let order = AISettings::as_ref(app).byor_failover_order.value();
    let saved: Vec<String> = if order.is_empty() {
        PROVIDERS.iter().map(|provider| provider.id.to_owned()).collect()
    } else {
        order.clone()
    };
    let keys = ApiProviderKeys::as_ref(app);
    let local_models = local_failover_models(app);
    saved
        .into_iter()
        .filter_map(|entry| {
            if entry.contains('/') {
                return Some(entry);
            }
            let provider = provider(&entry)?;
            let (model, _) = keys.models_for(provider).into_iter().next()?;
            Some(model_id_for(provider, &model))
        })
        .filter(|model_id| match ollama_model_name(model_id) {
            Some(_) => local_models.contains(model_id),
            None => route_for(model_id, app).is_some() || cli_route_for(model_id, app).is_some(),
        })
        .collect()
}

/// The tier the user set on `model_id` (Settings, the row's tier button), if any.
pub fn model_tier(model_id: &str, app: &AppContext) -> Option<Difficulty> {
    AISettings::as_ref(app)
        .byor_model_tiers
        .value()
        .iter()
        .filter_map(|entry| entry.rsplit_once('='))
        .find(|(id, _)| *id == model_id)
        .and_then(|(_, tier)| Difficulty::from_label(tier))
}

/// How strong `route`'s model is: the tier the user set for it, else the guess from its name,
/// price and context window. The one number failover, the Auto entries and the queue all go by.
pub fn route_strength(route: &ApiRoute, app: &AppContext) -> f64 {
    strength_with_tier(
        &route.model,
        model_price(route.provider, &route.model),
        model_context_window(route.provider, &route.model),
        model_tier(&route.model_id, app),
    )
}

/// Where a turn on `model_id` goes if that model fails, in order: every
/// failover row except `model_id` itself, rows switched off and models
/// OpenRouter reports down, each once, and only free ones when "Free models
/// only" is on. A local app's row hands the turn over to that app.
pub fn failover_routes(model_id: &str, app: &AppContext) -> Vec<FailoverTarget> {
    let settings = AISettings::as_ref(app);
    let free_only = *settings.byor_failover_free_only.value();
    let disabled = settings.byor_failover_disabled.value();
    let mut routes: Vec<FailoverTarget> = Vec::new();
    let health = FailoverHealth::as_ref(app);
    for row in failover_rows(app) {
        if row == model_id
            || disabled.contains(&row)
            // OpenRouter says it has no working endpoint right now.
            || health.is_known_down(&row)
            || (free_only && !is_free_model(&row))
            || routes.iter().any(|route| route.model_id() == row)
        {
            continue;
        }
        if let Some(route) = route_for(&row, app) {
            routes.push(FailoverTarget::Api(route));
        } else if let Some(route) = cli_route_for(&row, app) {
            routes.push(FailoverTarget::Cli(route));
        }
    }
    // A model that failed is replaced by one about as capable, or more, before a weaker one: the
    // API rows go in order of strength from the failed model's, the list's order only breaking
    // ties. A local agent app (which cannot hand the turn back) stays after them, in list order.
    // With no failed model (the queue and the Auto entries pick for themselves) the list's order
    // stands.
    if let Some(failed) = route_for(model_id, app) {
        let failed_strength = route_strength(&failed, app);
        let (api, cli): (Vec<FailoverTarget>, Vec<FailoverTarget>) = routes
            .into_iter()
            .partition(|route| matches!(route, FailoverTarget::Api(_)));
        let strengths: Vec<f64> = api
            .iter()
            .map(|route| match route {
                FailoverTarget::Api(route) => route_strength(route, app),
                FailoverTarget::Cli(_) => 0.0,
            })
            .collect();
        let mut api: Vec<Option<FailoverTarget>> = api.into_iter().map(Some).collect();
        let mut ordered: Vec<FailoverTarget> = failover_order(failed_strength, &strengths)
            .into_iter()
            .filter_map(|index| api[index].take())
            .collect();
        ordered.extend(cli);
        return ordered;
    }
    routes
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

    /// One provider's models as `(id, display name)`, best first: its live
    /// free list when it has one, else the curated list.
    pub fn models_for(&self, provider: &'static ApiProvider) -> Vec<LiveModel> {
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
