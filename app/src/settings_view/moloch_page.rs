//! What your Moloch is doing.
//!
//! Moloch is the app server: it serves the AI-manager functions (next-command
//! ghost text, prompt suggestions, natural-language completion) and brokers
//! agent runs. All of that happens out of sight, which makes "is it working?"
//! surprisingly hard to answer — previously it meant a shell on whichever
//! machine the server runs on.
//!
//! This page asks the server itself. `/api/v1/status` says where its
//! intelligence comes from, and `/api/v1/logs` returns the tail of its log,
//! so the answer arrives the same way whether Moloch is on this machine or
//! another one.

use std::cell::RefCell;

use krustui::elements::{
    ConstrainedBox, Container, CrossAxisAlignment, Flex, MouseStateHandle, ParentElement, Text,
};
use krustui::ui_components::button::ButtonVariant;
use krustui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use krustui::ui_components::switch::SwitchStateHandle;
use krustui::{
    AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};

use crate::editor::{
    EditorView, Event as EditorEvent, SingleLineEditorOptions, TextColors, TextOptions,
};

use super::settings_page::{
    render_separator, render_sub_header, MatchData, PageTitleWidget, PageType, SettingsPageMeta,
    SettingsPageViewHandle, SettingsWidget,
};
use super::SettingsSection;
use crate::appearance::Appearance;

const VERTICAL_MARGIN: f32 = 16.;
const HEADER_PADDING: f32 = 8.;

/// How the last attempt to reach the server went.
#[derive(Debug, Clone, PartialEq)]
enum Reachability {
    /// Not asked yet this session.
    Unknown,
    Checking,
    /// Answered, with what it said about itself.
    Up {
        gateway: Option<String>,
        mode: String,
        version: String,
        queued: i64,
        running: i64,
    },
    /// Did not answer, with something worth showing.
    Down(String),
}

/// One agent run as shown in the in-app list.
#[derive(Clone)]
struct RunRow {
    id: String,
    title: String,
    state: String,
}

/// One cell of the vmlab eval leaderboard: how a model did on one task.
#[derive(Clone)]
struct LeaderCell {
    task: String,
    model: String,
    passed: i64,
    total: i64,
    median_seconds: Option<i64>,
}

pub struct MolochPageView {
    page: PageType<Self>,
    reachability: Reachability,
    /// Agent runs this Moloch is brokering, newest first.
    runs: Vec<RunRow>,
    /// The vmlab eval leaderboard, one cell per (task, model).
    leaderboard: Vec<LeaderCell>,
    /// Tail of the server's log, most recent last.
    log_lines: Vec<String>,
    /// The connection as saved in `moloch.env`: the "Use Moloch" switch, the address and token
    /// fields, and what saving last said.
    use_moloch: bool,
    url_editor: ViewHandle<EditorView>,
    token_editor: ViewHandle<EditorView>,
    save_message: Option<String>,
    state_handles: RefCell<StateHandles>,
}

#[derive(Default)]
struct StateHandles {
    use_switch: SwitchStateHandle,
    save_button: MouseStateHandle,
    refresh_link: MouseStateHandle,
    /// Beside Refresh: Moloch's web panel in the browser, and the Moloch app's window.
    web_link: MouseStateHandle,
    app_link: MouseStateHandle,
    /// One per run row, for its Stop link; resized when the run list changes.
    stop_links: Vec<MouseStateHandle>,
}

/// Where this page asks, and with what token: what is saved now, while Moloch is switched on — so
/// Save, then Refresh, tests a new address or token before Krust restarts onto it.
fn target() -> Option<(String, String)> {
    let saved = crate::moloch::saved();
    let url = saved.url.trim().trim_end_matches('/').to_owned();
    (saved.enabled && !url.is_empty()).then(|| (url, saved.token.trim().to_owned()))
}

