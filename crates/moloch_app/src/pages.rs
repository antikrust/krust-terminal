//! Moloch's pages: Overview, Agents, Acolytes, Live sessions, Leaderboard, Logs, Connection — and
//! the Output view an agent opens in a tab. Each draws from the one [`Moloch`] model and redraws
//! when it changes.

use std::time::Duration;

use krust_app_shell::page;
use krust_app_shell::{Page, Tabs, TextField, TextFieldEvent};
use krust_core::ui::appearance::Appearance;
use krustui::elements::{
    ChildView, ClippedScrollStateHandle, ConstrainedBox, Container, Element, Flex,
    MouseStateHandle, ParentElement,
};
use krustui::r#async::Timer;
use krustui::ui_components::button::ButtonVariant;
use krustui::ui_components::components::UiComponent;
use krustui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};
use serde_json::Value;

use crate::moloch::{self, Moloch, Source};

/// How often an open output tab looks for new lines.
const OUTPUT_REFRESH: Duration = Duration::from_secs(3);

/// What any page's links and buttons do.
#[derive(Clone, Debug, PartialEq)]
pub enum MolochAction {
    Refresh,
    /// Open `<address><path>` in the browser: the panel, or a session's watch page.
    OpenWeb(String),
    Stop(String),
    /// Open an agent's output in a tab: (where it comes from, its title).
    OpenOutput(Source, String),
    Save,
}

/// The run states that are still going, so can be stopped.
fn is_active(state: &str) -> bool {
    matches!(
        state.to_ascii_uppercase().as_str(),
        "QUEUED" | "RUNNING" | "CLAIMED"
    )
}

/// Every page redraws when the model changes.
fn follow<V: View>(ctx: &mut ViewContext<V>) {
    ctx.subscribe_to_model(&Moloch::handle(ctx), |_, _, _, ctx| ctx.notify());
}

/// The actions every page shares.
fn handle_common<V: View>(action: &MolochAction, ctx: &mut ViewContext<V>) {
    match action {
        MolochAction::Refresh => Moloch::handle(ctx).update(ctx, |moloch, ctx| moloch.refresh(ctx)),
        MolochAction::OpenWeb(path) => {
            let base = Moloch::as_ref(ctx).url.clone();
            if !base.is_empty() {
                ctx.open_url(&format!("{base}{path}"));
            }
        }
        MolochAction::Stop(run_id) => {
            let run_id = run_id.clone();
            Moloch::handle(ctx).update(ctx, |moloch, ctx| moloch.stop_run(run_id, ctx));
        }
        MolochAction::OpenOutput(source, title) => {
            let key = format!("{source:?}");
            let already = Tabs::as_ref(ctx).tabs().iter().position(|tab| tab.key == key);
            match already {
                Some(index) => Tabs::handle(ctx).update(ctx, |tabs, ctx| tabs.select(Some(index), ctx)),
                None => {
                    let source = source.clone();
                    let output_title = title.clone();
                    let view = ctx.add_typed_action_view(move |ctx| Output::new(source, output_title, ctx));
                    let title = title.clone();
                    Tabs::handle(ctx).update(ctx, |tabs, ctx| {
                        tabs.open(&key, &title, Page::view("", view), ctx)
                    });
                }
            }
        }
        // The Connection page's own.
        MolochAction::Save => {}
    }
}

/// The error, if the server didn't answer, above a page's body.
fn with_error(mut body: Vec<Box<dyn Element>>, moloch: &Moloch, appearance: &Appearance) -> Vec<Box<dyn Element>> {
    if let Some(error) = &moloch.error {
        body.insert(0, page::note(error, appearance));
    }
    body
}

/// Mouse states for a list's per-row links, kept as long as the rows last.
fn resize(handles: &mut Vec<MouseStateHandle>, len: usize) {
    handles.resize_with(len, MouseStateHandle::default);
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or("?").to_owned()
}

// ---------------------------------------------------------------------------

pub struct Overview {
    scroll: ClippedScrollStateHandle,
    refresh: MouseStateHandle,
    web: MouseStateHandle,
}

