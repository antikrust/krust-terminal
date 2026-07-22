use std::collections::HashMap;
use std::time::Duration;

use command::blocking::Command;
use instant::Instant;
use serde::{Deserialize, Serialize};
use settings::Setting as _;
use warp_cli::agent::Harness;
use warp_core::features::FeatureFlag;
use warp_errors::report_error;
use warp_managed_secrets::client::SecretOwner;
use warp_managed_secrets::{ManagedSecretManager, ManagedSecretValue};
use warpui::{AppContext, Entity, ModelContext, RequestState, SingletonEntity};

use crate::ai::agent_sdk::driver::harness::ollama_committee::{
    COMMITTEE_DISPLAY_NAME, COMMITTEE_MODEL_NAME, OLLAMA_COMMITTEE_MODEL_ID,
};
use crate::ai::agent_sdk::driver::harness::ollama_stream_exec::{
    ollama_model_name, OLLAMA_HARNESS_CONFIG_NAME, OLLAMA_MODEL_ID_PREFIX,
};
use crate::ai::cloud_agent_settings::CloudAgentSettings;
use crate::ai::harness_display;
use crate::auth::auth_manager::{AuthManager, AuthManagerEvent};
use crate::auth::AuthStateProvider;
use crate::network::{NetworkStatus, NetworkStatusEvent, NetworkStatusKind};
use crate::server::retry_strategies::{
    is_transient_graphql_or_http_error, OUT_OF_BAND_REQUEST_RETRY_STRATEGY,
};
use crate::server::server_api::ServerApiProvider;
use crate::workspaces::user_workspaces::{UserWorkspaces, UserWorkspacesEvent};

const AUTH_SECRET_FETCH_FAILURE_COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HarnessModelInfo {
    pub id: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_level: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HarnessAvailability {
    pub harness: Harness,
    pub display_name: String,
    pub enabled: bool,
    #[serde(default)]
    pub available_models: Vec<HarnessModelInfo>,
}

/// The harness list for this fork: local CLIs only, enabled by whether the
/// binary is actually installed. Oz (Warp's cloud agent) is gone — it needs a
/// Warp account and a live server; Gemini/OpenCode return if/when a local
/// runner exists for them. Model lists are curated, version-stable aliases
/// the CLIs resolve themselves; nothing here is overwritten by a server
/// (see [`HarnessAvailabilityModel::refresh`]).
fn default_harnesses(disabled: &[String]) -> Vec<HarnessAvailability> {
    let claude_model = |id: &str, display_name: &str| HarnessModelInfo {
        id: id.to_string(),
        display_name: display_name.to_string(),
        reasoning_level: None,
    };
    vec![
        HarnessAvailability {
            harness: Harness::Claude,
            display_name: harness_display::display_name(Harness::Claude).to_string(),
            enabled: local_cli_on_path("claude"),
            available_models: vec![
                claude_model("claude-fable-5", "Fable 5"),
                claude_model("claude-sonnet-5", "Sonnet 5"),
                claude_model("claude-opus-4-8", "Opus 4.8"),
                claude_model("claude-haiku-4-5-20251001", "Haiku 4.5"),
            ],
        },
        HarnessAvailability {
            harness: Harness::Codex,
            display_name: harness_display::display_name(Harness::Codex).to_string(),
            enabled: local_cli_on_path("codex"),
            available_models: append_available_ollama_models(
                codex_models_from_local_state(),
                disabled,
            ),
        },
    ]
}

/// Ollama model records share the Codex-shaped storage row, but the picker
/// presents them as their own peer provider. Any `ollama/…`-prefixed id is
/// intercepted by the direct Ollama runner (see [`ollama_model_name`]), so
/// every installed model routes there regardless of which one it is.
///
/// The list is what Ollama actually has on disk, not a fixed pair — pull a
/// model and it shows up here (and thus in the picker); remove one and it
/// leaves. A model too big for this machine carries its fit badge in the
/// display name so the warning follows it into the menu.
fn append_available_ollama_models(
    mut models: Vec<HarnessModelInfo>,
    disabled: &[String],
) -> Vec<HarnessModelInfo> {
    if !local_cli_on_path("ollama") {
        return models;
    }

    let total_memory = crate::ai::ollama::capability::total_system_memory_bytes();
    let mut ollama_model_count = 0_usize;
    for installed in crate::ai::ollama::service::installed_models_from_disk() {
        // A user model literally named "committee" can't be listed: its id
        // would be the committee pseudo-entry's id, which the harness routes
        // to the committee runner, never to the model.
        if installed.name == COMMITTEE_MODEL_NAME {
            continue;
        }
        // Switched off on the Ollama page: still on disk, offered nowhere.
        if disabled.iter().any(|off| off == &installed.name) {
            continue;
        }
        let badge = total_memory
            .and_then(|total| crate::ai::ollama::capability::assess_tag(&installed.name, total))
            .filter(|verdict| verdict.fit.is_warning())
            .and_then(|verdict| verdict.fit.badge());
        let display_name = match badge {
            Some(badge) => format!("{} [{}]", installed.name, badge),
            None => installed.name.clone(),
        };
        models.push(HarnessModelInfo {
            id: format!("{OLLAMA_MODEL_ID_PREFIX}{}", installed.name),
            display_name,
            reasoning_level: None,
        });
        ollama_model_count += 1;
    }
    // The multi-model committee needs someone to argue with: offered only
    // when at least two real models are installed. See MULTIAGENT-DESIGN.md.
    if ollama_model_count >= 2 {
        models.push(HarnessModelInfo {
            id: OLLAMA_COMMITTEE_MODEL_ID.to_string(),
            display_name: COMMITTEE_DISPLAY_NAME.to_string(),
            reasoning_level: None,
        });
    }
    models
}

/// Parses `codex debug models` JSON into picker entries: listed-visibility
/// models only, ordered by the catalog's priority.
fn parse_codex_catalog(raw: &[u8]) -> Vec<HarnessModelInfo> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(entries) = value.get("models").and_then(|models| models.as_array()) else {
        return Vec::new();
    };
    let mut models: Vec<(i64, HarnessModelInfo)> = Vec::new();
    for entry in entries {
        let Some(slug) = entry.get("slug").and_then(|slug| slug.as_str()) else {
            continue;
        };
        if entry.get("visibility").and_then(|v| v.as_str()) != Some("list") {
            continue;
        }
        let display_name = entry
            .get("display_name")
            .and_then(|name| name.as_str())
            .unwrap_or(slug);
        let priority = entry
            .get("priority")
            .and_then(|priority| priority.as_i64())
            .unwrap_or(i64::MAX);
        models.push((
            priority,
            HarnessModelInfo {
                id: slug.to_string(),
                display_name: display_name.to_string(),
                reasoning_level: None,
            },
        ));
    }
    models.sort_by_key(|(priority, _)| *priority);
    models.into_iter().map(|(_, model)| model).take(8).collect()
}

