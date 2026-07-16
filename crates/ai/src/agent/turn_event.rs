//! Provider-agnostic representation of a single event emitted while a local
//! CLI coding agent (Claude Code, and eventually Codex/Gemini) runs in
//! headless/structured-output mode.
//!
//! Each supported CLI gets its own parser translating its native streaming
//! format into a `Vec<TurnEvent>`; everything downstream (mapping into
//! Warp's own `AIAgentOutputMessageType`/`AIAgentActionType` rendering
//! vocabulary) only ever depends on this type, not on any provider's wire
//! format.

use serde::{Deserialize, Serialize};

/// One event in a local CLI harness's run.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEvent {
    /// A chunk of assistant prose. Providers that emit one complete
    /// content block per line (rather than token-level deltas) should
    /// treat each block as one `Text` event.
    Text { text: String },

    /// Extended-thinking / reasoning content, rendered distinctly from
    /// regular prose.
    Thinking { text: String },

    /// The agent is invoking a tool. `call_id` correlates with the
    /// matching `ToolCallResult`.
    ToolCallStarted {
        call_id: String,
        name: String,
        input: serde_json::Value,
    },

    /// The result of a previously-started tool call.
    ToolCallResult {
        call_id: String,
        outcome: ToolCallOutcome,
    },

    /// The run has finished (successfully or not).
    TurnCompleted {
        usage: Option<TurnUsage>,
        is_error: bool,
        error_message: Option<String>,
    },

    /// Session metadata, emitted once near the start of a run. Not
    /// rendered; kept for resume/bookkeeping purposes only.
    SessionInit {
        session_id: String,
        model: Option<String>,
        cwd: Option<String>,
    },
}

/// The outcome of a tool call, as reported by the CLI.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolCallOutcome {
    Success(serde_json::Value),
    Error(String),
    Cancelled,
}

/// Usage/cost/timing info reported at the end of a run.
///
/// Token counts describe the run's *final* API call, so
/// `input_tokens + cache_read_input_tokens + cache_creation_input_tokens +
/// output_tokens` approximates how full the model's context window is
/// after the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Prompt tokens served from the provider's cache on the final call.
    pub cache_read_input_tokens: u64,
    /// Prompt tokens newly written to the provider's cache on the final call.
    pub cache_creation_input_tokens: u64,
    /// The run's primary model, when the CLI reports per-model usage.
    pub model_id: Option<String>,
    /// The primary model's context-window size in tokens, when reported.
    pub context_window: Option<u64>,
    pub cost_usd: Option<f64>,
    pub duration_ms: Option<u64>,
}
