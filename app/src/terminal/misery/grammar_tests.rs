use rand::rngs::StdRng;
use rand::SeedableRng;

use super::*;

fn ctx() -> MiseryContext {
    MiseryContext {
        command_name: "ls".to_owned(),
        directory_name: Some("warp".to_owned()),
        nearby_files: vec![
            "Cargo.toml".to_owned(),
            "README.md".to_owned(),
            "src".to_owned(),
        ],
        exit_code: Some(0),
        succeeded: Some(true),
        duration: Some(1500),
        output_lines: Some(42),
        swear_mode: false,
    }
}

fn rng() -> StdRng {
    StdRng::seed_from_u64(7)
}

#[test]
fn expands_literal_text_unchanged() {
    let out = expand("nothing to fill here", &ctx(), &mut rng()).unwrap();
    assert_eq!(out, "nothing to fill here");
}

#[test]
fn fills_context_slots() {
    let out = expand("{CMD} in {DIR} took {DURATION}", &ctx(), &mut rng()).unwrap();
    assert_eq!(out, "`ls` in `warp` took 1.5 seconds");
}

#[test]
fn activity_uses_real_filenames() {
    let out = expand("I was {ACTIVITY}", &ctx(), &mut rng()).unwrap();
    let names_a_file = ["Cargo.toml", "README.md", "src"]
        .iter()
        .any(|f| out.contains(f));
    assert!(names_a_file, "activity should name a real file: {out}");
}

#[test]
fn repeated_file_slots_do_not_repeat_a_name() {
    // Two FILE slots must draw different files, or the line reads as broken.
    for seed in 0..50 {
        let mut r = StdRng::seed_from_u64(seed);
        let out = expand("{FILE} and {FILE}", &ctx(), &mut r).unwrap();
        let (left, right) = out.split_once(" and ").unwrap();
        assert_ne!(left, right, "seed {seed} produced a duplicate: {out}");
    }
}

#[test]
fn missing_slot_data_fails_the_whole_expansion() {
    // A submit-phase context has no duration; a duration template must be
    // rejected outright rather than rendering an empty gap.
    let mut ctx = ctx();
    ctx.duration = None;
    assert!(expand("took {DURATION}", &ctx, &mut rng()).is_none());
}

#[test]
fn empty_directory_rejects_file_templates() {
    let mut ctx = ctx();
    ctx.nearby_files.clear();
    assert!(expand("I was {ACTIVITY}", &ctx, &mut rng()).is_none());
    assert!(expand("never even {NEVER_EVEN} {FILE}", &ctx, &mut rng()).is_none());
    // But a template needing no files still works.
    assert!(expand("{CMD} is {ADJECTIVE}", &ctx, &mut rng()).is_some());
}

#[test]
fn unknown_slot_is_rejected_not_rendered() {
    assert!(expand("{NO_SUCH_SLOT}", &ctx(), &mut rng()).is_none());
}

#[test]
fn unclosed_brace_is_rejected() {
    assert!(expand("dangling {CMD", &ctx(), &mut rng()).is_none());
}

#[test]
fn verdict_follows_success_state() {
    let mut failed = ctx();
    failed.succeeded = Some(false);
    // Sample enough times to be confident we're drawing from the right list.
    for seed in 0..20 {
        let mut r = StdRng::seed_from_u64(seed);
        let out = expand("{VERDICT}", &failed, &mut r).unwrap();
        assert!(
            super::super::vocab::VERDICT_FAILURE.contains(&out.as_str()),
            "expected a failure verdict, got: {out}"
        );
    }
}

#[test]
fn swear_mode_selects_harsher_vocabulary() {
    let mut sweary = ctx();
    sweary.swear_mode = true;
    for seed in 0..20 {
        let mut r = StdRng::seed_from_u64(seed);
        let out = expand("{EXCLAIM}", &sweary, &mut r).unwrap();
        assert!(
            super::super::vocab::EXCLAIM_SWEAR.contains(&out.as_str()),
            "expected sweary vocabulary, got: {out}"
        );
    }
}

#[test]
fn doubled_sentence_punctuation_is_collapsed() {
    // "there. happy?" + a template's own full stop must not give "happy?.".
    assert_eq!(tidy_punctuation("there. happy?."), "there. happy?");
    assert_eq!(tidy_punctuation("done.."), "done.");
    assert_eq!(tidy_punctuation("what!?"), "what!");
    // Punctuation that isn't doubled is untouched.
    assert_eq!(tidy_punctuation("one. two. three."), "one. two. three.");
    assert_eq!(tidy_punctuation("no punctuation"), "no punctuation");
}

#[test]
fn expansion_never_emits_doubled_punctuation() {
    let mut failed = ctx();
    failed.succeeded = Some(false);
    for seed in 0..100 {
        let mut r = StdRng::seed_from_u64(seed);
        for template in ["{VERDICT}.", "{VERDICT}. {COMPLAINT}.", "{EXCLAIM}. {VERDICT}."] {
            let out = expand(template, &failed, &mut r).unwrap();
            for pair in ["?.", "!.", "..", ".?", ".!"] {
                assert!(!out.contains(pair), "doubled punctuation {pair:?} in: {out}");
            }
        }
    }
}

#[test]
fn duration_formatting() {
    assert_eq!(format_duration(400), "400ms");
    assert_eq!(format_duration(1500), "1.5 seconds");
    assert_eq!(format_duration(42_000), "42 seconds");
    assert_eq!(format_duration(120_000), "2 minutes");
    assert_eq!(format_duration(95_000), "1m35s");
}
