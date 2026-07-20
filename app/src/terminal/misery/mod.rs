//! Misery mode: an escalation of swear mode in which the terminal makes it
//! clear that it considers your work beneath it.
//!
//! The design goal is to *ape* an AI without being one. Everything here is a
//! weighted template grammar over static vocabulary plus live context
//! (filenames, exit codes, timings, repetition). No model, no network, no
//! latency — but because activities are synthesised from the user's real
//! filenames and lines are never repeated within a session, the output reads
//! as though something is paying attention.
//!
//! ```text
//!   parse ──► classify ──► deserve? ──► pick template ──► expand ──► line
//!   (classify.rs) (deserve.rs)  (templates.rs)  (grammar.rs)
//! ```
//!
//! Two voices, driven by [`Phase`]: weary anticipation when a command is
//! submitted, a verdict when it completes.
//!
//! ## What this module does not do
//!
//! It does not write to the PTY. Lines produced here are rendered as block
//! decorations by the UI layer. Injecting into the real output stream would
//! corrupt piped output and break anything parsing it — the joke is not worth
//! a data-integrity bug.

use std::collections::VecDeque;

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

pub mod classify;
mod deserve;
mod grammar;
mod session;
mod templates;
mod view;
mod vocab;

pub use classify::{CommandClass, ParsedCommand};
pub use session::{CompletedCommand, MiserySession};
pub use templates::Phase;
pub use view::MiseryLineView;

/// How many recently-emitted lines to remember, to avoid repeats.
const RECENT_LINES_MEMORY: usize = 40;
/// How many recent commands to consider when detecting repetition.
const RECENT_COMMANDS_MEMORY: usize = 6;
/// How many recent commands each voice backs off over.
///
/// Tracked *per phase*. A single shared window meant the two voices starved
/// each other: a completion line would suppress the very next submit roll,
/// and since completions are the more likely of the two, submit lines almost
/// never surfaced. Each voice now paces itself independently.
const MISERY_BACKOFF_WINDOW: usize = 3;

/// Everything the grammar can draw on when expanding a template.
#[derive(Debug, Clone, Default)]
pub struct MiseryContext {
    /// Bare command name, e.g. `ls`.
    pub command_name: String,
    /// Basename of the working directory.
    pub directory_name: Option<String>,
    /// Filenames in the working directory, used to synthesise activities.
    /// Names only — contents are never read.
    pub nearby_files: Vec<String>,
    /// `None` on submit; `Some` once the command has finished.
    pub exit_code: Option<i32>,
    /// Convenience mirror of `exit_code == Some(0)`.
    pub succeeded: Option<bool>,
    pub duration: Option<u64>,
    pub output_lines: Option<u32>,
    /// Whether the user has swear mode on, which selects the harsher
    /// vocabulary tiers.
    pub swear_mode: bool,
    /// Fork: the user's natural-language agent prompt, for the `{QUOTE}` slot.
    /// Set only in agent mode; `None` for shell commands, which is what keeps
    /// the quote templates from ever firing in the terminal (a shell command
    /// has nothing to quote back).
    pub user_prompt: Option<String>,
}

/// The stateful generator. One per session — the anti-repetition memory is
/// what makes consecutive lines feel authored rather than sampled.
pub struct MiseryEngine {
    rng: StdRng,
    recent_lines: VecDeque<String>,
    recent_commands: VecDeque<String>,
    /// Whether each of the last few submit rolls produced a line.
    recent_spoke_submit: VecDeque<bool>,
    /// Whether each of the last few completion rolls produced a line.
    recent_spoke_complete: VecDeque<bool>,
}

impl Default for MiseryEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl MiseryEngine {
    pub fn new() -> Self {
        Self {
            rng: StdRng::from_entropy(),
            recent_lines: VecDeque::with_capacity(RECENT_LINES_MEMORY),
            recent_commands: VecDeque::with_capacity(RECENT_COMMANDS_MEMORY),
            recent_spoke_submit: VecDeque::with_capacity(MISERY_BACKOFF_WINDOW),
            recent_spoke_complete: VecDeque::with_capacity(MISERY_BACKOFF_WINDOW),
        }
    }

