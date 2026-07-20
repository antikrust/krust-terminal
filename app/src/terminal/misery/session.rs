//! Per-terminal misery state and context assembly.
//!
//! Owns the engine (so the anti-repetition memory spans the whole terminal
//! session rather than resetting per block) and turns block-completion facts
//! into a [`MiseryContext`].

use std::time::Instant;

use super::{MiseryContext, MiseryEngine, Phase};

/// Cap on how many directory entries we hand to the grammar. The templates use
/// at most two, so this only needs to be enough for variety.
const MAX_NEARBY_FILES: usize = 24;

/// Facts about a finished command, gathered by the caller from the block.
pub struct CompletedCommand<'a> {
    pub command: &'a str,
    pub working_directory: Option<&'a str>,
    pub exit_code: i32,
    pub started_at: Option<Instant>,
    pub output_lines: u64,
}

/// Per-terminal misery state.
pub struct MiserySession {
    engine: MiseryEngine,
    /// Directory listing cache, keyed by path. Re-listing on every command
    /// would be a syscall per block for a cosmetic feature; the contents only
    /// matter as raw material for insults, so a slightly stale list is fine.
    cached_directory: Option<(String, Vec<String>)>,
}

impl Default for MiserySession {
    fn default() -> Self {
        Self::new()
    }
}

impl MiserySession {
    pub fn new() -> Self {
        Self {
            engine: MiseryEngine::new(),
            cached_directory: None,
        }
    }

    /// Called when a command is submitted. Records it for repetition detection
    /// and may return a line to show above the output.
    pub fn on_command_submitted(
        &mut self,
        command: &str,
        working_directory: Option<&str>,
        swear_mode: bool,
    ) -> Option<String> {
        let parsed = super::classify::parse(command)?;
        self.engine.note_command(&parsed);

        let ctx = MiseryContext {
            command_name: command.to_owned(),
            directory_name: directory_name(working_directory),
            nearby_files: self.nearby_files(working_directory),
            exit_code: None,
            succeeded: None,
            duration: None,
            output_lines: None,
            swear_mode,
            user_prompt: None,
        };
        self.engine.generate(&ctx, Phase::Submit)
    }

    /// Called when a command completes. May return a line to show below the
    /// output.
    pub fn on_command_completed(
        &mut self,
        completed: CompletedCommand<'_>,
        swear_mode: bool,
    ) -> Option<String> {
        let duration = completed
            .started_at
            .map(|started| started.elapsed().as_millis() as u64);

        let ctx = MiseryContext {
            command_name: completed.command.to_owned(),
            directory_name: directory_name(completed.working_directory),
            nearby_files: self.nearby_files(completed.working_directory),
            exit_code: Some(completed.exit_code),
            succeeded: Some(completed.exit_code == 0),
            duration,
            output_lines: Some(completed.output_lines.min(u32::MAX as u64) as u32),
            swear_mode,
            user_prompt: None,
        };
        self.engine.generate(&ctx, Phase::Complete)
    }

    /// Called when the user submits a natural-language prompt to the agent.
    /// May return a line that flings a fragment of that prompt back at them —
    /// agent mode only (a shell command has nothing to quote).
    pub fn on_agent_prompt(&mut self, prompt: &str, swear_mode: bool) -> Option<String> {
        let ctx = MiseryContext {
            command_name: prompt
                .split_whitespace()
                .next()
                .unwrap_or("agent")
                .to_owned(),
            user_prompt: Some(prompt.to_owned()),
            swear_mode,
            ..Default::default()
        };
        self.engine.generate(&ctx, Phase::Quote)
    }

    /// Filenames in `path`, cached. Names only — file contents are never read.
    fn nearby_files(&mut self, path: Option<&str>) -> Vec<String> {
        let Some(path) = path else {
            return Vec::new();
        };

        if let Some((cached_path, files)) = &self.cached_directory {
            if cached_path == path {
                return files.clone();
            }
        }

        let files = list_directory(path);
        self.cached_directory = Some((path.to_owned(), files.clone()));
        files
    }
}

/// Basename of a directory path, for the `{DIR}` slot.
fn directory_name(path: Option<&str>) -> Option<String> {
    let path = path?.trim_end_matches('/');
    let name = path.rsplit(['/', '\\']).next()?;
    (!name.is_empty()).then(|| name.to_owned())
}

/// Reads up to [`MAX_NEARBY_FILES`] entry names from `path`.
///
/// Dotfiles are skipped: `.DS_Store` and `.git` are not funny, and naming a
/// user's hidden files reads as prying rather than as a joke.
fn list_directory(path: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };

    entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .take(MAX_NEARBY_FILES)
        .collect()
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