/// Instant startup stand-in until the async catalog lands: the
/// `~/.codex/config.toml` default plus models recent codex sessions
/// actually ran. Unknown ids 400 on ChatGPT accounts, so only locally
/// evidenced ids are offered; the `gpt-` filter drops junk recorded from
/// failed runs (e.g. a claude id leaking into a resume).
fn codex_models_from_local_state() -> Vec<HarnessModelInfo> {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return Vec::new();
    };

    let mut ids: Vec<String> = Vec::new();

    if let Ok(config) = std::fs::read_to_string(home.join(".codex/config.toml")) {
        for line in config.lines() {
            // Exactly the top-level `model = "..."` key; `model_provider`,
            // `model_reasoning_effort` etc. fail the `=` check after strip.
            if let Some(value) = line
                .trim()
                .strip_prefix("model")
                .map(str::trim_start)
                .and_then(|rest| rest.strip_prefix('='))
            {
                let value = value.trim().trim_matches('"');
                if !value.is_empty() {
                    ids.push(value.to_string());
                }
                break;
            }
        }
    }

    // Rollout files live under sessions/YYYY/MM/DD/rollout-*.jsonl; the
    // model appears in a `"model":"..."` field within the first lines.
    let mut rollouts: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    let mut stack = vec![home.join(".codex/sessions")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            {
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                rollouts.push((modified, path));
            }
        }
    }
    rollouts.sort_by(|a, b| b.0.cmp(&a.0));

    for (_, path) in rollouts.into_iter().take(40) {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(16 * 1024)]).into_owned();
        if let Some(start) = head.find("\"model\":\"") {
            let rest = &head[start + "\"model\":\"".len()..];
            if let Some(end) = rest.find('"') {
                ids.push(rest[..end].to_string());
            }
        }
    }

    let mut models: Vec<HarnessModelInfo> = Vec::new();
    for id in ids {
        if !id.starts_with("gpt-") {
            continue;
        }
        if models.iter().any(|model| model.id == id) {
            continue;
        }
        models.push(HarnessModelInfo {
            id: id.clone(),
            display_name: id,
            reasoning_level: None,
        });
        if models.len() >= 6 {
            break;
        }
    }
    models
}

