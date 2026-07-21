//! Talking to the local Ollama installation and to the library page.
//!
//! Everything here is I/O; the parsing and judgement live in
//! [`super::catalog`] and [`super::capability`] so they stay testable without
//! a network or an `ollama` binary.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use super::capability;
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

/// What a model can do, from Ollama's `/api/show`. Drives the agent-vs-chat
/// split: a model that advertises native tool-calling is "good enough to be
/// an agent" (run shell/MCP tools); everything else is chat-only.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCapabilities {
    /// Advertises native tool-calling — the signal for agent mode.
    pub tools: bool,
    /// Trained context window in tokens, if reported.
    pub context_length: Option<u64>,
    /// Human parameter size, e.g. `8.0B`, if reported.
    pub parameter_size: Option<String>,
}

impl ModelCapabilities {
    /// A terse role label for the settings page.
    pub fn role_label(&self) -> &'static str {
        if self.tools {
            "Agent — runs tools"
        } else {
            "Chat only"
        }
    }
}

/// Queries `/api/show` for a model's capabilities. `None` if the server is
/// unreachable or the response can't be read — callers should degrade to
/// "unknown / chat-only" rather than block.
pub async fn fetch_capabilities(
    client: &reqwest::Client,
    model: &str,
) -> Option<ModelCapabilities> {
    let response = client
        .post(format!("{API_BASE}/api/show"))
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value: serde_json::Value = response.json().await.ok()?;

    let tools = value
        .get("capabilities")
        .and_then(|caps| caps.as_array())
        .is_some_and(|caps| caps.iter().any(|cap| cap.as_str() == Some("tools")));
    let context_length = value
        .get("model_info")
        .and_then(|info| info.as_object())
        .and_then(|info| {
            info.iter()
                .find(|(key, _)| key.ends_with("context_length"))
                .and_then(|(_, val)| val.as_u64())
        });
    let parameter_size = value
        .get("details")
        .and_then(|details| details.get("parameter_size"))
        .and_then(|size| size.as_str())
        .map(str::to_string);

    Some(ModelCapabilities {
        tools,
        context_length,
        parameter_size,
    })
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
///
/// An API response of *zero* models is treated the same as an error and
/// double-checked against disk: a long-running server only scans its
/// manifests directory at startup, so it has no way to notice models that
/// appeared on disk afterward — exactly what happens right after
/// `move_models_dir` moves files into a fresh folder while an old server
/// process (started before the move, possibly not even one Warp spawned)
/// keeps running against its stale in-memory view. Without this, a
/// perfectly intact move looks like every model vanished.
pub async fn list_installed(client: &reqwest::Client) -> Result<Vec<InstalledModel>> {
    match list_installed_via_api(client).await {
        Ok(models) if !models.is_empty() => Ok(models),
        Ok(_) => Ok(list_installed_from_disk(&models_dir())),
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

/// Where Ollama keeps its models on disk, for display and "reveal in folder".
/// `OLLAMA_MODELS` overrides the default of `~/.ollama/models`, matching
/// Ollama's own resolution.
pub fn models_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("OLLAMA_MODELS") {
        return std::path::PathBuf::from(dir);
    }
    default_models_dir()
}

/// The models folder Ollama uses absent any override — what "Reset to
/// default" moves models back to.
pub fn default_models_dir() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".ollama")
        .join("models")
}

/// Applies the persisted custom models folder (if any) to this process's
/// `OLLAMA_MODELS`, so `models_dir()` and any `ollama serve` this process
/// later spawns both pick it up.
///
/// Only ever *sets* the variable, never clears it: a user who exported
/// `OLLAMA_MODELS` in their own shell before launching Warp gets to keep
/// that (see `models_dir`'s doc comment) when Warp has no override of its
/// own opinion on the matter.
pub fn apply_models_dir_override(app: &warpui::AppContext) {
    use crate::settings::ai::AISettings;
    use settings::Setting as _;
    use warpui::SingletonEntity as _;
    if let Some(dir) = AISettings::as_ref(app)
        .ollama_models_dir_override
        .value()
        .as_deref()
        .filter(|dir| !dir.is_empty())
    {
        std::env::set_var("OLLAMA_MODELS", dir);
    }
}

/// One update from a streaming models-folder move.
#[derive(Debug, Clone)]
pub enum MoveEvent {
    /// Fraction 0.0..=1.0 of total bytes copied so far.
    Progress(f32),
    /// The move finished; `Ok` means models were copied and the old folder
    /// removed, `Err` carries a user-facing reason.
    Finished(std::result::Result<(), String>),
}