impl Overview {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        follow(ctx);
        Self {
            scroll: Default::default(),
            refresh: Default::default(),
            web: Default::default(),
        }
    }
}

impl Entity for Overview {
    type Event = ();
}

impl View for Overview {
    fn ui_name() -> &'static str {
        "MolochOverview"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let moloch = Moloch::as_ref(app);
        let mut body = vec![page::row("Address", if moloch.url.is_empty() { "not set" } else { &moloch.url }, appearance)];
        match &moloch.status {
            Some(status) => {
                body.push(page::row(
                    "Status",
                    &format!("up (v{})", status["version"].as_str().unwrap_or("?")),
                    appearance,
                ));
                let serving = match (status["gateway"].as_str(), status["mode"].as_str()) {
                    (Some(gateway), _) => gateway.to_owned(),
                    (None, Some("relay")) => "its acolytes (relay: no provider on the server)".to_owned(),
                    (None, mode) => mode.unwrap_or("?").to_owned(),
                };
                body.push(page::row("Serving from", &serving, appearance));
                body.push(page::row(
                    "Jobs",
                    &format!(
                        "{} queued, {} running",
                        status["jobs_queued"].as_i64().unwrap_or(0),
                        status["jobs_running"].as_i64().unwrap_or(0)
                    ),
                    appearance,
                ));
            }
            None => body.push(page::row("Status", "not answering", appearance)),
        }
        let active = moloch
            .runs
            .iter()
            .filter(|run| is_active(run["state"].as_str().unwrap_or("")))
            .count();
        let local = match &moloch.local_tasks {
            Ok(tasks) => format!(", {} in Krust's queue on this Mac", tasks.len()),
            Err(_) => String::new(),
        };
        body.push(page::row(
            "Agents",
            &format!("{active} going through Moloch, {} in all{local}", moloch.runs.len()),
            appearance,
        ));
        let connected = moloch
            .workers
            .iter()
            .filter(|worker| worker["connected"].as_bool().unwrap_or(true))
            .count();
        body.push(page::row(
            "Acolytes",
            &format!("{connected} connected of {}", moloch.workers.len()),
            appearance,
        ));
        body.push(page::row(
            "Live sessions",
            &moloch.sessions.len().to_string(),
            appearance,
        ));
        let mut links = vec![page::link("Refresh", MolochAction::Refresh, &self.refresh, appearance)];
        if !moloch.url.is_empty() {
            links.push(page::link(
                "Open web",
                MolochAction::OpenWeb("/panel".to_owned()),
                &self.web,
                appearance,
            ));
        }
        page::frame("Overview", links, with_error(body, moloch, appearance), &self.scroll, appearance)
    }
}

impl TypedActionView for Overview {
    type Action = MolochAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        handle_common(action, ctx);
    }
}

// ---------------------------------------------------------------------------

/// The agents: the runs Moloch is brokering (wherever they run — this machine, an acolyte, a cloud
/// server), and the ones in Krust's own queue on this Mac. Each opens its output in a tab.
pub struct Agents {
    scroll: ClippedScrollStateHandle,
    refresh: MouseStateHandle,
    /// Per run: open, stop. Per local task: open.
    runs: Vec<(MouseStateHandle, MouseStateHandle)>,
    tasks: Vec<MouseStateHandle>,
}

impl Agents {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        ctx.subscribe_to_model(&Moloch::handle(ctx), |me, moloch, _, ctx| {
            let moloch = moloch.as_ref(ctx);
            me.runs.resize_with(moloch.runs.len(), Default::default);
            let tasks = moloch.local_tasks.as_ref().map_or(0, Vec::len);
            resize(&mut me.tasks, tasks);
            ctx.notify();
        });
        Self {
            scroll: Default::default(),
            refresh: Default::default(),
            runs: Vec::new(),
            tasks: Vec::new(),
        }
    }
}

impl Entity for Agents {
    type Event = ();
}

