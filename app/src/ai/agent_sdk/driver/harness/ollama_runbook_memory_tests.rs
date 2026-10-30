use super::*;

const GB: u64 = 1_073_741_824;

fn inventory(total_gb: u64, installed: &[(&str, u64)], loaded: &[(&str, u64)]) -> Inventory {
    Inventory {
        budget_bytes: total_gb * GB * 3 / 4,
        total_bytes: total_gb * GB,
        installed: installed
            .iter()
            .map(|(name, gb)| (name.to_string(), gb * GB))
            .collect(),
        loaded: loaded
            .iter()
            .map(|(name, gb)| LoadedModel {
                name: name.to_string(),
                bytes: gb * GB,
            })
            .collect(),
    }
}

#[test]
fn with_no_judge_or_helper_chosen_the_worker_does_every_job() {
    let machine = inventory(24, &[("w:14b", 15), ("j:8b", 5)], &[]);
    let picks = choose(&machine, "w:14b", None, None).unwrap();
    assert_eq!(picks.judge, "w:14b");
    assert_eq!(picks.helper, "w:14b");
    assert!(picks.warnings.is_empty());
}

#[test]
fn a_chosen_set_that_fits_together_is_used_as_chosen() {
    let machine = inventory(32, &[("w:14b", 7), ("j:14b", 9), ("h:7b", 4)], &[]);
    let picks = choose(&machine, "w:14b", Some("j:14b"), Some("h:7b")).unwrap();
    assert_eq!(picks.judge, "j:14b");
    assert_eq!(picks.helper, "h:7b");
    assert!(picks.warnings.is_empty());
}

#[test]
fn chosen_models_that_fit_only_one_at_a_time_are_left_to_ollama_to_swap() {
    // 24 GB machine: 18 GB usable. 9+1 and 9+1 would be 20 together.
    let machine = inventory(24, &[("w:14b", 9), ("j:14b", 9)], &[]);
    let picks = choose(&machine, "w:14b", Some("j:14b"), None).unwrap();
    assert_eq!(picks.judge, "j:14b");
    assert!(picks.warnings.iter().any(|warning| warning.contains("swap")));
}

#[test]
fn a_big_worker_alone_goes_ahead_flagged_as_a_tight_fit() {
    // 24 GB machine, 18 GB usable: 20 + 1 GB is over that but within the machine.
    let machine = inventory(24, &[("glm:flash", 20)], &[]);
    let picks = choose(&machine, "glm:flash", None, None).unwrap();
    assert_eq!(picks.judge, "glm:flash");
    assert!(picks.warnings.iter().any(|warning| warning.contains("slow")));
}

#[test]
fn a_worker_bigger_than_the_machine_stops_the_run_naming_the_models() {
    let machine = inventory(24, &[("glm:flash", 25), ("q:9b", 8)], &[]);
    let message = choose(&machine, "glm:flash", Some("q:9b"), None).unwrap_err();
    assert!(message.contains("`glm:flash`"));
    assert!(message.contains("`q:9b`"));
    assert!(message.contains("24 GB"));
    assert!(message.contains("18 GB"));
}

#[test]
fn models_already_loaded_elsewhere_use_up_the_budget() {
    let machine = inventory(24, &[("w:7b", 4)], &[("image:xl", 20)]);
    let message = choose(&machine, "w:7b", None, None).unwrap_err();
    assert!(message.contains("`image:xl`"));
    assert!(message.contains("ollama stop"));
}

#[test]
fn a_loaded_model_of_ours_counts_once_at_its_loaded_size() {
    let machine = inventory(24, &[("w:7b", 4)], &[("w:7b:latest", 6)]);
    assert_eq!(needed(&machine, &["w:7b", "w:7b"]), 6 * GB);
}

#[test]
fn models_on_other_machines_take_nothing_here() {
    let machine = inventory(24, &[], &[]);
    assert_eq!(cost(&machine, "big:70b@@http://192.168.1.5:11434"), 0);
}

#[test]
fn loopback_servers_are_recognised() {
    assert!(is_loopback("http://127.0.0.1:11435"));
    assert!(is_loopback("http://localhost:11434"));
    assert!(!is_loopback("http://192.168.1.150:11434"));
    assert!(!is_loopback("http://localhost.example.com:11434"));
}

#[test]
fn ollama_listings_are_read() {
    let body = json!({"models": [{"name": "a:7b", "size": 4_000_000_000u64}, {"name": "bad"}]});
    assert_eq!(parse_installed(&body).len(), 1);
    let body = json!({"models": [{"name": "a:7b", "size": 5_000_000_000u64}]});
    assert_eq!(parse_loaded(&body)[0].bytes, 5_000_000_000);
}
