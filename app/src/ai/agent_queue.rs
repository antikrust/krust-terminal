//! The agent queue: tasks that run unattended, several at once, each on its own free model.
//!
//! A job like reversing a plugin splits into many small tasks that depend on nothing else (add
//! one lifter rule, build one widget) and whose result a script can check. They are worth
//! spreading across the failover list: each task goes to the first model of that list that is
//! switched on, isn't known to be down and isn't already busy with another task, so a free
//! tier's rate limit holds up one task instead of all of them. A task that fails on a model goes
//! back in the queue for another one.
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
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ai::agent::conversation::{AIConversation, AIConversationId};
use crate::ai::agent_sdk::driver::harness::api_stream_exec::run_api_stream;
use crate::ai::agent_sdk::driver::harness::claude_stream_exec::cancel_harness_session;
use crate::ai::agent_sdk::driver::harness::turn_events_to_conversation::{
    turn_event_to_messages, user_query_message,
};
use crate::ai::api_providers::{
    failover_routes, is_free_model, model_context_window, model_price, FailoverTarget,
};
use crate::ai::failover_health::{FailoverHealth, Health};
use crate::ai::task_router::{choose, decide, speed, strength, Candidate, Difficulty, Policy};

/// Tasks running at once, at most, however long the failover list is.
const MAX_RUNNING: usize = 8;
/// Models one task is tried on before it is given up.
const MAX_ATTEMPTS: usize = 3;
/// How much of a task's answer is kept.
const ANSWER_CHARS: usize = 1_200;

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
    pub fn new() -> Self {
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
        }
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
        );
        self.changed(ctx);
        self.pump(ctx);
        true
    }

    fn push(&mut self, key: String, title: String, prompt: String, working_dir: PathBuf) {
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
                self.push(key, task.title, task.prompt, working_dir.clone());
                added += 1;
            }
        }
        if added > 0 {
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
            self.pump(ctx);
        } else {
            for active in self.active.values() {
                cancel_harness_session(&active.session);
            }
        }
        ctx.emit(AgentQueueEvent::Changed);
    }

    /// Starts waiting tasks on free models: each on the first row of the failover list that is on,
    /// isn't known to be down, isn't busy with another task and hasn't failed this task. A task
    /// every model has failed (or that failed on [`MAX_ATTEMPTS`]) is given up.
    fn pump(&mut self, ctx: &mut ModelContext<Self>) {
        if !self.running {
            return;
        }
        let routes: Vec<_> = failover_routes("", ctx)
            .into_iter()
            .filter_map(|target| match target {
                FailoverTarget::Api(route) => Some(route),
                // A local agent app can't be given a task headlessly here.
                FailoverTarget::Cli(_) => None,
            })
            .collect();
        let down: HashSet<String> = routes
            .iter()
            .filter(|route| {
                FailoverHealth::as_ref(ctx).health(&route.model_id, &route.account) == Health::Down
            })
            .map(|route| route.model_id.clone())
            .collect();
        let waiting: Vec<u64> = self
            .tasks
            .iter()
            .filter(|task| task.state == TaskState::Waiting)
            .map(|task| task.id)
            .collect();
        let mut gave_up = false;
        for id in waiting {
            let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) else {
                continue;
            };
            let exhausted = !task.tried.is_empty()
                && (task.tried.len() >= MAX_ATTEMPTS
                    || routes.iter().all(|route| task.tried.contains(&route.model_id)));
            if exhausted {
                task.state = TaskState::Failed {
                    error: task
                        .last_error
                        .clone()
                        .unwrap_or_else(|| "no model could answer".to_string()),
                };
                gave_up = true;
                continue;
            }
            if self.active.len() >= MAX_RUNNING {
                continue;
            }
            // The decider rates the task once; a task that already failed on a model goes a tier
            // up for each one.
            if task.difficulty.is_none() {
                let decision = decide(&task.prompt);
                task.difficulty = Some(decision.difficulty);
                task.reason = decision.reason;
            }
            let mut difficulty = task.difficulty.unwrap_or(Difficulty::Medium);
            for _ in &task.tried {
                difficulty = difficulty.harder();
            }
            let busy: HashSet<&String> = self.active.values().map(|active| &active.account).collect();
            let available: Vec<&crate::ai::api_providers::ApiRoute> = routes
                .iter()
                .filter(|route| {
                    !busy.contains(&route.account)
                        && !down.contains(&route.model_id)
                        && !task.tried.contains(&route.model_id)
                })
                .collect();
            let candidates: Vec<Candidate> = available
                .iter()
                .map(|route| Candidate {
                    strength: strength(
                        &route.model,
                        model_price(route.provider, &route.model),
                        model_context_window(route.provider, &route.model),
                    ),
                    speed: speed(&route.model),
                    free: is_free_model(&route.model_id),
                })
                .collect();
            let pick = choose(Policy::CostEfficient, difficulty, &candidates)
                .map(|index| available[index].clone());
            if let Some(route) = pick {
                self.start(id, route, ctx);
            }
        }
        if gave_up {
            self.changed(ctx);
        }
    }

    /// Runs task `id` on `route`: a headless turn of the API runner, on a background thread, with
    /// no failover of its own (the queue picks the next model).
    fn start(&mut self, id: u64, route: crate::ai::api_providers::ApiRoute, ctx: &mut ModelContext<Self>) {
        let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) else {
            return;
        };
        let session = Uuid::new_v4();
        task.state = TaskState::Running {
            model: route.model_name.clone(),
        };
        task.transcript.clear();
        task.events.clear();
        let prompt = task.prompt.clone();
        let working_dir = task.working_dir.clone();
        let (model_id, model_name) = (route.model_id.clone(), route.model_name.clone());
        self.active.insert(
            id,
            Active {
                account: route.account.clone(),
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
                let result = run_api_stream(
                    &prompt,
                    route,
                    &session,
                    &working_dir,
                    &tools_path(),
                    &[],
                    None,
                    Vec::new(),
                    move |event| {
                        if !lines_of(&event).is_empty() {
                            let _ = line_tx.unbounded_send(event.clone());
                        }
                        match event {
                            // What the model says last is its answer.
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
                            // The rest only shows in the transcript, if at all.
                            _ => {}
                        }
                    },
                )
                .await;
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
