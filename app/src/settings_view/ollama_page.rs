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
use warp_errors::report_if_error;
use warpui::elements::{
    Border, Container, CrossAxisAlignment, Element, Empty, Flex, MouseStateHandle, ParentElement,
    Text,
};
use warpui::fonts::Weight;
use crate::editor::{EditorView, SingleLineEditorOptions};
use warpui::elements::ChildView;
use warpui::ui_components::components::UiComponent;
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
use warp_core::ui::theme::Fill as ThemeFill;
use crate::settings::ai::AISettings;

const VERTICAL_MARGIN: f32 = 16.;
const OLLAMA_DOWNLOAD_URL: &str = "https://ollama.com/download";

/// How many catalog entries to show before requiring a search. The live
/// library is 230+ models; a wall of them is not a browsing experience.
const VISIBLE_CATALOG_LIMIT: usize = 12;

#[derive(Debug, Clone)]
pub enum OllamaPageAction {
    ToggleEnabled,
    RefreshCatalog,
    OpenOllamaDownload,
    Install { tag: String },
    /// Install exactly what's in the free-text field.
    InstallTyped,
}

/// Progress of a pull the user started.
#[derive(Debug, Clone, PartialEq)]
pub enum InstallState {
    Running,
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
    state_handles: RefCell<StateHandles>,
}

#[derive(Default)]
struct StateHandles {
    enabled_toggle: SwitchStateHandle,
    refresh_link: MouseStateHandle,
    install_typed_link: MouseStateHandle,
    download_link: MouseStateHandle,
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

        let mut view = Self {
            page: PageType::new_uncategorized(widgets, Some("Ollama")),
            model_input,
            input_error: None,
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

    /// Starts `ollama pull <tag>`.
    fn install(&mut self, tag: String, ctx: &mut ViewContext<Self>) {
        if matches!(self.installs.get(&tag), Some(InstallState::Running)) {
            return;
        }
        let Some(binary) = service::resolve_binary() else {
            self.installs.insert(
                tag,
                InstallState::Failed("Ollama isn't installed".to_string()),
            );
            ctx.notify();
            return;
        };

        self.installs.insert(tag.clone(), InstallState::Running);
        ctx.notify();

        let pull_tag = tag.clone();
        ctx.spawn(
            async move {
                // `ollama pull` streams progress to stderr; we only surface
                // the outcome. Anything richer means parsing a progress format
                // Ollama does not commit to.
                let result = command::r#async::Command::new(binary)
                    .arg("pull")
                    .arg(&pull_tag)
                    .output()
                    .await;

                let state = match result {
                    Ok(output) if output.status.success() => InstallState::Done,
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let reason = stderr
                            .lines()
                            .rev()
                            .find(|line| !line.trim().is_empty())
                            .unwrap_or("pull failed")
                            .trim()
                            .to_string();
                        InstallState::Failed(reason)
                    }
                    Err(err) => InstallState::Failed(err.to_string()),
                };
                (pull_tag, state)
            },
            |me, (pull_tag, state), ctx| {
                let finished = state == InstallState::Done;
                me.installs.insert(pull_tag, state);
                ctx.notify();
                // A completed pull changes what's on disk.
                if finished {
                    me.refresh(ctx);
                }
            },
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
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.ollama_enabled.toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            OllamaPageAction::RefreshCatalog => self.refresh(ctx),
            OllamaPageAction::OpenOllamaDownload => ctx.open_url(OLLAMA_DOWNLOAD_URL),
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
            let status = if view.service_running {
                "Ollama is installed and running."
            } else {
                "Ollama is installed but not currently running. It starts automatically \
                 when a model is used."
            };
            column.add_child(Container::new(body_text(status, appearance)).with_margin_top(8.).finish());

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
        column.add_child(render_separator(appearance));
        column.add_child(
            Container::new(render_sub_header(appearance, "Installed", None))
                .with_margin_top(VERTICAL_MARGIN)
                .finish(),
        );

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
                    column.add_child(
                        Container::new(
                            Flex::row()
                                .with_child(
                                    Text::new(
                                        model.name.clone(),
                                        appearance.ui_font_family(),
                                        appearance.ui_font_size(),
                                    )
                                    .with_style(
                                        warpui::fonts::Properties::default().weight(Weight::Medium),
                                    )
                                    .finish(),
                                )
                                .with_child(
                                    Container::new(sub_text(
                                        &capability::format_gb(model.size_bytes),
                                        appearance,
                                    ))
                                    .with_margin_left(12.)
                                    .finish(),
                                )
                                .finish(),
                        )
                        .with_margin_top(6.)
                        .finish(),
                    );
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

        column.add_child(render_separator(appearance));
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

        if let Some(note) = view.catalog_source.note() {
            column.add_child(notice(note, ThemeFill::error(), appearance));
        }

        if view.catalog_loading {
            column.add_child(body_text("Loading models from ollama.com…", appearance));
            return Container::new(column.finish()).finish();
        }

        if let Some(total) = view.total_memory_bytes {
            column.add_child(sub_text(
                &format!(
                    "This machine has {}. Sizes below are estimates — they're a warning, \
                     not a limit.",
                    capability::format_gb(total)
                ),
                appearance,
            ));
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
        row.add_child(
            Text::new(
                size.params.to_string(),
                appearance.ui_font_family(),
                appearance.ui_font_size(),
            )
            .finish(),
        );

        let status = match (already, state) {
            (true, _) => "Installed".to_string(),
            (_, Some(InstallState::Running)) => "Downloading…".to_string(),
            (_, Some(InstallState::Done)) => "Installed".to_string(),
            (_, Some(InstallState::Failed(reason))) => format!("Failed: {reason}"),
            _ => verdict
                .as_ref()
                .map(|v| capability::format_gb(v.required_bytes))
                .unwrap_or_default(),
        };
        row.add_child(
            Container::new(sub_text(&status, appearance))
                .with_margin_left(12.)
                .finish(),
        );

        if !already && !matches!(state, Some(InstallState::Running | InstallState::Done)) {
            let action_tag = tag.clone();
            let mouse_state = handles
                .install_buttons
                .get(&tag)
                .cloned()
                .unwrap_or_default();
            row.add_child(
                Container::new(
                    appearance
                        .ui_builder()
                        .link(
                            "Download".into(),
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
                )
                .with_margin_left(12.)
                .finish(),
            );
        }

        card.add_child(Container::new(row.finish()).with_margin_top(4.).finish());

        // The whole point of the page: say plainly when something won't fit.
        if let Some(verdict) = verdict.filter(|v| v.fit.is_warning()) {
            let color = match verdict.fit {
                Fit::Exceeds => ThemeFill::error(),
                _ => ThemeFill::warn(),
            };
            card.add_child(
                Container::new(notice(&verdict.message, color, appearance))
                    .with_margin_left(12.)
                    .finish(),
            );
        }
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
