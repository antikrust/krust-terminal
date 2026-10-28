//! The agent queue: tasks that run unattended, several at once, each on its own free model.
//!
//! A job like reversing a plugin splits into many small tasks that depend on nothing else (add
//! one lifter rule, build one widget) and whose result a script can check. They are worth
//! spreading across the failover list: each task goes to the first model of that list that is
//! switched on, isn't known to be down and isn't already busy with another task, so a free
//! tier's rate limit holds up one task instead of all of them. A task that fails on a model goes
//! back in the queue for another one.
//!
//! A task can also be pinned to one model (`model` on the task, or the queue's default model in
//! Settings): it then runs there and nowhere else, retried there on failure. Workers are API and
//! Ollama models through the API runner, or the local Claude Code and Codex apps run headless.
//!
//! Tasks are added by hand or loaded from a plan file (`notes/plan.json`, written by
//! tools/plan.py in reverse-lab): every task of a lane marked `"spawn": true`. A task runs as a
//! headless turn of the API runner with the agent's own tools, in the plan's working directory;
//! what it answered, or why it failed, stays on the task. The queue is saved, so tasks that were
//! waiting when Krust closed are still there.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ai::agent::{ToolCallOutcome, TurnEvent};
use krust_multi_agent_api as api;
use krustui::{Entity, ModelContext, SingletonEntity};
use settings::Setting as _;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ai::agent::conversation::{AIConversation, AIConversationId};
use crate::ai::agent_sdk::driver::harness::api_stream_exec::{fail_fast, run_api_stream};
use crate::ai::agent_sdk::driver::harness::claude_stream_exec::cancel_harness_session;
use crate::ai::agent_sdk::driver::harness::turn_events_to_conversation::{
    turn_event_to_messages, user_query_message,
};
use crate::ai::agent_sdk::driver::harness::cli_failover::run_handed_off_turn;
use crate::ai::api_providers::{
    agent_pool, cli_failover_models, cli_model_id_for, cli_model_parts, cli_route_for,
    is_free_model, model_tier, pool_targets, route_for, route_strength, ApiRoute, CliRoute,
    FailoverTarget,
};
use crate::ai::failover_health::{FailoverHealth, Health};
use crate::ai::task_router::{
    choose, decide, speed, strength_with_tier, Candidate, Difficulty, Policy,
};
use crate::settings::AISettings;

/// Tasks running at once, at most, however long the failover list is.
const MAX_RUNNING: usize = 8;
/// Models one task is tried on before it is given up.
const MAX_ATTEMPTS: usize = 3;
/// How much of a task's answer is kept.
const ANSWER_CHARS: usize = 1_200;
/// Why an auto task waits when there is nothing to run it on.
const NO_POOL: &str = "no models to run on: switch an agent model on (Settings → AI → Agents), \
    add a provider key or an Ollama model, or pin the task to a model such as cli/claude/default";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    /// Waiting for a free model.
    Waiting,
    /// Running on `model` (the model as the user knows it).
    Running { model: String },
    Done { model: String, answer: String },
    Failed { error: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueueTask {
    pub id: u64,
    /// Where the task came from, so loading a plan twice doesn't add it twice.
    pub key: String,
    pub title: String,
    pub prompt: String,
    pub working_dir: PathBuf,
    pub state: TaskState,
    /// Models (by picker id) the task already failed on.
    #[serde(default)]
    pub tried: Vec<String>,
    /// Why the last model failed it.
    #[serde(default)]
    pub last_error: Option<String>,
    /// How hard the decider rates the task, and what it went on. Set when the task is first
    /// scheduled; every failed model raises the tier it is sent to by one.
    #[serde(default)]
    pub difficulty: Option<Difficulty>,
    #[serde(default)]
    pub reason: String,
    /// The model this task must run on (a picker id), or `"auto"` for the chooser; `None` takes
    /// the queue's default model (Settings).
    #[serde(default)]
    pub model: Option<String>,
    /// The agent pool the auto chooser is restricted to (a pool name), or `None`/empty for the whole
    /// on-set. Ignored when the task is pinned to one model.
    #[serde(default)]
    pub pool: Option<String>,
    /// What the task has said and done, live while it runs. Not saved.
    #[serde(skip)]
    pub transcript: Vec<TranscriptLine>,
    /// The events behind the transcript, to rebuild the conversation a tab shows. Not saved.
    #[serde(skip)]
    events: Vec<TurnEvent>,
}

/// One line of a task's transcript, kept in memory only: what the model said, the tools it
/// called and what they answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptLine {
    pub kind: LineKind,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Said,
    Tool,
    Result,
    Error,
}