/// The connection: the "Use Moloch" switch, the address and token fields, Save, and what saving
/// last said (or that a restart is due).
fn render_connection(view: &MolochPageView, appearance: &Appearance) -> Box<dyn Element> {
    let theme = appearance.theme();
    let handles = view.state_handles.borrow();
    let input = |editor: &ViewHandle<EditorView>| {
        ConstrainedBox::new(
            appearance
                .ui_builder()
                .text_input(editor.clone())
                .with_style(UiComponentStyles {
                    padding: Some(Coords {
                        top: 6.,
                        bottom: 6.,
                        left: 10.,
                        right: 10.,
                    }),
                    background: Some(theme.surface_2().into()),
                    ..Default::default()
                })
                .build()
                .finish(),
        )
        .with_width(360.)
        .finish()
    };
    let labelled = |label: &str, field: Box<dyn Element>| {
        Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                ConstrainedBox::new(
                    Text::new_inline(label.to_owned(), appearance.ui_font_family(), 13.)
                        .with_color(theme.active_ui_text_color().into())
                        .finish(),
                )
                .with_width(90.)
                .finish(),
            )
            .with_child(field)
            .finish()
    };
    let switch = appearance
        .ui_builder()
        .switch(handles.use_switch.clone())
        .check(view.use_moloch)
        .build()
        .on_click(|ctx, _, _| ctx.dispatch_typed_action(MolochPageAction::ToggleUseMoloch))
        .finish();
    let mut column = Flex::column()
        .with_spacing(8.)
        .with_child(labelled("Use Moloch", switch))
        .with_child(labelled("Address", input(&view.url_editor)))
        .with_child(labelled("Token", input(&view.token_editor)))
        .with_child(
            Container::new(
                appearance
                    .ui_builder()
                    .button(ButtonVariant::Secondary, handles.save_button.clone())
                    .with_text_label("Save".to_owned())
                    .build()
                    .on_click(|ctx, _, _| {
                        ctx.dispatch_typed_action(MolochPageAction::SaveConnection)
                    })
                    .finish(),
            )
            .with_margin_left(90.)
            .finish(),
        );
    let note = view.save_message.clone().or_else(|| {
        crate::moloch::restart_needed().then(|| {
            "What's saved differs from what Krust started with — it switches when it next starts."
                .to_owned()
        })
    });
    if let Some(note) = note {
        column.add_child(
            Container::new(sub_text(&note, appearance))
                .with_margin_left(90.)
                .finish(),
        );
    }
    Container::new(column.finish())
        .with_margin_bottom(VERTICAL_MARGIN)
        .finish()
}

/// The run states worth offering a Stop control for — the ones still going.
fn is_active(state: &str) -> bool {
    matches!(
        state.to_ascii_uppercase().as_str(),
        "QUEUED" | "RUNNING" | "CLAIMED"
    )
}

