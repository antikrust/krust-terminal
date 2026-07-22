use super::*;
use crate::ai::agent_sdk::driver::harness::ollama_stream_exec::OLLAMA_MODEL_ID_PREFIX;

fn member(name: &str, thinking: bool) -> Member {
    Member {
        name: name.to_string(),
        family: model_family(name),
        coder: is_coder_model(name),
        thinking,
        size_billions: model_size_billions(name),
    }
}

#[test]
fn committee_model_id_matches_the_ollama_prefix_convention() {
    assert_eq!(
        OLLAMA_COMMITTEE_MODEL_ID,
        format!("{OLLAMA_MODEL_ID_PREFIX}{COMMITTEE_MODEL_NAME}")
    );
}

// --- config ---------------------------------------------------------------

#[test]
fn config_defaults_when_setting_is_absent_or_garbage() {
    assert_eq!(
        OllamaCommitteeConfig::from_settings_json(None),
        OllamaCommitteeConfig::default()
    );
    assert_eq!(
        OllamaCommitteeConfig::from_settings_json(Some("not json")),
        OllamaCommitteeConfig::default()
    );
    assert_eq!(
        OllamaCommitteeConfig::from_settings_json(Some("{}")),
        OllamaCommitteeConfig::default()
    );
}

#[test]
fn config_parses_and_clamps_out_of_range_values() {
    let parsed = OllamaCommitteeConfig::from_settings_json(Some(
        r#"{"reviewers": 3, "max_loaded": 2, "panel_plan": ["notes", "exemplar"]}"#,
    ));
    assert_eq!(
        parsed,
        OllamaCommitteeConfig {
            reviewers: 3,
            max_loaded: 2,
            panel_plan: vec![PanelRoundKind::Notes, PanelRoundKind::Exemplar],
            roles: BTreeMap::new(),
        }
    );

    // Unknown kinds are skipped; the plan is capped at the extra-round max.
    let messy = OllamaCommitteeConfig::from_settings_json(Some(
        r#"{"reviewers": 99, "max_loaded": 0, "panel_plan": ["notes", "vibes", "peers", "converge"]}"#,
    ));
    assert_eq!(messy.reviewers, MAX_REVIEWERS);
    assert_eq!(messy.max_loaded, 1);
    assert_eq!(
        messy.panel_plan,
        vec![PanelRoundKind::Notes, PanelRoundKind::Peers]
    );
}

