use std::collections::HashSet;

use super::*;

fn ctx(command: &str) -> MiseryContext {
    MiseryContext {
        command_name: command.to_owned(),
        directory_name: Some("krust".to_owned()),
        nearby_files: vec![
            "Cargo.toml".to_owned(),
            "README.md".to_owned(),
            "src".to_owned(),
            "build.rs".to_owned(),
        ],
        exit_code: Some(0),
        succeeded: Some(true),
        duration: Some(2_000),
        output_lines: Some(12),
        swear_mode: false,
        user_prompt: None,
    }
}

#[test]
fn forced_generation_always_produces_a_line() {
    let mut engine = MiseryEngine::with_seed(1);
    for command in ["ls", "cd ..", "git status", "cargo build", "frobnicate"] {
        let line = engine.generate_forced(&ctx(command), Phase::Complete);
        assert!(line.is_some(), "no line for {command}");
    }
}

#[test]
fn generated_lines_have_no_unfilled_slots() {
    let mut engine = MiseryEngine::with_seed(2);
    for i in 0..500 {
        let phase = if i % 2 == 0 {
            Phase::Submit
        } else {
            Phase::Complete
        };
        let mut c = ctx("ls");
        if phase == Phase::Submit {
            // Submit has no results yet; templates needing them must be skipped.
            c.exit_code = None;
            c.succeeded = None;
            c.duration = None;
            c.output_lines = None;
        }
        if let Some(line) = engine.generate_forced(&c, phase) {
            assert!(!line.contains('{'), "unfilled slot in: {line}");
            assert!(!line.contains('}'), "unfilled slot in: {line}");
            assert!(!line.contains("  "), "double space in: {line}");
            assert!(!line.trim().is_empty());
        }
    }
}

#[test]
fn does_not_repeat_within_its_memory() {
    let mut engine = MiseryEngine::with_seed(3);
    let mut seen = HashSet::new();
    // Fewer than RECENT_LINES_MEMORY draws, so none may collide.
    for _ in 0..25 {
        if let Some(line) = engine.generate_forced(&ctx("ls"), Phase::Complete) {
            assert!(seen.insert(line.clone()), "repeated too soon: {line}");
        }
    }
}

#[test]
fn produces_substantial_variety() {
    // The whole premise is that a small template bank yields a large surface.
    let mut engine = MiseryEngine::with_seed(4);
    let mut seen = HashSet::new();
    for _ in 0..400 {
        if let Some(line) = engine.generate_forced(&ctx("ls"), Phase::Complete) {
            seen.insert(line);
        }
    }
    assert!(
        seen.len() > 60,
        "expected wide variety, got {} distinct lines",
        seen.len()
    );
}

#[test]
fn topical_lines_differ_by_command_class() {
    let mut engine = MiseryEngine::with_seed(5);
    let mut listing = HashSet::new();
    let mut destructive = HashSet::new();
    for _ in 0..300 {
        if let Some(l) = engine.generate_forced(&ctx("ls"), Phase::Submit) {
            listing.insert(l);
        }
        if let Some(l) = engine.generate_forced(&ctx("rm -rf build"), Phase::Submit) {
            destructive.insert(l);
        }
    }
    // Each class should reach lines the other never can.
    assert!(
        listing.iter().any(|l| !destructive.contains(l)),
        "listing produced nothing unique"
    );
    assert!(
        destructive.iter().any(|l| !listing.contains(l)),
        "destructive produced nothing unique"
    );
}

#[test]
fn never_mentions_output_volume_when_there_was_none() {
    // "0 lines. you'll read four of them." is nonsense.
    let mut engine = MiseryEngine::with_seed(20);
    let mut c = ctx("cd ..");
    c.output_lines = Some(0);
    for _ in 0..400 {
        if let Some(line) = engine.generate_forced(&c, Phase::Complete) {
            assert!(
                !line.contains("0 lines") && !line.contains("0 filenames"),
                "claimed a volume of nothing: {line}"
            );
        }
    }
}

#[test]
fn does_not_report_a_failure_exit_on_success() {
    // "exit 0. I'd explain, but you wouldn't read that either." reads as a
    // failure line and must never fire on a successful command.
    let mut engine = MiseryEngine::with_seed(21);
    let c = ctx("rm -rf target"); // succeeded: Some(true)
    for _ in 0..400 {
        if let Some(line) = engine.generate_forced(&c, Phase::Complete) {
            assert!(!line.contains("exit 0"), "failure voice on success: {line}");
        }
    }
}

#[test]
fn failures_never_get_a_congratulatory_voice() {
    let mut engine = MiseryEngine::with_seed(22);
    let mut c = ctx("git push");
    c.exit_code = Some(1);
    c.succeeded = Some(false);
    for _ in 0..400 {
        if let Some(line) = engine.generate_forced(&c, Phase::Complete) {
            for success_only in ["done in", "built. in", "it's gone", "congratulations"] {
                assert!(
                    !line.contains(success_only),
                    "success voice on failure: {line}"
                );
            }
        }
    }
}