/// Lines kept per task, newest last, and how much of one.
const TRANSCRIPT_LINES: usize = 400;
/// Events kept per task, for the conversation a tab shows.
const TASK_EVENTS: usize = 800;
const LINE_CHARS: usize = 700;

/// The transcript lines an event adds, if any.
fn lines_of(event: &TurnEvent) -> Vec<TranscriptLine> {
    let line = |kind, text: &str| TranscriptLine {
        kind,
        text: first_chars(text, LINE_CHARS),
    };
    match event {
        TurnEvent::Text { text } if !text.trim().is_empty() => vec![line(LineKind::Said, text)],
        TurnEvent::ToolCallStarted { name, input, .. } => {
            vec![line(LineKind::Tool, &format!("{name} {input}"))]
        }
        TurnEvent::ToolCallResult { outcome, .. } => match outcome {
            ToolCallOutcome::Success(value) => {
                let text = value
                    .get("result")
                    .and_then(|result| result.as_str())
                    .map_or_else(|| value.to_string(), str::to_string);
                vec![line(LineKind::Result, &text)]
            }
            ToolCallOutcome::Error(message) => vec![line(LineKind::Error, message)],
            ToolCallOutcome::Cancelled => vec![line(LineKind::Error, "cancelled")],
        },
        TurnEvent::TurnCompleted {
            is_error: true,
            error_message,
            ..
        } => vec![line(
            LineKind::Error,
            error_message.as_deref().unwrap_or("the turn failed"),
        )],
        // Thinking, token deltas, plan updates and session metadata are not shown.
        _ => Vec::new(),
    }
}

/// What runs a task: a model through the API runner, or a local agent app.
#[derive(Clone)]
enum Worker {
    Api(ApiRoute),
    Cli(CliRoute),
}

impl Worker {
    fn model_id(&self) -> &str {
        match self {
            Worker::Api(route) => &route.model_id,
            Worker::Cli(route) => &route.model_id,
        }
    }

    fn model_name(&self) -> String {
        match self {
            Worker::Api(route) => route.model_name.clone(),
            Worker::Cli(route) => format!("{} on {}", route.model_name, route.app_name),
        }
    }

    /// What takes one task at a time: a provider account, or a local app.
    fn account(&self) -> String {
        match self {
            Worker::Api(route) => route.account.clone(),
            Worker::Cli(route) => format!("cli:{}", route.harness.config_name()),
        }
    }
}

/// The worker for a model id: an API or Ollama model, or a local app's (`cli/<app>/<model>`,
/// where `default` is that app's first model).
fn worker_for(model_id: &str, ctx: &ModelContext<AgentQueue>) -> Option<Worker> {
    if let Some(route) = route_for(model_id, ctx) {
        return Some(Worker::Api(route));
    }
    let (harness, model) = cli_model_parts(model_id)?;
    let resolved = if model == "default" {
        let (first, _) = cli_failover_models(harness, ctx).into_iter().next()?;
        cli_model_id_for(harness, &first)
    } else {
        model_id.to_string()
    };
    cli_route_for(&resolved, ctx).map(Worker::Cli)
}

/// What should happen to one waiting task now.
enum Pick {
    Run(Worker),
    /// Its model (or account) is busy; leave it waiting.
    Wait,
    Fail(String),
}

/// A running task's account (it takes one task at a time) and agent session, to stop it.
struct Active {
    account: String,
    session: Uuid,
}

