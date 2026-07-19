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

/// Lists installed models.
///
/// The running server is authoritative, so try its API first. When it isn't
/// running — a Homebrew Ollama has no login service, so a cold machine has
/// models on disk but no API — fall back to reading Ollama's manifests
/// directly. That keeps "what's installed" answerable without starting the
/// engine just to look, which is the whole point of the Installed list.
pub async fn list_installed(client: &reqwest::Client) -> Result<Vec<InstalledModel>> {
    match list_installed_via_api(client).await {
        Ok(models) => Ok(models),
        Err(api_err) => {
            let from_disk = list_installed_from_disk(&models_dir());
            if from_disk.is_empty() {
                // Nothing on disk either: surface the API error, since that's
                // the more informative "why is this empty".
                Err(api_err)
            } else {
                Ok(from_disk)
            }
        }
    }
}

async fn list_installed_via_api(client: &reqwest::Client) -> Result<Vec<InstalledModel>> {
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

/// Installed models read straight from disk, without the server. Synchronous
/// and side-effect-free, so callers that can't await — like the model menu's
/// availability list — can still reflect what's actually pulled.
pub fn installed_models_from_disk() -> Vec<InstalledModel> {
    list_installed_from_disk(&models_dir())
}

/// Where Ollama keeps its models. `OLLAMA_MODELS` overrides the default of
/// `~/.ollama/models`, matching Ollama's own resolution.
fn models_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("OLLAMA_MODELS") {
        return std::path::PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_default()
        .join(".ollama")
        .join("models")
}

/// One layer inside an on-disk manifest. Only the size matters here; a model's
/// footprint is the sum of its layers.
#[derive(Deserialize, Default)]
struct DiskManifestLayer {
    #[serde(default)]
    size: u64,
}

#[derive(Deserialize)]
struct DiskManifest {
    #[serde(default)]
    config: DiskManifestLayer,
    #[serde(default)]
    layers: Vec<DiskManifestLayer>,
}

/// Lists installed models by reading `<models_dir>/manifests`, without the
/// server. Each file sits at `<registry>/<namespace>/<name>/<tag>` and is a
/// JSON manifest whose layer sizes sum to the model's footprint. Best-effort:
/// unreadable or malformed manifests are skipped rather than failing the lot.
fn list_installed_from_disk(models_dir: &std::path::Path) -> Vec<InstalledModel> {
    let manifests_root = models_dir.join("manifests");
    let mut models: Vec<InstalledModel> = walkdir::WalkDir::new(&manifests_root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let rel = entry.path().strip_prefix(&manifests_root).ok()?;
            let name = model_ref_from_manifest_path(rel)?;
            let manifest: DiskManifest =
                serde_json::from_slice(&std::fs::read(entry.path()).ok()?).ok()?;
            let size_bytes =
                manifest.config.size + manifest.layers.iter().map(|l| l.size).sum::<u64>();
            Some(InstalledModel { name, size_bytes })
        })
        .collect();
    models.sort_by(|a, b| a.name.cmp(&b.name));
    models.dedup_by(|a, b| a.name == b.name);
    models
}

/// Reconstructs the model reference Ollama would print from a manifest's path
/// relative to the manifests root, e.g. `registry.ollama.ai/library/codellama/13b`
/// → `codellama:13b`. The default registry and `library` namespace are
/// implicit in Ollama's short names, so they're dropped; anything else is
/// kept (`hf.co/user/model/latest` → `hf.co/user/model:latest`).
fn model_ref_from_manifest_path(rel: &std::path::Path) -> Option<String> {
    let parts: Vec<&str> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    // Need at least a name and a tag.
    if parts.len() < 2 {
        return None;
    }
    let (tag, name_parts) = parts.split_last()?;
    let mut name_parts = name_parts;
    if name_parts.first() == Some(&"registry.ollama.ai") {
        name_parts = &name_parts[1..];
    }
    if name_parts.first() == Some(&"library") {
        name_parts = &name_parts[1..];
    }
    if name_parts.is_empty() {
        return None;
    }
    Some(format!("{}:{}", name_parts.join("/"), tag))
}

/// Whether the background service is answering.
pub async fn is_running(client: &reqwest::Client) -> bool {
    client
        .get(format!("{API_BASE}/api/version"))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

/// How long to wait for `ollama serve` to come up before giving up. A cold
/// start is usually well under a second; this leaves generous headroom.
const SERVER_START_POLLS: usize = 20;
const SERVER_START_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

/// Ensures the Ollama server is answering, starting it if it isn't.
///
/// A Homebrew install runs no login service, so enabling local models on a
/// cold machine has to boot the engine. `ollama serve` is long-lived, so it is
/// spawned detached — it outlives this call and the settings page — and we
/// poll the API until it answers. Returns whether it is up by the time we
/// stop waiting. A second `serve` merely fails to bind the port, harmlessly.
pub async fn ensure_running(client: &reqwest::Client) -> bool {
    if is_running(client).await {
        return true;
    }
    let Some(binary) = resolve_binary() else {
        return false;
    };
    if command::r#async::Command::new(binary)
        .arg("serve")
        .spawn()
        .is_err()
    {
        return false;
    }
    for _ in 0..SERVER_START_POLLS {
        warpui::r#async::Timer::after(SERVER_START_POLL_INTERVAL).await;
        if is_running(client).await {
            return true;
        }
    }
    false
}

/// Unloads every currently-running model, freeing the RAM they hold.
///
/// Used when the user turns local models off. The server process is left
/// alone — it may be managed by launchd or shared with the user's own
/// terminal, so killing it would overstep — but nothing should stay resident
/// in memory once local models are switched off.
pub async fn stop_running_models() {
    let Some(binary) = resolve_binary() else {
        return;
    };
    let Ok(output) = command::r#async::Command::new(&binary)
        .arg("ps")
        .output()
        .await
    else {
        return;
    };
    let listing = String::from_utf8_lossy(&output.stdout);
    for name in running_model_names(&listing) {
        let _ = command::r#async::Command::new(&binary)
            .arg("stop")
            .arg(&name)
            .output()
            .await;
    }
}

/// Extracts the model names from `ollama ps` output — the first column of
/// each row after the header. Pure, so it's testable without a live Ollama.
fn running_model_names(ps_output: &str) -> Vec<String> {
    ps_output
        .lines()
        .skip(1) // the NAME / ID / SIZE / … header row
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
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
