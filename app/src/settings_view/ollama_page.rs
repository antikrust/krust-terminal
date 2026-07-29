//! Settings page for local models served by Ollama.
//!
//! The division of responsibility is deliberate: Ollama owns the models and
//! the disk they occupy. This page surfaces what its library offers, says
//! plainly whether a model stands a chance on this machine, and hands the
//! download to `ollama pull`. It never blocks a download — see
//! `ai::ollama::capability` for why the memory check is advisory.

use std::cell::RefCell;
use std::collections::HashMap;

use ::settings::{Setting, ToggleableSetting};
use bork_core::ui::theme::Fill as ThemeFill;
use bork_errors::report_if_error;
use borkui::elements::{
    Border, ChildView, ConstrainedBox, Container, CrossAxisAlignment, Element, Empty, Flex,
    MouseStateHandle, ParentElement, Shrinkable, Text,
};
use borkui::fonts::Weight;
use borkui::ui_components::components::{UiComponent, UiComponentStyles};
use borkui::ui_components::switch::SwitchStateHandle;
use borkui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use super::settings_page::{
    render_separator, render_sub_header, MatchData, PageTitleWidget, PageType, SettingsPageMeta,
    SettingsPageViewHandle, SettingsWidget,
};
use super::SettingsSection;
use crate::ai::agent_sdk::driver::harness::ollama_committee::{
    self, cycle_committee_role, cycle_evidence_stages, cycle_findings_cap, cycle_panel_round,
    cycle_proof_round, evidence_label, proof_label, CommitteeLeaderboard, OllamaCommitteeConfig,
    COMMITTEE_DISPLAY_NAME, COMMITTEE_MODEL_NAME, MAX_LOADED_MODELS, MAX_PANEL_EXTRA_ROUNDS,
    MAX_REVIEWERS,
};
use crate::ai::agent_sdk::driver::harness::ollama_stream_exec::{
    OLLAMA_HARNESS_CONFIG_NAME, OLLAMA_MODEL_ID_PREFIX,
};
use crate::ai::cloud_agent_settings::CloudAgentSettings;
use crate::ai::harness_availability::HarnessAvailabilityModel;
use crate::ai::ollama::capability::{self, Fit, MemoryVerdict};
use crate::ai::ollama::catalog::{CatalogEntry, ModelSize};
use crate::ai::ollama::remote;
use crate::ai::ollama::service::{self, CatalogSource, InstalledModel};
use crate::appearance::Appearance;
use crate::editor::{EditorView, SingleLineEditorOptions};
use crate::settings::ai::AISettings;
use crate::util::bindings::CustomAction;
use borkui::windowing::WindowManager;

const VERTICAL_MARGIN: f32 = 16.;
const OLLAMA_DOWNLOAD_URL: &str = "https://ollama.com/download";

/// Fixed column widths so each model's size rows line up like a table rather
/// than drifting with their content.
const SIZE_COL_WIDTH: f32 = 56.;
const STATUS_COL_WIDTH: f32 = 120.;

/// How many catalog entries to show before requiring a search. The live
/// library is 230+ models; a wall of them is not a browsing experience.
const VISIBLE_CATALOG_LIMIT: usize = 12;

#[derive(Debug, Clone)]
pub enum OllamaPageAction {
    ToggleEnabled,
    RefreshCatalog,
    OpenOllamaDownload,
    /// Reveal the folder Ollama stores models in.
    RevealModelFolder,
    /// Open a folder picker and, on selection, move existing models there and
    /// point future installs at it.
    ChangeModelsFolder,
    /// Move models back to the default folder and clear the override.
    ResetModelsFolder,
    /// Cycle a model's agent/chat override: Auto → Agent → Chat → Auto.
    CycleModelMode {
        tag: String,
    },
    /// Select this model as the local harness and open a fresh terminal tab
    /// to use it.
    OpenTerminalWithModel {
        tag: String,
    },
    Install {
        tag: String,
    },
    /// Install exactly what's in the free-text field.
    InstallTyped,
    /// Remove an installed model. Two-step: the first click arms the row's
    /// link into a "Really remove?" confirmation, the second deletes.
    Remove {
        tag: String,
    },
    /// Cycle how many reviewer models a committee run fans out to.
    CycleCommitteeReviewers,
    /// Cycle how many models a committee run may keep resident at once.
    CycleCommitteeMaxLoaded,
    /// Cycle one committee panel-round slot through its kinds (Off → judge
    /// notes → judge's rewrite → peer critique → converge → Off).
    CycleCommitteePanelRound {
        slot: usize,
    },
    /// Cycle which machine-evidence stages run before a review.
    CycleCommitteeEvidence,
    /// Cycle the proof round off, or on with a per-script time limit.
    CycleCommitteeProof,
    /// Cycle how many findings one reviewer may contribute.
    CycleCommitteeFindingsCap,
    /// Turn the committee's second-opinion pass on or off.
    ToggleCommitteeSecondOpinion,
    /// Turn the committee's closing audit on or off.
    ToggleCommitteeAudit,
    /// Switch between committee personas and plain model tags.
    ToggleCommitteePersonas,
    /// Add whatever address is in the remote-host field, then check it.
    AddRemoteHost,
    /// Re-ask a known host what models it has.
    CheckRemoteHost {
        url: String,
    },
    /// Forget a remote host and its models.
    ForgetRemoteHost {
        url: String,
    },
    /// Cycle a model's committee casting: auto → reviewer → judge → never.
    CycleCommitteeRole {
        tag: String,
    },
    /// Switch a model off (or back on) everywhere it would be offered.
    ToggleModelEnabled {
        tag: String,
    },
}

/// Where a models-folder move is headed.
enum ModelsFolderTarget {
    /// A folder the user picked.
    Custom(std::path::PathBuf),
    /// Back to `~/.ollama/models`, clearing the override.
    Default,
}

/// Progress of a pull the user started.
#[derive(Debug, Clone, PartialEq)]
pub enum InstallState {
    /// A pull is in flight. `fraction` is the download progress (0.0..=1.0)
    /// once Ollama reports byte counts, or `None` during byteless phases
    /// like "pulling manifest" — in which case `phase` names the phase.
    Running {
        fraction: Option<f32>,
        phase: String,
    },
    Failed(String),
    Done,
}

pub struct OllamaPageView {
    page: PageType<Self>,
    /// Models already on disk. `None` until the first probe completes.
    installed: Option<Vec<InstalledModel>>,
    /// Per-model capabilities (tool support, context, params), keyed by tag,
    /// for the role label in the Installed list. Empty until probed.
    capabilities: HashMap<String, service::ModelCapabilities>,
    /// Whether the background service answered on its fixed port.
    service_running: bool,
    catalog: Vec<CatalogEntry>,
    catalog_source: CatalogSource,
    catalog_loading: bool,
    /// Pull state per tag.
    installs: HashMap<String, InstallState>,
    /// Total RAM, or `None` on platforms we can't read — the page then omits
    /// the fit check rather than inventing one.
    total_memory_bytes: Option<u64>,
    /// Free-text "install any model by name" field.
    model_input: ViewHandle<EditorView>,
    /// Validation error for the typed tag, shown beneath the field.
    input_error: Option<String>,
    /// Tags submitted via the "Install by name" field this session, in
    /// submission order — each renders its own progress/status row beneath
    /// the field, so several installs started there run (and show) at once.
    /// A typed tag isn't necessarily in the visible catalog, which renders
    /// progress for its own rows only.
    typed_install_tags: Vec<String>,
    /// Set while a chosen folder's models are being copied to their new
    /// home. The "Change" link is disabled meanwhile — starting a second
    /// move before the first finishes would race on the same files.
    migrating_models: bool,
    /// Fraction (0.0..=1.0) of the in-progress move, for its progress bar.
    migration_progress: Option<f32>,
    /// Set if the last move failed, so the page can say why instead of
    /// silently reverting to the old folder.
    migration_error: Option<String>,
    /// The committee's per-model record on this machine, re-read on every
    /// refresh so a run's outcome shows up next time the page is opened.
    committee_leaderboard: CommitteeLeaderboard,
    /// The model whose Remove link is armed ("Really remove?") — cleared by
    /// refresh, by arming another row, or by the second click going through.
    pending_removal: Option<String>,
    /// The removal currently running against the server, if any. One at a
    /// time; the link says "Removing…" meanwhile.
    removing: Option<String>,
    /// Why the last removal failed, shown as a notice above the list.
    removal_error: Option<String>,
    /// Free-text "another machine's Ollama" address field.
    remote_input: ViewHandle<EditorView>,
    /// Validation error for the typed address, shown beneath the field.
    remote_error: Option<String>,
    /// Per-host outcome of the last check, keyed by base URL — what was found
    /// there, or why it could not be reached.
    remote_status: HashMap<String, String>,
    /// The host currently being probed, if any.
    remote_checking: Option<String>,
    state_handles: RefCell<StateHandles>,
}

#[derive(Default)]
struct StateHandles {
    enabled_toggle: SwitchStateHandle,
    refresh_link: MouseStateHandle,
    install_typed_link: MouseStateHandle,
    download_link: MouseStateHandle,
    reveal_folder_link: MouseStateHandle,
    change_folder_link: MouseStateHandle,
    reset_folder_link: MouseStateHandle,
    install_buttons: HashMap<String, MouseStateHandle>,
    /// Per-installed-model agent/chat override links, keyed by tag. Stable
    /// handles so the click registers (see the install-button note).
    mode_links: HashMap<String, MouseStateHandle>,
    /// Per-installed-model "open terminal" links, keyed by tag. Stable for the
    /// same reason as `mode_links`.
    terminal_links: HashMap<String, MouseStateHandle>,
    /// Per-installed-model remove links, keyed by tag. Stable likewise.
    remove_links: HashMap<String, MouseStateHandle>,
    /// Per-installed-model committee-casting links, keyed by tag.
    committee_role_links: HashMap<String, MouseStateHandle>,
    /// Per-installed-model on/off links, keyed by tag.
    enabled_links: HashMap<String, MouseStateHandle>,
    /// The committee section's cycling links.
    committee_reviewers_link: MouseStateHandle,
    committee_max_loaded_link: MouseStateHandle,
    committee_round_links: [MouseStateHandle; MAX_PANEL_EXTRA_ROUNDS],
    committee_evidence_link: MouseStateHandle,
    committee_proof_link: MouseStateHandle,
    committee_findings_cap_link: MouseStateHandle,
    committee_second_opinion_link: MouseStateHandle,
    committee_audit_link: MouseStateHandle,
    committee_personas_link: MouseStateHandle,
    add_remote_link: MouseStateHandle,
    /// Per-remote-host check links, keyed by base URL. Stable handles so the
    /// click registers (see the install-button note).
    remote_check_links: HashMap<String, MouseStateHandle>,
    /// Per-remote-host forget links, keyed by base URL.
    remote_forget_links: HashMap<String, MouseStateHandle>,
}

