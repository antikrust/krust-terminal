//! The acolyte this app runs, and what it can see of it: the process (started and stopped here,
//! its output kept), its config (a file of the app's own, handed to acolyte as `ACOLYTE_ENV`),
//! whether Ollama answers and with which models, and whether Moloch lists it as connected.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use krust_app_shell::env_file;
use krustui::r#async::Timer;
use krustui::{Entity, ModelContext, SingletonEntity};
use serde_json::Value;

/// How often the page is redrawn with new output, and how often Ollama and Moloch are asked.
const TICK: Duration = Duration::from_secs(1);
const CHECK_EVERY: u32 = 4;
/// Output lines kept.
const MAX_LINES: usize = 2_000;

/// The config keys the app shows: acolyte's own, and `ACOLYTE_BIN` (the app's: which acolyte).
pub const KEYS: &[(&str, &str)] = &[
    ("MOLOCH_URL", "Moloch address"),
    ("MOLOCH_TOKEN", "Moloch token"),
    ("OLLAMA_URL", "Ollama address"),
    ("ACOLYTE_NAME", "Worker name"),
    ("ACOLYTE_KINDS", "Job kinds"),
    ("ACOLYTE_MODEL", "Preferred model"),
    ("ACOLYTE_BIN", "Acolyte binary"),
];

/// The app's config file (`~/Library/Application Support/Acolyte/acolyte.env` on a Mac).
pub fn config_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_default()
        .join("Acolyte")
        .join("acolyte.env")
}

/// The name acolyte goes by when `ACOLYTE_NAME` is unset: the hostname, as acolyte works it out.
fn default_name() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "acolyte".to_owned())
}

/// Which acolyte to run: the config's `ACOLYTE_BIN`, else the one bundled beside this app's binary,
/// else `acolyte` on the PATH.
fn binary(config: &std::collections::HashMap<String, String>) -> PathBuf {
    if let Some(path) = config.get("ACOLYTE_BIN").filter(|path| !path.trim().is_empty()) {
        return PathBuf::from(path.trim());
    }
    if let Some(bundled) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("acolyte")))
        .filter(|path| path.exists())
    {
        return bundled;
    }
    PathBuf::from("acolyte")
}

pub struct Acolyte {
    child: Option<Child>,
    /// Its output, stdout and stderr together, oldest first.
    lines: Arc<Mutex<VecDeque<String>>>,
    shown_lines: usize,
    /// Why it stopped, or couldn't start.
    pub last_exit: Option<String>,
    pub ollama_models: Option<Vec<String>>,
    pub ollama_error: Option<String>,
    /// Whether Moloch lists this acolyte among its connected workers; `None` until asked.
    pub connected: Option<bool>,
    pub moloch_error: Option<String>,
    ticks: u32,
}

pub enum AcolyteEvent {
    Changed,
}

impl Entity for Acolyte {
    type Event = AcolyteEvent;
}

impl SingletonEntity for Acolyte {}

impl Drop for Acolyte {
    /// Acolyte runs for this app; it stops when the app quits.
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
        }
    }
}