#[test]
fn a_vintage_round_count_maps_to_notes_rounds() {
    // Configs saved before round kinds existed stored a plain count; N
    // rounds meant N-1 coaching passes.
    let two = OllamaCommitteeConfig::from_settings_json(Some(r#"{"panel_rounds": 2}"#));
    assert_eq!(two.panel_plan, vec![PanelRoundKind::Notes]);
    let three = OllamaCommitteeConfig::from_settings_json(Some(r#"{"panel_rounds": 3}"#));
    assert_eq!(
        three.panel_plan,
        vec![PanelRoundKind::Notes, PanelRoundKind::Notes]
    );
    let one = OllamaCommitteeConfig::from_settings_json(Some(r#"{"panel_rounds": 1}"#));
    assert_eq!(one.panel_plan, Vec::<PanelRoundKind>::new());
}

#[test]
fn config_roundtrips_through_its_settings_json() {
    let config = OllamaCommitteeConfig {
        reviewers: 3,
        max_loaded: 2,
        panel_plan: vec![PanelRoundKind::Exemplar, PanelRoundKind::Converge],
        roles: BTreeMap::new(),
    };
    assert_eq!(
        OllamaCommitteeConfig::from_settings_json(Some(&config.to_settings_json())),
        config
    );
}

#[test]
fn keep_alive_unloads_immediately_only_at_one_resident_model() {
    assert_eq!(OllamaCommitteeConfig::default().keep_alive(), "0");
    let warm = OllamaCommitteeConfig {
        reviewers: 2,
        max_loaded: 2,
        panel_plan: Vec::new(),
        roles: BTreeMap::new(),
    };
    assert_eq!(warm.keep_alive(), "5m");
}

#[test]
fn round_kinds_roundtrip_through_their_names() {
    for kind in PanelRoundKind::ALL {
        assert_eq!(PanelRoundKind::from_str(kind.as_str()), Some(kind));
    }
    assert_eq!(PanelRoundKind::from_str("vibes"), None);
}

#[test]
fn cycling_a_round_slot_walks_the_kinds_and_off_clears_later_rounds() {
    let mut plan: Vec<PanelRoundKind> = Vec::new();
    // Off → each kind in order → Off again.
    cycle_panel_round(&mut plan, 0);
    assert_eq!(plan, vec![PanelRoundKind::Notes]);
    cycle_panel_round(&mut plan, 0);
    assert_eq!(plan, vec![PanelRoundKind::Exemplar]);
    cycle_panel_round(&mut plan, 0);
    assert_eq!(plan, vec![PanelRoundKind::Peers]);
    cycle_panel_round(&mut plan, 0);
    assert_eq!(plan, vec![PanelRoundKind::Converge]);
    cycle_panel_round(&mut plan, 0);
    assert_eq!(plan, Vec::<PanelRoundKind>::new());

    // Turning slot 0 off drops slot 1 with it — a plan has no holes.
    let mut plan = vec![PanelRoundKind::Converge, PanelRoundKind::Notes];
    cycle_panel_round(&mut plan, 0);
    assert_eq!(plan, Vec::<PanelRoundKind>::new());

    // Cycling an out-of-range slot appends rather than leaving a hole.
    let mut plan: Vec<PanelRoundKind> = Vec::new();
    cycle_panel_round(&mut plan, 1);
    assert_eq!(plan, vec![PanelRoundKind::Notes]);
}

// --- model metadata -------------------------------------------------------

#[test]
fn family_is_the_leading_alphabetic_run() {
    assert_eq!(model_family("qwen2.5-coder:7b"), "qwen");
    assert_eq!(model_family("deepseek-r1:8b"), "deepseek");
    assert_eq!(model_family("codellama:13b"), "codellama");
    assert_eq!(model_family("llama3.1:8b"), "llama");
}

#[test]
fn coder_models_are_recognised_by_name_markers() {
    assert!(is_coder_model("qwen2.5-coder:7b"));
    assert!(is_coder_model("codellama:13b"));
    assert!(is_coder_model("deepseek-coder:6.7b"));
    assert!(!is_coder_model("llama3.1:8b"));
    assert!(!is_coder_model("deepseek-r1:8b"));
}

#[test]
fn size_parses_from_the_tag_suffix_and_defaults_to_zero() {
    assert_eq!(model_size_billions("qwen2.5-coder:7b"), 7.0);
    assert_eq!(model_size_billions("deepseek-coder:6.7b"), 6.7);
    assert_eq!(model_size_billions("mymodel:latest"), 0.0);
    assert_eq!(model_size_billions("untagged"), 0.0);
}

// --- role assignment ------------------------------------------------------

#[test]
fn reviewers_prefer_coder_models_and_the_judge_prefers_thinking() {
    let members = vec![
        member("llama3.1:8b", false),
        member("qwen2.5-coder:7b", false),
        member("deepseek-r1:8b", true),
        member("codellama:13b", false),
    ];
    let plan = assign_roles(
        members,
        2,
        &CommitteeLeaderboard::default(),
        &BTreeMap::new(),
    )
    .unwrap();

    // Both reviewer seats go to coder models (13B codellama outranks 7B qwen).
    assert_eq!(plan.reviewers.len(), 2);
    assert!(plan.reviewers.iter().all(|reviewer| reviewer.coder));
    assert_eq!(plan.reviewers[0].name, "codellama:13b");
    // The thinking model on the bench takes the gavel.
    assert_eq!(plan.judge.name, "deepseek-r1:8b");
    assert!(!plan.judge_is_reviewer);
}

#[test]
fn reviewer_seats_favour_distinct_families_before_second_helpings() {
    let members = vec![
        member("qwen2.5-coder:14b", false),
        member("qwen2.5-coder:7b", false),
        member("deepseek-coder:6.7b", false),
        member("llama3.1:8b", false),
    ];
    let plan = assign_roles(
        members,
        2,
        &CommitteeLeaderboard::default(),
        &BTreeMap::new(),
    )
    .unwrap();
    let families: Vec<&str> = plan
        .reviewers
        .iter()
        .map(|reviewer| reviewer.family.as_str())
        .collect();
    assert_eq!(families, vec!["qwen", "deepseek"]);
}

#[test]
fn a_single_installed_model_plays_every_role_and_discloses_it() {
    let plan = assign_roles(
        vec![member("qwen2.5-coder:7b", false)],
        2,
        &CommitteeLeaderboard::default(),
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(plan.reviewers.len(), 1);
    assert_eq!(plan.judge.name, "qwen2.5-coder:7b");
    assert!(plan.judge_is_reviewer);
}

#[test]
fn no_models_means_no_plan() {
    assert_eq!(
        assign_roles(
            Vec::new(),
            2,
            &CommitteeLeaderboard::default(),
            &BTreeMap::new()
        ),
        None
    );
}

#[test]
fn a_proven_confirm_rate_outranks_size_within_coder_models() {
    let mut leaderboard = CommitteeLeaderboard::default();
    leaderboard.insert(
        "deepseek-coder:6.7b".to_string(),
        ModelStats {
            runs: 5,
            findings_raised: 10,
            findings_confirmed: 9,
            panel_wins: 0,
        },
    );
    leaderboard.insert(
        "codellama:13b".to_string(),
        ModelStats {
            runs: 5,
            findings_raised: 10,
            findings_confirmed: 1,
            panel_wins: 0,
        },
    );
    let members = vec![
        member("codellama:13b", false),
        member("deepseek-coder:6.7b", false),
    ];
    let plan = assign_roles(members, 1, &leaderboard, &BTreeMap::new()).unwrap();
    // The smaller model earned the seat: 90% of its findings survive the
    // judge, against 10% for the bigger one.
    assert_eq!(plan.reviewers[0].name, "deepseek-coder:6.7b");
}

#[test]
fn a_thin_leaderboard_record_does_not_influence_ranking() {
    let mut leaderboard = CommitteeLeaderboard::default();
    // Two runs is below LEADERBOARD_MIN_RUNS — neutral, so size decides.
    leaderboard.insert(
        "deepseek-coder:6.7b".to_string(),
        ModelStats {
            runs: 2,
            findings_raised: 4,
            findings_confirmed: 4,
            panel_wins: 0,
        },
    );
    let members = vec![
        member("codellama:13b", false),
        member("deepseek-coder:6.7b", false),
    ];
    let plan = assign_roles(members, 1, &leaderboard, &BTreeMap::new()).unwrap();
    assert_eq!(plan.reviewers[0].name, "codellama:13b");
}

#[test]
fn casting_a_model_off_keeps_it_out_of_every_seat() {
    let mut roles = BTreeMap::new();
    roles.insert("qwen2.5-coder:32b".to_string(), CommitteeRole::Benched);
    let members = vec![
        member("qwen2.5-coder:32b", false),
        member("qwen2.5-coder:14b", false),
        member("deepseek-r1:14b", true),
    ];
    let plan = assign_roles(members, 2, &CommitteeLeaderboard::default(), &roles).unwrap();
    assert!(plan
        .reviewers
        .iter()
        .all(|reviewer| reviewer.name != "qwen2.5-coder:32b"));
    assert_ne!(plan.judge.name, "qwen2.5-coder:32b");
}

#[test]
fn a_forced_judge_takes_the_gavel_and_keeps_its_reviewer_seat_free() {
    let mut roles = BTreeMap::new();
    roles.insert("llama3.1:8b".to_string(), CommitteeRole::Judge);
    let members = vec![
        member("llama3.1:8b", false),
        member("qwen2.5-coder:14b", false),
        member("deepseek-r1:14b", true),
    ];
    let plan = assign_roles(members, 2, &CommitteeLeaderboard::default(), &roles).unwrap();
    // Chosen over the thinking model the heuristics would have preferred.
    assert_eq!(plan.judge.name, "llama3.1:8b");
    assert!(!plan.judge_is_reviewer);
    assert!(plan
        .reviewers
        .iter()
        .all(|reviewer| reviewer.name != "llama3.1:8b"));
}

#[test]
fn a_forced_reviewer_is_seated_ahead_of_the_heuristics() {
    let mut roles = BTreeMap::new();
    // A non-coder model the ranking would never seat on its own.
    roles.insert("llama3.1:8b".to_string(), CommitteeRole::Reviewer);
    let members = vec![
        member("llama3.1:8b", false),
        member("qwen2.5-coder:14b", false),
        member("codellama:13b", false),
        member("deepseek-r1:14b", true),
    ];
    let plan = assign_roles(members, 1, &CommitteeLeaderboard::default(), &roles).unwrap();
    assert_eq!(plan.reviewers.len(), 1);
    assert_eq!(plan.reviewers[0].name, "llama3.1:8b");
}

#[test]
fn casting_everything_off_leaves_no_committee() {
    let mut roles = BTreeMap::new();
    roles.insert("solo:7b".to_string(), CommitteeRole::Benched);
    assert_eq!(
        assign_roles(
            vec![member("solo:7b", false)],
            2,
            &CommitteeLeaderboard::default(),
            &roles
        ),
        None
    );
}

#[test]
fn casting_cycles_auto_reviewer_judge_off_and_back() {
    let mut roles = BTreeMap::new();
    cycle_committee_role(&mut roles, "qwen2.5-coder:14b");
    assert_eq!(
        roles.get("qwen2.5-coder:14b"),
        Some(&CommitteeRole::Reviewer)
    );
    cycle_committee_role(&mut roles, "qwen2.5-coder:14b");
    assert_eq!(roles.get("qwen2.5-coder:14b"), Some(&CommitteeRole::Judge));
    cycle_committee_role(&mut roles, "qwen2.5-coder:14b");
    assert_eq!(
        roles.get("qwen2.5-coder:14b"),
        Some(&CommitteeRole::Benched)
    );
    cycle_committee_role(&mut roles, "qwen2.5-coder:14b");
    assert_eq!(roles.get("qwen2.5-coder:14b"), None);
}

#[test]
fn casting_survives_the_settings_roundtrip() {
    let mut roles = BTreeMap::new();
    roles.insert("qwen2.5-coder:32b".to_string(), CommitteeRole::Benched);
    roles.insert("deepseek-r1:14b".to_string(), CommitteeRole::Judge);
    let config = OllamaCommitteeConfig {
        reviewers: 2,
        max_loaded: 1,
        panel_plan: Vec::new(),
        roles,
    };
    assert_eq!(
        OllamaCommitteeConfig::from_settings_json(Some(&config.to_settings_json())),
        config
    );
}

// --- mode selection -------------------------------------------------------

#[test]
fn a_dirty_tree_with_a_review_shaped_or_empty_prompt_reviews() {
    assert_eq!(committee_mode("", true), CommitteeMode::Review);
    assert_eq!(
        committee_mode("review my changes please", true),
        CommitteeMode::Review
    );
    assert_eq!(
        committee_mode("audit the diff", true),
        CommitteeMode::Review
    );
}

#[test]
fn a_general_question_goes_to_the_panel_even_with_a_dirty_tree() {
    assert_eq!(
        committee_mode("what is the capital of france", true),
        CommitteeMode::Panel
    );
}

#[test]
fn review_cues_match_whole_words_only() {
    // "difference" contains "diff", "preview" contains "review" — neither is
    // a review ask.
    assert!(!prompt_is_review_shaped(
        "what's the difference between zsh and fish"
    ));
    assert!(!prompt_is_review_shaped("add a preview feature to the app"));
    // The actual words still trigger.
    assert!(prompt_is_review_shaped("review the diff"));
    assert!(prompt_is_review_shaped("look over what I did"));
}

#[test]
fn a_review_request_on_a_clean_tree_has_nothing_to_review() {
    assert_eq!(
        committee_mode("review my changes", false),
        CommitteeMode::NothingToReview
    );
    assert_eq!(committee_mode("", false), CommitteeMode::NothingToReview);
}

#[test]
fn a_general_question_on_a_clean_tree_goes_to_the_panel() {
    assert_eq!(
        committee_mode("explain rust lifetimes", false),
        CommitteeMode::Panel
    );
}

// --- findings parsing -----------------------------------------------------

#[test]
fn parses_a_clean_findings_array() {
    let content = r#"[{"file":"src/main.rs","line":10,"severity":"high","title":"Off-by-one in loop","detail":"skips the last element"}]"#;
    let findings = parse_findings(content).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].file, "src/main.rs");
    assert_eq!(findings[0].line, Some(10));
}

#[test]
fn parses_findings_wrapped_in_a_code_fence_and_prose() {
    let content =
        "Here are my findings:\n```json\n[{\"file\":\"a.rs\",\"title\":\"Bug\"}]\n```\nThat's all.";
    let findings = parse_findings(content).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].title, "Bug");
}

#[test]
fn parses_the_object_wrapper_shape() {
    let content = r#"{"findings":[{"file":"a.rs","title":"Bug"}]}"#;
    let findings = parse_findings(content).unwrap();
    assert_eq!(findings.len(), 1);
}

#[test]
fn an_empty_array_is_a_valid_no_findings_answer() {
    assert_eq!(parse_findings("[]").unwrap(), Vec::<Finding>::new());
}

#[test]
fn prose_without_json_yields_no_findings() {
    assert_eq!(parse_findings("The diff looks fine to me."), None);
}

#[test]
fn untitled_findings_are_dropped() {
    let content = r#"[{"file":"a.rs","title":""},{"file":"b.rs","title":"Real"}]"#;
    let findings = parse_findings(content).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].title, "Real");
}

// --- reviewer sanitation --------------------------------------------------

const BOUNDED_DIFF: &str = "\
diff --git a/payday.py b/payday.py
--- a/payday.py
+++ b/payday.py
@@ -1,2 +1,9 @@
 def split_pot(total_cents, players):
";

#[test]
fn line_bounds_come_from_the_hunk_headers() {
    let bounds = diff_line_bounds(BOUNDED_DIFF);
    assert_eq!(bounds.get("payday.py"), Some(&10));
}

#[test]
fn a_repetition_loop_collapses_to_one_finding() {
    // The real failure: one model reported the same defect 100 times, on
    // lines 1..100 of a nine-line file.
    let runaway: Vec<Finding> = (1..=100)
        .map(|line| {
            finding(
                "payday.py",
                Some(line),
                "high",
                "Incorrect calculation of share amounts",
            )
        })
        .collect();
    let bounds = diff_line_bounds(BOUNDED_DIFF);
    let (kept, note) = sanitize_findings(runaway, &bounds);

    assert_eq!(kept.len(), 1, "a repeated title is one finding");
    // The surviving citation is the one the diff can actually support.
    assert_eq!(kept[0].line, Some(1));
    // The message has to name the misbehaviour and quote what it looped
    // on, not just report a count.
    let note = note.expect("the transcript must say what the model did");
    assert!(note.contains("went derp"), "note: {note}");
    assert!(
        note.contains("got stuck repeating \"Incorrect calculation of share amounts\" 100 times"),
        "note: {note}"
    );
    assert!(note.contains("never reaches"), "note: {note}");
    assert!(note.contains("Kept 1 of 100"), "note: {note}");
}

#[test]
fn distinct_findings_survive_but_the_cap_holds() {
    let bounds = diff_line_bounds(BOUNDED_DIFF);
    let varied: Vec<Finding> = (1..=20)
        .map(|n| {
            finding(
                "payday.py",
                Some(1),
                "high",
                &format!("Distinct defect {n}"),
            )
        })
        .collect();
    let (kept, note) = sanitize_findings(varied, &bounds);
    assert_eq!(kept.len(), MAX_FINDINGS_PER_REVIEWER);
    let note = note.unwrap();
    assert!(note.contains("went derp"), "note: {note}");
    assert!(note.contains("more than 12 findings"), "note: {note}");

    // An honest small report passes through untouched.
    let honest = vec![
        finding("payday.py", Some(2), "high", "Float division of cents"),
        finding(
            "payday.py",
            Some(8),
            "high",
            "top_scores returns the lowest scores",
        ),
    ];
    let (kept, note) = sanitize_findings(honest, &bounds);
    assert_eq!(kept.len(), 2);
    assert_eq!(note, None);
}

#[test]
fn out_of_range_lines_are_cleared_without_losing_the_finding() {
    let bounds = diff_line_bounds(BOUNDED_DIFF);
    let (kept, note) = sanitize_findings(
        vec![finding(
            "payday.py",
            Some(87),
            "high",
            "Real defect, invented line",
        )],
        &bounds,
    );
    assert_eq!(kept.len(), 1, "the finding survives");
    assert_eq!(kept[0].line, None, "the false citation does not");
    // One bad line number is sloppiness, not derping — no accusation.
    let note = note.unwrap();
    assert!(!note.contains("went derp"), "note: {note}");
    assert!(
        note.contains("cited 1 line the diff never reaches"),
        "note: {note}"
    );
}

// --- consensus ------------------------------------------------------------

fn finding(file: &str, line: Option<u64>, severity: &str, title: &str) -> Finding {
    Finding {
        file: file.to_string(),
        line,
        severity: Some(severity.to_string()),
        title: title.to_string(),
        detail: None,
    }
}

fn report(model: &str, findings: Vec<Finding>) -> ReviewerReport {
    ReviewerReport {
        model: model.to_string(),
        findings,
        commentary: None,
    }
}

#[test]
fn similar_titles_in_the_same_file_merge_with_an_agreement_count() {
    let reports = vec![
        report(
            "qwen",
            vec![finding(
                "src/a.rs",
                Some(10),
                "medium",
                "Null pointer dereference in parse",
            )],
        ),
        report(
            "deepseek",
            vec![finding(
                "src/a.rs",
                Some(40),
                "high",
                "parse has a null pointer dereference",
            )],
        ),
    ];
    let merged = merge_findings(&reports);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].raised_by, vec!["qwen", "deepseek"]);
    // The worse severity wins the merge.
    assert_eq!(merged[0].severity, "high");
}