impl OllamaPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let widgets: Vec<Box<dyn SettingsWidget<View = Self>>> = vec![
            // The standard large AI-page title (with its separator; the
            // widgets below draw none of their own at the top).
            Box::new(PageTitleWidget::new("Ollama", true)),
            Box::new(StatusWidget),
            Box::new(InstalledWidget),
            Box::new(CatalogWidget),
            Box::new(RemoteHostsWidget),
            Box::new(CommitteeWidget),
        ];

        let model_input = ctx.add_typed_action_view(|ctx| {
            let mut editor = EditorView::single_line(SingleLineEditorOptions::default(), ctx);
            editor.set_placeholder_text("Install by name, e.g. qwen2.5-coder:14b", ctx);
            editor
        });

        let remote_input = ctx.add_typed_action_view(|ctx| {
            let mut editor = EditorView::single_line(SingleLineEditorOptions::default(), ctx);
            editor.set_placeholder_text("Address, e.g. 192.168.1.150", ctx);
            editor
        });

        // Picks up the persisted custom folder (if any) so `models_dir()`
        // reflects it from the moment this page is open, not just after the
        // user next changes it.
        service::apply_models_dir_override(ctx);

        let mut view = Self {
            page: PageType::new_uncategorized(widgets, None),
            model_input,
            input_error: None,
            typed_install_tags: Vec::new(),
            migrating_models: false,
            migration_progress: None,
            migration_error: None,
            installed: None,
            capabilities: HashMap::new(),
            service_running: false,
            catalog: Vec::new(),
            catalog_source: CatalogSource::Live,
            catalog_loading: false,
            installs: HashMap::new(),
            total_memory_bytes: capability::total_system_memory_bytes(),
            committee_leaderboard: CommitteeLeaderboard::default(),
            pending_removal: None,
            removing: None,
            removal_error: None,
            remote_input,
            remote_error: None,
            remote_status: HashMap::new(),
            remote_checking: None,
            state_handles: RefCell::new(StateHandles::default()),
        };
        view.refresh(ctx);
        view
    }

    /// Opens a folder picker and, once the user chooses a folder, moves
    /// existing models there and points future installs at it.
    fn change_models_folder(&mut self, ctx: &mut ViewContext<Self>) {
        if self.migrating_models {
            return;
        }
        ctx.open_file_picker(
            move |result, ctx| {
                let Ok(paths) = result else { return };
                let Some(path) = paths.into_iter().next() else {
                    return;
                };
                let picked: std::path::PathBuf = path.into();
                // Always land in a `models` subfolder of whatever the user
                // picked, matching the `~/.ollama/models` convention —
                // otherwise blobs/manifests land loose in the picked folder
                // itself, which looks wrong next to any other folder
                // already living alongside it (and is inconsistent with a
                // folder that already has its own `models/` in it).
                let new_dir = if picked.file_name().and_then(|name| name.to_str()) == Some("models")
                {
                    picked
                } else {
                    picked.join("models")
                };
                if let Some(handle) = ctx.handle().upgrade(ctx) {
                    handle.update(ctx, |me, ctx| {
                        me.start_folder_move(ModelsFolderTarget::Custom(new_dir), ctx)
                    });
                }
            },
            borkui::platform::FilePickerConfiguration::new().folders_only(),
        );
    }

    /// Cycles a model's agent/chat override: Auto → force Agent → force
    /// Chat → Auto, persisting it to `ollama_model_mode_overrides`.
    fn cycle_model_mode(&mut self, tag: String, ctx: &mut ViewContext<Self>) {
        let current = service::model_mode_override(
            AISettings::as_ref(ctx)
                .ollama_model_mode_overrides
                .value()
                .as_deref(),
            &tag,
        );
        let next = match current {
            None => Some(true),        // Auto → force Agent
            Some(true) => Some(false), // Agent → force Chat
            Some(false) => None,       // Chat → Auto
        };
        let updated = service::with_mode_override(
            AISettings::as_ref(ctx)
                .ollama_model_mode_overrides
                .value()
                .as_deref(),
            &tag,
            next,
        );
        report_if_error!(AISettings::handle(ctx).update(ctx, |settings, ctx| {
            settings
                .ollama_model_mode_overrides
                .set_value(Some(updated), ctx)
        }));
        ctx.notify();
    }

    /// Selects `tag` as the local harness model and opens a fresh terminal tab
    /// to use it — the quick path from "this model is installed" to chatting
    /// with it, without going through the harness model picker.
    fn open_terminal_with_model(&mut self, tag: String, ctx: &mut ViewContext<Self>) {
        let model_id = format!("{OLLAMA_MODEL_ID_PREFIX}{tag}");
        // Persist the selection the same way the model picker does for Ollama,
        // so the new tab's harness picks it up when it initializes.
        CloudAgentSettings::handle(ctx).update(ctx, |settings, ctx| {
            let _ = settings
                .last_selected_harness
                .set_value(Some(OLLAMA_HARNESS_CONFIG_NAME.to_string()), ctx);
            settings.persist_harness_model_selection_for_key(
                OLLAMA_HARNESS_CONFIG_NAME,
                &model_id,
                None,
                ctx,
            );
        });
        // Keep the harness picker in sync with the new selection.
        HarnessAvailabilityModel::handle(ctx).update(ctx, |availability, ctx| {
            availability.refresh_ollama_models(ctx)
        });
        // Open a new tab in the active window; it reads the selection above
        // when its local harness initializes.
        let active_window = WindowManager::handle(ctx).as_ref(ctx).active_window();
        if let Some(window_id) = active_window {
            ctx.dispatch_custom_action(CustomAction::NewTab, window_id);
        }
    }

    /// Two-step model removal: the first click arms the row ("Really
    /// remove?"), the second deletes via the server and refreshes. Arming a
    /// different row moves the confirmation there instead.
    fn remove_model(&mut self, tag: String, ctx: &mut ViewContext<Self>) {
        if self.removing.is_some() {
            return;
        }
        if self.pending_removal.as_deref() != Some(tag.as_str()) {
            self.pending_removal = Some(tag);
            self.removal_error = None;
            ctx.notify();
            return;
        }
        self.pending_removal = None;
        self.removing = Some(tag.clone());
        ctx.notify();
        let client = reqwest::Client::new();
        let failed_tag = tag.clone();
        ctx.spawn(
            async move { service::remove_model(&client, &tag).await },
            move |me, removed, ctx| {
                me.removing = None;
                if !removed {
                    me.removal_error = Some(format!(
                        "Couldn't remove {failed_tag} — is the Ollama service running?"
                    ));
                }
                // Re-list from the server and sync the picker (the committee
                // row may also need to appear or vanish with the count).
                me.refresh(ctx);
            },
        );
    }

    /// Switches a model off, or back on. Off is not uninstall: the weights
    /// stay on disk, the model simply stops being offered — in the model
    /// menu, and to the committee. The picker is rebuilt immediately so the
    /// menu agrees with this page without a restart.
    fn toggle_model_enabled(&mut self, tag: String, ctx: &mut ViewContext<Self>) {
        let disabled_json = AISettings::as_ref(ctx)
            .ollama_disabled_models
            .value()
            .clone();
        let turning_off = !service::is_model_disabled(disabled_json.as_deref(), &tag);
        let updated = service::with_model_disabled(disabled_json.as_deref(), &tag, turning_off);
        report_if_error!(AISettings::handle(ctx).update(ctx, |settings, ctx| {
            settings
                .ollama_disabled_models
                .set_value(Some(updated), ctx)
        }));
        HarnessAvailabilityModel::handle(ctx).update(ctx, |availability, ctx| {
            availability.refresh_ollama_models(ctx)
        });
        ctx.notify();
    }

    /// Reads the committee config, applies one change, and persists it —
    /// both cycling links go through here.
    fn update_committee_config(
        &mut self,
        ctx: &mut ViewContext<Self>,
        mutate: impl FnOnce(&mut OllamaCommitteeConfig),
    ) {
        let mut config = OllamaCommitteeConfig::from_settings_json(
            AISettings::as_ref(ctx)
                .ollama_committee_config
                .value()
                .as_deref(),
        );
        mutate(&mut config);
        report_if_error!(AISettings::handle(ctx).update(ctx, |settings, ctx| {
            settings
                .ollama_committee_config
                .set_value(Some(config.to_settings_json()), ctx)
        }));
        ctx.notify();
    }

    /// Moves models back to `~/.ollama/models` and clears the custom-folder
    /// setting, without needing a picker.
    fn reset_models_folder(&mut self, ctx: &mut ViewContext<Self>) {
        if self.migrating_models {
            return;
        }
        self.start_folder_move(ModelsFolderTarget::Default, ctx);
    }

    fn start_folder_move(&mut self, target: ModelsFolderTarget, ctx: &mut ViewContext<Self>) {
        let old_dir = service::models_dir();
        let new_dir = match &target {
            ModelsFolderTarget::Custom(dir) => dir.clone(),
            ModelsFolderTarget::Default => service::default_models_dir(),
        };
        if new_dir == old_dir {
            return;
        }

        self.migrating_models = true;
        self.migration_progress = None;
        self.migration_error = None;
        ctx.notify();

        // The override value to persist once the move succeeds depends only
        // on the target, so resolve it up front — the streaming handler
        // below is `FnMut` and can't consume `target` out of itself.
        let is_default = matches!(target, ModelsFolderTarget::Default);
        let override_value = match target {
            ModelsFolderTarget::Custom(dir) => Some(dir.to_string_lossy().into_owned()),
            ModelsFolderTarget::Default => None,
        };

        let (tx, rx) = async_channel::unbounded();
        ctx.background_executor()
            .spawn(async move { service::move_models_dir(old_dir, new_dir, tx).await })
            .detach();

        ctx.spawn_stream_local(
            rx,
            move |me, event, ctx| {
                match event {
                    service::MoveEvent::Progress(fraction) => {
                        me.migration_progress = Some(fraction);
                    }
                    service::MoveEvent::Finished(Ok(())) => {
                        me.migrating_models = false;
                        me.migration_progress = None;
                        // `apply_models_dir_override` only ever *sets*
                        // `OLLAMA_MODELS` (never clears it, in case the
                        // user's own shell set it independently of Bork) —
                        // but resetting to default is the one place the
                        // user explicitly asked to stop overriding it, so
                        // clear the env var here too.
                        if is_default {
                            std::env::remove_var("OLLAMA_MODELS");
                        }
                        report_if_error!(AISettings::handle(ctx).update(ctx, |settings, ctx| {
                            settings
                                .ollama_models_dir_override
                                .set_value(override_value.clone(), ctx)
                        }));
                        service::apply_models_dir_override(ctx);
                        me.refresh(ctx);
                    }
                    service::MoveEvent::Finished(Err(error)) => {
                        me.migrating_models = false;
                        me.migration_progress = None;
                        me.migration_error = Some(error);
                    }
                }
                ctx.notify();
            },
            |_, _| {},
        );
    }

    /// Re-probes the local service and re-fetches the library listing.
    fn refresh(&mut self, ctx: &mut ViewContext<Self>) {
        if self.catalog_loading {
            return;
        }
        self.catalog_loading = true;
        // A refresh redraws the world — an armed "Really remove?" from
        // before it is stale.
        self.pending_removal = None;
        ctx.notify();

        let client = reqwest::Client::new();
        ctx.spawn(
            async move {
                let running = service::is_running(&client).await;
                let installed = service::list_installed(&client).await.ok();
                let (catalog, source) = service::fetch_catalog(&client).await;
                // Capabilities per installed model (tool support, context,
                // params) so the list can label each one's role. Only worth
                // asking when the server is up; /api/show is metadata-only and
                // doesn't load the model.
                let mut capabilities = HashMap::new();
                if running {
                    if let Some(models) = &installed {
                        for model in models {
                            if let Some(caps) =
                                service::fetch_capabilities(&client, &model.name).await
                            {
                                capabilities.insert(model.name.clone(), caps);
                            }
                        }
                    }
                }
                let leaderboard = ollama_committee::load_leaderboard();
                (
                    running,
                    installed,
                    catalog,
                    source,
                    capabilities,
                    leaderboard,
                )
            },
            |me, (running, installed, catalog, source, capabilities, leaderboard), ctx| {
                me.service_running = running;
                me.installed = installed;
                me.catalog = catalog;
                me.catalog_source = source;
                me.capabilities = capabilities;
                me.committee_leaderboard = leaderboard;
                me.catalog_loading = false;
                // Sync the model picker to what's actually on disk — this
                // catches models pulled outside Bork (e.g. `ollama pull` in a
                // terminal), which wouldn't otherwise appear until restart.
                crate::ai::harness_availability::HarnessAvailabilityModel::handle(ctx).update(
                    ctx,
                    |availability, ctx| {
                        availability.refresh_ollama_models(ctx);
                    },
                );
                ctx.notify();
            },
        );
    }

    /// Pulls `tag` via Ollama's streaming API, updating a live progress bar.
    fn install(&mut self, tag: String, ctx: &mut ViewContext<Self>) {
        if matches!(self.installs.get(&tag), Some(InstallState::Running { .. })) {
            return;
        }
        if service::resolve_binary().is_none() {
            self.installs.insert(
                tag,
                InstallState::Failed("Ollama isn't installed".to_string()),
            );
            ctx.notify();
            return;
        }

        // Starts in the byteless "Installing…" state; the first byte-count
        // update from the stream flips it to a determinate bar.
        self.installs.insert(
            tag.clone(),
            InstallState::Running {
                fraction: None,
                phase: String::new(),
            },
        );
        ctx.notify();

        let (progress_tx, progress_rx) = async_channel::unbounded();
        let client = reqwest::Client::new();
        let pull_tag = tag.clone();
        ctx.background_executor()
            .spawn(async move {
                service::pull_model(&client, &pull_tag, progress_tx).await;
            })
            .detach();

        let stream_tag = tag;
        ctx.spawn_stream_local(
            progress_rx,
            move |me, event, ctx| {
                match event {
                    service::PullEvent::Progress { fraction, status } => {
                        me.installs.insert(
                            stream_tag.clone(),
                            InstallState::Running {
                                fraction,
                                phase: status,
                            },
                        );
                    }
                    service::PullEvent::Done => {
                        me.installs.insert(stream_tag.clone(), InstallState::Done);
                        // A completed pull changes what's on disk.
                        me.refresh(ctx);
                        // Surface the new model in the picker menu without an
                        // app restart (the harness list is otherwise built
                        // once at launch).
                        crate::ai::harness_availability::HarnessAvailabilityModel::handle(ctx)
                            .update(ctx, |availability, ctx| {
                                availability.refresh_ollama_models(ctx);
                            });
                    }
                    service::PullEvent::Failed(reason) => {
                        me.installs
                            .insert(stream_tag.clone(), InstallState::Failed(reason));
                    }
                }
                ctx.notify();
            },
            |_, _| {},
        );
    }

    /// Validates and installs whatever is in the free-text field.
    fn install_typed(&mut self, ctx: &mut ViewContext<Self>) {
        let raw: String = self.model_input.as_ref(ctx).buffer_text(ctx);
        match service::validate_model_tag(&raw) {
            Ok(tag) => {
                self.input_error = None;
                self.model_input.update(ctx, |editor, ctx| {
                    editor.set_buffer_text("", ctx);
                });
                // Remember it so its progress renders under the field — a
                // typed tag may not be in the visible catalog. A list, not a
                // slot: several installs started here each keep their row
                // (re-submitting a tag just moves it to the end).
                self.typed_install_tags.retain(|existing| existing != &tag);
                self.typed_install_tags.push(tag.clone());
                self.install(tag, ctx);
            }
            Err(err) => {
                self.input_error = Some(err.to_string());
                ctx.notify();
            }
        }
    }

    /// Adds whatever address is in the field and immediately asks that
    /// machine what it has — a host with no models is useless, so there is no
    /// point separating "add" from "check".
    fn add_remote_host(&mut self, ctx: &mut ViewContext<Self>) {
        let raw: String = self.remote_input.as_ref(ctx).buffer_text(ctx);
        let url = match remote::normalize_host_input(&raw) {
            Ok(url) => url,
            Err(error) => {
                self.remote_error = Some(error);
                ctx.notify();
                return;
            }
        };
        self.remote_error = None;
        self.remote_input.update(ctx, |editor, ctx| {
            editor.set_buffer_text("", ctx);
        });

        let mut hosts = remote::configured(ctx);
        if !hosts.iter().any(|host| host.url == url) {
            hosts.push(remote::RemoteHost {
                url: url.clone(),
                models: Vec::new(),
                label: None,
            });
            remote::save(&hosts, ctx);
        }
        self.check_remote_host(url, ctx);
    }

    /// Asks a host what models it has and records the answer. The stored list
    /// is what the model menu reads, so this is also how a model pulled on
    /// that machine becomes selectable here.
    fn check_remote_host(&mut self, url: String, ctx: &mut ViewContext<Self>) {
        if self.remote_checking.is_some() {
            return;
        }
        self.remote_checking = Some(url.clone());
        self.remote_status
            .insert(url.clone(), "Checking…".to_owned());
        ctx.notify();

        let client = reqwest::Client::new();
        let probe_url = url.clone();
        ctx.spawn(
            async move { service::probe_host(&client, &probe_url).await },
            move |me, result, ctx| {
                me.remote_checking = None;
                let found = match result {
                    service::ProbeResult::Reachable { models } => models,
                    service::ProbeResult::Unreachable { reason } => {
                        me.remote_status
                            .insert(url.clone(), format!("Couldn't reach it — {reason}"));
                        ctx.notify();
                        return;
                    }
                };
                me.remote_status.insert(
                    url.clone(),
                    match found.len() {
                        0 => "Reached it, but it has no models pulled yet.".to_owned(),
                        1 => "1 model available.".to_owned(),
                        count => format!("{count} models available."),
                    },
                );

                let mut hosts = remote::configured(ctx);
                if let Some(host) = hosts.iter_mut().find(|host| host.url == url) {
                    host.models = found.into_iter().map(|model| model.name).collect();
                }
                remote::save(&hosts, ctx);
                // The picker's Ollama list is rebuilt from the saved hosts.
                HarnessAvailabilityModel::handle(ctx).update(ctx, |availability, ctx| {
                    availability.refresh_ollama_models(ctx)
                });
                ctx.notify();
            },
        );
    }

    fn forget_remote_host(&mut self, url: String, ctx: &mut ViewContext<Self>) {
        let mut hosts = remote::configured(ctx);
        hosts.retain(|host| host.url != url);
        remote::save(&hosts, ctx);
        self.remote_status.remove(&url);
        HarnessAvailabilityModel::handle(ctx).update(ctx, |availability, ctx| {
            availability.refresh_ollama_models(ctx)
        });
        ctx.notify();
    }

    fn is_enabled(&self, app: &AppContext) -> bool {
        *AISettings::as_ref(app).ollama_enabled.value()
    }

    /// The verdict for a size, or `None` when memory is unknown.
    fn verdict(&self, size: &ModelSize) -> Option<MemoryVerdict> {
        self.total_memory_bytes
            .map(|total| capability::assess(size.params, total))
    }

    fn is_installed_tag(&self, tag: &str) -> bool {
        self.installed.as_ref().is_some_and(|models| {
            models
                .iter()
                .any(|m| m.name == tag || m.name.strip_suffix(":latest") == Some(tag))
        })
    }
}