/// The local-harness selection for panes without an ambient (cloud composer)
/// view model: the persisted last-selected harness when it's an enabled local
/// harness, else the first enabled one; plus that harness's persisted model
/// and reasoning selection. This is what the plain-terminal model picker
/// shows and what prompt submission runs.
pub fn selected_local_harness(ctx: &AppContext) -> (Harness, Option<String>, Option<String>) {
    let availability = HarnessAvailabilityModel::as_ref(ctx);
    let settings = CloudAgentSettings::as_ref(ctx);
    let selected_name = settings.last_selected_harness.value().as_deref();
    let ollama_selected = selected_name == Some(OLLAMA_HARNESS_CONFIG_NAME)
        && availability.is_harness_enabled(Harness::Codex)
        && availability
            .models_for(Harness::Codex)
            .is_some_and(|models| {
                models
                    .iter()
                    .any(|model| ollama_model_name(&model.id).is_some())
            });
    let harness = ollama_selected
        .then_some(Harness::Codex)
        .or_else(|| {
            selected_name
                .and_then(Harness::from_config_name)
                .filter(|harness| availability.is_harness_enabled(*harness))
        })
        .or_else(|| {
            availability
                .available_harnesses()
                .iter()
                .find(|h| h.enabled)
                .map(|h| h.harness)
        })
        .unwrap_or(Harness::Claude);
    let (model_id, reasoning_level) = if ollama_selected {
        persisted_ollama_model_for(ctx)
    } else {
        persisted_model_for(harness, ctx)
    };
    (harness, model_id, reasoning_level)
}

/// The persisted model selection for one specific harness.
pub fn persisted_model_for(harness: Harness, ctx: &AppContext) -> (Option<String>, Option<String>) {
    persisted_model_for_key(harness.config_name(), ctx)
}

pub fn persisted_ollama_model_for(ctx: &AppContext) -> (Option<String>, Option<String>) {
    persisted_model_for_key(OLLAMA_HARNESS_CONFIG_NAME, ctx)
}

fn persisted_model_for_key(key: &str, ctx: &AppContext) -> (Option<String>, Option<String>) {
    let selection = CloudAgentSettings::as_ref(ctx)
        .last_selected_harness_model
        .value()
        .get(key)
        .cloned();
    (
        // Reasoning-only selections store an empty model id ("default
        // model, explicit reasoning"); surface that as no model override.
        selection
            .as_ref()
            .map(|s| s.model_id.clone())
            .filter(|id| !id.is_empty()),
        selection.and_then(|s| s.reasoning_level),
    )
}

fn local_cli_on_path(binary: &str) -> bool {
    resolve_local_cli(binary).is_some()
}

/// Resolves a local agent CLI against the user's shell PATH plus the standard
/// install locations the GUI's launchd PATH tends to miss (mirrors the spawn
/// path augmentation in `TerminalView::run_local_claude_query`).
fn resolve_local_cli(binary: &str) -> Option<std::path::PathBuf> {
    let mut path = warp_util::path::warp_shell_path()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let mut extra_dirs = vec![
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".to_string(),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        extra_dirs.push(format!("{home}/.local/bin"));
        extra_dirs.push(format!("{home}/.npm-global/bin"));
    }
    for dir in extra_dirs {
        if !path.split(':').any(|segment| segment == dir) {
            path = if path.is_empty() {
                dir
            } else {
                format!("{dir}:{path}")
            };
        }
    }
    crate::util::path::resolve_executable_in_path(binary, std::ffi::OsStr::new(&path))
        .map(|resolved| resolved.into_owned())
}