#[test]
fn nearby_lines_with_unrelated_wording_stay_distinct() {
    // Proximity alone must not merge — two different defects can share a
    // line, and in a small file everything is near everything.
    let reports = vec![
        report(
            "qwen",
            vec![finding(
                "src/a.rs",
                Some(10),
                "low",
                "Lock held across await",
            )],
        ),
        report(
            "deepseek",
            vec![finding("src/a.rs", Some(12), "low", "Deadlock risk")],
        ),
    ];
    assert_eq!(merge_findings(&reports).len(), 2);
}

#[test]
fn adjacent_lines_with_shared_wording_still_merge() {
    let reports = vec![
        report(
            "qwen",
            vec![finding(
                "src/a.rs",
                Some(10),
                "low",
                "Deadlock from lock held across await",
            )],
        ),
        report(
            "deepseek",
            vec![finding(
                "src/a.rs",
                Some(11),
                "low",
                "Holding the mutex lock across an await point risks deadlock",
            )],
        ),
    ];
    assert_eq!(merge_findings(&reports).len(), 1);
}

#[test]
fn the_committee_demo_findings_do_not_collapse_into_one() {
    // Regression: the exact findings from a real run over the planted-bug
    // demo repo. Six findings about different defects chain-merged into ONE
    // under the old proximity rule ("agreed ×3" was manufactured). Only
    // deepseek's two identically-titled findings should merge.
    let reports = vec![
        report(
            "qwen2.5-coder:32b",
            vec![finding(
                "payday.py",
                Some(3),
                "high",
                "Incorrect calculation of share",
            )],
        ),
        report(
            "codellama:13b",
            vec![
                finding(
                    "payday.py",
                    Some(1),
                    "high",
                    "Integer division used for splitting pot",
                ),
                finding(
                    "payday.py",
                    Some(4),
                    "medium",
                    "Unnecessary dictionary comprehension in split_pot",
                ),
                finding(
                    "payday.py",
                    Some(6),
                    "low",
                    "Unused variable ordered in top_scores",
                ),
            ],
        ),
        report(
            "deepseek-coder:6.7b",
            vec![
                finding(
                    "payday.py",
                    Some(3),
                    "high",
                    "Possible division by zero error",
                ),
                finding(
                    "payday.py",
                    Some(5),
                    "high",
                    "Possible division by zero error",
                ),
            ],
        ),
    ];
    let merged = merge_findings(&reports);
    assert_eq!(merged.len(), 5, "got: {merged:#?}");
    // The one legitimate merge: deepseek's twin division-by-zero titles.
    let div_zero = merged
        .iter()
        .find(|finding| finding.title.contains("division by zero"))
        .unwrap();
    assert_eq!(div_zero.raised_by, vec!["deepseek-coder:6.7b"]);
    // No manufactured agreement anywhere.
    assert!(merged.iter().all(|finding| finding.raised_by.len() == 1));
}

