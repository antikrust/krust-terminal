//! Does the runbook's set of models fit in memory together?
//!
//! A runbook run keeps a worker, a judge and (in the work-file pass) a helper
//! loaded at once, because they alternate on every step and `KEEP_ALIVE`
//! stops them being unloaded between calls. A 20 GB worker, a 7.6 GB judge
//! and another process's image model on a 24 GB Mac filled swap and froze the
//! machine. So before the judge or helper is picked or loaded, this adds up
//! what the run would hold — plus whatever Ollama already has loaded — against
//! the memory the machine can give to models, and either picks the best
//! judge/helper that fits or says plainly why the run cannot start.
//!
//! The decision is a pure function over an [`Inventory`] (sizes from
//! `/api/tags`, loaded models from `/api/ps`), so it is deterministic and
//! testable; only [`inventory`] touches the network.

use std::collections::BTreeMap;
use std::time::Duration;

use command::r#async::Command;
use krustui::r#async::Timer;
use serde_json::{json, Value};

use super::ollama_stream_exec::{base_url_of, wire_name};
use crate::ai::ollama::capability::{
    format_gb, total_system_memory_bytes, USABLE_MEMORY_FRACTION,
};
use crate::ai::ollama::service::API_BASE;

/// Added to a model's weights for the 16k context and the runtime around it.
const CONTEXT_OVERHEAD_BYTES: u64 = 1_073_741_824;

/// A model Ollama already holds in memory.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LoadedModel {
    pub(crate) name: String,
    pub(crate) bytes: u64,
}

/// What the machine has, and what Ollama has on it.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Inventory {
    /// Memory the machine can give to models: all of it minus the share kept
    /// for macOS and the app.
    pub(crate) budget_bytes: u64,
    pub(crate) total_bytes: u64,
    /// Installed models' weights, from `/api/tags`.
    pub(crate) installed: BTreeMap<String, u64>,
    /// Models held in memory now, on this machine's Ollama servers.
    pub(crate) loaded: Vec<LoadedModel>,
}

/// The judge and helper a run goes ahead with.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Picks {
    pub(crate) judge: String,
    pub(crate) helper: String,
}

/// `name` without Ollama's implicit `:latest`, so `qwen3` and `qwen3:latest`
/// are one model.
fn canonical(name: &str) -> &str {
    name.strip_suffix(":latest").unwrap_or(name)
}

fn same_model(a: &str, b: &str) -> bool {
    canonical(wire_name(a)) == canonical(wire_name(b))
}

fn is_remote(model: &str) -> bool {
    crate::ai::ollama::remote::split(model).1.is_some()
}

/// What `model` takes in memory: what Ollama reports if it is loaded, else its
/// weights plus the context overhead. A model on another machine, or one whose
/// size is unknown, takes nothing here — an unknown size must not stop a run.
fn cost(inventory: &Inventory, model: &str) -> u64 {
    if is_remote(model) {
        return 0;
    }
    if let Some(loaded) = inventory
        .loaded
        .iter()
        .find(|loaded| same_model(&loaded.name, model))
    {
        return loaded.bytes;
    }
    inventory
        .installed
        .iter()
        .find(|(name, _)| same_model(name, model))
        .map(|(_, bytes)| bytes + CONTEXT_OVERHEAD_BYTES)
        .unwrap_or(0)
}

/// What holding `models` together would take: each of them once, plus
/// whatever else Ollama has loaded.
fn needed(inventory: &Inventory, models: &[&str]) -> u64 {
    let mut distinct: Vec<&str> = Vec::new();
    for &model in models {
        if !distinct.iter().any(|seen| same_model(seen, model)) {
            distinct.push(model);
        }
    }
    let ours: u64 = distinct.iter().map(|model| cost(inventory, model)).sum();
    let others: u64 = inventory
        .loaded
        .iter()
        .filter(|loaded| !distinct.iter().any(|model| same_model(&loaded.name, model)))
        .map(|loaded| loaded.bytes)
        .sum();
    ours + others
}

/// The judge and helper to go ahead with, or why the run cannot start.
///
/// A chosen model (`judge` / `helper` is `Some`) is used as chosen or not at
/// all; otherwise the candidates are tried in preference order, with the
/// worker itself last because it takes no memory beyond its own. The first
/// pair that fits wins.
pub(crate) fn choose(
    inventory: &Inventory,
    worker: &str,
    judge: Option<&str>,
    helper: Option<&str>,
    judge_candidates: &[String],
    helper_candidates: &[String],
) -> Result<Picks, String> {
    let options = |chosen: Option<&str>, candidates: &[String]| -> Vec<String> {
        match chosen {
            Some(chosen) => vec![chosen.to_string()],
            None => candidates
                .iter()
                .cloned()
                .chain(std::iter::once(worker.to_string()))
                .collect(),
        }
    };
    let judges = options(judge, judge_candidates);
    let helpers = options(helper, helper_candidates);
    for judge in &judges {
        for helper in &helpers {
            if needed(inventory, &[worker, judge, helper]) <= inventory.budget_bytes {
                return Ok(Picks {
                    judge: judge.clone(),
                    helper: helper.clone(),
                });
            }
        }
    }
    Err(refusal(
        inventory,
        worker,
        &judges[0],
        &helpers[0],
        judge.is_some() || helper.is_some(),
    ))
}

