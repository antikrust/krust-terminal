//! Ollama instances on other machines.
//!
//! The local install is not the only one worth using: a desktop with more
//! memory can hold larger models — or several at once — while the laptop that
//! runs Krust cannot. Ollama's HTTP API is the same wherever it runs, so a LAN
//! address is enough to borrow that machine's models.
//!
//! A model therefore has to say where it lives. [`qualify`] appends the host
//! to the model name (`qwen2.5-coder:32b@@http://192.168.1.150:11434`) and
//! [`split`] takes it apart again; unqualified names mean the local install,
//! so every existing id keeps working untouched.
//!
//! Discovered model lists are cached in the setting alongside the address.
//! The model menu is built synchronously — it cannot wait on a network round
//! trip to a machine that may be asleep — so what a host offers is recorded
//! when the user checks it, not fetched on demand.

use serde::{Deserialize, Serialize};

/// Separates a model name from its host. Two `@` because Ollama model names
/// may contain `.`, `_`, `-`, `:` and `/`, but never `@`.
pub const HOST_SEPARATOR: &str = "@@";

/// Ollama's default port, so users can type a bare address.
const DEFAULT_PORT: u16 = 11434;

/// A configured remote host and what it had when last checked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteHost {
    /// Normalized base URL, e.g. `http://192.168.1.150:11434`.
    pub url: String,
    /// Model names as the remote reported them, unqualified.
    #[serde(default)]
    pub models: Vec<String>,
    /// Optional label so a list of IPs is readable.
    #[serde(default)]
    pub label: Option<String>,
}

impl RemoteHost {
    /// What to show in a list: the label if given, else the host:port without
    /// the scheme, which is what the user typed.
    pub fn display(&self) -> String {
        if let Some(label) = self.label.as_deref().filter(|l| !l.trim().is_empty()) {
            return label.to_owned();
        }
        self.url
            .strip_prefix("http://")
            .or_else(|| self.url.strip_prefix("https://"))
            .unwrap_or(&self.url)
            .to_owned()
    }
}

/// Attaches a host to a model name. A model on the local install is left
/// alone, so ids already stored in settings keep resolving.
pub fn qualify(model: &str, base_url: &str) -> String {
    if base_url.is_empty() {
        return model.to_owned();
    }
    format!("{model}{HOST_SEPARATOR}{base_url}")
}

/// Splits a possibly-qualified model into `(name, host)`. `None` for the host
/// means the local install.
pub fn split(model: &str) -> (&str, Option<&str>) {
    match model.split_once(HOST_SEPARATOR) {
        Some((name, host)) if !host.is_empty() => (name, Some(host)),
        _ => (model, None),
    }
}

/// The base URL a model's requests should go to, given the local default.
pub fn base_url_for<'a>(model: &'a str, local: &'a str) -> &'a str {
    split(model).1.unwrap_or(local)
}

/// Turns what a user typed into a base URL: accepts `192.168.1.150`,
/// `192.168.1.150:11434`, or a full `http://…`, and fills in the scheme and
/// Ollama's port. Rejects anything that would not survive being pasted into a
/// URL, so a typo fails here rather than as a confusing request error later.
pub fn normalize_host_input(input: &str) -> Result<String, String> {
    let raw = input.trim().trim_end_matches('/');
    if raw.is_empty() {
        return Err("Enter an address, for example 192.168.1.150".to_owned());
    }

    let (scheme, rest) = match raw.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => ("http", rest),
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("https") => ("https", rest),
        Some((other, _)) => return Err(format!("Unsupported scheme '{other}' — use http://")),
        None => ("http", raw),
    };
    if rest.is_empty() {
        return Err("Enter an address, for example 192.168.1.150".to_owned());
    }
    // A path would be silently dropped when endpoints are appended; say so.
    if rest.contains('/') {
        return Err("Enter only the host and port, with no path".to_owned());
    }

    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) => {
            let port: u16 = port
                .parse()
                .map_err(|_| format!("'{port}' is not a valid port"))?;
            (host, port)
        }
        None => (rest, DEFAULT_PORT),
    };
    if host.is_empty() {
        return Err("Enter an address, for example 192.168.1.150".to_owned());
    }
    let ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_');
    if !host.chars().all(ok) {
        return Err(format!("'{host}' is not a valid host name or IP"));
    }

    Ok(format!("{scheme}://{host}:{port}"))
}

/// The hosts the user has configured, as recorded in settings.
pub fn configured(app: &krustui::AppContext) -> Vec<RemoteHost> {
    use crate::settings::ai::AISettings;
    use settings::Setting as _;
    use krustui::SingletonEntity as _;
    parse_hosts(
        AISettings::as_ref(app)
            .ollama_remote_hosts
            .value()
            .as_deref(),
    )
}

/// Persists the host list back to settings.
pub fn save(hosts: &[RemoteHost], ctx: &mut krustui::AppContext) {
    use crate::settings::ai::AISettings;
    use settings::Setting as _;
    use krustui::SingletonEntity as _;
    let json = to_json(hosts);
    AISettings::handle(ctx).update(ctx, |settings, ctx| {
        if let Err(error) = settings.ollama_remote_hosts.set_value(Some(json), ctx) {
            log::warn!("could not save the remote Ollama hosts: {error}");
        }
    });
}

/// Reads the hosts out of the setting's JSON. A malformed value yields no
/// hosts rather than an error: the setting is a cache, and losing it should
/// degrade to "no remotes", never break the model list.
pub fn parse_hosts(json: Option<&str>) -> Vec<RemoteHost> {
    json.filter(|raw| !raw.trim().is_empty())
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default()
}

/// Serializes hosts back to the setting's JSON.
pub fn to_json(hosts: &[RemoteHost]) -> String {
    serde_json::to_string(hosts).unwrap_or_else(|_| "[]".to_owned())
}

/// Every remote model as a qualified name, in host order. This is what the
/// model picker and the committee add to the local install's list.
pub fn qualified_models(hosts: &[RemoteHost]) -> Vec<String> {
    hosts
        .iter()
        .flat_map(|host| {
            host.models
                .iter()
                .map(|model| qualify(model, &host.url))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// How a qualified model should read in a menu: the model, then where it is.
pub fn display_name(model: &str, hosts: &[RemoteHost]) -> String {
    let (name, host) = split(model);
    match host {
        None => name.to_owned(),
        Some(url) => {
            let where_ = hosts
                .iter()
                .find(|host| host.url == url)
                .map(RemoteHost::display)
                .unwrap_or_else(|| url.trim_start_matches("http://").to_owned());
            format!("{name} ({where_})")
        }
    }
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
