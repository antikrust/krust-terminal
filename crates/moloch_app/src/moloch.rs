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

/// Krust's agent-queue socket on this Mac (`queue-tools.sock` in its state folder).
fn krust_socket_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_default()
        .join("krust.baby.Krust")
        .join("queue-tools.sock")
}

/// One request to Krust's queue socket (`{op, arguments}` in, `{ok, result}` out, `result` itself
/// JSON for these reads).
pub fn krust_request(op: &str, arguments: Value) -> Result<Value, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    let stream = UnixStream::connect(krust_socket_path())
        .map_err(|_| "Krust isn't running on this Mac".to_owned())?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut writer = stream.try_clone().map_err(|error| error.to_string())?;
    writeln!(writer, "{}", serde_json::json!({ "op": op, "arguments": arguments }))
        .map_err(|error| error.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    let reply: Value = serde_json::from_str(line.trim()).map_err(|error| error.to_string())?;
    let text = reply["result"].as_str().unwrap_or_default();
    if reply["ok"].as_bool() != Some(true) {
        // An older Krust answers ops it doesn't know with "no tool named ''".
        return Err(if text.contains("no tool named") {
            "this Krust is too old to list its agents — update it".to_owned()
        } else {
            text.to_owned()
        });
    }
    serde_json::from_str(text).map_err(|error| error.to_string())
}

/// Where an agent's output comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// A run Moloch brokered (its id).
    Run(String),
    /// A task in Krust's queue on this Mac (its id).
    Task(u64),
}

/// An agent's output as terminal lines: what it said, the tools it called (`▸`) and what they
/// answered (`↳`).
pub async fn output(source: Source, base: String, token: String) -> Result<Vec<String>, String> {
    match source {
        Source::Run(run_id) => {
            let response = reqwest::Client::new()
                .get(format!("{base}/api/v1/agent/runs/{run_id}/transcript"))
                .bearer_auth(&token)
                .timeout(Duration::from_secs(8))
                .send()
                .await
                .map_err(|error| error.to_string())?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Err("Moloch has no output for this run yet.".to_owned());
            }
            let text = response.text().await.map_err(|error| error.to_string())?;
            Ok(transcript_lines(&text))
        }
        Source::Task(id) => {
            let reply = krust_request("task_output", serde_json::json!({ "id": id }))?;
            let mut lines = Vec::new();
            for line in reply["lines"].as_array().into_iter().flatten() {
                let text = line["text"].as_str().unwrap_or_default();
                let prefix = match line["kind"].as_str().unwrap_or_default() {
                    "tool" => "▸ ",
                    "result" => "  ↳ ",
                    "error" => "! ",
                    _ => "",
                };
                push_text(&mut lines, prefix, text);
            }
            Ok(lines)
        }
    }
}

/// Adds `text` as lines, the first with `prefix`, the rest indented to match.
fn push_text(lines: &mut Vec<String>, prefix: &str, text: &str) {
    let indent = " ".repeat(prefix.chars().count());
    for (index, line) in text.lines().enumerate() {
        let lead = if index == 0 { prefix } else { indent.as_str() };
        lines.push(format!("{lead}{line}"));
    }
}

/// A short form of a tool call's input or result.
fn brief(value: &Value) -> String {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str().map(str::to_owned))
            .collect::<Vec<_>>()
            .join(" "),
        other => other.to_string(),
    };
    let text = text.replace('\n', " ");
    if text.chars().count() > 300 {
        format!("{}…", text.chars().take(300).collect::<String>())
    } else {
        text
    }
}

/// A run's transcript — Claude-Code-shaped `stream-json` lines, as runners write them — as
/// terminal lines. Lines it doesn't recognise show as they are.
fn transcript_lines(ndjson: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for raw in ndjson.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(event) = serde_json::from_str::<Value>(raw) else {
            push_text(&mut lines, "", raw);
            continue;
        };
        match event["type"].as_str() {
            Some("assistant") | Some("user") => {
                for block in event["message"]["content"].as_array().into_iter().flatten() {
                    match block["type"].as_str() {
                        Some("text") => push_text(&mut lines, "", block["text"].as_str().unwrap_or_default()),
                        Some("tool_use") => lines.push(format!(
                            "▸ {} {}",
                            block["name"].as_str().unwrap_or("tool"),
                            brief(&block["input"])
                        )),
                        Some("tool_result") => lines.push(format!("  ↳ {}", brief(&block["content"]))),
                        _ => {}
                    }
                }
            }
            Some("result") => {
                let outcome = event["subtype"].as_str().unwrap_or("done");
                lines.push(format!("— {outcome} —"));
            }
            Some("system") => {}
            _ => push_text(&mut lines, "", &brief(&event)),
        }
    }
    lines
}

pub struct Moloch {
    pub url: String,
    pub token: String,
    /// What `/api/v1/status` said, or why nothing did.
    pub status: Option<Value>,
    pub error: Option<String>,
    pub runs: Vec<Value>,
    /// Every worker Moloch has seen (acolytes, Krust's harness), connected or not.
    pub workers: Vec<Value>,
    /// The agents in Krust's own queue on this Mac; `Err` says why there are none to show.
    pub local_tasks: Result<Vec<Value>, String>,
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

/// One answer from each endpoint, and from Krust's queue.
struct Snapshot {
    status: Result<Value, String>,
    runs: Option<Value>,
    workers: Option<Value>,
    local_tasks: Result<Value, String>,
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
    // Every worker; a Moloch older than /api/v1/workers lists the connected ones only.
    let workers = match get("/api/v1/workers").await {
        Ok(workers) => Some(workers),
        Err(_) => get("/api/v1/agent/connected-self-hosted-workers").await.ok(),
    };
    Snapshot {
        status: get("/api/v1/status").await,
        runs: get("/api/v1/agent/runs").await.ok(),
        workers,
        local_tasks: krust_request("list_tasks", Value::Null),
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
        let mut me = Self {
            url: String::new(),
            token: String::new(),
            status: None,
            error: None,
            runs: Vec::new(),
            workers: Vec::new(),
            local_tasks: Err("Checking…".to_owned()),
            sessions: Vec::new(),
            leaderboard: Vec::new(),
            logs: Vec::new(),
            fetching: false,
        };
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
            me.local_tasks = snapshot.local_tasks.map(|reply| list(Some(reply), "tasks"));
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