impl Acolyte {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let me = Self {
            child: None,
            lines: Default::default(),
            shown_lines: 0,
            last_exit: None,
            ollama_models: None,
            ollama_error: None,
            connected: None,
            moloch_error: None,
            ticks: 0,
        };
        Self::tick(ctx);
        ctx.spawn(async {}, |me, _, ctx| me.check(ctx));
        me
    }

    pub fn config() -> std::collections::HashMap<String, String> {
        env_file::read(&config_path())
    }

    pub fn name() -> String {
        Self::config()
            .get("ACOLYTE_NAME")
            .filter(|name| !name.trim().is_empty())
            .cloned()
            .unwrap_or_else(default_name)
    }

    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// The output lines, oldest first.
    pub fn lines(&self) -> Vec<String> {
        self.lines
            .lock()
            .map(|lines| lines.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn tick(ctx: &mut ModelContext<Self>) {
        ctx.spawn(
            async { Timer::after(TICK).await },
            |me, _, ctx| {
                me.poll(ctx);
                Self::tick(ctx);
            },
        );
    }

    /// Notices it stopping and new output each tick; asks Ollama and Moloch every few.
    fn poll(&mut self, ctx: &mut ModelContext<Self>) {
        let mut changed = false;
        if let Some(child) = &mut self.child {
            if let Ok(Some(status)) = child.try_wait() {
                self.last_exit = Some(format!("It stopped ({status})."));
                self.child = None;
                changed = true;
            }
        }
        let count = self.lines.lock().map(|lines| lines.len()).unwrap_or(0);
        if count != self.shown_lines {
            self.shown_lines = count;
            changed = true;
        }
        self.ticks += 1;
        if self.ticks % CHECK_EVERY == 0 {
            self.check(ctx);
        }
        if changed {
            ctx.emit(AcolyteEvent::Changed);
        }
    }

    fn push_line(lines: &Arc<Mutex<VecDeque<String>>>, line: String) {
        if let Ok(mut lines) = lines.lock() {
            lines.push_back(line);
            while lines.len() > MAX_LINES {
                lines.pop_front();
            }
        }
    }

    /// Starts acolyte with the app's config, keeping its output.
    pub fn start(&mut self, ctx: &mut ModelContext<Self>) {
        if self.child.is_some() {
            return;
        }
        let config = Self::config();
        let binary = binary(&config);
        let started = Command::new(&binary)
            .env("ACOLYTE_ENV", config_path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        match started {
            Ok(mut child) => {
                for stream in [
                    child.stdout.take().map(|out| Box::new(out) as Box<dyn std::io::Read + Send>),
                    child.stderr.take().map(|err| Box::new(err) as Box<dyn std::io::Read + Send>),
                ]
                .into_iter()
                .flatten()
                {
                    let lines = Arc::clone(&self.lines);
                    std::thread::spawn(move || {
                        for line in BufReader::new(stream).lines().map_while(Result::ok) {
                            Self::push_line(&lines, line);
                        }
                    });
                }
                Self::push_line(
                    &self.lines,
                    format!("— started {} (pid {}) —", binary.display(), child.id()),
                );
                self.child = Some(child);
                self.last_exit = None;
            }
            Err(error) => {
                self.last_exit = Some(format!(
                    "Couldn't start {}: {error}. Set the acolyte binary under Config.",
                    binary.display()
                ));
            }
        }
        ctx.emit(AcolyteEvent::Changed);
    }

    pub fn stop(&mut self, ctx: &mut ModelContext<Self>) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
            Self::push_line(&self.lines, "— stopped —".to_owned());
            self.last_exit = Some("Stopped.".to_owned());
            ctx.emit(AcolyteEvent::Changed);
        }
    }

    /// Asks Ollama for its models and Moloch for its connected workers.
    pub fn check(&mut self, ctx: &mut ModelContext<Self>) {
        let config = Self::config();
        let ollama = config
            .get("OLLAMA_URL")
            .filter(|url| !url.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| "http://127.0.0.1:11434".to_owned());
        let moloch = config.get("MOLOCH_URL").cloned().unwrap_or_default();
        let token = config.get("MOLOCH_TOKEN").cloned().unwrap_or_default();
        let name = Self::name();
        ctx.spawn(
            async move {
                let client = reqwest::Client::new();
                let get_json = |url: String, token: Option<String>| {
                    let mut request = client.get(url).timeout(Duration::from_secs(4));
                    if let Some(token) = token {
                        request = request.bearer_auth(token);
                    }
                    async move {
                        let response = request.send().await.map_err(|error| {
                            if error.is_connect() {
                                "not answering".to_owned()
                            } else {
                                error.to_string()
                            }
                        })?;
                        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
                            return Err("refused the token".to_owned());
                        }
                        if !response.status().is_success() {
                            return Err(format!("answered {}", response.status()));
                        }
                        let text = response.text().await.map_err(|error| error.to_string())?;
                        serde_json::from_str::<Value>(&text).map_err(|error| error.to_string())
                    }
                };
                let models = get_json(
                    format!("{}/api/tags", ollama.trim_end_matches('/')),
                    None,
                )
                .await
                .map(|tags| {
                    tags["models"]
                        .as_array()
                        .map(|models| {
                            models
                                .iter()
                                .filter_map(|model| model["name"].as_str().map(str::to_owned))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                });
                let connected = if moloch.trim().is_empty() {
                    Err("no Moloch address set".to_owned())
                } else {
                    get_json(
                        format!(
                            "{}/api/v1/agent/connected-self-hosted-workers",
                            moloch.trim().trim_end_matches('/')
                        ),
                        Some(token),
                    )
                    .await
                    .map(|workers| {
                        workers["workers"].as_array().is_some_and(|workers| {
                            workers.iter().any(|worker| worker["id"].as_str() == Some(&name))
                        })
                    })
                };
                (models, connected)
            },
            |me, (models, connected), ctx| {
                match models {
                    Ok(models) => {
                        me.ollama_models = Some(models);
                        me.ollama_error = None;
                    }
                    Err(error) => {
                        me.ollama_models = None;
                        me.ollama_error = Some(error);
                    }
                }
                match connected {
                    Ok(connected) => {
                        me.connected = Some(connected);
                        me.moloch_error = None;
                    }
                    Err(error) => {
                        me.connected = None;
                        me.moloch_error = Some(error);
                    }
                }
                ctx.emit(AcolyteEvent::Changed);
            },
        );
    }

    /// Saves the config; acolyte reads it when it next starts.
    pub fn save_config(&mut self, values: &[(&str, String)], ctx: &mut ModelContext<Self>) -> std::io::Result<()> {
        env_file::write(&config_path(), values)?;
        self.check(ctx);
        Ok(())
    }
}