pub enum AgentQueueEvent {
    Changed,
}

pub struct AgentQueue {
    tasks: Vec<QueueTask>,
    active: HashMap<u64, Active>,
    /// Whether waiting tasks are started. Off, running ones finish and nothing new begins.
    running: bool,
    next_id: u64,
    /// What Moloch last said about its workers (acolytes), for the panel; `None` until asked, or
    /// when no Moloch is configured.
    moloch: Option<String>,
}

impl Entity for AgentQueue {
    type Event = AgentQueueEvent;
}

impl SingletonEntity for AgentQueue {}

/// A task an orchestrating model hands to the queue (see `queue_tools`).
pub struct PlannedTask {
    pub title: String,
    pub prompt: String,
    pub working_dir: PathBuf,
    /// A model id to pin it to, `"auto"`, or `None` for the queue's default.
    pub model: Option<String>,
    /// The agent pool to draw on (a pool name), or `None`/empty for the whole on-set.
    pub pool: Option<String>,
}

/// Where a task stands, for a model waiting on its results.
pub struct TaskSnapshot {
    pub id: u64,
    pub title: String,
    pub state: TaskState,
}

/// What a finished run reports back.
struct Outcome {
    answer: String,
    error: Option<String>,
}

fn queue_file() -> PathBuf {
    krust_core::paths::state_dir().join("agent_queue.json")
}

/// The PATH a task's tools run with: the user's shell path, plus where Homebrew and user tools live.
fn tools_path() -> String {
    let mut path = krust_util::path::krust_shell_path()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let mut dirs = vec!["/opt/homebrew/bin".to_string(), "/usr/local/bin".to_string()];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(format!("{}/.local/bin", home.to_string_lossy()));
    }
    for dir in dirs {
        if !path.split(':').any(|segment| segment == dir) {
            path = if path.is_empty() {
                dir
            } else {
                format!("{dir}:{path}")
            };
        }
    }
    path
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn first_chars(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{cut}…")
}

#[derive(Deserialize)]
struct PlanFile {
    #[serde(default)]
    workdir: Option<String>,
    #[serde(default)]
    lanes: Vec<PlanLane>,
}

#[derive(Deserialize)]
struct PlanLane {
    #[serde(default)]
    spawn: bool,
    #[serde(default)]
    tasks: Vec<PlanTask>,
}

#[derive(Deserialize)]
struct PlanTask {
    id: String,
    title: String,
    #[serde(default)]
    prompt: String,
}