impl Entity for OllamaPageView {
    type Event = ();
}

impl View for OllamaPageView {
    fn ui_name() -> &'static str {
        "OllamaPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

impl TypedActionView for OllamaPageView {
    type Action = OllamaPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            OllamaPageAction::ToggleEnabled => {
                let now_enabled = AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.ollama_enabled.toggle_and_save_value(ctx));
                    *settings.ollama_enabled.value()
                });
                // Make the switch actually drive the engine: enabling boots
                // the server so models are ready; disabling unloads them to
                // free memory. Turning it off then on therefore reboots the
                // working state, which is what a user expects a power switch
                // to do. Best-effort and off-thread; the follow-up refresh
                // reflects the new running state.
                let client = reqwest::Client::new();
                ctx.spawn(
                    async move {
                        if now_enabled {
                            service::ensure_running(&client).await;
                        } else {
                            service::stop_running_models().await;
                        }
                    },
                    |me, (), ctx| me.refresh(ctx),
                );
                ctx.notify();
            }
            OllamaPageAction::RefreshCatalog => self.refresh(ctx),
            OllamaPageAction::OpenOllamaDownload => ctx.open_url(OLLAMA_DOWNLOAD_URL),
            OllamaPageAction::RevealModelFolder => {
                ctx.open_file_path_in_explorer(&service::models_dir());
            }
            OllamaPageAction::ChangeModelsFolder => self.change_models_folder(ctx),
            OllamaPageAction::ResetModelsFolder => self.reset_models_folder(ctx),
            OllamaPageAction::CycleModelMode { tag } => self.cycle_model_mode(tag.clone(), ctx),
            OllamaPageAction::OpenTerminalWithModel { tag } => {
                self.open_terminal_with_model(tag.clone(), ctx)
            }
            OllamaPageAction::Install { tag } => self.install(tag.clone(), ctx),
            OllamaPageAction::InstallTyped => self.install_typed(ctx),
            OllamaPageAction::Remove { tag } => self.remove_model(tag.clone(), ctx),
            OllamaPageAction::CycleCommitteeReviewers => {
                self.update_committee_config(ctx, |config| {
                    config.reviewers = config.reviewers % MAX_REVIEWERS + 1;
                });
            }
            OllamaPageAction::CycleCommitteeMaxLoaded => {
                self.update_committee_config(ctx, |config| {
                    config.max_loaded = config.max_loaded % MAX_LOADED_MODELS + 1;
                });
            }
            OllamaPageAction::CycleCommitteePanelRound { slot } => {
                let slot = *slot;
                self.update_committee_config(ctx, |config| {
                    cycle_panel_round(&mut config.panel_plan, slot);
                });
            }
            OllamaPageAction::CycleCommitteeEvidence => {
                self.update_committee_config(ctx, cycle_evidence_stages);
            }
            OllamaPageAction::CycleCommitteeProof => {
                self.update_committee_config(ctx, cycle_proof_round);
            }
            OllamaPageAction::CycleCommitteeFindingsCap => {
                self.update_committee_config(ctx, cycle_findings_cap);
            }
            OllamaPageAction::ToggleCommitteeSecondOpinion => {
                self.update_committee_config(ctx, |config| {
                    config.second_opinion = !config.second_opinion;
                });
            }
            OllamaPageAction::ToggleCommitteeAudit => {
                self.update_committee_config(ctx, |config| {
                    config.audit = !config.audit;
                });
            }
            OllamaPageAction::ToggleCommitteePersonas => {
                self.update_committee_config(ctx, |config| {
                    config.personas = !config.personas;
                });
            }
            OllamaPageAction::AddRemoteHost => self.add_remote_host(ctx),
            OllamaPageAction::CheckRemoteHost { url } => self.check_remote_host(url.clone(), ctx),
            OllamaPageAction::ForgetRemoteHost { url } => self.forget_remote_host(url.clone(), ctx),
            OllamaPageAction::CycleCommitteeRole { tag } => {
                let tag = tag.clone();
                self.update_committee_config(ctx, |config| {
                    cycle_committee_role(&mut config.roles, &tag);
                });
            }
            OllamaPageAction::ToggleModelEnabled { tag } => {
                self.toggle_model_enabled(tag.clone(), ctx);
            }
        }
    }
}

