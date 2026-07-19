use super::*;

const GB: u64 = 1_073_741_824;

fn size(billions: f64) -> ParameterSize {
    ParameterSize { billions }
}

#[test]
fn parses_the_tag_forms_the_library_uses() {
    assert_eq!(ParameterSize::parse("8b"), Some(size(8.0)));
    assert_eq!(ParameterSize::parse("405b"), Some(size(405.0)));
    assert_eq!(ParameterSize::parse("1.5b"), Some(size(1.5)));
    assert_eq!(ParameterSize::parse("3.8B"), Some(size(3.8)));
    assert_eq!(ParameterSize::parse(" 7b "), Some(size(7.0)));
    // Sub-billion models are advertised in millions.
    assert_eq!(ParameterSize::parse("137m"), Some(size(0.137)));
}

#[test]
fn parses_mixture_of_experts_notation() {
    // Mixtral is advertised as 8x7b. All experts stay resident, so the memory
    // estimate uses the total rather than the active subset.
    assert_eq!(ParameterSize::parse("8x7b"), Some(size(56.0)));
    assert_eq!(ParameterSize::parse("8x22b"), Some(size(176.0)));
}

#[test]
fn parses_gemma_effective_parameter_notation() {
    // Gemma 3n advertises e2b / e4b.
    assert_eq!(ParameterSize::parse("e2b"), Some(size(2.0)));
    assert_eq!(ParameterSize::parse("e4b"), Some(size(4.0)));
}

#[test]
fn rejects_spans_that_are_not_sizes() {
    // These all appear as spans in a listing entry and must not be mistaken
    // for parameter counts.
    for junk in [
        "tools", "vision", "Pulls", "Updated", "", "b", "-4b", "0b", "xb", "8xb", "x7b",
    ] {
        assert_eq!(ParameterSize::parse(junk), None, "accepted {junk:?}");
    }
}

#[test]
fn estimates_land_near_published_model_footprints() {
    // A 7B at Ollama's default quantisation is ~4.1 GB of weights; with
    // runtime overhead the estimate should sit a little above that and
    // comfortably under double it.
    let seven_b = size(7.0).estimated_bytes();
    assert!(
        seven_b > 4 * GB && seven_b < 8 * GB,
        "7B estimate looks wrong: {}",
        format_gb(seven_b)
    );

    // Estimates must grow with parameter count.
    assert!(size(70.0).estimated_bytes() > size(8.0).estimated_bytes());
    assert!(size(8.0).estimated_bytes() > size(1.5).estimated_bytes());
}

#[test]
fn a_small_model_is_comfortable_on_a_modest_machine() {
    let verdict = assess(size(3.0), 16 * GB);
    assert_eq!(verdict.fit, Fit::Comfortable);
    assert!(!verdict.fit.is_warning());
}

#[test]
fn a_large_model_on_a_small_machine_warns_but_is_not_blocked() {
    // The user chose warn-but-allow: every verdict is advisory.
    let verdict = assess(size(70.0), 16 * GB);
    assert_eq!(verdict.fit, Fit::Exceeds);
    assert!(verdict.fit.is_warning());
    assert!(
        verdict.message.contains("GB"),
        "message should quote real numbers: {}",
        verdict.message
    );
}

#[test]
fn a_model_that_fits_ram_but_not_the_budget_is_tight() {
    // 24 GB machine, usable budget 18 GB. A 32B needs ~20.4 GB: fits in RAM,
    // but not alongside anything else.
    let verdict = assess(size(32.0), 24 * GB);
    assert_eq!(verdict.fit, Fit::Tight);
    assert!(verdict.message.contains("swapping"));
}

#[test]
fn this_machines_real_verdicts_are_sane() {
    // Guards the estimate against drifting into nonsense on a normal laptop.
    let Some(total) = total_system_memory_bytes() else {
        return; // Unsupported platform; the UI skips the check entirely.
    };
    assert!(
        total >= 2 * GB,
        "implausible memory reading: {}",
        format_gb(total)
    );

    // Something tiny should always be fine, and a 405B never is.
    assert_eq!(assess(size(1.0), total).fit, Fit::Comfortable);
    assert_eq!(assess(size(405.0), total).fit, Fit::Exceeds);
}

#[test]
fn formats_sizes_for_display_without_noise() {
    assert_eq!(size(8.0).to_string(), "8B");
    assert_eq!(size(1.5).to_string(), "1.5B");
    assert_eq!(size(0.5).to_string(), "500M");
}

#[test]
fn formats_gigabytes_readably() {
    assert_eq!(format_gb(4 * GB), "4.0 GB");
    assert_eq!(format_gb(24 * GB), "24 GB");
}

#[test]
fn badges_flag_only_the_warnings() {
    assert_eq!(Fit::Comfortable.badge(), None);
    assert_eq!(Fit::Tight.badge(), Some("STARVED"));
    assert_eq!(Fit::Exceeds.badge(), Some("WON'T FIT"));
    // Every warning badge has a legend line, and comfortable has none.
    assert!(Fit::Comfortable.badge_meaning().is_none());
    assert!(Fit::Tight.badge_meaning().is_some());
    assert!(Fit::Exceeds.badge_meaning().is_some());
}

#[test]
fn assess_tag_reads_the_size_after_the_colon() {
    let total = 24 * GB;
    // A small model is comfortable; a giant one won't fit — same verdict as
    // assessing the parsed size directly.
    assert_eq!(
        assess_tag("codellama:1b", total).map(|v| v.fit),
        Some(Fit::Comfortable)
    );
    assert_eq!(
        assess_tag("llama3.1:405b", total).map(|v| v.fit),
        Some(Fit::Exceeds)
    );
    // No parseable size → no verdict, so callers show nothing rather than guess.
    assert!(assess_tag("mystery-model:latest", total).is_none());
    // A bare name with an embedded size still resolves.
    assert_eq!(assess_tag("70b", total).map(|v| v.fit), Some(Fit::Exceeds));
}