    /// Deterministic constructor for tests and for anyone who wants
    /// reproducible misery.
    pub fn with_seed(seed: u64) -> Self {
        Self {
            rng: StdRng::seed_from_u64(seed),
            ..Self::new()
        }
    }

    /// Records that a command was run, for repetition detection. Call once per
    /// command, at submit time.
    pub fn note_command(&mut self, command: &ParsedCommand) {
        if self.recent_commands.len() == RECENT_COMMANDS_MEMORY {
            self.recent_commands.pop_front();
        }
        self.recent_commands.push_back(command.name.clone());
    }

    /// Produces a line, or `None` if this command doesn't deserve one (the
    /// common case) or if no template could be filled from the context.
    pub fn generate(&mut self, ctx: &MiseryContext, phase: Phase) -> Option<String> {
        let command = classify::parse(&ctx.command_name)?;
        let history = self.history(&command, phase);

        let probability =
            deserve::probability(&command, phase, history, ctx.exit_code, ctx.duration);
        let speaking = self.rng.gen::<f32>() < probability;
        self.note_spoke(speaking, phase);
        if !speaking {
            return None;
        }

        let line = self.pick_line(ctx, phase, command.class)?;
        self.remember(line.clone());
        Some(line)
    }

    /// Generates a line unconditionally, bypassing the deservedness roll.
    /// Used by the settings preview so the user can see what they're enabling.
    pub fn generate_forced(&mut self, ctx: &MiseryContext, phase: Phase) -> Option<String> {
        let command = classify::parse(&ctx.command_name)?;
        let line = self.pick_line(ctx, phase, command.class)?;
        self.remember(line.clone());
        Some(line)
    }

    /// Weighted choice over the templates valid for this phase and class,
    /// retried until one expands to something we haven't said recently.
    fn pick_line(
        &mut self,
        ctx: &MiseryContext,
        phase: Phase,
        class: CommandClass,
    ) -> Option<String> {
        let candidates: Vec<&templates::Template> =
            templates::candidates(phase, class, ctx).collect();
        if candidates.is_empty() {
            return None;
        }

        // Several attempts: a template may fail to expand (missing slot) or
        // produce a line we've used recently. Both are cheap to retry.
        for _ in 0..24 {
            let template = candidates
                .choose_weighted(&mut self.rng, |t| t.weight)
                .ok()?;
            let Some(line) = grammar::expand(template.text, ctx, &mut self.rng) else {
                continue;
            };
            if self.recent_lines.contains(&line) {
                continue;
            }
            return Some(line);
        }
        None
    }

    fn history(&self, command: &ParsedCommand, phase: Phase) -> deserve::History {
        let repeat_count = self
            .recent_commands
            .iter()
            .filter(|name| *name == &command.name)
            .count()
            // The current command is already recorded by `note_command`, so
            // discount it to get "how many times *before* this one".
            .saturating_sub(1) as u32;

        let recent_misery_count = self
            .recent_spoke_for(phase)
            .iter()
            .filter(|spoke| **spoke)
            .count() as u32;

        deserve::History {
            repeat_count,
            recent_misery_count,
        }
    }

    fn recent_spoke_for(&self, phase: Phase) -> &VecDeque<bool> {
        match phase {
            Phase::Submit => &self.recent_spoke_submit,
            Phase::Complete => &self.recent_spoke_complete,
            // Quote has no backoff window (its rate is fixed); this value is
            // never read for it, so any window will do.
            Phase::Quote => &self.recent_spoke_submit,
        }
    }

    fn note_spoke(&mut self, spoke: bool, phase: Phase) {
        let window = match phase {
            Phase::Submit => &mut self.recent_spoke_submit,
            Phase::Complete => &mut self.recent_spoke_complete,
            // Quote doesn't pace itself off a window.
            Phase::Quote => return,
        };
        if window.len() == MISERY_BACKOFF_WINDOW {
            window.pop_front();
        }
        window.push_back(spoke);
    }

    fn remember(&mut self, line: String) {
        if self.recent_lines.len() == RECENT_LINES_MEMORY {
            self.recent_lines.pop_front();
        }
        self.recent_lines.push_back(line);
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "density_test.rs"]
mod density;