#[test]
fn different_files_never_merge() {
    let reports = vec![
        report(
            "qwen",
            vec![finding("src/a.rs", Some(10), "low", "Deadlock risk")],
        ),
        report(
            "deepseek",
            vec![finding("src/b.rs", Some(10), "low", "Deadlock risk")],
        ),
    ];
    assert_eq!(merge_findings(&reports).len(), 2);
}

#[test]
fn agreed_findings_sort_ahead_of_singletons() {
    let reports = vec![
        report(
            "qwen",
            vec![
                finding("src/z.rs", Some(1), "high", "Singleton finding"),
                finding("src/a.rs", Some(10), "low", "Agreed finding"),
            ],
        ),
        report(
            "deepseek",
            vec![finding("src/a.rs", Some(11), "low", "Agreed finding")],
        ),
    ];
    let merged = merge_findings(&reports);
    assert_eq!(merged[0].title, "Agreed finding");
    assert_eq!(merged[0].raised_by.len(), 2);
}

#[test]
fn location_verification_checks_the_tree_and_the_diff() {
    let dir = std::env::temp_dir().join(format!("committee-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/real.rs"), "fn main() {}").unwrap();

    let mut merged = vec![
        MergedFinding {
            file: "src/real.rs".to_string(),
            line: None,
            severity: "low".to_string(),
            title: "In the tree".to_string(),
            detail: String::new(),
            raised_by: vec!["qwen".to_string()],
            location_verified: false,
            proof: None,
            proof_script: None,
        },
        MergedFinding {
            file: "src/diff_only.rs".to_string(),
            line: None,
            severity: "low".to_string(),
            title: "Only in the diff".to_string(),
            detail: String::new(),
            raised_by: vec!["qwen".to_string()],
            location_verified: false,
            proof: None,
            proof_script: None,
        },
        MergedFinding {
            file: "src/invented.rs".to_string(),
            line: None,
            severity: "low".to_string(),
            title: "Hallucinated".to_string(),
            detail: String::new(),
            raised_by: vec!["qwen".to_string()],
            location_verified: false,
            proof: None,
            proof_script: None,
        },
    ];
    verify_locations(
        &mut merged,
        &dir,
        "--- a/src/diff_only.rs\n+++ b/src/diff_only.rs",
    );
    assert!(merged[0].location_verified);
    assert!(merged[1].location_verified);
    assert!(!merged[2].location_verified);

    let _ = std::fs::remove_dir_all(&dir);
}

