use super::super::classify;
use super::*;

fn cmd(input: &str) -> ParsedCommand {
    classify::parse(input).expect("should parse")
}

fn p(input: &str, history: History, exit: Option<i32>, millis: Option<u64>) -> f32 {
    probability(&cmd(input), Phase::Complete, history, exit, millis)
}

#[test]
fn trivial_commands_score_above_real_work() {
    let listing = p("ls", History::default(), Some(0), None);
    let building = p("cargo build", History::default(), Some(0), None);
    assert!(
        listing > building,
        "ls ({listing}) should outrank cargo build ({building})"
    );
}

#[test]
fn repetition_increases_the_score() {
    let once = p("ls", History::default(), Some(0), None);
    let thrice = p(
        "ls",
        History {
            repeat_count: 3,
            ..Default::default()
        },
        Some(0),
        None,
    );
    assert!(thrice > once, "repeats should escalate: {once} -> {thrice}");
}

#[test]
fn failure_increases_the_score() {
    let ok = p("git push", History::default(), Some(0), None);
    let failed = p("git push", History::default(), Some(1), None);
    assert!(failed > ok, "failure should escalate: {ok} -> {failed}");
}

#[test]
fn pipelines_are_treated_as_effort() {
    let bare = p("ls", History::default(), Some(0), None);
    let piped = p("ls | wc -l", History::default(), Some(0), None);
    assert!(
        piped < bare,
        "pipelines should be spared: {bare} -> {piped}"
    );
}

#[test]
fn slow_real_work_is_spared_but_slow_trivia_is_not() {
    let slow_build = p("cargo build", History::default(), Some(0), Some(60_000));
    let fast_build = p("cargo build", History::default(), Some(0), Some(100));
    assert!(slow_build < fast_build, "long builds should be spared");

    let slow_ls = p("ls", History::default(), Some(0), Some(60_000));
    let fast_ls = p("ls", History::default(), Some(0), Some(100));
    assert!(slow_ls > fast_ls, "a slow ls is its own punchline");
}

#[test]
fn recent_misery_backs_off() {
    let quiet = p("ls", History::default(), Some(0), None);
    let noisy = p(
        "ls",
        History {
            recent_misery_count: 3,
            ..Default::default()
        },
        Some(0),
        None,
    );
    assert!(noisy < quiet, "should back off: {quiet} -> {noisy}");
}

#[test]
fn submit_phase_is_rarer_than_completion() {
    let history = History::default();
    let submit = probability(&cmd("ls"), Phase::Submit, history, None, None);
    let complete = probability(&cmd("ls"), Phase::Complete, history, Some(0), None);
    assert!(
        submit < complete,
        "submit ({submit}) should be rarer than complete ({complete})"
    );
}

#[test]
fn always_within_bounds() {
    // Hammer the extremes from both directions.
    let extreme_high = p(
        "clear",
        History {
            repeat_count: 99,
            recent_misery_count: 0,
        },
        Some(1),
        Some(60_000),
    );
    assert!(extreme_high <= MAX_PROBABILITY, "{extreme_high}");

    let extreme_low = p(
        "cargo build && ./deploy",
        History {
            repeat_count: 0,
            recent_misery_count: 99,
        },
        Some(0),
        Some(600_000),
    );
    assert!(extreme_low >= MIN_PROBABILITY, "{extreme_low}");
    assert!(extreme_low > 0.0, "never fully silent");
}