// ── Widgets ──────────────────────────────────────────────────────────────

/// Installed / running state, and the enable switch.
#[derive(Default)]
struct StatusWidget;

impl SettingsWidget for StatusWidget {
    type View = OllamaPageView;

    fn search_terms(&self) -> &str {
        "ollama local models enable install running service"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let installed = service::is_installed();

        let mut column = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);

        column.add_child(body_text(
            "Run models on your own machine through Ollama. Models are downloaded \
             and stored by Ollama, not by this app — you choose what to install and \
             how much disk to spend.",
            appearance,
        ));

        if installed {
            // The switch is the top-level state: when local models are off,
            // the engine's running state is irrelevant and would only confuse.
            let status = if !view.is_enabled(app) {
                "Ollama is off. Turn on Use local models to add and run models."
            } else if view.service_running {
                "Ollama is installed and running."
            } else {
                "Ollama is installed but not currently running. It starts automatically \
                 when a model is used."
            };
            column.add_child(
                Container::new(body_text(status, appearance))
                    .with_margin_top(8.)
                    .finish(),
            );

            let handles = view.state_handles.borrow();
            column.add_child(
                Container::new(
                    Flex::row()
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_child(
                            Text::new(
                                "Use local models".to_string(),
                                appearance.ui_font_family(),
                                appearance.ui_font_size(),
                            )
                            .finish(),
                        )
                        .with_child(
                            Container::new(
                                appearance
                                    .ui_builder()
                                    .switch(handles.enabled_toggle.clone())
                                    .check(view.is_enabled(app))
                                    .build()
                                    .on_click(|ctx, _, _| {
                                        ctx.dispatch_typed_action(OllamaPageAction::ToggleEnabled);
                                    })
                                    .finish(),
                            )
                            .with_margin_left(12.)
                            .finish(),
                        )
                        .finish(),
                )
                .with_margin_top(VERTICAL_MARGIN)
                .finish(),
            );
        } else {
            column.add_child(
                Container::new(notice(
                    "Ollama isn't installed on this machine. Install it first, then \
                     come back here to add models.",
                    ThemeFill::error(),
                    appearance,
                ))
                .with_margin_top(8.)
                .finish(),
            );
            let handles = view.state_handles.borrow();
            column.add_child(
                Container::new(
                    appearance
                        .ui_builder()
                        .link(
                            "Get Ollama".into(),
                            None,
                            Some(Box::new(|ctx| {
                                ctx.dispatch_typed_action(OllamaPageAction::OpenOllamaDownload);
                            })),
                            handles.download_link.clone(),
                        )
                        .soft_wrap(false)
                        .build()
                        .finish(),
                )
                .with_margin_top(8.)
                .finish(),
            );
        }

        Container::new(column.finish())
            .with_margin_top(VERTICAL_MARGIN)
            .finish()
    }
}

/// Models already pulled onto this machine.
#[derive(Default)]
struct InstalledWidget;

impl SettingsWidget for InstalledWidget {
    type View = OllamaPageView;

    fn search_terms(&self) -> &str {
        "ollama installed models downloaded disk"
    }

    fn should_render(&self, _app: &AppContext) -> bool {
        service::is_installed()
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let mut column = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        column.add_child(section_break(appearance));
        // "Installed" on the left; the model folder and a Reveal button on the
        // right, so the location is shown and openable.
        column.add_child(
            Container::new(
                Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(
                        // `render_sub_header` is itself a flex row with a
                        // flexible child; sharing this row with the Expanded
                        // spacer below means it's measured under an infinite
                        // width constraint unless it's flexible too — bare it
                        // panics with "flex contains flexible children but
                        // has an infinite constraint" (matches the idiom in
                        // privacy_page.rs's safe-mode title row).
                        Shrinkable::new(1.0, render_sub_header(appearance, "Installed", None))
                            .finish(),
                    )
                    .with_child(borkui::elements::Expanded::new(1., Empty::new().finish()).finish())
                    .with_child(model_folder_header(view, appearance))
                    .finish(),
            )
            .with_margin_top(VERTICAL_MARGIN)
            .finish(),
        );

        if view.migrating_models {
            let mut moving = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Start);
            moving.add_child(body_text(
                "Moving models to the new folder… this can take a while for large models.",
                appearance,
            ));
            // Wider bar here than the install cell — there's a full row to
            // fill, and a big model move is exactly where the user wanted
            // reassurance something's happening.
            moving.add_child(
                Container::new(progress_row(
                    view.migration_progress.unwrap_or(0.0),
                    240.,
                    appearance,
                ))
                .with_margin_top(6.)
                .finish(),
            );
            column.add_child(Container::new(moving.finish()).with_margin_top(8.).finish());
        } else if let Some(error) = &view.migration_error {
            column.add_child(
                Container::new(notice(
                    &format!("Couldn't move models folder: {error}"),
                    ThemeFill::error(),
                    appearance,
                ))
                .with_margin_top(8.)
                .finish(),
            );
        }

        // The per-model agent/chat override map, and stable click handles for
        // each installed model's mode link (a click spans several frames — a
        // throwaway handle would never register; same reason as the catalog
        // install buttons).
        let overrides_json = AISettings::as_ref(app)
            .ollama_model_mode_overrides
            .value()
            .clone();
        let disabled_json = AISettings::as_ref(app)
            .ollama_disabled_models
            .value()
            .clone();
        if let Some(models) = view.installed.as_ref() {
            let mut handles = view.state_handles.borrow_mut();
            for model in models {
                handles.mode_links.entry(model.name.clone()).or_default();
                handles
                    .terminal_links
                    .entry(model.name.clone())
                    .or_default();
                handles.remove_links.entry(model.name.clone()).or_default();
                handles
                    .committee_role_links
                    .entry(model.name.clone())
                    .or_default();
                handles.enabled_links.entry(model.name.clone()).or_default();
            }
        }

        if let Some(error) = &view.removal_error {
            column.add_child(
                Container::new(notice(error, ThemeFill::error(), appearance))
                    .with_margin_top(8.)
                    .finish(),
            );
        }