#[derive(Debug, Clone)]
pub enum AuthSecretFetchState {
    NotFetched,
    Loading,
    Loaded(Vec<AuthSecretEntry>),
    Failed(#[allow(dead_code)] String),
}

#[derive(Debug, Clone)]
pub struct AuthSecretEntry {
    pub name: String,
    pub owner: SecretOwner,
}

pub enum HarnessAvailabilityEvent {
    /// Availability or model lists changed after launch (e.g. the async
    /// codex catalog landing); pickers re-render off this.
    Changed,
    AuthSecretsLoaded,
    /// Emitted when a lazy auth-secrets fetch fails. Subscribers should
    /// re-render so any "Loading…" placeholders can transition to an
    /// error state — without this signal the picker would otherwise be
    /// stuck on the loading placeholder until the next refetch.
    AuthSecretsFetchFailed,
    AuthSecretCreated {
        harness: Harness,
        name: String,
    },
    AuthSecretCreationFailed {
        error: String,
    },
    AuthSecretDeleted {
        harness: Harness,
        name: String,
        owner: SecretOwner,
    },
    AuthSecretDeletionFailed {
        harness: Harness,
        name: String,
        owner: SecretOwner,
        error: String,
    },
}

pub struct HarnessAvailabilityModel {
    harnesses: Vec<HarnessAvailability>,
    auth_secrets: HashMap<Harness, AuthSecretFetchState>,
    auth_secret_retry_after: HashMap<Harness, Instant>,
}

impl HarnessAvailabilityModel {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        // Availability is derived locally from PATH detection on every
        // launch; the old server-list cache could only serve stale data.
        let harnesses = default_harnesses(&crate::ai::ollama::service::disabled_models_for(ctx));

        ctx.subscribe_to_model(&NetworkStatus::handle(ctx), |me, _, event, ctx| {
            if let NetworkStatusEvent::NetworkStatusChanged {
                new_status: NetworkStatusKind::Online,
            } = event
            {
                me.refresh(ctx);
            }
        });

        ctx.subscribe_to_model(&AuthManager::handle(ctx), |me, _, event, ctx| {
            if let AuthManagerEvent::AuthComplete = event {
                let cached_harnesses: Vec<Harness> = me.auth_secrets.keys().copied().collect();
                for harness in cached_harnesses {
                    me.invalidate_auth_secrets(harness);
                }
                me.refresh(ctx);
            }
        });

        ctx.subscribe_to_model(&UserWorkspaces::handle(ctx), |me, _, event, ctx| {
            if let UserWorkspacesEvent::TeamsChanged = event {
                me.refresh(ctx);
            }
        });

        let me = Self {
            harnesses,
            auth_secrets: HashMap::new(),
            auth_secret_retry_after: HashMap::new(),
        };
        me.refresh(ctx);
        me.refresh_codex_catalog(ctx);
        me
    }

    /// Replaces the codex row's session-harvested model list with the CLI's
    /// own catalog (`codex debug models`), asynchronously — the command takes
    /// ~0.7s, too slow to run during construction on the UI thread. The
    /// picker re-renders off the `Changed` event when the catalog lands.
    fn refresh_codex_catalog(&self, ctx: &mut ModelContext<Self>) {
        let Some(codex) = resolve_local_cli("codex") else {
            return;
        };
        ctx.spawn(
            async move {
                tokio::task::spawn_blocking(move || {
                    Command::new(codex).args(["debug", "models"]).output()
                })
                .await
            },
            |me, result, ctx| {
                let output = match result {
                    Ok(Ok(output)) if output.status.success() => output.stdout,
                    _ => {
                        log::warn!("harness_availability: codex debug models failed");
                        return;
                    }
                };
                let disabled = crate::ai::ollama::service::disabled_models_for(ctx);
                let models =
                    append_available_ollama_models(parse_codex_catalog(&output), &disabled);
                if models.is_empty() {
                    return;
                }
                if let Some(row) = me
                    .harnesses
                    .iter_mut()
                    .find(|h| h.harness == Harness::Codex)
                {
                    if row.available_models != models {
                        row.available_models = models;
                        ctx.emit(HarnessAvailabilityEvent::Changed);
                    }
                }
            },
        );
    }

    /// Re-reads installed Ollama models from disk and refreshes the picker's
    /// Ollama list in place — so a model pulled on the Ollama settings page
    /// appears in the model menu without an app restart. Ollama models are
    /// appended onto the Codex harness row (see [`append_available_ollama_models`]),
    /// so only that row's Ollama portion is rebuilt; a `Changed` event makes
    /// the picker re-render.
    pub fn refresh_ollama_models(&mut self, ctx: &mut ModelContext<Self>) {
        let Some(row) = self
            .harnesses
            .iter_mut()
            .find(|h| h.harness == Harness::Codex)
        else {
            return;
        };
        // Keep the codex models; drop the previously-appended Ollama ones and
        // re-append a fresh read from disk.
        let codex_only: Vec<HarnessModelInfo> = row
            .available_models
            .iter()
            .filter(|model| !model.id.starts_with(OLLAMA_MODEL_ID_PREFIX))
            .cloned()
            .collect();
        let disabled = crate::ai::ollama::service::disabled_models_for(ctx);
        let rebuilt = append_available_ollama_models(codex_only, &disabled);
        if row.available_models != rebuilt {
            row.available_models = rebuilt;
            ctx.emit(HarnessAvailabilityEvent::Changed);
        }
    }

