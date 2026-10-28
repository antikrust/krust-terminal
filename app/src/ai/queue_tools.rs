//! The orchestrator's tools: how a model hands work to the agent queue and waits for it.
//!
//! This is the local counterpart of Warp's `run_agents`: one model (the strongest available, or any
//! the user picked) splits a goal into tasks that need nothing from each other, `plan_tasks` puts
//! them in the queue, where each goes to the model the chooser picks for its difficulty, and
//! `collect_results` waits for them and returns what they answered.
//!
//! Two ways in. The API and Ollama tool loops get the tools in-process: [`QueueTools`] answers
//! `plan_tasks` / `collect_results` and passes any other call to the MCP servers. Claude Code and
//! Codex, which bring their own tools and never see ours, reach them over an MCP server: the app
//! serves a Unix socket ([`serve_socket`]) and hands those harnesses an MCP config pointing at a
//! subprocess of this binary ([`run_queue_mcp_server`]) that bridges MCP stdio to the socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
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
pub(crate) const LOAD_PLAN: &str = "load_plan";
/// Not a model's tool: what a script sends over the socket the moment it writes a plan file
/// (reverse-lab's `reverse_engineering_auto.py`). The queue hands it out only in spawn mode.
const OFFER_PLAN: &str = "offer_plan";

/// The Unix socket the queue-tools MCP subprocess talks to the app over.
pub(crate) fn socket_path() -> PathBuf {
    krust_core::paths::state_dir().join("queue-tools.sock")
}

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
                                },
                                "model": {
                                    "type": "string",
                                    "description": "Pin this task to a model id, or \"auto\" (optional; the queue's default when left out)."
                                }
                            },
                            "required": ["title", "prompt"]
                        }
                    },
                    "workdir": {
                        "type": "string",
                        "description": "The directory the tasks work in (absolute path)."
                    },
                    "model": {
                        "type": "string",
                        "description": "Pin every task to this model id, or \"auto\" (optional; a task's own model wins)."
                    },
                    "pool": {
                        "type": "string",
                        "description": "Run these tasks on a named agent pool (e.g. \"Free\", \"Local\", \"Fast\", \"Genius\", or one you made). Optional; the whole on-set when left out. Ignored for a task pinned to one model."
                    }
                },
                "required": ["tasks", "workdir"]
            }),
        },
        McpToolSpec {
            name: LOAD_PLAN.to_string(),
            description: "Hand every task of a plan file's spawn lanes to other agents at once and \
                start the queue — the fastest way to fan a job out. The plan is the \
                notes/plan.json the reverse-lab tools write (reverse_engineering_auto.py / \
                plan.py). Tasks already in the queue are not added twice. Returns the new tasks' \
                ids; pass them to collect_results. Do the plan's serial lane yourself."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The plan file's absolute path (e.g. …/<name>/notes/plan.json)."
                    }
                },
                "required": ["path"]
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
            LOAD_PLAN => self.load(&arguments, true).await,
            OFFER_PLAN => self.load(&arguments, false).await,
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
        // A model named for the whole call is each task's default; a task's own `model` wins.
        let call_model = arguments
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|model| !model.trim().is_empty());
        // The pool these tasks draw on (every task in the call), when none is pinned to a model.
        let call_pool = arguments
            .get("pool")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|pool| !pool.trim().is_empty());
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
                let model = task
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .filter(|model| !model.trim().is_empty())
                    .or_else(|| call_model.clone());
                Some(PlannedTask {
                    title: title.to_string(),
                    prompt,
                    working_dir: workdir.clone(),
                    model,
                    pool: call_pool.clone(),
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

    /// `load_plan` (a model asked: always hand the plan out) or `offer_plan` (a script offered it:
    /// only in spawn mode).
    async fn load(&self, arguments: &Value, asked: bool) -> Result<String, String> {
        let path = arguments
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .ok_or("load_plan needs the plan file's `path`.")?;
        self.spawner
            .spawn(move |queue, ctx| {
                if asked {
                    queue.load_plan_tasks(&path, true, ctx).map(|ids| {
                        format!(
                            "Queued {} new task(s) from {} and started the queue. Call \
                             collect_results with ids {ids:?} to wait for them.",
                            ids.len(),
                            path.display()
                        )
                    })
                } else {
                    queue.offer_plan(&path, ctx)
                }
            })
            .await
            .map_err(|_| "the agent queue is not available".to_string())?
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

// ---------------------------------------------------------------------------
// Serving the tools to Claude Code / Codex over a socket + MCP subprocess
// ---------------------------------------------------------------------------

/// Serves the queue tools over a Unix socket for the MCP subprocess. The listener lives on a
/// background thread for the app's lifetime; one request per connection.
pub(crate) fn serve_socket(spawner: ModelSpawner<AgentQueue>) {
    let path = socket_path();
    let _ = std::fs::remove_file(&path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            log::warn!("queue_tools: could not bind {}: {error}", path.display());
            return;
        }
    };
    std::thread::Builder::new()
        .name("queue-tools-socket".to_string())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let spawner = spawner.clone();
                std::thread::spawn(move || handle_socket_client(stream, spawner));
            }
        })
        .ok();
}