        match view.installed.as_ref() {
            None => column.add_child(body_text(
                "Couldn't reach Ollama to list installed models.",
                appearance,
            )),
            Some(models) if models.is_empty() => column.add_child(body_text(
                "No models installed yet. Pick one below.",
                appearance,
            )),
            Some(models) => {
                for model in models {
                    let mut row = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);
                    row.add_child(
                        Text::new(
                            model.name.clone(),
                            appearance.ui_font_family(),
                            appearance.ui_font_size(),
                        )
                        .with_style(borkui::fonts::Properties::default().weight(Weight::Medium))
                        .finish(),
                    );
                    row.add_child(
                        Container::new(sub_text(
                            &capability::format_gb(model.size_bytes),
                            appearance,
                        ))
                        .with_margin_left(12.)
                        .finish(),
                    );
                    // Role + capability, reflecting any override: what the
                    // harness will actually do with this model, plus its
                    // context window. A model switched off says only that —
                    // its context window is beside the point.
                    let off = service::is_model_disabled(disabled_json.as_deref(), &model.name);
                    let override_mode =
                        service::model_mode_override(overrides_json.as_deref(), &model.name);
                    let caps = view.capabilities.get(&model.name);
                    let summary = if off {
                        "Off — hidden from the model menu and the committee".to_string()
                    } else {
                        role_summary(caps, override_mode)
                    };
                    row.add_child(
                        Container::new(sub_text(&summary, appearance))
                            .with_margin_left(12.)
                            .finish(),
                    );
                    // The master switch. Off keeps the model on disk but takes
                    // it out of every menu — the answer to a model too heavy
                    // for this machine that the user isn't ready to delete.
                    let enabled_state = view
                        .state_handles
                        .borrow()
                        .enabled_links
                        .get(&model.name)
                        .cloned()
                        .unwrap_or_default();
                    let enabled_tag = model.name.clone();
                    row.add_child(
                        Container::new(
                            appearance
                                .ui_builder()
                                .link(
                                    if off { "Turn on" } else { "Turn off" }.into(),
                                    None,
                                    Some(Box::new(move |ctx| {
                                        ctx.dispatch_typed_action(
                                            OllamaPageAction::ToggleModelEnabled {
                                                tag: enabled_tag.clone(),
                                            },
                                        );
                                    })),
                                    enabled_state,
                                )
                                .soft_wrap(false)
                                .build()
                                .finish(),
                        )
                        .with_margin_left(12.)
                        .finish(),
                    );
                    // Everything below acts on a model that runs. Switched
                    // off, none of it applies, so none of it is offered.
                    if !off {
                        // The mode override control: cycles Auto → Agent → Chat.
                        let mode_state = view
                            .state_handles
                            .borrow()
                            .mode_links
                            .get(&model.name)
                            .cloned()
                            .unwrap_or_default();
                        let cycle_tag = model.name.clone();
                        row.add_child(
                            Container::new(
                                appearance
                                    .ui_builder()
                                    .link(
                                        override_link_label(override_mode).into(),
                                        None,
                                        Some(Box::new(move |ctx| {
                                            ctx.dispatch_typed_action(
                                                OllamaPageAction::CycleModelMode {
                                                    tag: cycle_tag.clone(),
                                                },
                                            );
                                        })),
                                        mode_state,
                                    )
                                    .soft_wrap(false)
                                    .build()
                                    .finish(),
                            )
                            .with_margin_left(12.)
                            .finish(),
                        );
                        // Open a terminal already set to use this model.
                        let terminal_state = view
                            .state_handles
                            .borrow()
                            .terminal_links
                            .get(&model.name)
                            .cloned()
                            .unwrap_or_default();
                        let terminal_tag = model.name.clone();
                        row.add_child(
                            Container::new(
                                appearance
                                    .ui_builder()
                                    .link(
                                        "Open terminal".into(),
                                        None,
                                        Some(Box::new(move |ctx| {
                                            ctx.dispatch_typed_action(
                                                OllamaPageAction::OpenTerminalWithModel {
                                                    tag: terminal_tag.clone(),
                                                },
                                            );
                                        })),
                                        terminal_state,
                                    )
                                    .soft_wrap(false)
                                    .build()
                                    .finish(),
                            )
                            .with_margin_left(12.)
                            .finish(),
                        );
                        // Committee casting: force this model into (or out of)
                        // the bench. "never" is the escape hatch for a model
                        // too heavy to sit through a run on this machine.
                        let committee_config = OllamaCommitteeConfig::from_settings_json(
                            AISettings::as_ref(app)
                                .ollama_committee_config
                                .value()
                                .as_deref(),
                        );
                        let role_state = view
                            .state_handles
                            .borrow()
                            .committee_role_links
                            .get(&model.name)
                            .cloned()
                            .unwrap_or_default();
                        let role_label = match committee_config.roles.get(&model.name) {
                            Some(role) => format!("Committee: {}", role.display_name()),
                            None => "Committee: auto".to_string(),
                        };
                        let role_tag = model.name.clone();
                        row.add_child(
                            Container::new(
                                appearance
                                    .ui_builder()
                                    .link(
                                        role_label,
                                        None,
                                        Some(Box::new(move |ctx| {
                                            ctx.dispatch_typed_action(
                                                OllamaPageAction::CycleCommitteeRole {
                                                    tag: role_tag.clone(),
                                                },
                                            );
                                        })),
                                        role_state,
                                    )
                                    .soft_wrap(false)
                                    .build()
                                    .finish(),
                            )
                            .with_margin_left(12.)
                            .finish(),
                        );
                    }
                    // Removal, with an in-row confirmation step — deleting
                    // gigabytes deserves a second click, not a first-click
                    // surprise.
                    let remove_state = view
                        .state_handles
                        .borrow()
                        .remove_links
                        .get(&model.name)
                        .cloned()
                        .unwrap_or_default();
                    let remove_label = if view.removing.as_deref() == Some(model.name.as_str()) {
                        "Removing…".to_string()
                    } else if view.pending_removal.as_deref() == Some(model.name.as_str()) {
                        format!("Really remove {}?", capability::format_gb(model.size_bytes))
                    } else {
                        "Remove".to_string()
                    };
                    let remove_tag = model.name.clone();
                    row.add_child(
                        Container::new(
                            appearance
                                .ui_builder()
                                .link(
                                    remove_label,
                                    None,
                                    Some(Box::new(move |ctx| {
                                        ctx.dispatch_typed_action(OllamaPageAction::Remove {
                                            tag: remove_tag.clone(),
                                        });
                                    })),
                                    remove_state,
                                )
                                .soft_wrap(false)
                                .build()
                                .finish(),
                        )
                        .with_margin_left(12.)
                        .finish(),
                    );
                    // Same fit flag as the catalog, so an already-installed
                    // model that is too big for this machine still says so.
                    if let Some(badge) = view
                        .total_memory_bytes
                        .and_then(|total| capability::assess_tag(&model.name, total))
                        .filter(|v| v.fit.is_warning())
                        .and_then(|v| fit_badge(v.fit, appearance))
                    {
                        row.add_child(Container::new(badge).with_margin_left(12.).finish());
                    }
                    column.add_child(Container::new(row.finish()).with_margin_top(6.).finish());
                }
            }
        }

        Container::new(column.finish()).finish()
    }
}

/// The library listing, with a fit verdict per size.
#[derive(Default)]
struct CatalogWidget;

impl SettingsWidget for CatalogWidget {
    type View = OllamaPageView;

    fn search_terms(&self) -> &str {
        "ollama library catalog add model download pull llama qwen mistral gemma deepseek"
    }

    fn should_render(&self, _app: &AppContext) -> bool {
        service::is_installed()
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        let mut column = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);

        column.add_child(section_break(appearance));
        column.add_child(
            Container::new(render_sub_header(appearance, "Add a model", None))
                .with_margin_top(VERTICAL_MARGIN)
                .finish(),
        );

        // Free-text install: the catalog is a shortlist, but the user can
        // pull anything on the registry by name. This is the path the
        // fallback note points at when the live list is unavailable.
        {
            let handles = view.state_handles.borrow();
            column.add_child(
                Container::new(
                    Flex::row()
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_child(
                            borkui::elements::Expanded::new(
                                1.,
                                ChildView::new(&view.model_input).finish(),
                            )
                            .finish(),
                        )
                        .with_child(
                            Container::new(
                                appearance
                                    .ui_builder()
                                    .link(
                                        "Install".into(),
                                        None,
                                        Some(Box::new(|ctx| {
                                            ctx.dispatch_typed_action(
                                                OllamaPageAction::InstallTyped,
                                            );
                                        })),
                                        handles.install_typed_link.clone(),
                                    )
                                    .soft_wrap(false)
                                    .build()
                                    .finish(),
                            )
                            .with_margin_left(12.)
                            .finish(),
                        )
                        .finish(),
                )
                .with_margin_top(8.)
                .finish(),
            );
        }
        if let Some(err) = &view.input_error {
            column.add_child(notice(err, ThemeFill::error(), appearance));
        }