impl MolochPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let widgets: Vec<Box<dyn SettingsWidget<View = Self>>> = vec![
            Box::new(PageTitleWidget::new("Moloch", true)),
            Box::new(ServerWidget),
            Box::new(RunsWidget),
            Box::new(LeaderboardWidget),
            Box::new(LogWidget),
        ];
        let saved = crate::moloch::saved();
        let url_editor = Self::field("http://host:10102", &saved.url, false, ctx);
        let token_editor = Self::field("Token or API key (wk-…)", &saved.token, true, ctx);
        let mut view = Self {
            page: PageType::new_uncategorized(widgets, None),
            reachability: Reachability::Unknown,
            runs: Vec::new(),
            leaderboard: Vec::new(),
            log_lines: Vec::new(),
            use_moloch: saved.enabled,
            url_editor,
            token_editor,
            save_message: None,
            state_handles: RefCell::new(StateHandles::default()),
        };
        view.refresh(ctx);
        view
    }

    /// A single-line field of the connection; Enter saves.
    fn field(
        placeholder: &'static str,
        text: &str,
        is_password: bool,
        ctx: &mut ViewContext<Self>,
    ) -> ViewHandle<EditorView> {
        let text = text.to_owned();
        let editor = ctx.add_typed_action_view(move |ctx| {
            let appearance = Appearance::handle(ctx).as_ref(ctx);
            let options = SingleLineEditorOptions {
                is_password,
                text: TextOptions {
                    font_size_override: Some(appearance.ui_font_size()),
                    text_colors_override: Some(TextColors {
                        default_color: appearance.theme().active_ui_text_color(),
                        disabled_color: appearance.theme().disabled_ui_text_color(),
                        hint_color: appearance.theme().disabled_ui_text_color(),
                    }),
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut editor = EditorView::single_line(options, ctx);
            editor.set_placeholder_text(placeholder, ctx);
            if !text.is_empty() {
                editor.set_buffer_text(&text, ctx);
            }
            editor
        });
        ctx.subscribe_to_view(&editor, |me, _, event, ctx| {
            if matches!(event, EditorEvent::Enter) {
                me.save_connection(ctx);
            }
        });
        editor
    }

    /// Saves the switch, address and token to `moloch.env`, then asks the server again with them.
    fn save_connection(&mut self, ctx: &mut ViewContext<Self>) {
        let settings = crate::moloch::SavedSettings {
            enabled: self.use_moloch,
            url: self.url_editor.as_ref(ctx).buffer_text(ctx),
            token: self.token_editor.as_ref(ctx).buffer_text(ctx),
        };
        self.save_message = Some(match crate::moloch::save(&settings) {
            Ok(()) if crate::moloch::overridden_by_environment() => {
                "Saved, but MOLOCH_* environment variables are set and win over it.".to_owned()
            }
            Ok(()) if crate::moloch::restart_needed() => {
                "Saved. Krust switches to it when it next starts; Refresh tests it now.".to_owned()
            }
            Ok(()) => "Saved.".to_owned(),
            Err(error) => format!("Couldn't save: {error}"),
        });
        self.refresh(ctx);
    }

    /// Asks the server what it is doing and for its recent log. Both are one
    /// request each and cheap; a server that does not answer leaves the page
    /// saying so rather than showing stale numbers as if they were current.
    fn refresh(&mut self, ctx: &mut ViewContext<Self>) {
        let Some((base, token)) = target() else {
            self.reachability = Reachability::Down(
                if self.use_moloch {
                    "No Moloch address is saved — enter it above."
                } else {
                    "Moloch is switched off."
                }
                .to_owned(),
            );
            self.log_lines.clear();
            ctx.notify();
            return;
        };
        self.reachability = Reachability::Checking;
        ctx.notify();
        let client = reqwest::Client::new();
        ctx.spawn(
            async move {
                let get = |path: &str| {
                    let request = client
                        .get(format!("{base}{path}"))
                        .bearer_auth(&token)
                        .timeout(std::time::Duration::from_secs(5));
                    async move {
                        let response = request.send().await.ok()?;
                        if !response.status().is_success() {
                            return None;
                        }
                        response.json::<serde_json::Value>().await.ok()
                    }
                };
                (
                    get("/api/v1/status").await,
                    get("/api/v1/agent/runs").await,
                    get("/api/v1/logs").await,
                    get("/api/v1/agent/leaderboard").await,
                )
            },
            |me, (status, runs, logs, leaderboard), ctx| {
                me.reachability = match status {
                    Some(status) => Reachability::Up {
                        gateway: status["gateway"].as_str().map(str::to_owned),
                        mode: status["mode"].as_str().unwrap_or("?").to_owned(),
                        version: status["version"].as_str().unwrap_or("?").to_owned(),
                        queued: status["jobs_queued"].as_i64().unwrap_or(0),
                        running: status["jobs_running"].as_i64().unwrap_or(0),
                    },
                    None => Reachability::Down(
                        "Couldn't reach it. Is the server running at the address below?".to_owned(),
                    ),
                };
                me.runs = runs
                    .and_then(|runs| runs["runs"].as_array().cloned())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|run| {
                        Some(RunRow {
                            id: run["task_id"].as_str()?.to_owned(),
                            title: run["title"].as_str().unwrap_or("Untitled run").to_owned(),
                            state: run["state"].as_str().unwrap_or("?").to_owned(),
                        })
                    })
                    .collect();
                me.state_handles.borrow_mut().stop_links =
                    (0..me.runs.len()).map(|_| MouseStateHandle::default()).collect();
                me.log_lines = logs
                    .and_then(|logs| logs["lines"].as_array().cloned())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|line| line.as_str().map(str::to_owned))
                    .collect();
                me.leaderboard = leaderboard
                    .and_then(|board| board["cells"].as_array().cloned())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|cell| {
                        Some(LeaderCell {
                            task: cell["task"].as_str()?.to_owned(),
                            model: cell["model"].as_str().unwrap_or("?").to_owned(),
                            passed: cell["passed"].as_i64().unwrap_or(0),
                            total: cell["total"].as_i64().unwrap_or(0),
                            median_seconds: cell["median_seconds"].as_i64(),
                        })
                    })
                    .collect();
                ctx.notify();
            },
        );
    }

    /// Cancels a run from the in-app list, then refreshes so its row updates.
    /// Best-effort — a failed cancel just leaves the run as it was.
    fn stop_run(&mut self, id: String, ctx: &mut ViewContext<Self>) {
        let Some((base, token)) = target() else {
            return;
        };
        let client = reqwest::Client::new();
        ctx.spawn(
            async move {
                let _ = client
                    .post(format!("{base}/api/v1/agent/runs/{id}/cancel"))
                    .bearer_auth(&token)
                    .json(&serde_json::json!({}))
                    .timeout(std::time::Duration::from_secs(5))
                    .send()
                    .await;
            },
            |me, _result, ctx| me.refresh(ctx),
        );
    }
}