// --- judge parsing --------------------------------------------------------

#[test]
fn parses_a_judge_verdict_with_rulings() {
    let content = r#"{"rulings":[{"index":1,"verdict":"confirm","reason":"the diff shows it"}],"summary":"One real bug.","best_reviewer":"qwen2.5-coder:7b"}"#;
    let verdict: JudgeVerdict = parse_json_object(content).unwrap();
    assert_eq!(verdict.rulings.len(), 1);
    assert_eq!(verdict.rulings[0].index, Some(1));
    assert_eq!(verdict.rulings[0].verdict, "confirm");
    assert_eq!(verdict.best_reviewer.as_deref(), Some("qwen2.5-coder:7b"));
}

#[test]
fn parses_a_panel_verdict_out_of_surrounding_prose() {
    let content = "Here is my ruling:\n{\"winner\":\"llama3.1:8b\",\"reason\":\"most complete\",\"synthesis\":\"combined answer\"}\nDone.";
    let verdict: PanelVerdict = parse_json_object(content).unwrap();
    assert_eq!(verdict.winner.as_deref(), Some("llama3.1:8b"));
    assert_eq!(verdict.synthesis, "combined answer");
}

#[test]
fn judge_prose_without_json_yields_none() {
    assert!(parse_json_object::<JudgeVerdict>("I confirm finding one.").is_none());
}