        // Progress/status for models installed via the field above (their
        // tags may not appear in the catalog list, which shows its own rows'
        // progress). One row per typed install, so concurrent downloads all
        // stay visible. Mirrors the catalog cell: a live bar while
        // downloading, a phase label before bytes flow, then a terminal
        // status. Bars are labelled with their tag — several unlabelled bars
        // stacked would be ambiguous.
        for tag in &view.typed_install_tags {
            if let Some(state) = view.installs.get(tag) {
                let status: Box<dyn Element> = match state {
                    InstallState::Running {
                        fraction: Some(fraction),
                        ..
                    } => {
                        let mut labelled =
                            Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);
                        labelled.add_child(sub_text(tag, appearance));
                        labelled.add_child(
                            Container::new(progress_row(*fraction, 200., appearance))
                                .with_margin_left(12.)
                                .finish(),
                        );
                        labelled.finish()
                    }
                    InstallState::Running { phase, .. } if !phase.is_empty() => sub_text(
                        &format!("{}: {}…", tag, capitalize_first(phase)),
                        appearance,
                    ),
                    InstallState::Running { .. } => {
                        sub_text(&format!("Installing {tag}…"), appearance)
                    }
                    InstallState::Done => sub_text(&format!("Installed {tag}."), appearance),
                    InstallState::Failed(reason) => notice(
                        &format!("Couldn't install {tag}: {reason}"),
                        ThemeFill::error(),
                        appearance,
                    ),
                };
                column.add_child(Container::new(status).with_margin_top(8.).finish());
            }
        }

        // A line between "install by name" above and the browsable library
        // below, so the two ways to add a model read as distinct.
        column.add_child(section_break(appearance));
        column.add_child(sub_text("Or choose from the library:", appearance));

        if let Some(note) = view.catalog_source.note() {
            column.add_child(notice(note, ThemeFill::error(), appearance));
        }

        if view.catalog_loading {
            column.add_child(body_text("Loading models from ollama.com…", appearance));
            return Container::new(column.finish()).finish();
        }

        if let Some(total) = view.total_memory_bytes {
            column.add_child(
                Container::new(sub_text(
                    &format!(
                        "This machine has {}. Sizes below are estimates — they're a \
                         warning, not a limit.",
                        capability::format_gb(total)
                    ),
                    appearance,
                ))
                .with_margin_top(6.)
                .finish(),
            );
            // The key for the inline badges, so each model row can stay terse.
            column.add_child(fit_legend(appearance));
        }

        // Ensure every visible model tag has a *stable* install-button
        // mouse-state handle before the immutable borrow below. A click spans
        // several render frames (down → hover → up); without a persistent
        // handle each render handed the link a throwaway `unwrap_or_default`
        // one, so the click never completed and Install did nothing.
        {
            let mut handles_mut = view.state_handles.borrow_mut();
            for entry in view
                .catalog
                .iter()
                .filter(|e| !e.is_embedding_only() && !e.sizes.is_empty())
                .take(VISIBLE_CATALOG_LIMIT)
            {
                for size in &entry.sizes {
                    handles_mut
                        .install_buttons
                        .entry(entry.tag_for(size))
                        .or_default();
                }
            }
        }

        let handles = view.state_handles.borrow();
        // Chat models only: embedding models can't be talked to, and listing
        // them here would just be noise.
        for entry in view
            .catalog
            .iter()
            .filter(|e| !e.is_embedding_only() && !e.sizes.is_empty())
            .take(VISIBLE_CATALOG_LIMIT)
        {
            column.add_child(render_catalog_entry(view, entry, &handles, appearance));
        }

        column.add_child(
            Container::new(
                appearance
                    .ui_builder()
                    .link(
                        "Refresh list".into(),
                        None,
                        Some(Box::new(|ctx| {
                            ctx.dispatch_typed_action(OllamaPageAction::RefreshCatalog);
                        })),
                        handles.refresh_link.clone(),
                    )
                    .soft_wrap(false)
                    .build()
                    .finish(),
            )
            .with_margin_top(VERTICAL_MARGIN)
            .finish(),
        );

        Container::new(column.finish()).finish()
    }
}

/// The multi-model committee: its two knobs and this machine's standings.
/// See MULTIAGENT-DESIGN.md for the pipeline this configures.
/// Ollama instances on other machines.
///
/// The models a laptop can hold are limited by its memory; a desktop with
/// more of it can run larger ones, or several at once. Ollama speaks the same
/// HTTP API wherever it runs, so an address is all that is needed to use that
/// machine's models here — they appear in the model picker and are eligible
/// for the committee exactly like local ones.
#[derive(Default)]
struct RemoteHostsWidget;

impl SettingsWidget for RemoteHostsWidget {
    type View = OllamaPageView;

    fn search_terms(&self) -> &str {
        "ollama remote host lan network another machine server pc address ip"
    }

    fn should_render(&self, _app: &AppContext) -> bool {
        // Deliberately not gated on a local Ollama install: borrowing another
        // machine's models is exactly what someone without one would want.
        true
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let hosts = remote::configured(app);
        let mut column = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);

        column.add_child(section_break(appearance));
        column.add_child(
            Container::new(render_sub_header(appearance, "Other machines", None))
                .with_margin_top(VERTICAL_MARGIN)
                .finish(),
        );
        column.add_child(
            Container::new(body_text(
                "Use the models on another computer running Ollama. Give it the machine's \
                 address and Bork will ask what it has; those models then appear in the \
                 model picker, and can serve on the committee, like models installed here. \
                 Ollama only listens to its own machine by default, so set \
                 OLLAMA_HOST=0.0.0.0 there first.",
                appearance,
            ))
            .with_margin_top(8.)
            .finish(),
        );

        {
            let handles = view.state_handles.borrow();
            column.add_child(
                Container::new(
                    Flex::row()
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_child(
                            borkui::elements::Expanded::new(
                                1.,
                                ChildView::new(&view.remote_input).finish(),
                            )
                            .finish(),
                        )
                        .with_child(
                            Container::new(
                                appearance
                                    .ui_builder()
                                    .link(
                                        "Add".into(),
                                        None,
                                        Some(Box::new(|ctx| {
                                            ctx.dispatch_typed_action(
                                                OllamaPageAction::AddRemoteHost,
                                            );
                                        })),
                                        handles.add_remote_link.clone(),
                                    )
                                    .soft_wrap(false)
                                    .build()
                                    .finish(),
                            )
                            .with_margin_left(12.)
                            .finish(),
                        )
                        .finish(),
                )
                .with_margin_top(8.)
                .finish(),
            );
        }
        if let Some(error) = &view.remote_error {
            column.add_child(notice(error, ThemeFill::error(), appearance));
        }

        for host in &hosts {
            let mut handles = view.state_handles.borrow_mut();
            let check_handle = handles
                .remote_check_links
                .entry(host.url.clone())
                .or_default()
                .clone();
            let forget_handle = handles
                .remote_forget_links
                .entry(host.url.clone())
                .or_default()
                .clone();
            drop(handles);

            let checking = view.remote_checking.as_deref() == Some(host.url.as_str());
            let check_url = host.url.clone();
            let forget_url = host.url.clone();

            let mut row = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);
            row.add_child(
                borkui::elements::Expanded::new(1., body_text(&host.display(), appearance))
                    .finish(),
            );
            row.add_child(
                Container::new(
                    appearance
                        .ui_builder()
                        .link(
                            if checking { "Checking…" } else { "Check" }.into(),
                            None,
                            // Always clickable: a second click while one is
                            // in flight is ignored by the handler rather than
                            // by making this link's type conditional.
                            Some(Box::new(move |ctx| {
                                ctx.dispatch_typed_action(OllamaPageAction::CheckRemoteHost {
                                    url: check_url.clone(),
                                });
                            })),
                            check_handle,
                        )
                        .soft_wrap(false)
                        .build()
                        .finish(),
                )
                .with_margin_left(12.)
                .finish(),
            );
            row.add_child(
                Container::new(
                    appearance
                        .ui_builder()
                        .link(
                            "Forget".into(),
                            None,
                            Some(Box::new(move |ctx| {
                                ctx.dispatch_typed_action(OllamaPageAction::ForgetRemoteHost {
                                    url: forget_url.clone(),
                                });
                            })),
                            forget_handle,
                        )
                        .soft_wrap(false)
                        .build()
                        .finish(),
                )
                .with_margin_left(12.)
                .finish(),
            );
            column.add_child(Container::new(row.finish()).with_margin_top(10.).finish());

            if let Some(status) = view.remote_status.get(&host.url) {
                column.add_child(Container::new(sub_text(status, appearance)).finish());
            }
            if !host.models.is_empty() {
                column.add_child(
                    Container::new(sub_text(&host.models.join(", "), appearance))
                        .with_margin_top(4.)
                        .finish(),
                );
            }
        }

        column.finish()
    }
}

#[derive(Default)]
struct CommitteeWidget;

impl SettingsWidget for CommitteeWidget {
    type View = OllamaPageView;

    fn search_terms(&self) -> &str {
        "ollama committee multi model reviewers judge panel leaderboard multi-agent"
    }