impl Entity for MolochPageView {
    type Event = super::settings_page::SettingsPageEvent;
}

impl View for MolochPageView {
    fn ui_name() -> &'static str {
        "MolochPageView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MolochPageAction {
    Refresh,
    StopRun(String),
    /// Moloch's own web panel, in the browser (it asks for the token once, and keeps it).
    OpenWeb,
    /// The Moloch app (Moloch.app, an app of its own built on Krust's skeleton).
    OpenApp,
    /// The "Use Moloch" switch (saved at once), and Save for the address and token.
    ToggleUseMoloch,
    SaveConnection,
}

impl TypedActionView for MolochPageView {
    type Action = MolochPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            MolochPageAction::Refresh => self.refresh(ctx),
            MolochPageAction::StopRun(id) => self.stop_run(id.clone(), ctx),
            MolochPageAction::OpenWeb => {
                if let Some((base, _)) = target() {
                    ctx.open_url(&format!("{base}/panel"));
                }
            }
            MolochPageAction::OpenApp => {
                // By bundle id, so it opens wherever Moloch.app is installed.
                if let Err(error) = std::process::Command::new("/usr/bin/open")
                    .args(["-b", "krust.baby.Moloch"])
                    .spawn()
                {
                    log::warn!("moloch_page: could not open Moloch.app: {error}");
                }
            }
            MolochPageAction::ToggleUseMoloch => {
                self.use_moloch = !self.use_moloch;
                self.save_connection(ctx);
            }
            MolochPageAction::SaveConnection => self.save_connection(ctx),
        }
    }
}

impl SettingsPageMeta for MolochPageView {
    fn section() -> SettingsSection {
        SettingsSection::MolochStatus
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        true
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

impl From<ViewHandle<MolochPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<MolochPageView>) -> Self {
        SettingsPageViewHandle::MolochStatus(view_handle)
    }
}

// ── Widgets ──────────────────────────────────────────────────────────────

/// Address, reachability, and where the intelligence actually comes from.
#[derive(Default)]
struct ServerWidget;

impl SettingsWidget for ServerWidget {
    type View = MolochPageView;

    fn search_terms(&self) -> &str {
        "moloch server status gateway broker relay address token jobs"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let mut column = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);

        column.add_child(
            Container::new(body_text(
                "Moloch is the app server that manages AI. It answers the AI-manager functions — \
                 next-command suggestions, the prompt banner, natural-language completion — and \
                 brokers agent runs to whichever machine has the models.",
                appearance,
            ))
            .with_margin_bottom(VERTICAL_MARGIN)
            .finish(),
        );

        column.add_child(render_connection(view, appearance));

        match &view.reachability {
            Reachability::Unknown => {}
            Reachability::Checking => {
                column.add_child(row("Status", "checking…", appearance));
            }
            Reachability::Down(reason) => {
                column.add_child(row("Status", "not answering", appearance));
                column.add_child(Container::new(sub_text(reason, appearance)).finish());
            }
            Reachability::Up {
                gateway,
                mode,
                version,
                queued,
                running,
            } => {
                column.add_child(row("Status", &format!("up (v{version})"), appearance));
                // The distinction that matters: is this server calling a
                // provider itself, or queueing for a machine that will?
                match (mode.as_str(), gateway) {
                    (_, Some(gateway)) => {
                        column.add_child(row("Serving from", gateway, appearance));
                    }
                    ("relay", None) => {
                        column.add_child(row("Serving from", "a worker (relay)", appearance));
                        column.add_child(
                            Container::new(sub_text(
                                "No provider is configured on the server, so AI functions wait \
                                 for a machine to claim them.",
                                appearance,
                            ))
                            .finish(),
                        );
                    }
                    _ => {}
                }
                column.add_child(row(
                    "Jobs",
                    &format!("{queued} queued, {running} running"),
                    appearance,
                ));
            }
        }