#[test]
fn parses_the_judges_exemplar_rewrite() {
    let content = r#"Here you go:
{"based_on":"qwen2.5-coder:14b","exemplar":"import os\nprint('better')","changes":"tracked per-column state"}"#;
    let exemplar: ExemplarRound = parse_json_object(content).unwrap();
    assert_eq!(exemplar.based_on.as_deref(), Some("qwen2.5-coder:14b"));
    assert!(exemplar.exemplar.contains("better"));
    assert_eq!(exemplar.changes, "tracked per-column state");
}

#[test]
fn parses_coach_notes_with_a_shared_direction() {
    let content = r#"{"notes":[{"model":"qwen2.5-coder:14b","feedback":"right: color / wrong: overlap"},{"model":"codellama:13b","feedback":"right: structure / wrong: no green"}],"direction":"track per-column state"}"#;
    let notes: CoachNotes = parse_json_object(content).unwrap();
    assert_eq!(notes.notes.len(), 2);
    assert_eq!(notes.notes[0].model, "qwen2.5-coder:14b");
    assert_eq!(notes.direction, "track per-column state");
}

// --- leaderboard ----------------------------------------------------------

#[test]
fn standings_rank_by_confirmed_findings_and_panel_wins() {
    let mut leaderboard = CommitteeLeaderboard::default();
    leaderboard.insert(
        "quiet:7b".to_string(),
        ModelStats {
            runs: 10,
            findings_raised: 2,
            findings_confirmed: 1,
            panel_wins: 0,
        },
    );
    leaderboard.insert(
        "sharp:7b".to_string(),
        ModelStats {
            runs: 4,
            findings_raised: 8,
            findings_confirmed: 6,
            panel_wins: 1,
        },
    );
    let lines = leaderboard_lines(&leaderboard);
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("sharp:7b"), "got: {}", lines[0]);
}

#[test]
fn confirm_rate_is_neutral_until_a_model_has_enough_runs() {
    let thin = ModelStats {
        runs: LEADERBOARD_MIN_RUNS - 1,
        findings_raised: 4,
        findings_confirmed: 4,
        panel_wins: 0,
    };
    assert_eq!(confirm_rate(Some(&thin)), 0.5);
    assert_eq!(confirm_rate(None), 0.5);

    let proven = ModelStats {
        runs: LEADERBOARD_MIN_RUNS,
        findings_raised: 4,
        findings_confirmed: 3,
        panel_wins: 0,
    };
    assert_eq!(confirm_rate(Some(&proven)), 0.75);
}

// --- winner resolution ----------------------------------------------------

#[test]
fn winner_resolution_canonicalises_abbreviated_judge_names() {
    let answers = vec![
        ("qwen2.5-coder:14b".to_string(), "a".to_string()),
        ("codellama:13b".to_string(), "b".to_string()),
    ];
    // Exact tag.
    assert_eq!(
        resolve_winner(Some("codellama:13b"), &answers).as_deref(),
        Some("codellama:13b")
    );
    // The judge's shorthand still credits the full tag.
    assert_eq!(
        resolve_winner(Some("qwen2.5-coder"), &answers).as_deref(),
        Some("qwen2.5-coder:14b")
    );
    // Judges decorate names with backticks or quotes; the tag still resolves.
    assert_eq!(
        resolve_winner(Some("`codellama:13b`"), &answers).as_deref(),
        Some("codellama:13b")
    );
    // Names that match no panelist, and non-answers, resolve to none.
    assert_eq!(resolve_winner(Some("gpt-4"), &answers), None);
    assert_eq!(resolve_winner(Some("null"), &answers), None);
    assert_eq!(resolve_winner(Some("  "), &answers), None);
    assert_eq!(resolve_winner(None, &answers), None);
}

