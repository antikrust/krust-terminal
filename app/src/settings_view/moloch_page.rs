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

use warpui::elements::{
    Container, CrossAxisAlignment, Flex, MouseStateHandle, ParentElement, Text,
};
use warpui::ui_components::components::UiComponent;
use warpui::{
    AppContext, Element, Entity, TypedActionView, View, ViewContext, ViewHandle,
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

pub struct MolochPageView {
    page: PageType<Self>,
    reachability: Reachability,
    /// Tail of the server's log, most recent last.
    log_lines: Vec<String>,
    state_handles: RefCell<StateHandles>,
}

#[derive(Default)]
struct StateHandles {
    refresh_link: MouseStateHandle,
}

impl MolochPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let widgets: Vec<Box<dyn SettingsWidget<View = Self>>> = vec![
            Box::new(PageTitleWidget::new("Moloch", true)),
            Box::new(ServerWidget),
            Box::new(LogWidget),
        ];
        let mut view = Self {
            page: PageType::new_uncategorized(widgets, None),
            reachability: Reachability::Unknown,
            log_lines: Vec::new(),
            state_handles: RefCell::new(StateHandles::default()),
        };
        view.refresh(ctx);
        view
    }

    /// Asks the server what it is doing and for its recent log. Both are one
    /// request each and cheap; a server that does not answer leaves the page
    /// saying so rather than showing stale numbers as if they were current.
    fn refresh(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(config) = crate::moloch::config() else {
            self.reachability =
                Reachability::Down("No Moloch is configured on this machine.".to_owned());
            self.log_lines.clear();
            ctx.notify();
            return;
        };
        self.reachability = Reachability::Checking;
        ctx.notify();

        let base = config.url.clone();
        let token = config.token.clone().unwrap_or_default();
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
                (get("/api/v1/status").await, get("/api/v1/logs").await)
            },
            |me, (status, logs), ctx| {
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
                me.log_lines = logs
                    .and_then(|logs| logs["lines"].as_array().cloned())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|line| line.as_str().map(str::to_owned))
                    .collect();
                ctx.notify();
            },
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
}

impl TypedActionView for MolochPageView {
    type Action = MolochPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            MolochPageAction::Refresh => self.refresh(ctx),
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

        let address = crate::moloch::config()
            .map(|config| config.url.clone())
            .unwrap_or_else(|| "not configured".to_owned());
        column.add_child(row("Address", &address, appearance));

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
        column.add_child(
            Container::new(
                appearance
                    .ui_builder()
                    .link(
                        "Refresh".into(),
                        None,
                        Some(Box::new(|ctx| {
                            ctx.dispatch_typed_action(MolochPageAction::Refresh);
                        })),
                        handles.refresh_link.clone(),
                    )
                    .soft_wrap(false)
                    .build()
                    .finish(),
            )
            .with_margin_top(12.)
            .finish(),
        );

        let _ = app;
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
