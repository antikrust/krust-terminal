//! The orchestrator's tools: how a model hands work to the agent queue and waits for it.
//!
//! This is the local counterpart of Warp's `run_agents`: one model (the strongest available, or any
//! the user picked) splits a goal into tasks that need nothing from each other, `plan_tasks` puts
//! them in the queue, where each goes to the model the decider picks for its difficulty, and
//! `collect_results` waits for them and returns what they answered. The tools are offered to the
//! API and Ollama tool loops beside the MCP tools; `QueueTools` answers them and passes any other
//! call on.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use krustui::r#async::Timer;
use krustui::ModelSpawner;
use serde_json::{json, Value};

use crate::ai::agent_queue::{AgentQueue, PlannedTask, TaskState};
use crate::ai::agent_sdk::driver::harness::ollama_stream_exec::{McpToolCaller, McpToolSpec};

pub(crate) const PLAN_TASKS: &str = "plan_tasks";
pub(crate) const COLLECT_RESULTS: &str = "collect_results";

/// How often `collect_results` looks, and the longest it waits unless asked for less.
const POLL: Duration = Duration::from_secs(3);
const DEFAULT_WAIT_SECS: u64 = 900;
const MAX_WAIT_SECS: u64 = 3_600;
/// What a result shows of one task's answer.
const ANSWER_CHARS: usize = 1_200;

pub(crate) fn tool_specs() -> Vec<McpToolSpec> {
    vec![
        McpToolSpec {
            name: PLAN_TASKS.to_string(),
            description: "Hand work to other agents. Use it for a goal that splits into pieces that \
                need nothing from each other (one unit each, one file each, one widget each); do \
                the pieces that depend on each other yourself. Each task is a complete brief of its \
                own: the agent that takes it has none of this conversation, so give the files, \
                what done looks like and how to check it. Say how hard each is (easy, medium or \
                hard) and the queue sends it to a model that fits. Returns the tasks' ids; pass \
                them to collect_results."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "tasks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string", "description": "A few words." },
                                "prompt": { "type": "string", "description": "The whole brief." },
                                "difficulty": {
                                    "type": "string",
                                    "enum": ["easy", "medium", "hard"],
                                    "description": "How hard the task is (optional; judged from the brief when left out)."
                                }
                            },
                            "required": ["title", "prompt"]
                        }
                    },
                    "workdir": {
                        "type": "string",
                        "description": "The directory the tasks work in (absolute path)."
                    }
                },
                "required": ["tasks", "workdir"]
            }),
        },
        McpToolSpec {
            name: COLLECT_RESULTS.to_string(),
            description: "Wait for tasks handed out with plan_tasks and return what each answered, \
                or why it failed. Returns early if `wait_seconds` pass; the tasks still running \
                are listed, and you can call it again."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ids": { "type": "array", "items": { "type": "integer" } },
                    "wait_seconds": {
                        "type": "integer",
                        "description": "The longest to wait (optional; 900 by default, 3600 at most)."
                    }
                },
                "required": ["ids"]
            }),
        },
    ]
}

/// Answers `plan_tasks` and `collect_results` through the queue's own thread, and gives any other
/// call to `inner`, the MCP servers' caller.
pub(crate) struct QueueTools {
    pub(crate) spawner: ModelSpawner<AgentQueue>,
    pub(crate) inner: Option<Arc<dyn McpToolCaller>>,
}

#[async_trait]
impl McpToolCaller for QueueTools {
    async fn call(&self, name: &str, arguments: Value) -> Result<String, String> {
        match name {
            PLAN_TASKS => self.plan(&arguments).await,
            COLLECT_RESULTS => self.collect(&arguments).await,
            other => match &self.inner {
                Some(inner) => inner.call(other, arguments).await,
                None => Err(format!("no tool named '{other}' is available")),
            },
        }
    }
}

