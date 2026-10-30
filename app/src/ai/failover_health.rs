//! Health lights for the BYOR failover list: green, amber or red per row.
//!
//! A row's light comes from what the API runner saw that model (or its whole
//! account) do in the last few minutes, else from a check: OpenRouter
//! publishes each model's endpoint health, and a local Ollama server either
//! answers or doesn't. Other providers publish nothing, so their rows stay
//! grey until a turn has used them.
//!
//! A model that hit a limit it can't get past for a while — a daily cap, or a
//! rate limit whose reset the provider named — *rests* until then: it is red,
//! and failover and the agent queue pass it over without spending a request to
//! find out again. Rests are saved, so a restart remembers them, as is how many
//! requests each model was sent today.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use krustui::{Entity, ModelContext, SingletonEntity};
use serde::{Deserialize, Serialize};

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

/// A model, or an account's free models, resting after a lasting limit.
#[derive(Clone, Serialize, Deserialize)]
struct Rest {
    /// When it lifts, in Unix seconds.
    until: u64,
    /// What it hit, as the user is told: "daily limit", "rate limit".
    cause: String,
}

/// What is saved across restarts: the rests, and today's request counts.
#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    /// By `model:<id>`, or `free:<account>` for every free model of an account (OpenRouter's free
    /// models share one daily allowance).
    #[serde(default)]
    rests: HashMap<String, Rest>,
    /// The UTC day (days since the epoch) `requests` counts.
    #[serde(default)]
    day: u64,
    /// Requests sent on `day`, by model id.
    #[serde(default)]
    requests: HashMap<String, u32>,
}

fn ledger_file() -> PathBuf {
    krust_core::paths::state_dir().join("failover_ledger.json")
}

/// Written from turns running off the main thread, like the observations.
fn ledger() -> &'static Mutex<Ledger> {
    static LEDGER: OnceLock<Mutex<Ledger>> = OnceLock::new();
    LEDGER.get_or_init(|| {
        let ledger = std::fs::read_to_string(ledger_file())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Mutex::new(ledger)
    })
}

fn save_ledger(ledger: &Ledger) {
    match serde_json::to_string(ledger) {
        Ok(text) => {
            if let Err(error) = std::fs::write(ledger_file(), text) {
                log::warn!("failover_health: could not save the ledger: {error}");
            }
        }
        Err(error) => log::warn!("failover_health: could not write the ledger: {error}"),
    }
}

/// Seconds since the Unix epoch, now.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Rests `model_id` until `until` (Unix seconds), having hit `cause`. With `free_wide`, every free
/// model of `account` rests instead: the limit was the account's free allowance as a whole.
pub fn rest(model_id: &str, account: &str, until: u64, cause: &str, free_wide: bool) {
    let Ok(mut ledger) = ledger().lock() else {
        return;
    };
    let key = if free_wide {
        format!("free:{account}")
    } else {
        format!("model:{model_id}")
    };
    let now = unix_now();
    ledger.rests.retain(|_, rest| rest.until > now);
    ledger.rests.insert(
        key,
        Rest {
            until,
            cause: cause.to_string(),
        },
    );
    save_ledger(&ledger);
}

/// When `model_id` (on `account`) can be asked again and what it hit, while it rests.
pub fn resting(model_id: &str, account: &str) -> Option<(u64, String)> {
    let ledger = ledger().lock().ok()?;
    let now = unix_now();
    let mut keys = vec![format!("model:{model_id}")];
    if model_id.ends_with(":free") {
        keys.push(format!("free:{account}"));
    }
    keys.iter()
        .filter_map(|key| ledger.rests.get(key))
        .filter(|rest| rest.until > now)
        .max_by_key(|rest| rest.until)
        .map(|rest| (rest.until, rest.cause.clone()))
}

/// Counts a request sent to `model_id` today (UTC).
pub fn note_request(model_id: &str) {
    let Ok(mut ledger) = ledger().lock() else {
        return;
    };
    let today = unix_now() / 86_400;
    if ledger.day != today {
        ledger.day = today;
        ledger.requests.clear();
    }
    *ledger.requests.entry(model_id.to_string()).or_insert(0) += 1;
    save_ledger(&ledger);
}

/// How many requests `model_id` was sent today (UTC).
pub fn requests_today(model_id: &str) -> u32 {
    let Ok(ledger) = ledger().lock() else {
        return 0;
    };
    if ledger.day != unix_now() / 86_400 {
        return 0;
    }
    ledger.requests.get(model_id).copied().unwrap_or(0)
}

/// "6h 12m", "4m", "40s".
pub fn span_text(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3_599 => format!("{}m", secs / 60),
        _ => format!("{}h {}m", secs / 3_600, secs % 3_600 / 60),
    }
}

/// "01:00 (in 6h 12m)": when `until` (Unix seconds) comes, in local time, and how far off it is.
pub fn until_text(until: u64) -> String {
    let local = chrono::DateTime::<chrono::Local>::from(UNIX_EPOCH + Duration::from_secs(until));
    format!(
        "{} (in {})",
        local.format("%H:%M"),
        span_text(until.saturating_sub(unix_now()))
    )
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
                self.check_caps(model_id.clone(), tag.to_owned(), ctx);
            }
        }
    }

    /// The light for a row on `model_id`, whose whole-account failures are
    /// recorded under `account`: what a turn saw lately, or a newer check.
    pub fn health(&self, model_id: &str, account: &str) -> Health {
        // Resting after a lasting limit: it would only refuse again.
        if resting(model_id, account).is_some() {
            return Health::Down;
        }
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

    /// Asks Ollama what `tag` can do (reading images, say) once; the answer is kept for the agent
    /// pools' capability filter. A model it could not ask about is tried again at the next refresh.
    fn check_caps(&mut self, model_id: String, tag: String, ctx: &mut ModelContext<Self>) {
        let key = format!("caps:{model_id}");
        if crate::ai::model_caps::recorded(&model_id).is_some() || !self.checking.insert(key.clone())
        {
            return;
        }
        ctx.spawn(
            async move { crate::ai::model_caps::probe_ollama(&tag).await },
            move |me, found, ctx| {
                me.checking.remove(&key);
                if let Some(caps) = found {
                    crate::ai::model_caps::record(&model_id, caps);
                    ctx.emit(FailoverHealthEvent::Changed);
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