#[test]
fn submit_phase_never_uses_outcome_conditional_lines() {
    // Nothing has happened yet, so no line may imply an outcome.
    let mut engine = MiseryEngine::with_seed(23);
    let mut c = ctx("cargo build");
    c.exit_code = None;
    c.succeeded = None;
    c.duration = None;
    c.output_lines = None;
    for _ in 0..400 {
        if let Some(line) = engine.generate_forced(&c, Phase::Submit) {
            for outcome_word in ["exit", "lines", "failed", "done in", "seconds"] {
                assert!(
                    !line.contains(outcome_word),
                    "outcome language before the command ran: {line}"
                );
            }
        }
    }
}

#[test]
fn survives_an_empty_directory() {
    let mut engine = MiseryEngine::with_seed(6);
    let mut c = ctx("ls");
    c.nearby_files.clear();
    c.directory_name = None;
    // File-dependent templates are unfillable, but the bank must still have
    // something to say.
    let mut produced = 0;
    for _ in 0..100 {
        if engine.generate_forced(&c, Phase::Complete).is_some() {
            produced += 1;
        }
    }
    assert!(produced > 0, "should still manage contempt without files");
}

#[test]
fn unparseable_command_yields_nothing() {
    let mut engine = MiseryEngine::with_seed(7);
    let mut c = ctx("");
    c.command_name = String::new();
    assert!(engine.generate(&c, Phase::Complete).is_none());
    assert!(engine.generate_forced(&c, Phase::Complete).is_none());
}

#[test]
fn repetition_is_tracked_across_commands() {
    let mut engine = MiseryEngine::with_seed(8);
    let command = classify::parse("ls").unwrap();
    for _ in 0..4 {
        engine.note_command(&command);
    }
    let history = engine.history(&command, Phase::Submit);
    assert_eq!(history.repeat_count, 3, "four runs means three repeats");
}

#[test]
fn the_two_voices_do_not_starve_each_other() {
    // A shared backoff window let completions suppress the next submit roll,
    // which pushed submit lines down to a rate where the voice never
    // registered. Each phase must pace itself independently.
    let commands = ["ls -la", "cd ~/x", "git status", "ls", "cat f.txt"];
    let (mut submits, mut completes, mut total) = (0, 0, 0);
    for seed in 0..60 {
        let mut engine = MiseryEngine::with_seed(seed);
        for command in commands {
            total += 1;
            let mut submit_ctx = ctx(command);
            submit_ctx.exit_code = None;
            submit_ctx.succeeded = None;
            submit_ctx.duration = None;
            submit_ctx.output_lines = None;
            if engine.generate(&submit_ctx, Phase::Submit).is_some() {
                submits += 1;
            }
            if engine.generate(&ctx(command), Phase::Complete).is_some() {
                completes += 1;
            }
        }
    }

    let submit_rate = submits as f64 / total as f64;
    let complete_rate = completes as f64 / total as f64;

    assert!(
        submit_rate > 0.15,
        "submit voice too rare to register: {submit_rate:.2}"
    );
    assert!(
        submit_rate < complete_rate,
        "submit should stay the rarer voice: {submit_rate:.2} vs {complete_rate:.2}"
    );
}

#[test]
fn the_verdict_fires_on_essentially_every_command() {
    // Fork: the adaptive rate is gone at completion. The terminal always has
    // something to say at the end — trivia or real work, success or failure,
    // all get a verdict. (`generate` can still miss when no template fills, so
    // the rate is near-total rather than exactly total.)
    let commands: &[(&str, i32)] = &[
        ("ls", 0),
        ("cargo build", 0),
        ("git push", 1),
        ("clear", 0),
        ("cat notes.md", 0),
    ];
    let (mut spoke, mut total) = (0, 0);
    for seed in 0..40 {
        let mut engine = MiseryEngine::with_seed(seed);
        for (command, exit) in commands {
            let mut c = ctx(command);
            c.exit_code = Some(*exit);
            c.succeeded = Some(*exit == 0);
            total += 1;
            if engine.generate(&c, Phase::Complete).is_some() {
                spoke += 1;
            }
        }
    }
    let rate = spoke as f64 / total as f64;
    assert!(
        rate > 0.95,
        "the verdict should fire on nearly every command, got {rate:.2}"
    );
}

#[test]
fn quotes_fling_a_fragment_of_the_prompt_back() {
    let mut engine = MiseryEngine::with_seed(7);
    let prompt = "refactor the auth module to use the new session api";
    let mut c = ctx("refactor");
    c.user_prompt = Some(prompt.to_owned());
    let line = engine
        .generate_forced(&c, Phase::Quote)
        .expect("agent mode should produce a quote");
    assert!(
        prompt.split_whitespace().any(|word| line.contains(word)),
        "the quote should echo the prompt back: {line}"
    );
}

#[test]
fn quotes_never_fire_for_a_shell_command() {
    // No `user_prompt` (terminal mode), so every quote template fails to fill.
    let mut engine = MiseryEngine::with_seed(7);
    assert!(engine.generate_forced(&ctx("ls"), Phase::Quote).is_none());
}