        let handles = view.state_handles.borrow();
        let link = |label: &str, action: MolochPageAction, mouse: &MouseStateHandle| {
            appearance
                .ui_builder()
                .link(
                    label.into(),
                    None,
                    Some(Box::new(move |ctx| {
                        ctx.dispatch_typed_action(action.clone());
                    })),
                    mouse.clone(),
                )
                .soft_wrap(false)
                .build()
                .finish()
        };
        let mut links = Flex::row()
            .with_spacing(16.)
            .with_child(link("Refresh", MolochPageAction::Refresh, &handles.refresh_link));
        // Open it: the web panel needs an address to go to; the app's window works either way
        // (it says so when nothing is configured).
        if target().is_some() {
            links.add_child(link("Open web", MolochPageAction::OpenWeb, &handles.web_link));
        }
        links.add_child(link("Open app", MolochPageAction::OpenApp, &handles.app_link));
        column.add_child(Container::new(links.finish()).with_margin_top(12.).finish());

        let _ = app;
        column.finish()
    }
}

/// Agent runs this Moloch is brokering — monitor them, and stop one without
/// leaving the app for the web panel.
#[derive(Default)]
struct RunsWidget;

impl SettingsWidget for RunsWidget {
    type View = MolochPageView;

    fn search_terms(&self) -> &str {
        "moloch runs agents cloud agent stop cancel monitor tasks"
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
            Container::new(render_sub_header(appearance, "Runs", None))
                .with_margin_top(VERTICAL_MARGIN)
                .with_padding_bottom(HEADER_PADDING)
                .finish(),
        );
        column.add_child(
            Container::new(body_text(
                "Agent runs this Moloch is brokering, newest first. Stop one here without \
                 opening the web panel.",
                appearance,
            ))
            .with_margin_bottom(12.)
            .finish(),
        );

        if view.runs.is_empty() {
            column.add_child(sub_text("No runs yet.", appearance));
            return column.finish();
        }

        let handles = view.state_handles.borrow();
        for (index, run) in view.runs.iter().enumerate() {
            let mut line = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(
                    Container::new(sub_text(&run.state, appearance))
                        .with_margin_right(12.)
                        .finish(),
                )
                .with_child(body_text(&run.title, appearance));
            if is_active(&run.state) {
                if let Some(handle) = handles.stop_links.get(index) {
                    let id = run.id.clone();
                    line = line.with_child(
                        Container::new(
                            appearance
                                .ui_builder()
                                .link(
                                    "Stop".into(),
                                    None,
                                    Some(Box::new(move |ctx| {
                                        ctx.dispatch_typed_action(MolochPageAction::StopRun(
                                            id.clone(),
                                        ));
                                    })),
                                    handle.clone(),
                                )
                                .soft_wrap(false)
                                .build()
                                .finish(),
                        )
                        .with_margin_left(12.)
                        .finish(),
                    );
                }
            }
            column.add_child(Container::new(line.finish()).with_margin_top(6.).finish());
        }
        column.finish()
    }
}

/// How the local models do on the vmlab sysadmin eval — the runs the eval
/// streamed in, grouped by task and ranked best-first. Each run was graded by
/// the machine it left behind, never its own opinion.
#[derive(Default)]
struct LeaderboardWidget;

impl SettingsWidget for LeaderboardWidget {
    type View = MolochPageView;

    fn search_terms(&self) -> &str {
        "moloch leaderboard eval vmlab benchmark sysadmin models pass rate tasks scores"
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
            Container::new(render_sub_header(appearance, "Leaderboard", None))
                .with_margin_top(VERTICAL_MARGIN)
                .with_padding_bottom(HEADER_PADDING)
                .finish(),
        );
        column.add_child(
            Container::new(body_text(
                "How the local models do on the vmlab sysadmin eval, per task, best first. \
                 Each run is graded by the machine it leaves behind, not its own opinion.",
                appearance,
            ))
            .with_margin_bottom(12.)
            .finish(),
        );