impl View for Agents {
    fn ui_name() -> &'static str {
        "MolochAgents"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let moloch = Moloch::as_ref(app);
        let mut body = vec![page::heading("Through Moloch", appearance)];
        if moloch.runs.is_empty() {
            body.push(page::note("No agent runs.", appearance));
        }
        for (run, (open, stop)) in moloch.runs.iter().zip(&self.runs) {
            let state = text(run, "state");
            let id = text(run, "task_id");
            let title = run["title"].as_str().unwrap_or("Untitled run").to_owned();
            let mut line = Flex::row()
                .with_spacing(12.)
                .with_child(page::row(&state.to_lowercase(), &title, appearance))
                .with_child(page::link(
                    "Open",
                    MolochAction::OpenOutput(Source::Run(id.clone()), title.clone()),
                    open,
                    appearance,
                ));
            if is_active(&state) {
                line.add_child(page::link("Stop", MolochAction::Stop(id.clone()), stop, appearance));
            }
            body.push(line.finish());
        }

        body.push(page::heading("In Krust's queue on this Mac", appearance));
        match &moloch.local_tasks {
            Err(why) => body.push(page::note(why, appearance)),
            Ok(tasks) if tasks.is_empty() => body.push(page::note("No queued agents.", appearance)),
            Ok(tasks) => {
                for (task, open) in tasks.iter().zip(&self.tasks) {
                    let id = task["id"].as_u64().unwrap_or(0);
                    let title = task["title"].as_str().unwrap_or("Task").to_owned();
                    let mut summary = title.clone();
                    if let Some(model) = task["model"].as_str() {
                        summary.push_str(&format!(" — {model}"));
                    }
                    body.push(
                        Flex::row()
                            .with_spacing(12.)
                            .with_child(page::row(&text(task, "state"), &summary, appearance))
                            .with_child(page::link(
                                "Open",
                                MolochAction::OpenOutput(Source::Task(id), title),
                                open,
                                appearance,
                            ))
                            .finish(),
                    );
                    if let Some(detail) = task["detail"].as_str().filter(|detail| !detail.is_empty()) {
                        body.push(page::note(detail, appearance));
                    }
                }
            }
        }
        let links = vec![page::link("Refresh", MolochAction::Refresh, &self.refresh, appearance)];
        page::frame("Agents", links, with_error(body, moloch, appearance), &self.scroll, appearance)
    }
}

impl TypedActionView for Agents {
    type Action = MolochAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        handle_common(action, ctx);
    }
}

// ---------------------------------------------------------------------------

/// Every acolyte (and any other worker) Moloch has seen — connected or not — with its models and
/// the job it is on, which opens in a tab.
pub struct Acolytes {
    scroll: ClippedScrollStateHandle,
    refresh: MouseStateHandle,
    jobs: Vec<MouseStateHandle>,
}

impl Acolytes {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        ctx.subscribe_to_model(&Moloch::handle(ctx), |me, moloch, _, ctx| {
            let len = moloch.as_ref(ctx).workers.len();
            resize(&mut me.jobs, len);
            ctx.notify();
        });
        Self {
            scroll: Default::default(),
            refresh: Default::default(),
            jobs: Vec::new(),
        }
    }
}

impl Entity for Acolytes {
    type Event = ();
}