/// One `{op, arguments}` request on the socket, answered with `{ok, result}`.
fn handle_socket_client(stream: UnixStream, spawner: ModelSpawner<AgentQueue>) {
    let Ok(read_half) = stream.try_clone() else { return };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
        return;
    }
    let request: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
    let op = request.get("op").and_then(Value::as_str).unwrap_or("");
    let arguments = request.get("arguments").cloned().unwrap_or(Value::Null);
    let name = match op {
        "plan" => PLAN_TASKS,
        "collect" => COLLECT_RESULTS,
        "load_plan" => LOAD_PLAN,
        "offer_plan" => OFFER_PLAN,
        _ => "",
    };
    let tools = QueueTools { spawner, inner: None };
    let response = match futures::executor::block_on(tools.call(name, arguments)) {
        Ok(text) => json!({ "ok": true, "result": text }),
        Err(error) => json!({ "ok": false, "result": error }),
    };
    let _ = writeln!(writer, "{response}");
    let _ = writer.flush();
}

/// The queue-tools MCP server: a subprocess of this binary (the `queue-mcp-server` worker),
/// speaking MCP over stdio to Claude Code / Codex and relaying each tool call to the app over the
/// Unix socket. Returns when stdin closes.
pub fn run_queue_mcp_server() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        // Notifications (no id) need no response.
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        let response = handle_mcp_request(method, &params, id);
        let _ = writeln!(stdout, "{response}");
        let _ = stdout.flush();
    }
}

fn handle_mcp_request(method: &str, params: &Value, id: Value) -> Value {
    match method {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "krust-queue-tools", "version": env!("CARGO_PKG_VERSION") },
            }
        }),
        "tools/list" => {
            let tools: Vec<Value> = tool_specs()
                .into_iter()
                .map(|spec| {
                    json!({
                        "name": spec.name,
                        "description": spec.description,
                        "inputSchema": spec.input_schema,
                    })
                })
                .collect();
            json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": tools } })
        }
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
            let op = if name == PLAN_TASKS {
                "plan"
            } else if name == COLLECT_RESULTS {
                "collect"
            } else if name == LOAD_PLAN {
                "load_plan"
            } else {
                ""
            };
            let (text, is_error) = if op.is_empty() {
                (format!("Unknown tool: {name}"), true)
            } else {
                match socket_request(op, &arguments) {
                    Ok(text) => (text, false),
                    Err(error) => (error, true),
                }
            };
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "content": [{ "type": "text", "text": text }], "isError": is_error },
            })
        }
        other => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("Method not found: {other}") },
        }),
    }
}

/// Sends one `{op, arguments}` to the app's socket and returns its `result`.
fn socket_request(op: &str, arguments: &Value) -> Result<String, String> {
    let stream = UnixStream::connect(socket_path())
        .map_err(|error| format!("the agent queue is not reachable: {error}"))?;
    let mut writer = stream
        .try_clone()
        .map_err(|error| error.to_string())?;
    let request = json!({ "op": op, "arguments": arguments });
    writeln!(writer, "{request}").map_err(|error| error.to_string())?;
    writer.flush().ok();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|error| error.to_string())?;
    let response: Value = serde_json::from_str(line.trim()).map_err(|error| error.to_string())?;
    let text = response
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if response.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        Ok(text)
    } else {
        Err(text)
    }
}

/// Writes (once per run) an MCP config file naming the queue-tools subprocess, and returns its
/// path, for Claude Code's `--mcp-config` so a terminal Claude Code conversation can call
/// `plan_tasks` / `collect_results`. `None` if the executable or the file can't be resolved.
/// Only the terminal (boss) Claude run gets this; a Claude *worker* (run through
/// `run_handed_off_turn`) is given no MCP config, so it never spawns further agents.
pub(crate) fn claude_mcp_config_path() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let config = json!({
        "mcpServers": {
            "krust-queue-tools": {
                "command": exe.to_string_lossy(),
                "args": ["queue-mcp-server"],
            }
        }
    });
    let path = krust_core::paths::state_dir().join("queue-mcp-config.json");
    std::fs::write(&path, config.to_string()).ok()?;
    Some(path.to_string_lossy().into_owned())
}
