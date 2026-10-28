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
/// The "Use Moloch" switch: `0` / `false` / `off` leaves Moloch out even with an address saved.
const ENABLED_KEY: &str = "MOLOCH_ENABLED";

fn is_off(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "off" | "no"
    )
}

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

    // Switched off (Settings → Moloch, "Use Moloch"): the app keeps its own server and auth.
    let enabled = non_empty(std::env::var(ENABLED_KEY).ok())
        .or_else(|| non_empty(from_file.get(ENABLED_KEY).cloned()));
    if enabled.as_deref().is_some_and(is_off) {
        return None;
    }

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

/// What `moloch.env` says now — the settings page's fields and switch, which may differ from what
/// this run started with ([`config`], read once at startup).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedSettings {
    pub enabled: bool,
    pub url: String,
    pub token: String,
}

/// Reads `moloch.env` afresh. A missing switch counts as on, as it always has.
pub fn saved() -> SavedSettings {
    let values = std::fs::read_to_string(config_path())
        .ok()
        .map(|contents| parse_env_file(&contents))
        .unwrap_or_default();
    SavedSettings {
        enabled: !values.get(ENABLED_KEY).is_some_and(|value| is_off(value)),
        url: values.get(URL_KEY).cloned().unwrap_or_default(),
        token: values.get(TOKEN_KEY).cloned().unwrap_or_default(),
    }
}

/// Whether `MOLOCH_*` environment variables are set — they win over the file, so what the settings
/// page saves won't take effect while they are.
pub fn overridden_by_environment() -> bool {
    [ENABLED_KEY, URL_KEY, TOKEN_KEY]
        .iter()
        .any(|key| non_empty(std::env::var(key).ok()).is_some())
}

/// Whether what is saved differs from what this run is using, so the page can say a restart is due.
pub fn restart_needed() -> bool {
    let saved = saved();
    let saved_config = (saved.enabled && !saved.url.trim().is_empty()).then(|| MolochConfig {
        url: saved.url.trim().trim_end_matches('/').to_owned(),
        token: non_empty(Some(saved.token.clone())),
    });
    saved_config.as_ref() != config()
}

/// Saves the switch, address and token to `moloch.env`, keeping every other line as it was and the
/// previous file as `moloch.env.bak`. The file holds a secret, so only you can read it.
pub fn save(settings: &SavedSettings) -> std::io::Result<()> {
    let path = config_path();
    let previous = std::fs::read_to_string(&path).unwrap_or_default();
    if !previous.is_empty() {
        let _ = std::fs::write(path.with_extension("env.bak"), &previous);
    }
    let wanted = [
        (ENABLED_KEY, if settings.enabled { "1" } else { "0" }.to_owned()),
        (URL_KEY, settings.url.trim().trim_end_matches('/').to_owned()),
        (TOKEN_KEY, settings.token.trim().to_owned()),
    ];
    let mut written = [false; 3];
    let mut lines: Vec<String> = previous
        .lines()
        .map(|line| {
            let key = line
                .trim()
                .strip_prefix("export ")
                .unwrap_or(line.trim())
                .split_once('=')
                .map(|(key, _)| key.trim().to_owned());
            match key.and_then(|key| wanted.iter().position(|(wanted, _)| *wanted == key)) {
                Some(index) => {
                    written[index] = true;
                    format!("{}={}", wanted[index].0, wanted[index].1)
                }
                None => line.to_owned(),
            }
        })
        .collect();
    for (index, (key, value)) in wanted.iter().enumerate() {
        if !written[index] {
            lines.push(format!("{key}={value}"));
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, lines.join("\n") + "\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "moloch_tests.rs"]
mod tests;