    fn should_render(&self, _app: &AppContext) -> bool {
        service::is_installed()
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let config = OllamaCommitteeConfig::from_settings_json(
            AISettings::as_ref(app)
                .ollama_committee_config
                .value()
                .as_deref(),
        );

        let mut column = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        column.add_child(section_break(appearance));
        column.add_child(
            Container::new(render_sub_header(appearance, "Committee", None))
                .with_margin_top(VERTICAL_MARGIN)
                .finish(),
        );
        column.add_child(
            Container::new(body_text(
                "Fan one prompt across several installed models: reviewers examine your \
                 diff independently, machines verify what they can, a judge rules on the \
                 rest, and the verdict says which model won out. Models load one at a \
                 time, so a full run takes minutes — that's the point.",
                appearance,
            ))
            .with_margin_top(8.)
            .finish(),
        );

        // The pseudo-model only appears in the picker with 2+ real models
        // installed (one model has no one to argue with) — say which state
        // this machine is in rather than leaving the entry to be hunted for.
        let installed_count = view
            .installed
            .as_ref()
            .map(|models| {
                models
                    .iter()
                    .filter(|model| model.name != COMMITTEE_MODEL_NAME)
                    .count()
            })
            .unwrap_or(0);
        let hint = if installed_count < 2 {
            "The committee joins the model menu once at least two models are installed.".to_string()
        } else {
            format!("Pick \"{COMMITTEE_DISPLAY_NAME}\" in the model menu to run it.")
        };
        column.add_child(
            Container::new(sub_text(&hint, appearance))
                .with_margin_top(6.)
                .finish(),
        );
        // Casting lives on the model rows above, where the models are.
        let cast: Vec<String> = config
            .roles
            .iter()
            .map(|(tag, role)| format!("{tag}: {}", role.display_name()))
            .collect();
        let cast_line = if cast.is_empty() {
            "The bench is picked automatically. Use each model's \"Committee\" link in \
             Installed above to force one in as reviewer or judge, or keep a heavy model \
             out entirely."
                .to_string()
        } else {
            format!("Your casting — {}.", cast.join("; "))
        };
        column.add_child(
            Container::new(sub_text(&cast_line, appearance))
                .with_margin_top(6.)
                .finish(),
        );

        let handles = view.state_handles.borrow();
        column.add_child(
            Container::new(
                appearance
                    .ui_builder()
                    .link(
                        format!("Reviewers per run: {}", config.reviewers),
                        None,
                        Some(Box::new(|ctx| {
                            ctx.dispatch_typed_action(OllamaPageAction::CycleCommitteeReviewers);
                        })),
                        handles.committee_reviewers_link.clone(),
                    )
                    .soft_wrap(false)
                    .build()
                    .finish(),
            )
            .with_margin_top(VERTICAL_MARGIN)
            .finish(),
        );
        column.add_child(
            Container::new(sub_text(
                "How many models independently examine the evidence before the judge \
                 rules. More reviewers, more minutes.",
                appearance,
            ))
            .with_margin_top(2.)
            .finish(),
        );
        column.add_child(
            Container::new(
                appearance
                    .ui_builder()
                    .link(
                        format!("Models kept loaded: {}", config.max_loaded),
                        None,
                        Some(Box::new(|ctx| {
                            ctx.dispatch_typed_action(OllamaPageAction::CycleCommitteeMaxLoaded);
                        })),
                        handles.committee_max_loaded_link.clone(),
                    )
                    .soft_wrap(false)
                    .build()
                    .finish(),
            )
            .with_margin_top(12.)
            .finish(),
        );
        column.add_child(
            Container::new(sub_text(
                "At 1, each model unloads as its turn ends — right for a machine that \
                 fits one model. Raise it only if this machine has memory to keep \
                 several resident.",
                appearance,
            ))
            .with_margin_top(2.)
            .finish(),
        );
        // The review-mode stages: what the machine checks for itself, before
        // and after the models have their say.
        column.add_child(
            Container::new(
                appearance
                    .ui_builder()
                    .link(
                        evidence_label(&config).to_string(),
                        None,
                        Some(Box::new(|ctx| {
                            ctx.dispatch_typed_action(OllamaPageAction::CycleCommitteeEvidence);
                        })),
                        handles.committee_evidence_link.clone(),
                    )
                    .soft_wrap(false)
                    .build()
                    .finish(),
            )
            .with_margin_top(12.)
            .finish(),
        );
        column.add_child(
            Container::new(sub_text(
                "What a review runs before any model reads the diff. The test suite is \
                 this project's own (cargo, pytest, npm or go) and is the slow one on a \
                 large repo. The behavior probe calls the changed functions against a \
                 generic input battery and hands the models what actually happened — \
                 they can't run anything themselves, so with both off they see the diff \
                 alone.",
                appearance,
            ))
            .with_margin_top(2.)
            .finish(),
        );
        column.add_child(
            Container::new(
                appearance
                    .ui_builder()
                    .link(
                        proof_label(&config),
                        None,
                        Some(Box::new(|ctx| {
                            ctx.dispatch_typed_action(OllamaPageAction::CycleCommitteeProof);
                        })),
                        handles.committee_proof_link.clone(),
                    )
                    .soft_wrap(false)
                    .build()
                    .finish(),
            )
            .with_margin_top(12.)
            .finish(),
        );
        column.add_child(
            Container::new(sub_text(
                "After the reviewers agree, each finding goes to the interpreter: the \
                 model that raised it writes a script that exits 0 if the defect is \
                 real, and the harness runs it here. It costs one more model call per \
                 finding and it's what separates \"definite\" from \"might be\" — with \
                 the round off, nothing is executed and every finding stays an opinion. \
                 The time limit is per script; raise it if proofs time out on a slow \
                 import.",
                appearance,
            ))
            .with_margin_top(2.)
            .finish(),
        );
        column.add_child(
            Container::new(
                appearance
                    .ui_builder()
                    .link(
                        format!("Findings kept per reviewer: {}", config.findings_cap),
                        None,
                        Some(Box::new(|ctx| {
                            ctx.dispatch_typed_action(OllamaPageAction::CycleCommitteeFindingsCap);
                        })),
                        handles.committee_findings_cap_link.clone(),
                    )
                    .soft_wrap(false)
                    .build()
                    .finish(),
            )
            .with_margin_top(12.)
            .finish(),
        );
        column.add_child(
            Container::new(sub_text(
                "A small model that loses the plot repeats one finding hundreds of \
                 times, so anything past this count is dropped and the transcript says \
                 which model went bork. Repeats are always collapsed regardless — raise \
                 this only for a diff big enough to honestly hold that many distinct \
                 defects.",
                appearance,
            ))
            .with_margin_top(2.)
            .finish(),
        );
        column.add_child(
            Container::new(
                appearance
                    .ui_builder()
                    .link(
                        format!(
                            "Second opinion: {}",
                            if config.second_opinion { "on" } else { "off" }
                        ),
                        None,
                        Some(Box::new(|ctx| {
                            ctx.dispatch_typed_action(
                                OllamaPageAction::ToggleCommitteeSecondOpinion,
                            );
                        })),
                        handles.committee_second_opinion_link.clone(),
                    )
                    .soft_wrap(false)
                    .build()
                    .finish(),
            )
            .with_margin_top(12.)
            .finish(),
        );
        column.add_child(
            Container::new(sub_text(
                "Each reviewer reads the diff twice — once at temperature 0, once at \
                 0.8 — and what it reports both times is marked \"said twice\". A model \
                 asked once can invent a defect the sampler suggested; asked again down \
                 a different path, it mostly repeats what is actually in the code. \
                 Nothing is thrown away for being said once. This doubles the reviewing \
                 half of a run, so it is off by default. Reviews only — panel answers \
                 already differ, they come from different models.",
                appearance,
            ))
            .with_margin_top(2.)
            .finish(),
        );
        column.add_child(
            Container::new(
                appearance
                    .ui_builder()
                    .link(
                        format!("Audit pass: {}", if config.audit { "on" } else { "off" }),
                        None,
                        Some(Box::new(|ctx| {
                            ctx.dispatch_typed_action(OllamaPageAction::ToggleCommitteeAudit);
                        })),
                        handles.committee_audit_link.clone(),
                    )
                    .soft_wrap(false)
                    .build()
                    .finish(),
            )
            .with_margin_top(12.)
            .finish(),
        );
        column.add_child(
            Container::new(sub_text(
                "When the verdict is in, a model that sat the run out reads the diff and \
                 the committee's conclusions cold, and says which rulings look wrong and \
                 what nobody reported. Everything before it was written by models arguing \
                 with each other; this is the first opinion in the run with nothing \
                 invested in it. One extra call — and if every model was seated, the judge \
                 checks its own work and the run says so.",
                appearance,
            ))
            .with_margin_top(2.)
            .finish(),
        );
        column.add_child(
            Container::new(
                appearance
                    .ui_builder()
                    .link(
                        format!(
                            "Committee is introduced by: {}",
                            if config.personas { "name" } else { "model tag" }
                        ),
                        None,
                        Some(Box::new(|ctx| {
                            ctx.dispatch_typed_action(OllamaPageAction::ToggleCommitteePersonas);
                        })),
                        handles.committee_personas_link.clone(),
                    )
                    .soft_wrap(false)
                    .build()
                    .finish(),
            )
            .with_margin_top(12.)
            .finish(),
        );
        column.add_child(
            Container::new(sub_text(
                "A model tag is a serial number; a committee is a room full of people \
                 arguing, and reads like one. Each model draws the same name every time, \
                 and the bench block prints both, so you always know which tag is which \
                 person. Names are display only — settings, prompts and standings still \
                 use the tag.",
                appearance,
            ))
            .with_margin_top(2.)
            .finish(),
        );
        for slot in 0..MAX_PANEL_EXTRA_ROUNDS {
            let label = match config.panel_plan.get(slot) {
                Some(kind) => format!("Round {}: {}", slot + 2, kind.display_name()),
                None => format!("Round {}: Off", slot + 2),
            };
            column.add_child(
                Container::new(
                    appearance
                        .ui_builder()
                        .link(
                            label,
                            None,
                            Some(Box::new(move |ctx| {
                                ctx.dispatch_typed_action(
                                    OllamaPageAction::CycleCommitteePanelRound { slot },
                                );
                            })),
                            handles.committee_round_links[slot].clone(),
                        )
                        .soft_wrap(false)
                        .build()
                        .finish(),
                )
                .with_margin_top(12.)
                .finish(),
            );
        }
        column.add_child(
            Container::new(sub_text(
                "Extra panel rounds before the final verdict, in order. Judge notes: \
                 everyone revises with the judge's right/wrong notes. Judge's rewrite: \
                 the judge rewrites the best answer itself and everyone responds to it. \
                 Peer critique: panelists critique each other round-robin. Converge: \
                 everyone builds on the current leader's answer. Each round is a full \
                 extra pass.",
                appearance,
            ))
            .with_margin_top(2.)
            .finish(),
        );

        let standings = ollama_committee::leaderboard_lines(&view.committee_leaderboard);
        if !standings.is_empty() {
            column.add_child(
                Container::new(body_text("Standings on this machine:", appearance))
                    .with_margin_top(VERTICAL_MARGIN)
                    .finish(),
            );
            for line in standings.iter().take(6) {
                column.add_child(
                    Container::new(sub_text(line, appearance))
                        .with_margin_top(2.)
                        .finish(),
                );
            }
        }

        Container::new(column.finish())
            .with_margin_top(VERTICAL_MARGIN)
            .finish()
    }
}