    pub fn available_harnesses(&self) -> &[HarnessAvailability] {
        &self.harnesses
    }

    pub fn display_name_for(&self, harness: Harness) -> &str {
        self.harnesses
            .iter()
            .find(|h| h.harness == harness)
            .map(|h| h.display_name.as_str())
            .unwrap_or_else(|| harness_display::display_name(harness))
    }

    /// Whether the harness selector should be shown (>1 known harness, including disabled).
    pub fn should_show_harness_selector(&self) -> bool {
        FeatureFlag::AgentHarness.is_enabled() && self.harnesses.len() > 1
    }

    /// Whether any harness is available at all (at least one enabled).
    pub fn has_any_enabled_harness(&self) -> bool {
        self.harnesses.iter().any(|h| h.enabled)
    }

    /// Whether a harness is both known and enabled.
    pub fn is_harness_enabled(&self, harness: Harness) -> bool {
        self.harnesses
            .iter()
            .any(|h| h.harness == harness && h.enabled)
    }

    pub fn models_for(&self, harness: Harness) -> Option<&[HarnessModelInfo]> {
        self.harnesses
            .iter()
            .find(|h| h.harness == harness)
            .map(|h| h.available_models.as_slice())
            .filter(|m| !m.is_empty())
    }

    pub fn auth_secrets_for(&self, harness: Harness) -> &AuthSecretFetchState {
        self.auth_secrets
            .get(&harness)
            .unwrap_or(&AuthSecretFetchState::NotFetched)
    }

    pub fn ensure_auth_secrets_fetched(&mut self, harness: Harness, ctx: &mut ModelContext<Self>) {
        match self.auth_secrets_for(harness) {
            AuthSecretFetchState::NotFetched => self.fetch_auth_secrets(harness, ctx),
            AuthSecretFetchState::Failed(_) if self.can_retry_auth_secret_fetch(harness) => {
                self.fetch_auth_secrets(harness, ctx);
            }
            AuthSecretFetchState::Failed(_)
            | AuthSecretFetchState::Loading
            | AuthSecretFetchState::Loaded(_) => {}
        }
    }

    fn fetch_auth_secrets(&mut self, harness: Harness, ctx: &mut ModelContext<Self>) {
        let Some(agent_harness) = harness_to_graphql_harness(harness) else {
            return;
        };

        if !AuthStateProvider::as_ref(ctx).get().is_logged_in() {
            return;
        }

        self.auth_secrets
            .insert(harness, AuthSecretFetchState::Loading);
        self.auth_secret_retry_after.remove(&harness);

        let api = ServerApiProvider::as_ref(ctx).get_managed_secrets_client();
        ctx.spawn_with_retry_on_error_when(
            move || {
                let api = api.clone();
                let agent_harness = agent_harness.clone();
                async move { api.list_harness_auth_secrets(agent_harness).await }
            },
            OUT_OF_BAND_REQUEST_RETRY_STRATEGY,
            is_transient_graphql_or_http_error,
            move |me,
                  result: RequestState<Vec<warp_graphql::managed_secrets::ManagedSecret>>,
                  ctx| match result {
                RequestState::RequestSucceeded(secrets) => {
                    let entries = secrets
                        .into_iter()
                        .map(|s| AuthSecretEntry {
                            owner: secret_owner_from_space(&s.owner),
                            name: s.name,
                        })
                        .collect();
                    me.auth_secrets
                        .insert(harness, AuthSecretFetchState::Loaded(entries));
                    me.auth_secret_retry_after.remove(&harness);
                    ctx.emit(HarnessAvailabilityEvent::AuthSecretsLoaded);
                }
                RequestState::RequestFailedRetryPending(e) => {
                    log::warn!("Failed to fetch harness auth secrets; retrying: {e:#}");
                }
                RequestState::RequestFailed(e) => {
                    let msg = e.to_string();
                    report_error!(e.context("Failed to fetch harness auth secrets"));
                    me.auth_secrets
                        .insert(harness, AuthSecretFetchState::Failed(msg));
                    me.auth_secret_retry_after
                        .insert(harness, Instant::now() + AUTH_SECRET_FETCH_FAILURE_COOLDOWN);
                    // Notify subscribers so they can drop any
                    // "Loading…" placeholder rendered during the
                    // in-flight fetch and surface the error state.
                    ctx.emit(HarnessAvailabilityEvent::AuthSecretsFetchFailed);
                }
            },
        );
    }