/// Moves everything under `from` into `to`, streaming byte-level progress on
/// `tx` and always ending with exactly one [`MoveEvent::Finished`].
///
/// Copies before deleting: `from` is only removed once every file has copied
/// successfully, so a mid-copy failure (disk full, permissions) leaves the
/// original models exactly as they were rather than half-migrated. Refuses
/// up front if `to`'s volume doesn't have room — this is exactly how models
/// end up stranded on a too-small system drive in the first place.
pub async fn move_models_dir(
    from: std::path::PathBuf,
    to: std::path::PathBuf,
    tx: async_channel::Sender<MoveEvent>,
) {
    let result = move_models_dir_inner(from, to, tx.clone()).await;
    let _ = tx
        .send(MoveEvent::Finished(result.map_err(|err| err.to_string())))
        .await;
}

async fn move_models_dir_inner(
    from: std::path::PathBuf,
    to: std::path::PathBuf,
    tx: async_channel::Sender<MoveEvent>,
) -> Result<()> {
    if from == to {
        return Ok(());
    }
    if to.starts_with(&from) {
        return Err(anyhow!(
            "Can't move the models folder into itself ({})",
            to.display()
        ));
    }
    if !from.exists() {
        // Nothing to migrate — e.g. Ollama has no models yet. The new
        // folder still becomes the target for future installs.
        return Ok(());
    }

    tokio::task::spawn_blocking(move || {
        check_fits(&from, &to)?;
        let total = dir_size_bytes(&from).max(1);
        let mut copied: u64 = 0;
        copy_dir_recursive(&from, &to, &mut |bytes| {
            copied = copied.saturating_add(bytes);
            // Unbounded channel: `try_send` only fails if the receiver is
            // gone (the page closed), in which case dropping progress is
            // exactly right.
            let _ = tx.try_send(MoveEvent::Progress(
                (copied as f32 / total as f32).clamp(0.0, 1.0),
            ));
        })
        .and_then(|_| remove_dir_all_checked(&from))
    })
    .await
    .map_err(|err| anyhow!("Move task panicked: {err}"))?
}

/// Headroom required on top of the raw byte count: other processes can
/// consume space during a long copy, and filesystems have their own
/// overhead the raw file sizes don't account for.
const SPACE_CHECK_MARGIN_BYTES: u64 = 500 * 1024 * 1024;

/// Refuses the move if `to`'s volume doesn't have room for what's in `from`,
/// rather than starting a copy that runs out of space partway through.
/// Best-effort: if free space can't be determined for `to` (e.g. an
/// unrecognized mount), the move proceeds rather than blocking on an
/// unknown.
fn check_fits(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    let needed = dir_size_bytes(from);
    let Some(available) = available_space_bytes(to) else {
        return Ok(());
    };
    if needed + SPACE_CHECK_MARGIN_BYTES > available {
        return Err(anyhow!(
            "Not enough space at {}: this needs about {}, but only {} is free",
            to.display(),
            capability::format_gb(needed),
            capability::format_gb(available)
        ));
    }
    Ok(())
}

/// Total size in bytes of every file under `dir`. Best-effort: unreadable
/// entries are skipped rather than failing the whole count, since this only
/// ever feeds an advisory "will it fit" check, not the copy itself.
fn dir_size_bytes(dir: &std::path::Path) -> u64 {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}

/// Free space on the volume containing `path`, or `None` if it can't be
/// determined.
///
/// `path` (the chosen destination folder) may not exist yet, so this walks
/// up to the nearest existing ancestor before matching it against a mount
/// point — a fresh `Foo/Bar/Models` folder picked for the first time still
/// resolves to the right volume rather than falling through to `None`.
fn available_space_bytes(path: &std::path::Path) -> Option<u64> {
    let mut probe = path;
    while !probe.exists() {
        probe = probe.parent()?;
    }
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|disk| probe.starts_with(disk.mount_point()))
        // Longest mount point match wins, e.g. `/Volumes/Foo` over `/`.
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(|disk| disk.available_space())
}

/// Recursively copies `from`'s contents into `to`, creating directories as
/// needed and invoking `on_bytes` with the size of each chunk written so a
/// caller can drive a progress bar. Synchronous — always run inside
/// `spawn_blocking`.
fn copy_dir_recursive(
    from: &std::path::Path,
    to: &std::path::Path,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("Couldn't create {}", to.display()))?;
    for entry in walkdir::WalkDir::new(from) {
        let entry = entry.with_context(|| format!("Couldn't read {}", from.display()))?;
        let rel = entry
            .path()
            .strip_prefix(from)
            .expect("walkdir yields paths under `from`");
        if rel.as_os_str().is_empty() {
            continue;
        }
        let dest = to.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest)
                .with_context(|| format!("Couldn't create {}", dest.display()))?;
        } else {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Couldn't create {}", parent.display()))?;
            }
            copy_file_with_progress(entry.path(), &dest, on_bytes).with_context(|| {
                format!(
                    "Couldn't copy {} to {}",
                    entry.path().display(),
                    dest.display()
                )
            })?;
        }
    }
    Ok(())
}

