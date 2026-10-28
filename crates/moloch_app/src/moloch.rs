//! What the app knows about the server: one model that asks Moloch for everything the pages show
//! — status, runs, connected workers, live sessions, the leaderboard, the log — every few seconds,
//! and holds the connection (address and token) from Krust's `moloch.env`.

use std::path::PathBuf;
use std::time::Duration;

use krust_app_shell::env_file;
use krustui::r#async::Timer;
use krustui::{Entity, ModelContext, SingletonEntity};
use serde_json::Value;

/// How often the pages are brought up to date.
const REFRESH: Duration = Duration::from_secs(5);
const URL_KEY: &str = "MOLOCH_URL";
const TOKEN_KEY: &str = "MOLOCH_TOKEN";

/// Krust's connection file (its state folder, `krust.baby.Krust`); `MOLOCH_ENV` points elsewhere.
pub fn config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("MOLOCH_ENV") {
        return PathBuf::from(path);
    }
    dirs::data_dir()
        .unwrap_or_default()
        .join("krust.baby.Krust")
        .join("moloch.env")
}

#[derive(Default)]
pub struct Moloch {
    pub url: String,
    pub token: String,
    /// What `/api/v1/status` said, or why nothing did.
    pub status: Option<Value>,
    pub error: Option<String>,
    pub runs: Vec<Value>,
    pub workers: Vec<Value>,
    pub sessions: Vec<Value>,
    pub leaderboard: Vec<Value>,
    pub logs: Vec<String>,
    fetching: bool,
}

pub enum MolochEvent {
    Changed,
}

impl Entity for Moloch {
    type Event = MolochEvent;
}

impl SingletonEntity for Moloch {}

/// One answer from each endpoint.
struct Snapshot {
    status: Result<Value, String>,
    runs: Option<Value>,
    workers: Option<Value>,
    sessions: Option<Value>,
    leaderboard: Option<Value>,
    logs: Option<Value>,
}

async fn fetch(base: String, token: String) -> Snapshot {
    let client = reqwest::Client::new();
    let get = |path: &'static str| {
        let request = client
            .get(format!("{base}{path}"))
            .bearer_auth(&token)
            .timeout(Duration::from_secs(5));
        async move {
            let response = request.send().await.map_err(|error| {
                if error.is_connect() {
                    "it isn't answering at this address".to_owned()
                } else {
                    error.to_string()
                }
            })?;
            let status = response.status();
            if status == reqwest::StatusCode::UNAUTHORIZED {
                return Err("it refused the token".to_owned());
            }
            if !status.is_success() {
                return Err(format!("it answered {status}"));
            }
            let text = response.text().await.map_err(|error| error.to_string())?;
            serde_json::from_str::<Value>(&text).map_err(|error| error.to_string())
        }
    };
    Snapshot {
        status: get("/api/v1/status").await,
        runs: get("/api/v1/agent/runs").await.ok(),
        workers: get("/api/v1/agent/connected-self-hosted-workers").await.ok(),
        sessions: get("/api/v1/sessions").await.ok(),
        leaderboard: get("/api/v1/agent/leaderboard").await.ok(),
        logs: get("/api/v1/logs").await.ok(),
    }
}

fn list(value: Option<Value>, key: &str) -> Vec<Value> {
    value
        .and_then(|value| value[key].as_array().cloned())
        .unwrap_or_default()
}

impl Moloch {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let mut me = Self::default();
        me.load();
        me.refresh(ctx);
        me.tick(ctx);
        me
    }

    /// The connection, from the environment (which wins, as in Krust) or `moloch.env`.
    fn load(&mut self) {
        let file = env_file::read(&config_path());
        let pick = |key: &str| {
            std::env::var(key)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .or_else(|| file.get(key).cloned())
                .unwrap_or_default()
        };
        self.url = pick(URL_KEY).trim().trim_end_matches('/').to_owned();
        self.token = pick(TOKEN_KEY).trim().to_owned();
    }

    fn tick(&mut self, ctx: &mut ModelContext<Self>) {
        ctx.spawn(
            async { Timer::after(REFRESH).await },
            |me, _, ctx| {
                me.refresh(ctx);
                me.tick(ctx);
            },
        );
    }

    /// Asks the server for everything now.
    pub fn refresh(&mut self, ctx: &mut ModelContext<Self>) {
        if self.url.is_empty() {
            self.error = Some("No address yet — set it under Connection.".to_owned());
            ctx.emit(MolochEvent::Changed);
            return;
        }
        if self.fetching {
            return;
        }
        self.fetching = true;
        let (base, token) = (self.url.clone(), self.token.clone());
        ctx.spawn(fetch(base, token), |me, snapshot, ctx| {
            me.fetching = false;
            match snapshot.status {
                Ok(status) => {
                    me.status = Some(status);
                    me.error = None;
                }
                Err(error) => {
                    me.status = None;
                    me.error = Some(format!("Moloch at {}: {error}.", me.url));
                }
            }
            me.runs = list(snapshot.runs, "runs");
            me.workers = list(snapshot.workers, "workers");
            me.sessions = list(snapshot.sessions, "sessions");
            me.leaderboard = list(snapshot.leaderboard, "cells");
            me.logs = list(snapshot.logs, "lines")
                .iter()
                .filter_map(|line| line.as_str().map(str::to_owned))
                .collect();
            ctx.emit(MolochEvent::Changed);
        });
    }

    /// Stops a run, then looks again.
    pub fn stop_run(&mut self, run_id: String, ctx: &mut ModelContext<Self>) {
        let (base, token) = (self.url.clone(), self.token.clone());
        ctx.spawn(
            async move {
                let _ = reqwest::Client::new()
                    .post(format!("{base}/api/v1/agent/runs/{run_id}/cancel"))
                    .bearer_auth(&token)
                    .header("content-type", "application/json")
                    .body("{}")
                    .timeout(Duration::from_secs(5))
                    .send()
                    .await;
            },
            |me, _, ctx| me.refresh(ctx),
        );
    }

    /// Saves the connection to `moloch.env` (Krust's too, from its next start) and uses it now.
    pub fn save_connection(
        &mut self,
        url: &str,
        token: &str,
        ctx: &mut ModelContext<Self>,
    ) -> std::io::Result<()> {
        let url = url.trim().trim_end_matches('/').to_owned();
        let token = token.trim().to_owned();
        env_file::write(
            &config_path(),
            &[(URL_KEY, url.clone()), (TOKEN_KEY, token.clone())],
        )?;
        self.url = url;
        self.token = token;
        self.refresh(ctx);
        Ok(())
    }
}
