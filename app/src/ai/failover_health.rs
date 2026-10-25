//! Health lights for the BYOR failover list: green, amber or red per row.
//!
//! A row's light comes from what the API runner saw that model (or its whole
//! account) do in the last few minutes, else from a check: OpenRouter
//! publishes each model's endpoint health, and a local Ollama server either
//! answers or doesn't. Other providers publish nothing, so their rows stay
//! grey until a turn has used them.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use krustui::{Entity, ModelContext, SingletonEntity};
use serde::Deserialize;

use crate::ai::agent_sdk::driver::harness::ollama_stream_exec::{base_url_of, ollama_model_name};
use crate::ai::api_providers::api_model_parts;

/// A failover row's light.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// Green: answering.
    Good,
    /// Amber: up but struggling (rate-limited, overloaded, flaky).
    Degraded,
    /// Red: not answering.
    Down,
    /// Grey: nothing known yet.
    Unknown,
}

/// How long a check (OpenRouter's health, Ollama answering) stays fresh.
const CHECK_TTL: Duration = Duration::from_secs(3 * 60);

/// How long what a turn saw counts for.
const OBSERVED_TTL: Duration = Duration::from_secs(10 * 60);

/// An OpenRouter endpoint counts as healthy from this uptime (percent, last
/// 30 minutes) up.
const HEALTHY_UPTIME: f64 = 95.;

struct Observation {
    health: Health,
    at: Instant,
}