impl AgentQueue {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        // Claude Code and Codex reach plan_tasks / collect_results through an MCP server that
        // talks to this socket (see `queue_tools`).
        crate::ai::queue_tools::serve_socket(ctx.spawner());
        let mut tasks: Vec<QueueTask> = std::fs::read_to_string(queue_file())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        // Nothing is running after a restart: what was, waits again.
        for task in &mut tasks {
            if matches!(task.state, TaskState::Running { .. }) {
                task.state = TaskState::Waiting;
            }
        }
        let next_id = tasks.iter().map(|task| task.id).max().map_or(1, |id| id + 1);
        Self {
            tasks,
            active: HashMap::new(),
            running: false,
            next_id,
            moloch: None,
        }
    }

    /// What Moloch last said about its connected workers, for the panel.
    pub fn moloch_status(&self) -> Option<&str> {
        self.moloch.as_deref()
    }

    /// Asks the configured Moloch (`moloch.env`) which workers are connected and what models they
    /// serve. In the background; the panel follows. Nothing happens when no Moloch is configured.
    pub fn refresh_moloch(&mut self, ctx: &mut ModelContext<Self>) {
        let Some(config) = crate::moloch::config() else {
            self.moloch = None;
            return;
        };
        let url = format!("{}/api/v1/agent/connected-self-hosted-workers", config.url);
        let token = config.token.clone();
        let shown_url = config.url.clone();
        ctx.spawn(
            async move {
                let mut request = reqwest::Client::new()
                    .get(url)
                    .timeout(std::time::Duration::from_secs(5));
                if let Some(token) = token {
                    request = request.bearer_auth(token);
                }
                let response = request.send().await?.error_for_status()?;
                anyhow::Ok(response.json::<serde_json::Value>().await?)
            },
            move |me, result, ctx| {
                me.moloch = Some(match result {
                    Ok(body) => {
                        let workers: Vec<String> = body["workers"]
                            .as_array()
                            .map(|workers| {
                                workers
                                    .iter()
                                    .map(|worker| {
                                        let models = worker["models"]
                                            .as_array()
                                            .map_or(0, |models| models.len());
                                        format!(
                                            "{} ({models} models)",
                                            worker["id"].as_str().unwrap_or("?")
                                        )
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        if workers.is_empty() {
                            format!("Moloch at {shown_url}: up, no workers connected")
                        } else {
                            format!("Moloch at {shown_url}: {} connected", workers.join(", "))
                        }
                    }
                    Err(error) => format!("Moloch at {shown_url}: not answering ({error})"),
                });
                ctx.emit(AgentQueueEvent::Changed);
            },
        );
    }

    pub fn tasks(&self) -> &[QueueTask] {
        &self.tasks
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// (waiting, running, done, failed)
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut counts = (0, 0, 0, 0);
        for task in &self.tasks {
            match task.state {
                TaskState::Waiting => counts.0 += 1,
                TaskState::Running { .. } => counts.1 += 1,
                TaskState::Done { .. } => counts.2 += 1,
                TaskState::Failed { .. } => counts.3 += 1,
            }
        }
        counts
    }

    fn save(&self) {
        match serde_json::to_string_pretty(&self.tasks) {
            Ok(text) => {
                if let Err(error) = std::fs::write(queue_file(), text) {
                    log::warn!("agent_queue: could not save the queue: {error}");
                }
            }
            Err(error) => log::warn!("agent_queue: could not serialize the queue: {error}"),
        }
    }

    fn changed(&self, ctx: &mut ModelContext<Self>) {
        self.save();
        ctx.emit(AgentQueueEvent::Changed);
    }

    /// Adds a task. Returns false for an empty prompt.
    pub fn add(&mut self, prompt: &str, ctx: &mut ModelContext<Self>) -> bool {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return false;
        }
        self.push(
            format!("typed:{}", self.next_id),
            first_chars(prompt.lines().next().unwrap_or(prompt), 80),
            prompt.to_string(),
            home_dir(),
            None,
            None,
        );
        self.changed(ctx);
        self.pump(ctx);
        true
    }

    fn push(
        &mut self,
        key: String,
        title: String,
        prompt: String,
        working_dir: PathBuf,
        model: Option<String>,
        pool: Option<String>,
    ) {
        self.tasks.push(QueueTask {
            id: self.next_id,
            key,
            title,
            prompt,
            working_dir,
            state: TaskState::Waiting,
            tried: Vec::new(),
            last_error: None,
            difficulty: None,
            reason: String::new(),
            model,
            pool,
            transcript: Vec::new(),
            events: Vec::new(),
        });
        self.next_id += 1;
    }

    /// Adds the tasks of every `"spawn": true` lane of a plan file, except those already in the
    /// queue. Returns how many were added.
    pub fn load_plan(&mut self, path: &Path, ctx: &mut ModelContext<Self>) -> Result<usize, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
        let plan: PlanFile = serde_json::from_str(&text)
            .map_err(|error| format!("{} is not a plan file: {error}", path.display()))?;
        let working_dir = plan
            .workdir
            .map(PathBuf::from)
            .or_else(|| path.parent().map(Path::to_path_buf))
            .unwrap_or_else(home_dir);
        let mut added = 0;
        for lane in plan.lanes.into_iter().filter(|lane| lane.spawn) {
            for task in lane.tasks {
                let key = format!("plan:{}:{}", path.display(), task.id);
                if task.prompt.trim().is_empty() || self.tasks.iter().any(|known| known.key == key) {
                    continue;
                }
                self.push(key, task.title, task.prompt, working_dir.clone(), None, None);
                added += 1;
            }
        }
        if added > 0 {
            // Spawn mode: don't wait for the boss to start the queue — fan the plan out at once.
            if *AISettings::as_ref(ctx).queue_spawn_mode.value() {
                self.running = true;
            }
            self.changed(ctx);
            self.pump(ctx);
        }
        Ok(added)
    }

    /// Queues the tasks a model planned and starts the queue. Returns their ids.
    pub fn add_planned(&mut self, tasks: Vec<PlannedTask>, ctx: &mut ModelContext<Self>) -> Vec<u64> {
        let mut ids = Vec::new();
        for task in tasks {
            if task.prompt.trim().is_empty() {
                continue;
            }
            ids.push(self.next_id);
            self.push(
                format!("planned:{}", self.next_id),
                first_chars(&task.title, 80),
                task.prompt,
                task.working_dir,
                task.model,
                task.pool,
            );
        }
        self.running = true;
        self.changed(ctx);
        self.pump(ctx);
        ids
    }

    /// Where the tasks `ids` stand now. A task taken off the queue is left out.
    pub fn snapshot(&self, ids: &[u64]) -> Vec<TaskSnapshot> {
        self.tasks
            .iter()
            .filter(|task| ids.contains(&task.id))
            .map(|task| TaskSnapshot {
                id: task.id,
                title: task.title.clone(),
                state: task.state.clone(),
            })
            .collect()
    }

    pub fn remove(&mut self, id: u64, ctx: &mut ModelContext<Self>) {
        let before = self.tasks.len();
        // A running task isn't removed: stop the queue first.
        self.tasks
            .retain(|task| task.id != id || matches!(task.state, TaskState::Running { .. }));
        if self.tasks.len() != before {
            self.changed(ctx);
        }
    }

    /// Takes finished and failed tasks off the list.
    pub fn clear_finished(&mut self, ctx: &mut ModelContext<Self>) {
        let before = self.tasks.len();
        self.tasks
            .retain(|task| !matches!(task.state, TaskState::Done { .. } | TaskState::Failed { .. }));
        if self.tasks.len() != before {
            self.changed(ctx);
        }
    }

    /// Puts a failed task back in the queue, with every model free to try again.
    pub fn retry(&mut self, id: u64, ctx: &mut ModelContext<Self>) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
            if matches!(task.state, TaskState::Failed { .. }) {
                task.state = TaskState::Waiting;
                task.tried.clear();
                task.last_error = None;
                self.changed(ctx);
                self.pump(ctx);
            }
        }
    }

    /// Starts the queue, or stops it: running tasks are stopped and wait again.
    pub fn set_running(&mut self, running: bool, ctx: &mut ModelContext<Self>) {
        self.running = running;
        if running {
            self.refresh_moloch(ctx);
            self.pump(ctx);
        } else {
            for active in self.active.values() {
                cancel_harness_session(&active.session);
            }
        }
        ctx.emit(AgentQueueEvent::Changed);
    }

    /// Starts waiting tasks. An auto task goes to the model the chooser picks among the failover
    /// rows that are on, not down, not busy with another task and not already tried; a tier up for
    /// each model it failed on. A pinned task (its own model, or the queue's default) runs only on
    /// that model and that app's account, retried there up to [`MAX_ATTEMPTS`]. A task that runs
    /// nowhere left is given up.
    fn pump(&mut self, ctx: &mut ModelContext<Self>) {
        if !self.running {
            return;
        }
        // The agent models that are on (Settings → Agents): API and Ollama routes, and Claude
        // Code / Codex when switched on.
        let pool: Vec<Worker> = agent_pool(ctx)
            .into_iter()
            .map(|target| match target {
                FailoverTarget::Api(route) => Worker::Api(route),
                FailoverTarget::Cli(route) => Worker::Cli(route),
            })
            .collect();
        let health_of = |worker: &Worker| match worker {
            Worker::Api(route) => FailoverHealth::as_ref(ctx).health(&route.model_id, &route.account),
            Worker::Cli(_) => Health::Unknown,
        };
        let down: HashSet<String> = pool
            .iter()
            .filter(|worker| health_of(worker) == Health::Down)
            .map(|worker| worker.model_id().to_string())
            .collect();
        let struggling: HashSet<String> = pool
            .iter()
            .filter(|worker| health_of(worker) == Health::Degraded)
            .map(|worker| worker.model_id().to_string())
            .collect();
        let strengths: HashMap<String, (f64, f64, bool)> = pool
            .iter()
            .map(|worker| {
                let (strength, model) = match worker {
                    Worker::Api(route) => (route_strength(route, ctx), route.model.as_str()),
                    Worker::Cli(route) => (
                        strength_with_tier(
                            &route.model,
                            None,
                            None,
                            model_tier(&route.model_id, ctx),
                        ),
                        route.model.as_str(),
                    ),
                };
                let id = worker.model_id().to_string();
                let free = is_free_model(&id);
                (id, (strength, speed(model), free))
            })
            .collect();
        let default_model = AISettings::as_ref(ctx)
            .byor_queue_default_model
            .value()
            .clone();
        let waiting: Vec<u64> = self
            .tasks
            .iter()
            .filter(|task| task.state == TaskState::Waiting)
            .map(|task| task.id)
            .collect();
        let mut gave_up = false;
        for id in waiting {
            if self.active.len() >= MAX_RUNNING {
                break;
            }
            let busy: HashSet<String> =
                self.active.values().map(|active| active.account.clone()).collect();
            let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) else {
                continue;
            };
            if task.difficulty.is_none() {
                let decision = decide(&task.prompt);
                task.difficulty = Some(decision.difficulty);
                task.reason = decision.reason;
            }
            let tried = task.tried.clone();
            let base = task.difficulty.unwrap_or(Difficulty::Medium);
            let last_error = task.last_error.clone();
            // The model to run on: the task's own, else the queue default; both empty, or "auto",
            // means let the chooser pick across the failover rows.
            let pinned = task
                .model
                .clone()
                .filter(|model| !model.trim().is_empty())
                .or_else(|| (!default_model.trim().is_empty()).then(|| default_model.clone()))
                .filter(|model| model != "auto");
            // The ids this task's pool allows, if it names one; `None` means the whole on-set.
            let pool_ids: Option<Vec<String>> = task
                .pool
                .clone()
                .filter(|name| !name.trim().is_empty())
                .map(|name| {
                    pool_targets(ctx, &name)
                        .iter()
                        .map(|target| target.model_id().to_string())
                        .collect()
                });

            let pick = if let Some(model_id) = pinned {
                if tried.len() >= MAX_ATTEMPTS {
                    Pick::Fail(
                        last_error.unwrap_or_else(|| format!("{model_id} could not run the task")),
                    )
                } else {
                    match worker_for(&model_id, ctx) {
                        None => Pick::Fail(format!(
                            "no model {model_id:?} is available (a key removed, or the app not running)"
                        )),
                        Some(worker) if busy.contains(&worker.account()) => Pick::Wait,
                        Some(worker) => Pick::Run(worker),
                    }
                }
            } else {
                let exhausted = !tried.is_empty()
                    && (tried.len() >= MAX_ATTEMPTS
                        || pool.iter().all(|worker| tried.iter().any(|t| t == worker.model_id())));
                if exhausted {
                    Pick::Fail(last_error.unwrap_or_else(|| "no model could answer".to_string()))
                } else {
                    let mut difficulty = base;
                    for _ in &tried {
                        difficulty = difficulty.harder();
                    }
                    let mut available: Vec<&Worker> = pool
                        .iter()
                        .filter(|worker| {
                            let id = worker.model_id();
                            !busy.contains(&worker.account())
                                && !down.contains(id)
                                && !tried.iter().any(|t| t == id)
                                && pool_ids.as_ref().map_or(true, |ids| ids.iter().any(|x| x == id))
                        })
                        .collect();
                    // A model that was just rate-limited or flaky (amber) is passed over while
                    // another one can take the task.
                    if available.iter().any(|worker| !struggling.contains(worker.model_id())) {
                        available.retain(|worker| !struggling.contains(worker.model_id()));
                    }
                    let candidates: Vec<Candidate> = available
                        .iter()
                        .map(|worker| {
                            let (strength, speed, free) = strengths
                                .get(worker.model_id())
                                .copied()
                                .unwrap_or((4.0, 0.05, false));
                            Candidate { strength, speed, free }
                        })
                        .collect();
                    match choose(Policy::CostEfficient, difficulty, &candidates) {
                        Some(index) => Pick::Run(available[index].clone()),
                        // No pool at all (nothing switched on, no key, no Ollama model): say so on
                        // the task and keep it waiting, since models may still be loading.
                        None if pool.is_empty() => {
                            if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
                                task.last_error = Some(NO_POOL.to_string());
                            }
                            Pick::Wait
                        }
                        None => Pick::Wait,
                    }
                }
            };

            match pick {
                Pick::Run(worker) => self.start(id, worker, ctx),
                Pick::Wait => {}
                Pick::Fail(error) => {
                    if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
                        task.state = TaskState::Failed { error };
                    }
                    gave_up = true;
                }
            }
        }
        if gave_up {
            self.changed(ctx);
        }
    }

    /// Runs task `id` on `worker`: a headless turn on a background thread, its events streamed to
    /// the main thread. No failover of its own — the queue picks the next model when it fails.
    fn start(&mut self, id: u64, worker: Worker, ctx: &mut ModelContext<Self>) {
        let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) else {
            return;
        };
        let session = Uuid::new_v4();
        task.state = TaskState::Running {
            model: worker.model_name(),
        };
        task.transcript.clear();
        task.events.clear();
        let prompt = task.prompt.clone();
        let working_dir = task.working_dir.clone();
        let model_id = worker.model_id().to_string();
        let model_name = worker.model_name();
        self.active.insert(
            id,
            Active {
                account: worker.account(),
                session,
            },
        );

        // The turn's events come to the main thread as lines of the task's transcript; its end
        // comes as one outcome.
        let (done_tx, done_rx) = futures::channel::oneshot::channel::<Outcome>();
        let (line_tx, line_rx) = futures::channel::mpsc::unbounded::<TurnEvent>();
        ctx.spawn_stream_local(
            line_rx,
            move |me, event, ctx| me.append_event(id, event, ctx),
            |_, _| {},
        );
        ctx.background_executor()
            .spawn(async move {
                let answer = Arc::new(Mutex::new(String::new()));
                let error = Arc::new(Mutex::new(None::<String>));
                let (sink_answer, sink_error) = (Arc::clone(&answer), Arc::clone(&error));
                // Every event a run emits: stream the shown ones to the transcript, and keep the
                // last assistant text as the answer and a failed completion as the error.
                let on_event = move |event: TurnEvent| {
                    if !lines_of(&event).is_empty() {
                        let _ = line_tx.unbounded_send(event.clone());
                    }
                    match event {
                        TurnEvent::Text { text } => {
                            if !text.trim().is_empty() {
                                if let Ok(mut answer) = sink_answer.lock() {
                                    *answer = text;
                                }
                            }
                        }
                        TurnEvent::TurnCompleted {
                            is_error: true,
                            error_message,
                            ..
                        } => {
                            if let Ok(mut error) = sink_error.lock() {
                                *error = Some(
                                    error_message.unwrap_or_else(|| "the turn failed".to_string()),
                                );
                            }
                        }
                        _ => {}
                    }
                };
                let result = match worker {
                    Worker::Api(route) => {
                        // A rate limit or a request too big for this model fails the task at once;
                        // the queue then hands it to another model instead of the turn waiting.
                        fail_fast(&session);
                        run_api_stream(
                            &prompt,
                            route,
                            &session,
                            &working_dir,
                            &tools_path(),
                            &[],
                            None,
                            Vec::new(),
                            on_event,
                        )
                        .await
                        .map(|_| ())
                    }
                    // Claude Code / Codex: the same headless run BYOR failover hands a turn to,
                    // with one target and no next.
                    Worker::Cli(route) => {
                        run_handed_off_turn(
                            vec![route],
                            &prompt,
                            &working_dir,
                            &tools_path(),
                            move |_, _, event| on_event(event),
                        )
                        .await
                    }
                };
                let mut outcome = Outcome {
                    answer: answer.lock().map(|answer| answer.clone()).unwrap_or_default(),
                    error: error.lock().ok().and_then(|error| error.clone()),
                };
                if let Err(error) = result {
                    outcome.error.get_or_insert_with(|| error.to_string());
                }
                let _ = done_tx.send(outcome);
            })
            .detach();
        ctx.spawn(
            async move {
                done_rx.await.unwrap_or(Outcome {
                    answer: String::new(),
                    error: Some("the task's worker stopped".to_string()),
                })
            },
            move |me, outcome, ctx| me.finish(id, model_id, model_name, outcome, ctx),
        );
        self.changed(ctx);
    }

    /// An event of a running task arrived: it adds to the transcript and to the events a tab shows.
    fn append_event(&mut self, id: u64, event: TurnEvent, ctx: &mut ModelContext<Self>) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
            task.transcript.extend(lines_of(&event));
            if task.transcript.len() > TRANSCRIPT_LINES {
                let extra = task.transcript.len() - TRANSCRIPT_LINES;
                task.transcript.drain(..extra);
            }
            task.events.push(event);
            if task.events.len() > TASK_EVENTS {
                let extra = task.events.len() - TASK_EVENTS;
                task.events.drain(..extra);
            }
            // Not saved: only the views follow it.
            ctx.emit(AgentQueueEvent::Changed);
        }
    }

    /// The conversation a task has had, to show in a view-only tab: the task as the user message,
    /// then what the model said and did. Built in memory from the task's events (or, for a task
    /// from an earlier run, its answer or error), and never saved with the user's conversations.
    pub fn transcript_conversation(&self, id: u64) -> Option<AIConversation> {
        let task = self.tasks.iter().find(|task| task.id == id)?;
        let task_id = Uuid::new_v4().to_string();
        let request_id = Uuid::new_v4().to_string();
        let mut messages = vec![user_query_message(&task_id, &request_id, &task.prompt)];
        let mut shell_commands = HashMap::new();
        let mut events = task.events.clone();
        if events.is_empty() {
            match &task.state {
                TaskState::Done { answer, .. } if !answer.is_empty() => events.push(TurnEvent::Text {
                    text: answer.clone(),
                }),
                TaskState::Failed { error } => events.push(TurnEvent::Text {
                    text: format!("**Failed:** {error}"),
                }),
                TaskState::Waiting | TaskState::Running { .. } | TaskState::Done { .. } => {}
            }
        }
        for event in &events {
            messages.extend(turn_event_to_messages(&task_id, &request_id, event, &mut shell_commands));
        }
        let api_task = api::Task {
            id: task_id,
            messages,
            ..Default::default()
        };
        AIConversation::new_restored(AIConversationId::new(), vec![api_task], None).ok()
    }

    /// A task's run ended: done, or back in the queue for another model, or waiting again when
    /// the queue was stopped under it.
    fn finish(
        &mut self,
        id: u64,
        model_id: String,
        model_name: String,
        outcome: Outcome,
        ctx: &mut ModelContext<Self>,
    ) {
        self.active.remove(&id);
        let stopped = !self.running;
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
            if stopped {
                task.state = TaskState::Waiting;
            } else if let Some(error) = outcome.error {
                task.tried.push(model_id);
                task.last_error = Some(first_chars(&error, 300));
                task.state = TaskState::Waiting;
            } else {
                task.state = TaskState::Done {
                    model: model_name,
                    answer: first_chars(&outcome.answer, ANSWER_CHARS),
                };
            }
        }
        self.changed(ctx);
        self.pump(ctx);
    }
}
