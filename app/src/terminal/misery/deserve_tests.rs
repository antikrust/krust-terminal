use super::super::classify;
use super::*;

fn cmd(input: &str) -> ParsedCommand {
    classify::parse(input).expect("should parse")
}

/// The anticipation line is the only one still driven by the adaptive score;
/// the verdict is unconditional (see `the_verdict_always_fires`).
fn submit_p(input: &str, history: History) -> f32 {
    probability(&cmd(input), Phase::Submit, history, None, None)
}

#[test]
fn the_verdict_always_fires() {
    // Fork: the terminal always passes comment at the end, whatever the command.
    for input in ["ls", "cargo build", "git push", "clear"] {
        let p = probability(&cmd(input), Phase::Complete, History::default(), Some(0), None);
        assert_eq!(p, 1.0, "{input} should always get a verdict");
    }
}

#[test]
fn the_anticipation_fires_most_of_the_time() {
    // Even the command that looks most like real work clears the floor.
    let p = submit_p("cargo build", History::default());
    assert!(
        (SUBMIT_MIN_PROBABILITY..=SUBMIT_MAX_PROBABILITY).contains(&p),
        "submit {p} should sit within [{SUBMIT_MIN_PROBABILITY}, {SUBMIT_MAX_PROBABILITY}]"
    );
    assert!(p >= 0.5, "the anticipation should fire most of the time, got {p}");
}

#[test]
fn repetition_escalates_the_anticipation() {
    let once = submit_p("ls", History::default());
    let thrice = submit_p(
        "ls",
        History {
            repeat_count: 3,
            ..Default::default()
        },
    );
    assert!(thrice >= once, "repeats should not lower it: {once} -> {thrice}");
    assert!(
        thrice > SUBMIT_MIN_PROBABILITY,
        "a third `ls` should lift the anticipation above the floor, got {thrice}"
    );
}

#[test]
fn the_anticipation_is_rarer_than_the_verdict() {
    let submit = submit_p("ls", History::default());
    let complete = probability(&cmd("ls"), Phase::Complete, History::default(), Some(0), None);
    assert!(
        submit < complete,
        "anticipation ({submit}) should be rarer than the certain verdict ({complete})"
    );
}

#[test]
fn the_anticipation_stays_within_bounds() {
    // Hammer the extremes from both directions.
    let extreme_high = submit_p(
        "clear",
        History {
            repeat_count: 99,
            recent_misery_count: 0,
        },
    );
    assert!(extreme_high <= SUBMIT_MAX_PROBABILITY, "{extreme_high}");

    let extreme_low = submit_p(
        "cargo build && ./deploy",
        History {
            repeat_count: 0,
            recent_misery_count: 99,
        },
    );
    assert!(extreme_low >= SUBMIT_MIN_PROBABILITY, "{extreme_low}");
}