impl QueueTools {
    async fn plan(&self, arguments: &Value) -> Result<String, String> {
        let workdir = arguments
            .get("workdir")
            .and_then(Value::as_str)
            .filter(|dir| !dir.trim().is_empty())
            .map(PathBuf::from)
            .ok_or("plan_tasks needs the `workdir` the tasks work in.")?;
        let planned: Vec<PlannedTask> = arguments
            .get("tasks")
            .and_then(Value::as_array)
            .ok_or("plan_tasks needs a `tasks` list.")?
            .iter()
            .filter_map(|task| {
                let prompt = task.get("prompt")?.as_str()?.trim();
                if prompt.is_empty() {
                    return None;
                }
                let title = task.get("title").and_then(Value::as_str).unwrap_or(prompt);
                // The difficulty goes in the brief, where the decider reads it.
                let prompt = match task.get("difficulty").and_then(Value::as_str) {
                    Some(level @ ("easy" | "medium" | "hard")) => {
                        format!("difficulty: {level}\n{prompt}")
                    }
                    _ => prompt.to_string(),
                };
                Some(PlannedTask {
                    title: title.to_string(),
                    prompt,
                    working_dir: workdir.clone(),
                })
            })
            .collect();
        if planned.is_empty() {
            return Err("plan_tasks needs at least one task with a `prompt`.".to_string());
        }
        let titles: Vec<String> = planned.iter().map(|task| task.title.clone()).collect();
        let ids = self
            .spawner
            .spawn(move |queue, ctx| queue.add_planned(planned, ctx))
            .await
            .map_err(|_| "the agent queue is not available".to_string())?;
        let listed: Vec<String> = ids
            .iter()
            .zip(titles)
            .map(|(id, title)| format!("#{id} {title}"))
            .collect();
        Ok(format!(
            "Queued {} task(s) and started the queue: {}. Call collect_results with ids {:?} to wait for them.",
            ids.len(),
            listed.join("; "),
            ids
        ))
    }

    async fn collect(&self, arguments: &Value) -> Result<String, String> {
        let ids: Vec<u64> = arguments
            .get("ids")
            .and_then(Value::as_array)
            .ok_or("collect_results needs the `ids` plan_tasks returned.")?
            .iter()
            .filter_map(Value::as_u64)
            .collect();
        if ids.is_empty() {
            return Err("collect_results needs at least one id.".to_string());
        }
        let wait = arguments
            .get("wait_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_WAIT_SECS)
            .min(MAX_WAIT_SECS);
        let mut waited = Duration::ZERO;
        loop {
            let wanted = ids.clone();
            let snapshot = self
                .spawner
                .spawn(move |queue, _| queue.snapshot(&wanted))
                .await
                .map_err(|_| "the agent queue is not available".to_string())?;
            let open = snapshot
                .iter()
                .filter(|task| matches!(task.state, TaskState::Waiting | TaskState::Running { .. }))
                .count();
            if open == 0 || waited >= Duration::from_secs(wait) {
                let mut report = String::new();
                for task in &snapshot {
                    let line = match &task.state {
                        TaskState::Done { model, answer } => {
                            let answer: String = answer.chars().take(ANSWER_CHARS).collect();
                            format!("#{} {} - done on {model}:\n{answer}", task.id, task.title)
                        }
                        TaskState::Failed { error } => {
                            format!("#{} {} - FAILED: {error}", task.id, task.title)
                        }
                        TaskState::Waiting => format!("#{} {} - still queued", task.id, task.title),
                        TaskState::Running { model } => {
                            format!("#{} {} - still running on {model}", task.id, task.title)
                        }
                    };
                    report.push_str(&line);
                    report.push_str("\n\n");
                }
                if open > 0 {
                    report.push_str(&format!("{open} task(s) have not finished; call collect_results again to keep waiting."));
                }
                return Ok(report.trim_end().to_string());
            }
            Timer::after(POLL).await;
            waited += POLL;
        }
    }
}
