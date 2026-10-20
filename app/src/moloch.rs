//! Client configuration for **MOLOCH** — this fork's replacement for Krust's
//! cloud agent backend (Oz).
//!
//! Upstream, the app's AI work goes to `https://app.krust.baby`. Those servers
//! are not ours, so this fork points the same call sites at a Moloch instance
//! we run (`crates/moloch`, docs/moloch.md). Moloch itself owns no AI: it
//! brokers the work out to a machine that does (a local harness, the Linux
//! box's Ollama models) and relays the answer back.
//!
//! Two settings do the whole job, because Moloch deliberately speaks the same
//! wire protocol the app already speaks:
//!
//! - **url** — becomes `ChannelState::server_root_url()`, so every
//!   `/api/v1/agent/*` call the app already makes lands on Moloch instead.
//! - **token** — becomes the API-key credential, so those calls carry
//!   `Authorization: Bearer <token>`, which is exactly what Moloch checks.
//!
//! Resolution order, first hit wins: environment, then a `MOLOCH_*` file in
//! the state directory (same `KEY=VALUE` format the server's own `.env`
//! uses). The server address changes between machines, so it lives in a file
//! rather than in code — a rebuild should never be needed to repoint the app.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

/// File in the state directory holding `KEY=VALUE` lines. Mirrors the
/// server-side `.env` so one format covers both ends of the link.
const CONFIG_FILE: &str = "moloch.env";

const URL_KEY: &str = "MOLOCH_URL";
const TOKEN_KEY: &str = "MOLOCH_TOKEN";

/// Where the app expects to find a Moloch instance, and how to authenticate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MolochConfig {
    /// Base URL, trailing slash trimmed (e.g. `https://192.168.1.50:10102`).
    /// The app appends `/api/v1/...` to this exactly as it did for Krust's
    /// cloud.
    pub url: String,
    /// Bearer token. Moloch accepts it with or without the app's `wk-`
    /// API-key prefix, so this is the plain token from the server's
    /// `api-token` file.
    pub token: Option<String>,
}

fn config_path() -> PathBuf {
    krust_core::paths::state_dir().join(CONFIG_FILE)
}

/// Parses `KEY=VALUE` lines, skipping blanks, `#` comments, and an optional
/// leading `export `. Surrounding quotes on the value are stripped. Same
/// dialect as the server's `.env` loader.
fn parse_env_file(contents: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value.trim().trim_matches('"').trim_matches('\'');
        values.insert(key.to_owned(), value.to_owned());
    }
    values
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// Resolves the configuration, or `None` when no Moloch is configured (in
/// which case the app keeps its built-in server URL and normal auth).
fn load() -> Option<MolochConfig> {
    let from_file = std::fs::read_to_string(config_path())
        .ok()
        .map(|contents| parse_env_file(&contents))
        .unwrap_or_default();

    // Environment wins over the file, so a one-off run can point elsewhere
    // without editing state.
    let url = non_empty(std::env::var(URL_KEY).ok())
        .or_else(|| non_empty(from_file.get(URL_KEY).cloned()))?;
    let token = non_empty(std::env::var(TOKEN_KEY).ok())
        .or_else(|| non_empty(from_file.get(TOKEN_KEY).cloned()));

    Some(MolochConfig {
        url: url.trim_end_matches('/').to_owned(),
        token,
    })
}

/// The process-wide configuration, resolved once on first use.
pub fn config() -> Option<&'static MolochConfig> {
    static CONFIG: OnceLock<Option<MolochConfig>> = OnceLock::new();
    CONFIG.get_or_init(load).as_ref()
}

/// Points the app's server root at Moloch. Call once during startup, before
/// any client is built.
///
/// Unlike upstream's `--server-root-url` (dev-channel only, so shipped builds
/// can't be redirected off Krust's servers), this override is always honored:
/// in this fork redirecting off those servers is the entire point.
pub fn apply_server_url_override() {
    let Some(config) = config() else { return };
    match krust_core::channel::ChannelState::override_server_root_url(config.url.clone()) {
        Ok(()) => log::info!("MOLOCH: server root pointed at {}", config.url),
        Err(e) => eprintln!(
            "Error: invalid {URL_KEY} ({}): {e:#} — falling back to the built-in server URL",
            config.url
        ),
    }
    // Moloch is also the session-sharing server (Share session, Remote
    // control): same host, WebSocket scheme. Without this the app keeps its
    // built-in sharing host, which is not ours and does not answer.
    if let Some(url) = session_sharing_url(&config.url) {
        match krust_core::channel::ChannelState::override_session_sharing_server_url(url.clone()) {
            Ok(()) => log::info!("MOLOCH: session sharing pointed at {url}"),
            Err(e) => eprintln!("Error: invalid session-sharing URL ({url}): {e:#}"),
        }
    }
}

/// Moloch's session-sharing address: its base URL with the WebSocket scheme
/// (`https` → `wss`, `http` → `ws`). `None` for anything else.
fn session_sharing_url(url: &str) -> Option<String> {
    if let Some(rest) = url.strip_prefix("https://") {
        Some(format!("wss://{rest}"))
    } else {
        url.strip_prefix("http://")
            .map(|rest| format!("ws://{rest}"))
    }
}

/// The token to authenticate with, if Moloch is configured with one. Fed into
/// the app's existing API-key credential path, so every authenticated request
/// carries it as a bearer token.
pub fn api_key() -> Option<String> {
    config().and_then(|config| config.token.clone())
}

#[cfg(test)]
#[path = "moloch_tests.rs"]
mod tests;
