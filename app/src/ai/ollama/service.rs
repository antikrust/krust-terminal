//! Talking to the local Ollama installation and to the library page.
//!
//! Everything here is I/O; the parsing and judgement live in
//! [`super::catalog`] and [`super::capability`] so they stay testable without
//! a network or an `ollama` binary.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use super::catalog::{self, CatalogEntry};

/// Ollama's local HTTP API. Fixed port — Ollama does not advertise itself.
const API_BASE: &str = "http://127.0.0.1:11434";

/// The library page is ~800 KB of HTML; a slow link shouldn't wedge the UI.
const LIBRARY_FETCH_TIMEOUT_SECS: u64 = 20;

/// Whether the `ollama` binary exists on the user's PATH.
pub fn is_installed() -> bool {
    resolve_binary().is_some()
}

/// Resolves the `ollama` binary. The GUI's launchd PATH routinely misses
/// Homebrew, so consult the user's shell PATH first — same problem the agent
/// CLI resolution has.
pub fn resolve_binary() -> Option<std::path::PathBuf> {
    let path = warp_util::path::warp_shell_path()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();

    let extra = [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/home/linuxbrew/.linuxbrew/bin",
    ];

    path.split(':')
        .map(std::path::PathBuf::from)
        .chain(extra.iter().map(std::path::PathBuf::from))
        .map(|dir| dir.join("ollama"))
        .find(|candidate| candidate.is_file())
}

/// A model already pulled onto this machine.
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledModel {
    /// Full tag, e.g. `qwen2.5-coder:7b`.
    pub name: String,
    /// On-disk size in bytes.
    pub size_bytes: u64,
}

#[derive(Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagEntry>,
}

#[derive(Deserialize)]
struct TagEntry {
    name: String,
    #[serde(default)]
    size: u64,
}

/// Lists installed models via the local API.
///
/// A connection error means the service isn't running, which is a normal
/// state rather than a failure — Ollama starts on demand.
pub async fn list_installed(client: &reqwest::Client) -> Result<Vec<InstalledModel>> {
    let response = client
        .get(format!("{API_BASE}/api/tags"))
        .send()
        .await
        .context("Ollama is not running")?;

    let tags: TagsResponse = response
        .json()
        .await
        .context("Ollama returned an unexpected model list")?;

    let mut models: Vec<InstalledModel> = tags
        .models
        .into_iter()
        .map(|entry| InstalledModel {
            name: entry.name,
            size_bytes: entry.size,
        })
        .collect();
    models.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(models)
}

/// Whether the background service is answering.
pub async fn is_running(client: &reqwest::Client) -> bool {
    client
        .get(format!("{API_BASE}/api/version"))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

/// Fetches and parses the library listing.
///
/// Falls back to the bundled catalog when the request fails *or* when parsing
/// yields nothing — an empty parse means ollama.com changed its markup, and a
/// stale list beats a blank page.
pub async fn fetch_catalog(client: &reqwest::Client) -> (Vec<CatalogEntry>, CatalogSource) {
    match fetch_catalog_inner(client).await {
        Ok(entries) if !entries.is_empty() => (entries, CatalogSource::Live),
        Ok(_) => (catalog::bundled(), CatalogSource::BundledMarkupChanged),
        Err(_) => (catalog::bundled(), CatalogSource::BundledOffline),
    }
}

/// Where the catalog on screen came from, so the page can say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogSource {
    Live,
    /// Fetch succeeded but nothing parsed — their markup moved.
    BundledMarkupChanged,
    /// Fetch failed, most likely no network.
    BundledOffline,
}

impl CatalogSource {
    pub fn note(self) -> Option<&'static str> {
        match self {
            CatalogSource::Live => None,
            CatalogSource::BundledMarkupChanged => Some(
                "Couldn't read ollama.com's model list — showing a built-in list instead. \
                 You can still install any model by name.",
            ),
            CatalogSource::BundledOffline => Some(
                "Couldn't reach ollama.com — showing a built-in list instead. \
                 You can still install any model by name.",
            ),
        }
    }
}

async fn fetch_catalog_inner(client: &reqwest::Client) -> Result<Vec<CatalogEntry>> {
    let response = client
        .get(catalog::LIBRARY_URL)
        .timeout(std::time::Duration::from_secs(LIBRARY_FETCH_TIMEOUT_SECS))
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow!("ollama.com returned {}", response.status()));
    }

    Ok(catalog::parse_library_html(&response.text().await?))
}

/// A model tag the user typed. Rejects anything that isn't a plausible
/// `name` or `name:tag` so we never hand shell-shaped input to a command.
pub fn validate_model_tag(input: &str) -> Result<String> {
    let tag = input.trim();
    if tag.is_empty() {
        return Err(anyhow!("Enter a model name, for example qwen2.5-coder:7b"));
    }
    if tag.len() > 128 {
        return Err(anyhow!("That model name is too long"));
    }

    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/');
    if !tag.chars().all(allowed) {
        return Err(anyhow!(
            "Model names use letters, numbers, and . _ - : / only"
        ));
    }
    if tag.matches(':').count() > 1 {
        return Err(anyhow!("Model names have at most one ':'"));
    }
    if tag.starts_with(['-', ':']) {
        return Err(anyhow!("Model names can't start with '-' or ':'"));
    }

    Ok(tag.to_string())
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
