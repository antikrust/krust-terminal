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
use warp_core::ui::theme::Fill as ThemeFill;
use warp_errors::report_if_error;
use warpui::elements::{
    Border, ChildView, ConstrainedBox, Container, CrossAxisAlignment, Element, Empty, Flex,
    MouseStateHandle, ParentElement, Shrinkable, Text,
};
use warpui::fonts::Weight;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::ui_components::switch::SwitchStateHandle;
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use super::settings_page::{
    render_separator, render_sub_header, MatchData, PageType, SettingsPageMeta,
    SettingsPageViewHandle, SettingsWidget,
};
use super::SettingsSection;
use crate::ai::ollama::capability::{self, Fit, MemoryVerdict};
use crate::ai::ollama::catalog::{CatalogEntry, ModelSize};
use crate::ai::ollama::service::{self, CatalogSource, InstalledModel};
use crate::appearance::Appearance;
use crate::editor::{EditorView, SingleLineEditorOptions};
use crate::settings::ai::AISettings;

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
    Install {
        tag: String,
    },
    /// Install exactly what's in the free-text field.
    InstallTyped,
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
    /// Set while a chosen folder's models are being copied to their new
    /// home. The "Change" link is disabled meanwhile — starting a second
    /// move before the first finishes would race on the same files.
    migrating_models: bool,
    /// Fraction (0.0..=1.0) of the in-progress move, for its progress bar.
    migration_progress: Option<f32>,
    /// Set if the last move failed, so the page can say why instead of
    /// silently reverting to the old folder.
    migration_error: Option<String>,
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
}

impl OllamaPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let widgets: Vec<Box<dyn SettingsWidget<View = Self>>> = vec![
            Box::new(StatusWidget),
            Box::new(InstalledWidget),
            Box::new(CatalogWidget),
        ];

        let model_input = ctx.add_typed_action_view(|ctx| {
            let mut editor = EditorView::single_line(SingleLineEditorOptions::default(), ctx);
            editor.set_placeholder_text("Install by name, e.g. qwen2.5-coder:14b", ctx);
            editor
        });

        // Picks up the persisted custom folder (if any) so `models_dir()`
        // reflects it from the moment this page is open, not just after the
        // user next changes it.
        service::apply_models_dir_override(ctx);

        let mut view = Self {
            page: PageType::new_uncategorized(widgets, Some("Ollama")),
            model_input,
            input_error: None,
            migrating_models: false,
            migration_progress: None,
            migration_error: None,
            installed: None,
            service_running: false,
            catalog: Vec::new(),
            catalog_source: CatalogSource::Live,
            catalog_loading: false,
            installs: HashMap::new(),
            total_memory_bytes: capability::total_system_memory_bytes(),
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
            warpui::platform::FilePickerConfiguration::new().folders_only(),
        );
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
                        // user's own shell set it independently of Warp) —
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
        ctx.notify();

        let client = reqwest::Client::new();
        ctx.spawn(
            async move {
                let running = service::is_running(&client).await;
                let installed = service::list_installed(&client).await.ok();
                let (catalog, source) = service::fetch_catalog(&client).await;
                (running, installed, catalog, source)
            },
            |me, (running, installed, catalog, source), ctx| {
                me.service_running = running;
                me.installed = installed;
                me.catalog = catalog;
                me.catalog_source = source;
                me.catalog_loading = false;
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
                self.install(tag, ctx);
            }
            Err(err) => {
                self.input_error = Some(err.to_string());
                ctx.notify();
            }
        }
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
            OllamaPageAction::Install { tag } => self.install(tag.clone(), ctx),
            OllamaPageAction::InstallTyped => self.install_typed(ctx),
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
        _app: &AppContext,
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
                    .with_child(warpui::elements::Expanded::new(1., Empty::new().finish()).finish())
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
                        .with_style(warpui::fonts::Properties::default().weight(Weight::Medium))
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
                            warpui::elements::Expanded::new(
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
        .with_style(warpui::fonts::Properties::default().weight(Weight::Medium))
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
                // reads as a bar. `.into()` bridges warp_core's theme `Fill`
                // to warpui's element `Fill`.
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
                .with_style(warpui::fonts::Properties::default().weight(Weight::Bold))
                .finish(),
        )
        .with_horizontal_padding(6.)
        .with_vertical_padding(2.)
        .with_corner_radius(warpui::elements::CornerRadius::with_all(
            warpui::elements::Radius::Pixels(4.),
        ))
        .with_border(warpui::elements::Border::all(1.).with_border_fill(color))
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