/// Copies one file in bounded chunks, reporting each chunk's byte count to
/// `on_bytes`. Chunked rather than `std::fs::copy` so a multi-GB model blob
/// advances the progress bar smoothly instead of jumping in one step when
/// the whole file lands.
fn copy_file_with_progress(
    src: &std::path::Path,
    dst: &std::path::Path,
    on_bytes: &mut dyn FnMut(u64),
) -> std::io::Result<()> {
    use std::io::{Read as _, Write as _};

    let mut reader = std::io::BufReader::new(std::fs::File::open(src)?);
    let mut writer = std::io::BufWriter::new(std::fs::File::create(dst)?);
    let mut buffer = vec![0_u8; 4 * 1024 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read])?;
        on_bytes(read as u64);
    }
    writer.flush()
}

/// Removes `dir` only after the copy above has fully succeeded — never
/// called on a partially-migrated source.
fn remove_dir_all_checked(dir: &std::path::Path) -> Result<()> {
    std::fs::remove_dir_all(dir).with_context(|| {
        format!(
            "Models copied, but couldn't remove old folder {}",
            dir.display()
        )
    })
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

/// One update from a streaming model pull.
#[derive(Debug, Clone)]
pub enum PullEvent {
    /// Download progress. `fraction` is `None` for phases without a byte
    /// count (e.g. "pulling manifest", "verifying"), where the bar should
    /// read as indeterminate; `status` is Ollama's own phase label.
    Progress {
        fraction: Option<f32>,
        status: String,
    },
    /// The pull completed and the model is now on disk.
    Done,
    /// The pull failed, with a user-facing reason.
    Failed(String),
}

#[derive(Deserialize)]
struct PullResponseLine {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    total: Option<u64>,
    #[serde(default)]
    completed: Option<u64>,
    #[serde(default)]
    error: Option<String>,
}

/// Pulls `tag` via Ollama's streaming `/api/pull`, forwarding progress on
/// `tx` until the pull ends. Starts the local service first (a cold machine
/// has no server to pull through). Always ends with exactly one
/// [`PullEvent::Done`] or [`PullEvent::Failed`].
///
/// Uses the HTTP API rather than shelling out to `ollama pull` so we get
/// structured byte counts (`completed`/`total`) to drive a progress bar,
/// instead of parsing the CLI's human-readable progress lines.
pub async fn pull_model(client: &reqwest::Client, tag: &str, tx: async_channel::Sender<PullEvent>) {
    use futures::StreamExt as _;

    if !ensure_running(client).await {
        let _ = tx
            .send(PullEvent::Failed(
                "Could not start the local Ollama service".to_string(),
            ))
            .await;
        return;
    }

    let response = client
        .post(format!("{API_BASE}/api/pull"))
        .json(&serde_json::json!({ "model": tag, "stream": true }))
        .send()
        .await;
    let response = match response {
        Ok(response) if response.status().is_success() => response,
        Ok(response) => {
            let _ = tx
                .send(PullEvent::Failed(format!(
                    "Ollama returned {} pulling {tag}",
                    response.status()
                )))
                .await;
            return;
        }
        Err(error) => {
            let _ = tx.send(PullEvent::Failed(error.to_string())).await;
            return;
        }
    };

    // The response is newline-delimited JSON; a single read chunk may hold
    // several lines or a partial one, so buffer across chunks and emit only
    // complete lines.
    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                let _ = tx.send(PullEvent::Failed(error.to_string())).await;
                return;
            }
        };
        buffer.extend_from_slice(&chunk);

        while let Some(newline) = buffer.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = buffer.drain(..=newline).collect();
            let line = &line[..line.len() - 1];
            if line.is_empty() {
                continue;
            }
            let Ok(parsed) = serde_json::from_slice::<PullResponseLine>(line) else {
                continue;
            };
            if let Some(error) = parsed.error {
                let _ = tx.send(PullEvent::Failed(error)).await;
                return;
            }
            let fraction = match (parsed.completed, parsed.total) {
                (Some(completed), Some(total)) if total > 0 => {
                    Some((completed as f32 / total as f32).clamp(0.0, 1.0))
                }
                _ => None,
            };
            let _ = tx
                .send(PullEvent::Progress {
                    fraction,
                    status: parsed.status.unwrap_or_default(),
                })
                .await;
        }
    }

    let _ = tx.send(PullEvent::Done).await;
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