#[test]
fn coach_feedback_matches_tags_but_never_blank_ones() {
    let notes = CoachNotes {
        notes: vec![
            CoachNote {
                model: String::new(),
                feedback: "anonymous note".to_string(),
            },
            CoachNote {
                model: "qwen2.5-coder".to_string(),
                feedback: "sharpen the loop".to_string(),
            },
        ],
        direction: String::new(),
    };
    // The blank-tagged note must not be handed to every panelist.
    assert_eq!(
        feedback_for(&notes, "qwen2.5-coder:14b"),
        Some("sharpen the loop")
    );
    assert_eq!(feedback_for(&notes, "codellama:13b"), None);
}

// --- server enumeration ---------------------------------------------------

#[test]
fn parses_model_names_from_an_api_tags_body() {
    let body = br#"{"models":[{"name":"qwen2.5-coder:14b","size":1},{"name":"llama3.1:8b"}]}"#;
    assert_eq!(
        parse_installed_model_names(body),
        Some(vec![
            "qwen2.5-coder:14b".to_string(),
            "llama3.1:8b".to_string()
        ])
    );
}

#[test]
fn an_empty_or_malformed_tags_body_falls_back_to_none() {
    assert_eq!(parse_installed_model_names(br#"{"models":[]}"#), None);
    assert_eq!(parse_installed_model_names(b"not json"), None);
    assert_eq!(parse_installed_model_names(br#"{"other":true}"#), None);
}

#[test]
fn tags_sharing_a_digest_keep_only_their_first_name() {
    // dolphin-mistral:latest and :7b are the same weights — one seat only.
    let body = br#"{"models":[
        {"name":"dolphin-mistral:latest","digest":"abc"},
        {"name":"dolphin-mistral:7b","digest":"abc"},
        {"name":"llama3.1:8b","digest":"def"},
        {"name":"no-digest-model"}
    ]}"#;
    assert_eq!(
        parse_installed_model_names(body),
        Some(vec![
            "dolphin-mistral:latest".to_string(),
            "llama3.1:8b".to_string(),
            "no-digest-model".to_string(),
        ])
    );
}

#[test]
fn diff_truncation_backs_off_to_a_char_boundary() {
    // A multibyte char straddling the cap must not panic String::truncate.
    let mut text = format!("{}é and more", "a".repeat(9));
    // "é" occupies bytes 9..11; cutting at 10 lands mid-char.
    truncate_bytes_on_char_boundary(&mut text, 10);
    assert_eq!(text, "a".repeat(9));

    let mut short = String::from("tiny");
    truncate_bytes_on_char_boundary(&mut short, 100);
    assert_eq!(short, "tiny");
}

// --- round commands -------------------------------------------------------

#[test]
fn round_commands_parse_and_ordinary_prompts_do_not() {
    assert_eq!(
        parse_round_command("round notes"),
        Some(PanelRoundKind::Notes)
    );
    assert_eq!(
        parse_round_command("/round rewrite"),
        Some(PanelRoundKind::Exemplar)
    );
    assert_eq!(
        parse_round_command("Another round of peers"),
        Some(PanelRoundKind::Peers)
    );
    assert_eq!(
        parse_round_command("  round converge  "),
        Some(PanelRoundKind::Converge)
    );
    // Ordinary prompts must never be eaten.
    assert_eq!(parse_round_command("round up the usual suspects"), None);
    assert_eq!(parse_round_command("what is a round robin"), None);
    assert_eq!(parse_round_command("review my changes"), None);
    assert_eq!(parse_round_command(""), None);
}

#[test]
fn inline_rounds_directives_parse_and_ordinary_prompts_pass_through() {
    // The canonical form: kinds, a dash, the question.
    let (kinds, question) =
        parse_inline_rounds("rounds: notes rewrite — write me a matrix rain script").unwrap();
    assert_eq!(kinds, vec![PanelRoundKind::Notes, PanelRoundKind::Exemplar]);
    assert_eq!(question, "write me a matrix rain script");

    // Commas, slash prefix, singular keyword, case.
    let (kinds, question) =
        parse_inline_rounds("/Round: peers, converge explain rust lifetimes").unwrap();
    assert_eq!(kinds, vec![PanelRoundKind::Peers, PanelRoundKind::Converge]);
    assert_eq!(question, "explain rust lifetimes");

    // A directive with no question still parses — the runner explains usage.
    let (kinds, question) = parse_inline_rounds("rounds: notes").unwrap();
    assert_eq!(kinds, vec![PanelRoundKind::Notes]);
    assert_eq!(question, "");

    // The plan is capped even when the user asks for a marathon.
    let (kinds, _) = parse_inline_rounds("rounds: notes notes notes notes notes go").unwrap();
    assert_eq!(kinds.len(), MAX_INLINE_ROUNDS);

    // Not directives: no colon, no known kind, or plain questions.
    assert_eq!(parse_inline_rounds("round notes"), None);
    assert_eq!(parse_inline_rounds("rounds: tomorrow we ride"), None);
    assert_eq!(
        parse_inline_rounds("how many rounds does boxing have"),
        None
    );
}

#[test]
fn save_commands_parse_and_ordinary_prompts_pass_through() {
    // Bare save: infer the name from the question later.
    assert_eq!(parse_save_command("save"), Some(None));
    assert_eq!(parse_save_command("/save"), Some(None));
    // Named, with conversational padding tolerated.
    assert_eq!(
        parse_save_command("save rain.py"),
        Some(Some("rain.py".to_string()))
    );
    assert_eq!(
        parse_save_command("save it as `rain.py`"),
        Some(Some("rain.py".to_string()))
    );
    // Not saves: no filename among the words, or a different verb entirely.
    assert_eq!(parse_save_command("save me some time"), None);
    assert_eq!(parse_save_command("saved by the bell"), None);
    assert_eq!(parse_save_command("write me a script"), None);
}

#[test]
fn filenames_are_plain_and_relative_or_rejected() {
    assert!(looks_like_filename("rain.py"));
    assert!(looks_like_filename("scripts/rain.py"));
    assert!(!looks_like_filename("/etc/passwd"));
    assert!(!looks_like_filename("../up.py"));
    assert!(!looks_like_filename("noext"));
    assert!(!looks_like_filename(".hidden"));
}

#[test]
fn artifact_filenames_infer_from_the_question() {
    assert_eq!(
        infer_artifact_filename("write a script called rain.py that draws rain"),
        Some("rain.py".to_string())
    );
    assert_eq!(infer_artifact_filename("explain rust lifetimes"), None);
}

#[test]
fn code_blocks_extract_from_final_versions() {
    let text = "Here you go:\n```python\nprint('rain')\n```\nEnjoy!";
    assert_eq!(extract_code_block(text), Some("print('rain')".to_string()));
    assert_eq!(extract_code_block("no fences here"), None);
}

#[test]
fn run_hints_follow_the_extension() {
    assert_eq!(run_hint("rain.py"), Some("python3 rain.py".to_string()));
    assert_eq!(run_hint("go.sh"), Some("bash go.sh".to_string()));
    assert_eq!(run_hint("notes.txt"), None);
}

#[test]
fn recheck_commands_parse_with_and_without_a_note() {
    assert_eq!(parse_recheck_command("recheck 2"), Some((2, None)));
    assert_eq!(parse_recheck_command("/redo 1"), Some((1, None)));
    assert_eq!(parse_recheck_command("recheck finding 3"), Some((3, None)));
    assert_eq!(
        parse_recheck_command("recheck 2 the script imported a package that doesn't exist"),
        Some((
            2,
            Some("the script imported a package that doesn't exist".to_string())
        ))
    );
    assert_eq!(
        parse_recheck_command("reprove #4: bad import"),
        Some((4, Some("bad import".to_string())))
    );
    // Not rechecks.
    assert_eq!(parse_recheck_command("recheck"), None);
    assert_eq!(parse_recheck_command("recheck everything"), None);
    assert_eq!(parse_recheck_command("recheck 0"), None);
    assert_eq!(parse_recheck_command("review my changes"), None);
}

// --- misc -----------------------------------------------------------------

#[test]
fn truncation_respects_char_boundaries_and_short_input() {
    assert_eq!(truncate_chars("short", 10), "short");
    assert_eq!(truncate_chars("héllo wörld", 5), "héllo…");
}

// --- live smoke test ------------------------------------------------------

/// Full pipeline against the real local Ollama service: builds a throwaway
/// git repo with a planted bug, runs a review-mode committee over its diff,
/// and prints the whole event transcript. Loads real models sequentially, so
/// it takes minutes — run it deliberately:
///
/// ```sh
/// cargo test -p warp committee_reviews_a_planted_bug -- --ignored --nocapture
/// ```
#[test]
#[ignore = "hits the live local Ollama service and loads real models"]
fn committee_reviews_a_planted_bug_against_live_ollama() {
    use futures_lite::future::block_on;

    let dir = std::env::temp_dir().join(format!("committee-live-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?} failed: {output:?}");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "committee@test"]);
    git(&["config", "user.name", "committee"]);
    std::fs::write(
        dir.join("split.py"),
        "def split_pot(total_cents, players):\n    return {p: total_cents // len(players) for p in players}\n",
    )
    .unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "initial"]);
    // Three planted bugs: cents divided with `/` (floats money), all
    // leftover cents to the first player plus a crash on an empty list, and
    // top_scores returning the LOWEST scores (ascending sort, head taken).
    std::fs::write(
        dir.join("split.py"),
        "def split_pot(total_cents, players):\n    share = total_cents / len(players)\n    shares = {p: share for p in players}\n    shares[players[0]] += total_cents - share * len(players)\n    return shares\n\ndef top_scores(scores, n):\n    ordered = sorted(scores)\n    return ordered[:n]\n",
    )
    .unwrap();

    let path = std::env::var("PATH").unwrap_or_default();
    let session_id = uuid::Uuid::new_v4();
    let mut events: Vec<TurnEvent> = Vec::new();
    let status = block_on(run_ollama_committee(
        "review my changes",
        &session_id,
        &dir,
        &format!("/opt/homebrew/bin:/usr/local/bin:{path}"),
        OllamaCommitteeConfig::default(),
        |event| events.push(event),
    ))
    .unwrap();

    for event in &events {
        match event {
            TurnEvent::Text { text } => println!("--- TEXT ---\n{text}\n"),
            TurnEvent::Thinking { text } => {
                println!("--- THINKING ({} chars) ---", text.len())
            }
            other => println!("--- EVENT: {other:?}"),
        }
    }

    assert!(status.success());
    assert!(matches!(
        events.first(),
        Some(TurnEvent::SessionInit { .. })
    ));
    let completed = events.iter().any(|event| {
        matches!(
            event,
            TurnEvent::TurnCompleted {
                is_error: false,
                ..
            }
        )
    });
    assert!(completed, "the committee run did not complete cleanly");
    // The verdict block is the last Text event of a review run.
    let verdict = events
        .iter()
        .rev()
        .find_map(|event| match event {
            TurnEvent::Text { text } => Some(text.clone()),
            _ => None,
        })
        .expect("no Text events at all");
    assert!(
        verdict.contains("Committee verdict") || verdict.contains("found nothing wrong"),
        "unexpected final block: {verdict}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