impl View for Acolytes {
    fn ui_name() -> &'static str {
        "MolochAcolytes"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let moloch = Moloch::as_ref(app);
        let mut body = vec![page::note(
            "The machines doing Moloch's work — acolytes with models, and Krust's own harness. One \
             counts as connected while it polls; it goes offline 30 seconds after it stops.",
            appearance,
        )];
        if moloch.workers.is_empty() {
            body.push(page::note("No acolyte has connected yet.", appearance));
        }
        for (worker, job_link) in moloch.workers.iter().zip(&self.jobs) {
            let join = |key: &str, empty: &str| {
                let values: Vec<&str> = worker[key]
                    .as_array()
                    .map(|values| values.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                if values.is_empty() {
                    empty.to_owned()
                } else {
                    values.join(", ")
                }
            };
            let name = text(worker, "id");
            let state = if worker["connected"].as_bool().unwrap_or(true) {
                "connected"
            } else {
                "offline"
            };
            let mut block = Flex::column().with_spacing(4.);
            block.add_child(page::heading(&format!("{name} — {state}"), appearance));
            block.add_child(page::row("Takes", &join("kinds", "any job"), appearance));
            block.add_child(page::row("Models", &join("models", "none listed"), appearance));
            block.add_child(page::row("Last seen", &text(worker, "last_seen"), appearance));
            match worker.get("job").filter(|job| !job.is_null()) {
                Some(job) => {
                    let kind = text(job, "kind");
                    let mut line = Flex::row()
                        .with_spacing(12.)
                        .with_child(page::row("Working on", &format!("{kind}, since {}", text(job, "claimed_at")), appearance));
                    if let Some(run_id) = job["run_id"].as_str() {
                        line.add_child(page::link(
                            "Open",
                            MolochAction::OpenOutput(Source::Run(run_id.to_owned()), format!("{name}: {kind}")),
                            job_link,
                            appearance,
                        ));
                    }
                    block.add_child(line.finish());
                }
                None => block.add_child(page::row("Working on", "nothing", appearance)),
            }
            body.push(block.finish());
        }
        let links = vec![page::link("Refresh", MolochAction::Refresh, &self.refresh, appearance)];
        page::frame("Acolytes", links, with_error(body, moloch, appearance), &self.scroll, appearance)
    }
}

impl TypedActionView for Acolytes {
    type Action = MolochAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        handle_common(action, ctx);
    }
}

// ---------------------------------------------------------------------------

/// One agent's output, in a tab, like a terminal: what it said, its tool calls (`▸`) and their
/// results (`↳`), refreshed every few seconds while the tab is open.
pub struct Output {
    source: Source,
    title: String,
    lines: Vec<String>,
    error: Option<String>,
    scroll: ClippedScrollStateHandle,
    refresh: MouseStateHandle,
}

impl Output {
    pub fn new(source: Source, title: String, ctx: &mut ViewContext<Self>) -> Self {
        let mut me = Self {
            source,
            title,
            lines: Vec::new(),
            error: None,
            scroll: Default::default(),
            refresh: Default::default(),
        };
        me.load(ctx);
        Self::tick(ctx);
        me
    }

    fn tick(ctx: &mut ViewContext<Self>) {
        ctx.spawn(
            async { Timer::after(OUTPUT_REFRESH).await },
            |me, _, ctx| {
                me.load(ctx);
                Self::tick(ctx);
            },
        );
    }

    fn load(&mut self, ctx: &mut ViewContext<Self>) {
        let source = self.source.clone();
        let (base, token) = {
            let moloch = Moloch::as_ref(ctx);
            (moloch.url.clone(), moloch.token.clone())
        };
        ctx.spawn(moloch::output(source, base, token), |me, result, ctx| {
            match result {
                Ok(lines) => {
                    me.lines = lines;
                    me.error = None;
                }
                Err(error) => me.error = Some(error),
            }
            ctx.notify();
        });
    }
}

impl Entity for Output {
    type Event = ();
}

impl View for Output {
    fn ui_name() -> &'static str {
        "MolochOutput"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let mut body = Vec::new();
        if let Some(error) = &self.error {
            body.push(page::note(error, appearance));
        }
        body.push(if self.lines.is_empty() {
            page::note("No output yet.", appearance)
        } else {
            page::mono_lines(&self.lines, appearance)
        });
        let links = vec![page::link("Refresh", OutputAction::Refresh, &self.refresh, appearance)];
        page::frame(&self.title, links, body, &self.scroll, appearance)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum OutputAction {
    Refresh,
}

impl TypedActionView for Output {
    type Action = OutputAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            OutputAction::Refresh => self.load(ctx),
        }
    }
}

// ---------------------------------------------------------------------------

pub struct Sessions {
    scroll: ClippedScrollStateHandle,
    refresh: MouseStateHandle,
    watches: Vec<MouseStateHandle>,
}

