//! Durable conversation → local-harness-session bindings.
//!
//! `TerminalView.local_claude_thread` only lives as long as the pane, so
//! without this registry a follow-up typed into a *restored* conversation
//! (after an app restart, or from another pane) silently starts a brand-new
//! conversation and a fresh CLI session. Recording every binding here lets
//! `run_local_claude_query` resume the original claude/codex session and
//! append to the conversation on screen instead.
//!
//! Storage is one small JSON file in the app state dir, keyed by
//! conversation id, rewritten atomically (write-then-rename) on every
//! update. All failures are logged and non-fatal — worst case a restored
//! conversation falls back to today's start-fresh behavior. Ollama uses the
//! same binding to retain its native generate context within an app session.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent_sdk::driver::harness::LocalHarnessKind;

const FILE_NAME: &str = "local_harness_threads.json";

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct ThreadBinding {
    harness: LocalHarnessKind,
    session_id: Uuid,
}

static REGISTRY: Mutex<Option<HashMap<String, ThreadBinding>>> = Mutex::new(None);

fn registry_path() -> PathBuf {
    krust_core::paths::state_dir().join(FILE_NAME)
}

fn load() -> HashMap<String, ThreadBinding> {
    let path = registry_path();
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|error| {
            log::warn!("local_harness_threads: ignoring malformed {path:?}: {error}");
            HashMap::new()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(error) => {
            log::warn!("local_harness_threads: failed to read {path:?}: {error}");
            HashMap::new()
        }
    }
}

fn persist(bindings: &HashMap<String, ThreadBinding>) {
    let path = registry_path();
    let Ok(serialized) = serde_json::to_string_pretty(bindings) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    let result = std::fs::write(&tmp, serialized).and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(error) = result {
        log::warn!("local_harness_threads: failed to persist {path:?}: {error}");
    }
}

/// The harness and CLI session bound to `conversation_id`, if one was ever
/// recorded (possibly by a previous app run).
pub(crate) fn lookup(conversation_id: AIConversationId) -> Option<(LocalHarnessKind, Uuid)> {
    let mut registry = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let bindings = registry.get_or_insert_with(load);
    bindings
        .get(&conversation_id.to_string())
        .map(|binding| (binding.harness, binding.session_id))
}

/// Records (or updates) the harness session bound to `conversation_id` and
/// persists the registry.
pub(crate) fn record(
    conversation_id: AIConversationId,
    harness: LocalHarnessKind,
    session_id: Uuid,
) {
    let mut registry = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let bindings = registry.get_or_insert_with(load);
    let entry = bindings.insert(
        conversation_id.to_string(),
        ThreadBinding {
            harness,
            session_id,
        },
    );
    let changed = entry
        .map(|previous| previous.session_id != session_id || previous.harness != harness)
        .unwrap_or(true);
    if changed {
        persist(bindings);
    }
}