    fn can_retry_auth_secret_fetch(&self, harness: Harness) -> bool {
        self.auth_secret_retry_after
            .get(&harness)
            .map(|retry_after| Instant::now() >= *retry_after)
            .unwrap_or(true)
    }

    pub fn invalidate_auth_secrets(&mut self, harness: Harness) {
        self.auth_secrets.remove(&harness);
        self.auth_secret_retry_after.remove(&harness);
    }

    pub fn create_auth_secret(
        &mut self,
        harness: Harness,
        name: String,
        value: ManagedSecretValue,
        owner: SecretOwner,
        ctx: &mut ModelContext<Self>,
    ) {
        let manager = ManagedSecretManager::handle(ctx);
        let create_future = manager.as_ref(ctx).create_secret(owner, name, value, None);
        ctx.spawn(create_future, move |me, result, ctx| match result {
            Ok(secret) => {
                let entry = AuthSecretEntry {
                    name: secret.name.clone(),
                    owner: secret_owner_from_space(&secret.owner),
                };
                match me.auth_secrets.get_mut(&harness) {
                    Some(AuthSecretFetchState::Loaded(entries)) => {
                        entries.push(entry);
                    }
                    _ => {
                        me.auth_secrets
                            .insert(harness, AuthSecretFetchState::Loaded(vec![entry]));
                    }
                }
                ctx.emit(HarnessAvailabilityEvent::AuthSecretCreated {
                    harness,
                    name: secret.name,
                });
            }
            Err(e) => {
                let msg = e.to_string();
                report_error!(e.context("Failed to create harness auth secret"));
                ctx.emit(HarnessAvailabilityEvent::AuthSecretCreationFailed { error: msg });
            }
        });
    }

    pub fn delete_auth_secret(
        &mut self,
        harness: Harness,
        name: String,
        owner: SecretOwner,
        ctx: &mut ModelContext<Self>,
    ) {
        let manager = ManagedSecretManager::handle(ctx);
        let delete_future = manager
            .as_ref(ctx)
            .delete_secret(owner.clone(), name.clone());
        ctx.spawn(delete_future, move |me, result, ctx| match result {
            Ok(()) => {
                if let Some(AuthSecretFetchState::Loaded(entries)) =
                    me.auth_secrets.get_mut(&harness)
                {
                    remove_deleted_auth_secret_entry(entries, &name, &owner);
                }
                ctx.emit(HarnessAvailabilityEvent::AuthSecretDeleted {
                    harness,
                    name,
                    owner,
                });
            }
            Err(e) => {
                let msg = e.to_string();
                report_error!(e.context("Failed to delete harness auth secret"));
                ctx.emit(HarnessAvailabilityEvent::AuthSecretDeletionFailed {
                    harness,
                    name,
                    owner,
                    error: msg,
                });
            }
        });
    }

    pub fn refresh(&self, _ctx: &mut ModelContext<Self>) {
        // Fork: harness availability is derived locally (PATH detection in
        // `default_harnesses`). There is no server list to fetch, and letting
        // the login-gated endpoint overwrite the local list would clear it.
    }
}

fn secret_owner_from_space(space: &warp_graphql::object::Space) -> SecretOwner {
    match space.type_ {
        warp_graphql::object::SpaceType::Team => SecretOwner::Team {
            team_uid: space.uid.clone().into_inner(),
        },
        warp_graphql::object::SpaceType::User => SecretOwner::CurrentUser,
    }
}

fn remove_deleted_auth_secret_entry(
    entries: &mut Vec<AuthSecretEntry>,
    name: &str,
    owner: &SecretOwner,
) {
    entries.retain(|entry| entry.name.as_str() != name || &entry.owner != owner);
}
fn harness_to_graphql_harness(harness: Harness) -> Option<warp_graphql::ai::AgentHarness> {
    match harness {
        Harness::Oz => Some(warp_graphql::ai::AgentHarness::Oz),
        Harness::Claude => Some(warp_graphql::ai::AgentHarness::ClaudeCode),
        Harness::Gemini => Some(warp_graphql::ai::AgentHarness::Gemini),
        Harness::Codex => Some(warp_graphql::ai::AgentHarness::Codex),
        Harness::OpenCode | Harness::Unknown => None,
    }
}

impl Entity for HarnessAvailabilityModel {
    type Event = HarnessAvailabilityEvent;
}

impl SingletonEntity for HarnessAvailabilityModel {}