impl Sessions {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        ctx.subscribe_to_model(&Moloch::handle(ctx), |me, moloch, _, ctx| {
            let len = moloch.as_ref(ctx).sessions.len();
            resize(&mut me.watches, len);
            ctx.notify();
        });
        Self {
            scroll: Default::default(),
            refresh: Default::default(),
            watches: Vec::new(),
        }
    }
}

impl Entity for Sessions {
    type Event = ();
}

impl View for Sessions {
    fn ui_name() -> &'static str {
        "MolochSessions"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let moloch = Moloch::as_ref(app);
        let mut body = vec![page::note(
            "Terminal sessions shared from Krust (Share session, Remote control), as Moloch relays them.",
            appearance,
        )];
        if moloch.sessions.is_empty() {
            body.push(page::note("None shared right now.", appearance));
        }
        for (session, watch) in moloch.sessions.iter().zip(&self.watches) {
            let sharer = if session["sharer_connected"].as_bool().unwrap_or(false) {
                "sharer connected"
            } else {
                "sharer away"
            };
            let summary = format!(
                "{sharer}, {} watching, {} events{} — since {}",
                session["viewers_present"].as_u64().unwrap_or(0),
                session["events"].as_u64().unwrap_or(0),
                if session["ended"].as_bool().unwrap_or(false) { ", ended" } else { "" },
                text(session, "started_at"),
            );
            let path = session["watch_path"].as_str().unwrap_or_default().to_owned();
            body.push(
                Flex::row()
                    .with_spacing(12.)
                    .with_child(page::row(&text(session, "id"), &summary, appearance))
                    .with_child(page::link("Watch", MolochAction::OpenWeb(path), watch, appearance))
                    .finish(),
            );
        }
        let links = vec![page::link("Refresh", MolochAction::Refresh, &self.refresh, appearance)];
        page::frame("Live sessions", links, with_error(body, moloch, appearance), &self.scroll, appearance)
    }
}

impl TypedActionView for Sessions {
    type Action = MolochAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        handle_common(action, ctx);
    }
}

// ---------------------------------------------------------------------------

pub struct Leaderboard {
    scroll: ClippedScrollStateHandle,
    refresh: MouseStateHandle,
}

impl Leaderboard {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        follow(ctx);
        Self {
            scroll: Default::default(),
            refresh: Default::default(),
        }
    }
}

impl Entity for Leaderboard {
    type Event = ();
}

impl View for Leaderboard {
    fn ui_name() -> &'static str {
        "MolochLeaderboard"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let moloch = Moloch::as_ref(app);
        let mut body = vec![page::note("How each model did on each eval task.", appearance)];
        if moloch.leaderboard.is_empty() {
            body.push(page::note("No eval results yet.", appearance));
        }
        for cell in &moloch.leaderboard {
            let median = cell["median_seconds"]
                .as_i64()
                .map(|seconds| format!(", median {seconds}s"))
                .unwrap_or_default();
            body.push(page::row(
                &text(cell, "task"),
                &format!(
                    "{} — {}/{} passed{median}",
                    text(cell, "model"),
                    cell["passed"].as_i64().unwrap_or(0),
                    cell["total"].as_i64().unwrap_or(0)
                ),
                appearance,
            ));
        }
        let links = vec![page::link("Refresh", MolochAction::Refresh, &self.refresh, appearance)];
        page::frame("Leaderboard", links, with_error(body, moloch, appearance), &self.scroll, appearance)
    }
}

impl TypedActionView for Leaderboard {
    type Action = MolochAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        handle_common(action, ctx);
    }
}

// ---------------------------------------------------------------------------

pub struct Logs {
    scroll: ClippedScrollStateHandle,
    refresh: MouseStateHandle,
}

impl Logs {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        follow(ctx);
        Self {
            scroll: Default::default(),
            refresh: Default::default(),
        }
    }
}

impl Entity for Logs {
    type Event = ();
}

