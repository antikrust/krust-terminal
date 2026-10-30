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

fn tags(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[test]
fn a_set_that_fits_keeps_the_preferred_judge_and_helper() {
    let machine = inventory(32, &[("w:14b", 7), ("j:14b", 9), ("h:7b", 4)], &[]);
    let picks = choose(
        &machine,
        "w:14b",
        None,
        None,
        &tags(&["j:14b"]),
        &tags(&["h:7b"]),
    )
    .unwrap();
    assert_eq!(picks.judge, "j:14b");
    assert_eq!(picks.helper, "h:7b");
}

#[test]
fn a_judge_that_does_not_fit_gives_way_to_a_smaller_one() {
    // 24 GB machine: 18 GB usable. 9+1 worker, 9+1 judge would be 20.
    let machine = inventory(24, &[("w:14b", 9), ("j:14b", 9), ("j:8b", 5), ("h:7b", 4)], &[]);
    let picks = choose(
        &machine,
        "w:14b",
        None,
        None,
        &tags(&["j:14b", "j:8b"]),
        &tags(&["h:7b"]),
    )
    .unwrap();
    assert_eq!(picks.judge, "j:8b");
}

#[test]
fn the_worker_is_the_last_judge_because_it_takes_no_more_memory() {
    let machine = inventory(24, &[("w:14b", 15), ("j:8b", 5)], &[]);
    let picks = choose(&machine, "w:14b", None, None, &tags(&["j:8b"]), &[]).unwrap();
    assert_eq!(picks.judge, "w:14b");
    assert_eq!(picks.helper, "w:14b");
}

#[test]
fn a_worker_too_big_alone_stops_the_run_naming_the_models() {
    let machine = inventory(24, &[("glm:flash", 20), ("q:9b", 8)], &[]);
    let message = choose(
        &machine,
        "glm:flash",
        None,
        None,
        &tags(&["q:9b"]),
        &tags(&["q:9b"]),
    )
    .unwrap_err();
    assert!(message.contains("`glm:flash`"));
    assert!(message.contains("`q:9b`"));
    assert!(message.contains("24 GB"));
    assert!(message.contains("18 GB"));
}

#[test]
fn a_chosen_judge_is_never_swapped_for_another() {
    let machine = inventory(24, &[("w:14b", 9), ("j:14b", 9), ("j:8b", 5)], &[]);
    let message = choose(
        &machine,
        "w:14b",
        Some("j:14b"),
        None,
        &tags(&["j:8b"]),
        &[],
    )
    .unwrap_err();
    assert!(message.contains("used as chosen"));
}

#[test]
fn models_already_loaded_elsewhere_use_up_the_budget() {
    let machine = inventory(24, &[("w:7b", 4), ("j:8b", 5)], &[("image:xl", 14)]);
    let message = choose(&machine, "w:7b", None, None, &tags(&["j:8b"]), &[]).unwrap_err();
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