fn render_catalog_entry(
    view: &OllamaPageView,
    entry: &CatalogEntry,
    handles: &StateHandles,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let mut card = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);

    card.add_child(
        Text::new(
            entry.name.clone(),
            appearance.ui_font_family(),
            appearance.ui_font_size(),
        )
        .with_style(borkui::fonts::Properties::default().weight(Weight::Medium))
        .finish(),
    );
    card.add_child(sub_text(&entry.description, appearance));

    for size in &entry.sizes {
        let tag = entry.tag_for(size);
        let verdict = view.verdict(size);
        let already = view.is_installed_tag(&tag);
        let state = view.installs.get(&tag);

        let mut row = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);
        // Size column.
        row.add_child(
            ConstrainedBox::new(
                Text::new(
                    size.params.to_string(),
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .finish(),
            )
            .with_width(SIZE_COL_WIDTH)
            .finish(),
        );

        // Status / estimated-RAM column, fixed-width so the Install links
        // line up down the table. While a pull is running with a known byte
        // count, this cell is a live progress bar with a percentage;
        // otherwise it's a short status string.
        let status_element: Box<dyn Element> = match (already, state) {
            (
                false,
                Some(InstallState::Running {
                    fraction: Some(fraction),
                    ..
                }),
            ) => install_progress(*fraction, appearance),
            _ => {
                let status = match (already, state) {
                    (true, _) => "Installed".to_string(),
                    // Byteless phase: surface Ollama's own label (e.g.
                    // "pulling manifest", "verifying sha256 digest") so the
                    // cell shows real activity before the download's %
                    // appears. Falls back to a generic verb when empty.
                    (_, Some(InstallState::Running { phase, .. })) if !phase.is_empty() => {
                        format!("{}…", capitalize_first(phase))
                    }
                    (_, Some(InstallState::Running { .. })) => "Installing…".to_string(),
                    (_, Some(InstallState::Done)) => "Installed".to_string(),
                    (_, Some(InstallState::Failed(reason))) => format!("Failed: {reason}"),
                    _ => verdict
                        .as_ref()
                        .map(|v| capability::format_gb(v.required_bytes))
                        .unwrap_or_default(),
                };
                sub_text(&status, appearance)
            }
        };
        row.add_child(
            ConstrainedBox::new(status_element)
                .with_width(STATUS_COL_WIDTH)
                .finish(),
        );

        if !already
            && !matches!(
                state,
                Some(InstallState::Running { .. } | InstallState::Done)
            )
        {
            let action_tag = tag.clone();
            let mouse_state = handles
                .install_buttons
                .get(&tag)
                .cloned()
                .unwrap_or_default();
            row.add_child(
                appearance
                    .ui_builder()
                    .link(
                        "Install".into(),
                        None,
                        Some(Box::new(move |ctx| {
                            ctx.dispatch_typed_action(OllamaPageAction::Install {
                                tag: action_tag.clone(),
                            });
                        })),
                        mouse_state,
                    )
                    .soft_wrap(false)
                    .build()
                    .finish(),
            );
        }

        // The fit warning, once a terse badge to the side of the row rather
        // than a full sentence beneath it (the legend above carries the
        // meaning). Nothing for a comfortable fit.
        if let Some(badge) = verdict
            .as_ref()
            .filter(|v| v.fit.is_warning())
            .and_then(|v| fit_badge(v.fit, appearance))
        {
            row.add_child(Container::new(badge).with_margin_left(12.).finish());
        }

        card.add_child(Container::new(row.finish()).with_margin_top(4.).finish());
    }

    Container::new(card.finish())
        .with_margin_top(VERTICAL_MARGIN)
        .with_padding_bottom(8.)
        .with_border(Border::bottom(1.).with_border_fill(theme.outline()))
        .finish()
}

// ── Small helpers ────────────────────────────────────────────────────────

fn body_text(text: &str, appearance: &Appearance) -> Box<dyn Element> {
    Text::new(
        text.to_string(),
        appearance.ui_font_family(),
        appearance.ui_font_size(),
    )
    .finish()
}

fn sub_text(text: &str, appearance: &Appearance) -> Box<dyn Element> {
    let theme = appearance.theme();
    Text::new(
        text.to_string(),
        appearance.ui_font_family(),
        appearance.ui_font_size(),
    )
    .with_color(theme.sub_text_color(theme.background()).into_solid())
    .finish()
}

/// A determinate progress bar of the given track width, followed by a
/// right-hand percentage label. Shared by model installs and folder moves so
/// both read the same. `fraction` is clamped to 0.0..=1.0.
fn progress_row(fraction: f32, width: f32, appearance: &Appearance) -> Box<dyn Element> {
    let fraction = fraction.clamp(0.0, 1.0);
    let theme = appearance.theme();
    let bar = appearance
        .ui_builder()
        .custom_progress_bar(
            fraction,
            UiComponentStyles {
                // A visible track (outline) behind the accent-filled portion,
                // rather than the page background, so the empty part still
                // reads as a bar. `.into()` bridges bork_core's theme `Fill`
                // to borkui's element `Fill`.
                background: Some(theme.outline().into()),
                foreground: Some(theme.accent().into()),
                width: Some(width),
                height: Some(4.),
                ..Default::default()
            },
        )
        .build()
        .finish();
    Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_child(bar)
        .with_child(
            Container::new(sub_text(
                &format!("{}%", (fraction * 100.0).round() as u32),
                appearance,
            ))
            .with_margin_left(8.)
            .finish(),
        )
        .finish()
}

/// Progress cell for an in-flight model install, sized to sit in the
/// catalog's status column.
fn install_progress(fraction: f32, appearance: &Appearance) -> Box<dyn Element> {
    progress_row(fraction, 72., appearance)
}

/// A one-line role + capability summary for an installed model, reflecting
/// any override. Examples: "Agent — runs tools · 128K context",
/// "Chat only · 2K context", "Agent (forced) · 16K context".
fn role_summary(caps: Option<&service::ModelCapabilities>, override_mode: Option<bool>) -> String {
    let native_tools = caps.is_some_and(|caps| caps.tools);
    let mut summary = match override_mode {
        Some(true) => "Agent (forced)".to_string(),
        Some(false) => "Chat (forced)".to_string(),
        None if native_tools => "Agent — runs tools".to_string(),
        None => "Chat only".to_string(),
    };
    if let Some(context) = caps.and_then(|caps| caps.context_length) {
        summary.push_str(&format!(" · {} context", format_context(context)));
    }
    summary
}

/// Label for the mode-override cycle link, showing where a click will go
/// next (Auto → Agent → Chat → Auto).
fn override_link_label(override_mode: Option<bool>) -> &'static str {
    match override_mode {
        None => "[auto — force agent?]",
        Some(true) => "[forced agent — force chat?]",
        Some(false) => "[forced chat — back to auto?]",
    }
}

/// Formats a token context window compactly: 131072 → "128K", 2048 → "2K".
fn format_context(tokens: u64) -> String {
    if tokens >= 1024 {
        format!("{}K", tokens / 1024)
    } else {
        tokens.to_string()
    }
}

/// Uppercases the first character of `text`, for turning Ollama's lowercase
/// phase labels ("pulling manifest") into sentence case.
fn capitalize_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// A divider between page sections, with room above and below so the line
/// doesn't crowd the text and buttons on either side.
fn section_break(appearance: &Appearance) -> Box<dyn Element> {
    Container::new(render_separator(appearance))
        .with_margin_top(VERTICAL_MARGIN)
        .finish()
}

/// "MODEL FOLDER: <path>  Reveal" — where Ollama keeps its models, with a
/// button that opens the folder. Sits at the top-right of the Installed
/// section so the location is visible and reachable at a glance.
fn model_folder_header(view: &OllamaPageView, appearance: &Appearance) -> Box<dyn Element> {
    let path = service::models_dir();
    let is_default = path == service::default_models_dir();
    let (reveal, change, reset) = {
        let handles = view.state_handles.borrow();
        (
            handles.reveal_folder_link.clone(),
            handles.change_folder_link.clone(),
            handles.reset_folder_link.clone(),
        )
    };

    let folder_link =
        |label: &'static str, action: OllamaPageAction, mouse_state: MouseStateHandle| {
            Container::new(
                appearance
                    .ui_builder()
                    .link(
                        label.into(),
                        None,
                        Some(Box::new(move |ctx| {
                            ctx.dispatch_typed_action(action.clone());
                        })),
                        mouse_state,
                    )
                    .soft_wrap(false)
                    .build()
                    .finish(),
            )
            .with_margin_left(12.)
            .finish()
        };

    let mut row = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);
    row.add_child(sub_text(
        &format!("MODEL FOLDER: {}", path.display()),
        appearance,
    ));
    row.add_child(folder_link(
        "Reveal",
        OllamaPageAction::RevealModelFolder,
        reveal,
    ));
    row.add_child(folder_link(
        if view.migrating_models {
            "Moving…"
        } else {
            "Change…"
        },
        OllamaPageAction::ChangeModelsFolder,
        change,
    ));
    // Only worth offering once there's actually somewhere else to come back
    // from — a fresh install with no override has nothing to reset.
    if !is_default {
        row.add_child(folder_link(
            if view.migrating_models {
                "Moving…"
            } else {
                "Reset to default"
            },
            OllamaPageAction::ResetModelsFolder,
            reset,
        ));
    }
    row.finish()
}

/// A compact fit badge — STARVED / WON'T FIT — coloured by severity, or
/// nothing for a comfortable fit. Rendered inline beside a model in place of
/// the old full-sentence warning; the legend above the list explains it.
fn fit_badge(fit: Fit, appearance: &Appearance) -> Option<Box<dyn Element>> {
    let label = fit.badge()?;
    let color: ThemeFill = match fit {
        Fit::Exceeds => ThemeFill::error(),
        _ => ThemeFill::warn(),
    };
    Some(
        Container::new(
            Text::new(label.to_string(), appearance.ui_font_family(), 10.)
                .with_color(color.into_solid())
                .with_style(borkui::fonts::Properties::default().weight(Weight::Bold))
                .finish(),
        )
        .with_horizontal_padding(6.)
        .with_vertical_padding(2.)
        .with_corner_radius(borkui::elements::CornerRadius::with_all(
            borkui::elements::Radius::Pixels(4.),
        ))
        .with_border(borkui::elements::Border::all(1.).with_border_fill(color))
        .finish(),
    )
}

/// The two-line key for the fit badges, shown once above the model list so the
/// inline badges can stay terse.
fn fit_legend(appearance: &Appearance) -> Box<dyn Element> {
    let mut column = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Start);
    for fit in [Fit::Tight, Fit::Exceeds] {
        let (Some(badge), Some(meaning)) = (fit_badge(fit, appearance), fit.badge_meaning()) else {
            continue;
        };
        column.add_child(
            Container::new(
                Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(badge)
                    .with_child(
                        Container::new(sub_text(meaning, appearance))
                            .with_margin_left(8.)
                            .finish(),
                    )
                    .finish(),
            )
            .with_margin_top(4.)
            .finish(),
        );
    }
    Container::new(column.finish())
        .with_margin_top(6.)
        .with_margin_bottom(4.)
        .finish()
}

fn notice(text: &str, color: ThemeFill, appearance: &Appearance) -> Box<dyn Element> {
    Container::new(
        Text::new(
            text.to_string(),
            appearance.ui_font_family(),
            appearance.ui_font_size(),
        )
        .with_color(color.into_solid())
        .finish(),
    )
    .with_margin_top(4.)
    .finish()
}

impl SettingsPageMeta for OllamaPageView {
    fn section() -> SettingsSection {
        SettingsSection::Ollama
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        true
    }

    fn on_page_selected(&mut self, _: bool, ctx: &mut ViewContext<Self>) {
        // The machine may have changed since the page was built — a model
        // pulled from the terminal, or Ollama installed in the meantime.
        self.refresh(ctx);
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<OllamaPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<OllamaPageView>) -> Self {
        SettingsPageViewHandle::Ollama(view_handle)
    }
}

#[allow(unused)]
fn _unused(_: Empty) {}