impl View for Logs {
    fn ui_name() -> &'static str {
        "MolochLogs"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let moloch = Moloch::as_ref(app);
        let body = vec![if moloch.logs.is_empty() {
            page::note("Nothing logged yet.", appearance)
        } else {
            page::mono_lines(&moloch.logs, appearance)
        }];
        let links = vec![page::link("Refresh", MolochAction::Refresh, &self.refresh, appearance)];
        page::frame("Logs", links, with_error(body, moloch, appearance), &self.scroll, appearance)
    }
}

impl TypedActionView for Logs {
    type Action = MolochAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        handle_common(action, ctx);
    }
}

// ---------------------------------------------------------------------------

pub struct Connection {
    scroll: ClippedScrollStateHandle,
    url: ViewHandle<TextField>,
    token: ViewHandle<TextField>,
    save: MouseStateHandle,
    message: Option<String>,
}

impl Connection {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        follow(ctx);
        let (url, token) = {
            let moloch = Moloch::as_ref(ctx);
            (moloch.url.clone(), moloch.token.clone())
        };
        let url = ctx.add_typed_action_view(move |_| TextField::new(&url, "http://host:10102", false));
        let token = ctx.add_typed_action_view(move |_| {
            TextField::new(&token, "Token or API key (wk-…)", true)
        });
        for field in [&url, &token] {
            ctx.subscribe_to_view(field, |me, _, event, ctx| {
                if *event == TextFieldEvent::Submitted {
                    me.save(ctx);
                }
            });
        }
        Self {
            scroll: Default::default(),
            url,
            token,
            save: Default::default(),
            message: None,
        }
    }

    fn save(&mut self, ctx: &mut ViewContext<Self>) {
        let url = self.url.as_ref(ctx).text().to_owned();
        let token = self.token.as_ref(ctx).text().to_owned();
        let result = Moloch::handle(ctx).update(ctx, |moloch, ctx| moloch.save_connection(&url, &token, ctx));
        self.message = Some(match result {
            Ok(()) => "Saved. Krust shares this connection and switches to it when it next starts.".to_owned(),
            Err(error) => format!("Couldn't save: {error}"),
        });
        ctx.notify();
    }
}

impl Entity for Connection {
    type Event = ();
}

impl View for Connection {
    fn ui_name() -> &'static str {
        "MolochConnection"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let moloch = Moloch::as_ref(app);
        let field = |handle: &ViewHandle<TextField>| {
            ConstrainedBox::new(ChildView::new(handle).finish())
                .with_width(380.)
                .finish()
        };
        let mut body = vec![
            page::note(
                "Which Moloch this app watches, and the token it signs in with — a server's api-token, \
                 or an API key made in Krust (Settings → Platform). Krust uses the same connection. \
                 Point it at a Moloch on another server to watch that one's agents and acolytes.",
                appearance,
            ),
            page::labelled("Address", field(&self.url), appearance),
            page::labelled("Token", field(&self.token), appearance),
            page::labelled(
                "",
                appearance
                    .ui_builder()
                    .button(ButtonVariant::Secondary, self.save.clone())
                    .with_text_label("Save".to_owned())
                    .build()
                    .on_click(|ctx, _, _| ctx.dispatch_typed_action(MolochAction::Save))
                    .finish(),
                appearance,
            ),
        ];
        if let Some(message) = &self.message {
            body.push(page::note(message, appearance));
        }
        let status = match (&moloch.status, &moloch.error) {
            (Some(_), _) => "Connected.".to_owned(),
            (None, Some(error)) => error.clone(),
            (None, None) => "Checking…".to_owned(),
        };
        body.push(page::row("Now", &status, appearance));
        page::frame(
            "Connection",
            Vec::new(),
            vec![Container::new({
                let mut column = Flex::column().with_spacing(12.);
                for block in body {
                    column.add_child(block);
                }
                column.finish()
            })
            .finish()],
            &self.scroll,
            appearance,
        )
    }
}

impl TypedActionView for Connection {
    type Action = MolochAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            MolochAction::Save => self.save(ctx),
            other => handle_common(other, ctx),
        }
    }
}