/// The message that explains a refusal: every model with its size, what else
/// is loaded, and the memory available.
fn refusal(
    inventory: &Inventory,
    worker: &str,
    judge: &str,
    helper: &str,
    chosen_by_user: bool,
) -> String {
    let mut lines = Vec::new();
    let mut listed: Vec<&str> = Vec::new();
    for (role, model) in [("worker", worker), ("judge", judge), ("helper", helper)] {
        if listed.iter().any(|seen| same_model(seen, model)) {
            continue;
        }
        listed.push(model);
        lines.push(format!(
            "- {role} `{model}`: {}",
            format_gb(cost(inventory, model))
        ));
    }
    let others: Vec<&LoadedModel> = inventory
        .loaded
        .iter()
        .filter(|loaded| !listed.iter().any(|model| same_model(&loaded.name, model)))
        .collect();
    for loaded in &others {
        lines.push(format!(
            "- already loaded in Ollama: `{}`: {}",
            loaded.name,
            format_gb(loaded.bytes)
        ));
    }
    let total = needed(inventory, &[worker, judge, helper]);
    format!(
        "**Runbook mode** — these models don't fit in memory together, so I haven't loaded any of them: loading \
         them would swap this Mac until it freezes.\n\n{}\n\nTogether that is {}, and this machine has {} of memory \
         with {} of it usable for models (the rest is kept for macOS and Krust).\n\n{}{}",
        lines.join("\n"),
        format_gb(total),
        format_gb(inventory.total_bytes),
        format_gb(inventory.budget_bytes),
        if chosen_by_user {
            "A judge or helper you chose is used as chosen, so no smaller one was tried in its place. "
        } else {
            "No smaller installed judge or helper brings it within that. "
        },
        if others.is_empty() {
            "Pick a smaller worker, or a smaller judge or helper in Settings → AI → Ollama, and send the runbook \
             again."
        } else {
            "Unload what is already loaded (`ollama stop <model>`) or pick smaller models, and send the runbook \
             again."
        }
    )
}

/// Everything `choose` needs from this machine and its Ollama servers, or
/// `None` when the machine's memory can't be read (the check is then skipped,
/// as the model menu's fit badges are). `other_servers` are further Ollama
/// servers by base URL; only ones on this machine take part, since a server
/// elsewhere holds its models in its own memory.
pub(crate) async fn inventory(path: &str, other_servers: &[String]) -> Option<Inventory> {
    let total_bytes = total_system_memory_bytes()?;
    let installed = get_json(path, &format!("{API_BASE}/api/tags"))
        .await
        .map(|body| parse_installed(&body))
        .unwrap_or_default();
    let mut loaded = Vec::new();
    let servers = std::iter::once(API_BASE)
        .chain(
            other_servers
                .iter()
                .map(String::as_str)
                .filter(|url| is_loopback(url)),
        )
        .collect::<Vec<_>>();
    for server in servers {
        if let Some(body) = get_json(path, &format!("{server}/api/ps")).await {
            loaded.extend(parse_loaded(&body));
        }
    }
    Some(Inventory {
        budget_bytes: (total_bytes as f64 * USABLE_MEMORY_FRACTION) as u64,
        total_bytes,
        installed,
        loaded,
    })
}

/// Whether a base URL is a server on this machine.
pub(crate) fn is_loopback(url: &str) -> bool {
    let host = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or_default();
    ["127.0.0.1", "localhost", "[::1]"].iter().any(|local| {
        host == *local
            || host
                .strip_prefix(local)
                .is_some_and(|rest| rest.starts_with(':'))
    })
}

fn parse_installed(body: &Value) -> BTreeMap<String, u64> {
    body.get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            Some((
                model.get("name")?.as_str()?.to_string(),
                model.get("size")?.as_u64()?,
            ))
        })
        .collect()
}

fn parse_loaded(body: &Value) -> Vec<LoadedModel> {
    body.get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let name = model.get("name").or_else(|| model.get("model"))?.as_str()?;
            Some(LoadedModel {
                name: name.to_string(),
                bytes: model.get("size")?.as_u64()?,
            })
        })
        .collect()
}

/// A short GET answered with JSON, via curl for the same non-Tokio-executor
/// reason as the other runbook probes. `None` if the server doesn't answer.
async fn get_json(path: &str, url: &str) -> Option<Value> {
    let mut command = Command::new("/usr/bin/curl");
    command.args(["--silent", "--fail", "--max-time", "5", url]);
    command.env("PATH", path);
    let output = command.output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

/// Asks each model's Ollama server to drop it from memory now, and waits a few
/// seconds for it to be gone, so what loads next sees the memory free. Best
/// effort: a model that is not loaded, or a server that doesn't answer, is not
/// an error.
pub(crate) async fn unload(path: &str, models: &[String]) {
    for model in models {
        let body = json!({ "model": wire_name(model), "keep_alive": 0 }).to_string();
        let mut command = Command::new("/usr/bin/curl");
        command.args([
            "--silent",
            "--max-time",
            "10",
            "-d",
            body.as_str(),
            format!("{}/api/generate", base_url_of(model)).as_str(),
        ]);
        command.env("PATH", path);
        let _ = command.output().await;
    }
    for _ in 0..10 {
        let mut still_loaded = false;
        for model in models.iter().filter(|model| !is_remote(model)) {
            if let Some(body) = get_json(path, &format!("{}/api/ps", base_url_of(model))).await {
                still_loaded |= parse_loaded(&body)
                    .iter()
                    .any(|loaded| same_model(&loaded.name, model));
            }
        }
        if !still_loaded {
            return;
        }
        Timer::after(Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
#[path = "ollama_runbook_memory_tests.rs"]
mod tests;