        if view.leaderboard.is_empty() {
            column.add_child(sub_text(
                "No eval runs yet — run the vmlab eval and they land here.",
                appearance,
            ));
            return column.finish();
        }

        let mut tasks: Vec<String> = view.leaderboard.iter().map(|c| c.task.clone()).collect();
        tasks.sort();
        tasks.dedup();

        for task in tasks {
            column.add_child(
                Container::new(body_text(&task, appearance))
                    .with_margin_top(12.)
                    .finish(),
            );
            let mut cells: Vec<&LeaderCell> =
                view.leaderboard.iter().filter(|c| c.task == task).collect();
            // Best first: higher pass-rate, then faster.
            cells.sort_by(|a, b| {
                let rate = |c: &LeaderCell| c.passed as f64 / c.total.max(1) as f64;
                rate(b)
                    .partial_cmp(&rate(a))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(
                        a.median_seconds
                            .unwrap_or(i64::MAX)
                            .cmp(&b.median_seconds.unwrap_or(i64::MAX)),
                    )
            });
            for cell in cells {
                let score = format!("{}/{}", cell.passed, cell.total);
                let time = cell
                    .median_seconds
                    .map(|s| format!("~{s}s"))
                    .unwrap_or_default();
                let line = Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(
                        Container::new(sub_text(&score, appearance))
                            .with_margin_right(12.)
                            .finish(),
                    )
                    .with_child(body_text(&cell.model, appearance))
                    .with_child(
                        Container::new(sub_text(&time, appearance))
                            .with_margin_left(12.)
                            .finish(),
                    );
                column.add_child(
                    Container::new(line.finish())
                        .with_margin_left(16.)
                        .with_margin_top(4.)
                        .finish(),
                );
            }
        }
        column.finish()
    }
}

/// The tail of the server's log — what it has actually been asked to do.
#[derive(Default)]
struct LogWidget;

impl SettingsWidget for LogWidget {
    type View = MolochPageView;

    fn search_terms(&self) -> &str {
        "moloch log logs activity requests conversation network what is it doing"
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
            Container::new(render_sub_header(appearance, "Recent activity", None))
                .with_margin_top(VERTICAL_MARGIN)
                .with_padding_bottom(HEADER_PADDING)
                .finish(),
        );
        column.add_child(
            Container::new(body_text(
                "The server's own log, newest last — every AI function it answered and what it \
                 replied. This is the live view of what leaves this machine.",
                appearance,
            ))
            .with_margin_bottom(12.)
            .finish(),
        );

        if view.log_lines.is_empty() {
            column.add_child(sub_text(
                "Nothing yet. Run a command in the terminal and refresh.",
                appearance,
            ));
            return column.finish();
        }

        // Newest last matches how a log reads in a terminal, so the most
        // recent line sits nearest the Refresh you just pressed.
        for line in &view.log_lines {
            column.add_child(Container::new(mono_text(line, appearance)).finish());
        }
        column.finish()
    }
}

// ── Small shared bits ────────────────────────────────────────────────────

/// A "Label   value" row, so the facts line up down the page.
fn row(label: &str, value: &str, appearance: &Appearance) -> Box<dyn Element> {
    Container::new(
        Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(Container::new(sub_text(label, appearance)).with_margin_right(12.).finish())
            .with_child(body_text(value, appearance))
            .finish(),
    )
    .with_margin_top(6.)
    .finish()
}

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

/// Log lines in the terminal font — they are machine output, and the
/// timestamps only line up in a monospaced face.
fn mono_text(text: &str, appearance: &Appearance) -> Box<dyn Element> {
    let theme = appearance.theme();
    Text::new(
        text.to_string(),
        appearance.monospace_font_family(),
        appearance.ui_font_size() - 1.,
    )
    .with_color(theme.sub_text_color(theme.background()).into_solid())
    .finish()
}

fn section_break(appearance: &Appearance) -> Box<dyn Element> {
    Container::new(render_separator(appearance))
        .with_margin_top(VERTICAL_MARGIN)
        .finish()
}