/// What the API runner saw, by `model:<id>` and `account:<account>`. Written
/// from turns running off the main thread, so it lives outside the model.
fn observations() -> &'static Mutex<HashMap<String, Observation>> {
    static OBSERVATIONS: OnceLock<Mutex<HashMap<String, Observation>>> = OnceLock::new();
    OBSERVATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Records how a request on `model_id` went: `Good` after an answer (which
/// also clears an earlier failure of its account), `Degraded` or `Down` after
/// a failure. A failure of the whole `account` (no credit, a rejected key, the
/// host unreachable) turns every row of that account red.
pub fn record(model_id: &str, account: &str, health: Health, whole_account: bool) {
    let Ok(mut observations) = observations().lock() else {
        return;
    };
    let at = Instant::now();
    observations.insert(format!("model:{model_id}"), Observation { health, at });
    if whole_account || health == Health::Good {
        observations.insert(format!("account:{account}"), Observation { health, at });
    }
}

/// The newest of what a turn saw of `model_id` and of its `account`, if
/// recent.
fn observed(model_id: &str, account: &str) -> Option<(Health, Instant)> {
    let observations = observations().lock().ok()?;
    [format!("model:{model_id}"), format!("account:{account}")]
        .iter()
        .filter_map(|key| observations.get(key))
        .filter(|observation| observation.at.elapsed() < OBSERVED_TTL)
        .max_by_key(|observation| observation.at)
        .map(|observation| (observation.health, observation.at))
}

/// The checks behind the lights, and when each was made.
pub struct FailoverHealth {
    /// OpenRouter's published health, by OpenRouter model id.
    openrouter: HashMap<String, (Health, Instant)>,
    /// Whether each Ollama server answered, by base URL.
    ollama: HashMap<String, (Health, Instant)>,
    /// Checks under way, so a refresh doesn't start one twice.
    checking: HashSet<String>,
}

pub enum FailoverHealthEvent {
    Changed,
}

impl Entity for FailoverHealth {
    type Event = FailoverHealthEvent;
}

impl SingletonEntity for FailoverHealth {}

impl FailoverHealth {
    pub fn new() -> Self {
        Self {
            openrouter: HashMap::new(),
            ollama: HashMap::new(),
            checking: HashSet::new(),
        }
    }

    /// Starts the checks for `model_ids` whose last check is stale. Runs in
    /// the background; [`FailoverHealthEvent::Changed`] follows each result.
    pub fn refresh(&mut self, model_ids: &[String], ctx: &mut ModelContext<Self>) {
        for model_id in model_ids {
            if let Some((provider, model)) = api_model_parts(model_id) {
                if provider.id == "openrouter" {
                    self.check_openrouter(model.to_owned(), ctx);
                }
            } else if let Some(tag) = ollama_model_name(model_id) {
                self.check_ollama(base_url_of(tag).to_owned(), ctx);
            }
        }
    }

    /// The light for a row on `model_id`, whose whole-account failures are
    /// recorded under `account`: what a turn saw lately, or a newer check.
    pub fn health(&self, model_id: &str, account: &str) -> Health {
        let checked = if let Some((provider, model)) = api_model_parts(model_id) {
            (provider.id == "openrouter")
                .then(|| self.openrouter.get(model).copied())
                .flatten()
        } else if let Some(tag) = ollama_model_name(model_id) {
            self.ollama.get(base_url_of(tag)).copied()
        } else {
            None
        };
        match (observed(model_id, account), checked) {
            (Some((seen, seen_at)), Some((check, checked_at))) => {
                if checked_at > seen_at {
                    check
                } else {
                    seen
                }
            }
            (Some((seen, _)), None) => seen,
            (None, Some((check, _))) => check,
            (None, None) => Health::Unknown,
        }
    }

    /// Whether OpenRouter itself says `model_id` has no working endpoint, so
    /// failover can pass it over without spending a request on it.
    pub fn is_known_down(&self, model_id: &str) -> bool {
        api_model_parts(model_id).is_some_and(|(provider, model)| {
            provider.id == "openrouter"
                && self.openrouter.get(model).is_some_and(|(health, at)| {
                    *health == Health::Down && at.elapsed() < CHECK_TTL
                })
        })
    }

    fn check_openrouter(&mut self, model: String, ctx: &mut ModelContext<Self>) {
        let key = format!("openrouter:{model}");
        let fresh = self
            .openrouter
            .get(&model)
            .is_some_and(|(_, at)| at.elapsed() < CHECK_TTL);
        if fresh || !self.checking.insert(key.clone()) {
            return;
        }
        let url = format!("https://openrouter.ai/api/v1/models/{model}/endpoints");
        ctx.spawn(
            async move {
                let body = reqwest::get(url).await?.error_for_status()?.text().await?;
                anyhow::Ok(body)
            },
            move |me, body, ctx| {
                me.checking.remove(&key);
                match body {
                    Ok(body) => {
                        let health = openrouter_health(&body);
                        me.openrouter.insert(model, (health, Instant::now()));
                        ctx.emit(FailoverHealthEvent::Changed);
                    }
                    Err(error) => {
                        log::warn!("failover_health: could not check {model} on OpenRouter: {error}");
                    }
                }
            },
        );
    }

    fn check_ollama(&mut self, base_url: String, ctx: &mut ModelContext<Self>) {
        let key = format!("ollama:{base_url}");
        let fresh = self
            .ollama
            .get(&base_url)
            .is_some_and(|(_, at)| at.elapsed() < CHECK_TTL);
        if fresh || !self.checking.insert(key.clone()) {
            return;
        }
        let url = format!("{base_url}/api/version");
        ctx.spawn(
            async move {
                reqwest::Client::new()
                    .get(url)
                    .timeout(Duration::from_secs(3))
                    .send()
                    .await
                    .and_then(|response| response.error_for_status())
                    .is_ok()
            },
            move |me, answered, ctx| {
                me.checking.remove(&key);
                let health = if answered {
                    Health::Good
                } else {
                    Health::Down
                };
                me.ollama.insert(base_url, (health, Instant::now()));
                ctx.emit(FailoverHealthEvent::Changed);
            },
        );
    }
}

#[derive(Deserialize)]
struct EndpointsResponse {
    data: EndpointsData,
}

#[derive(Deserialize)]
struct EndpointsData {
    #[serde(default)]
    endpoints: Vec<Endpoint>,
}

#[derive(Deserialize)]
struct Endpoint {
    /// 0 when fine; negative when degraded or down.
    #[serde(default)]
    status: i64,
    /// Percent of the last 30 minutes it answered; absent when not measured.
    uptime_last_30m: Option<f64>,
}

/// A model's light from OpenRouter's `/models/<id>/endpoints`: green when an
/// endpoint is fine and has been up, amber when one is up but struggling,
/// red when none is up.
fn openrouter_health(body: &str) -> Health {
    let Ok(response) = serde_json::from_str::<EndpointsResponse>(body) else {
        return Health::Unknown;
    };
    let endpoints = response.data.endpoints;
    let uptime = |endpoint: &Endpoint| endpoint.uptime_last_30m.unwrap_or(100.);
    if endpoints
        .iter()
        .any(|endpoint| endpoint.status == 0 && uptime(endpoint) >= HEALTHY_UPTIME)
    {
        Health::Good
    } else if endpoints.iter().any(|endpoint| uptime(endpoint) > 0.) {
        Health::Degraded
    } else {
        Health::Down
    }
}
