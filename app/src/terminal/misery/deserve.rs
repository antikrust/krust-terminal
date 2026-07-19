//! How much a given command deserves to be mocked.
//!
//! Adaptive frequency is what keeps misery mode funny past the first hour.
//! Repetition and triviality push the score up; genuine work pushes it down.
//! The score is a probability, so the engine stays quiet on the commands where
//! a joke would just be noise.

use super::classify::{CommandClass, ParsedCommand};
use super::templates::Phase;

/// Ceiling on how often we'll speak, regardless of how deserving the command
/// is. Even a fifth consecutive `ls` shouldn't be a certainty — unpredictability
/// is most of the comedy.
const MAX_PROBABILITY: f32 = 0.75;

/// Floor for commands that look like real work. Never quite zero: the rare
/// unearned insult during a long build is the funniest one.
const MIN_PROBABILITY: f32 = 0.04;

/// Inputs the scorer needs beyond the command itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct History {
    /// How many of the recent commands were this same command.
    pub repeat_count: u32,
    /// How many commands we've mocked in the last few commands. Used to back
    /// off so misery arrives in waves rather than a constant drizzle.
    pub recent_misery_count: u32,
}

/// Returns the probability, 0.0..=1.0, that we should say something.
pub fn probability(
    command: &ParsedCommand,
    phase: Phase,
    history: History,
    exit_code: Option<i32>,
    duration_millis: Option<u64>,
) -> f32 {
    let mut score = command.class.baseline_contempt();

    // Repetition is the richest seam. Third time is where "not this again"
    // stops being a joke and starts being an observation.
    score += match history.repeat_count {
        0 => 0.0,
        1 => 0.10,
        2 => 0.22,
        _ => 0.32,
    };

    // A bare command with no arguments is doing the least possible.
    if command.is_bare() {
        score += 0.08;
    }

    // A pipeline is at least an attempt at thought.
    if command.is_compound {
        score -= 0.18;
    }

    // Failure is irresistible.
    if matches!(exit_code, Some(code) if code != 0) {
        score += 0.30;
    }

    // Something that took real time was probably real work — unless it was
    // trivial, in which case waiting for it is its own punchline.
    if let Some(millis) = duration_millis {
        let trivial = matches!(
            command.class,
            CommandClass::Listing | CommandClass::Navigation | CommandClass::Clear
        );
        if millis > 5_000 {
            score += if trivial { 0.25 } else { -0.20 };
        }
    }

    // Back off if we've been talking a lot lately.
    score -= 0.16 * history.recent_misery_count as f32;

    // The completion phase is the more interesting one, so let submit be
    // rarer — this is what stops every command producing two lines.
    if phase == Phase::Submit {
        score *= 0.55;
    }

    score.clamp(MIN_PROBABILITY, MAX_PROBABILITY)
}

#[cfg(test)]
#[path = "deserve_tests.rs"]
mod tests;
