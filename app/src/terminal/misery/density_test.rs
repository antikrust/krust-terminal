//! A manual inspection tool, not an assertion.
//!
//! Simulates a plausible working session and prints what misery mode would
//! have said, at the real adaptive rate. Assertions catch structural bugs;
//! reading actual output catches *tone* bugs — the doubled punctuation and the
//! "0 lines. you'll read four of them." cases were both found this way, and
//! both type-checked fine.
//!
//! Run it after touching templates or vocabulary:
//!
//! ```sh
//! cargo test -p warp --lib misery::density -- --ignored --nocapture
//! ```

use super::session::{CompletedCommand, MiserySession};

/// A plausible half hour: some repetition, a couple of failures, one long
/// build, and several commands that deserve to be left alone.
const SCRIPT: &[(&str, i32, u64, u64)] = &[
    // (command, exit code, duration ms, output lines)
    ("ls -la", 0, 40, 42),
    ("cd app/src", 0, 5, 0),
    ("ls", 0, 30, 18),
    ("ls", 0, 28, 18),
    ("ls", 0, 31, 18),
    ("cargo build", 0, 64_000, 210),
    ("git status", 0, 200, 12),
    ("git diff", 0, 180, 90),
    ("cargo test", 1, 45_000, 320),
    ("clear", 0, 8, 0),
    ("grep -r todo .", 0, 900, 7),
    ("rm -rf target", 0, 2_200, 0),
    ("npm install", 1, 21_000, 340),
    ("ls", 0, 29, 18),
    ("vim notes.md", 0, 9_000, 0),
    ("cat notes.md", 0, 15, 24),
];

#[test]
#[ignore = "manual inspection tool; run with --ignored --nocapture"]
fn print_a_simulated_session() {
    let mut session = MiserySession::new();
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|path| path.to_str().map(str::to_owned));

    let mut spoken = 0;
    for (command, exit_code, duration_ms, output_lines) in SCRIPT {
        if let Some(line) = session.on_command_submitted(command, cwd.as_deref(), true) {
            println!("  > {line}");
            spoken += 1;
        }
        println!("$ {command}  ({duration_ms}ms, exit {exit_code}, {output_lines} lines)");
        if let Some(line) = session.on_command_completed(
            CompletedCommand {
                command,
                working_directory: cwd.as_deref(),
                exit_code: *exit_code,
                started_at: None,
                output_lines: *output_lines,
            },
            true,
        ) {
            println!("  > {line}");
            spoken += 1;
        }
    }

    println!("\n{spoken} lines across {} commands", SCRIPT.len());
}
