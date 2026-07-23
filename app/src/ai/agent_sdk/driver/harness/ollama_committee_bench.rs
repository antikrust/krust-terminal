//! The committee's own test bench: a whole run, on real models, against a
//! fixture whose defects are known in advance, scored automatically.
//!
//! Every stage of the pipeline has unit tests. None of them can answer the
//! only question that matters — *does a bench of small local models actually
//! find the bugs?* — because that question is about model behaviour, and
//! model behaviour is exactly what a unit test stubs out. So this runs the
//! real thing and counts.
//!
//! It is `#[ignore]`d: it needs an Ollama serving it and takes minutes.
//!
//! ```text
//! COMMITTEE_HOST=http://192.168.1.150:11434 \
//! COMMITTEE_REVIEWERS=qwen2.5-coder:32b,deepseek-coder-v2:16b,qwen2.5-coder:14b \
//! COMMITTEE_JUDGE=phi4:14b \
//! COMMITTEE_FIXTURE=stock \
//! cargo test -p warp --lib -- --ignored --nocapture a_real_committee
//! ```
//!
//! `COMMITTEE_HOST` is the load-bearing one. Everything it names runs over
//! there; this machine only marshals the run, which is the difference
//! between iterating on the pipeline and not being able to use the laptop.

use ai::agent::TurnEvent;

use super::ollama_committee::*;

/// A defect that was deliberately planted, and the word that identifies a
/// finding as being about it. Loose on purpose: the score is a signal for
/// the next change, and the transcript underneath it is the evidence.
struct Planted {
    /// The function the defect is in — how a finding gets matched to it.
    function: &'static str,
    /// What is actually wrong, for the report.
    defect: &'static str,
}

/// What each fixture in `committee-tests/` has planted in it.
fn planted(fixture: &str) -> &'static [Planted] {
    match fixture {
        "stock" => &[
            Planted {
                function: "cheapest",
                defect: "`if not best` treats a price of 0 as no price yet",
            },
            Planted {
                function: "needs_reorder",
                defect: "`<` where the docstring says fallen *to* the threshold",
            },
            Planted {
                function: "restock",
                defect: "`log=[]` is one list shared by every call",
            },
        ],
        "payday" => &[
            Planted {
                function: "split_pot",
                defect: "`/` makes whole cents into floats",
            },
            Planted {
                function: "split_pot",
                defect: "an empty roster divides by zero",
            },
            Planted {
                function: "top_scores",
                defect: "sorted ascending, so the *lowest* scores come back",
            },
        ],
        "orders" => &[
            Planted {
                function: "render_line",
                defect: "pence printed as pounds — £1250.00 for £12.50",
            },
            Planted {
                function: "checkout",
                defect: "a percentage handed to something wanting a fraction",
            },
        ],
        _ => &[],
    }
}

/// Everything the local Ollama has, so it can all be benched. The bench runs
/// on the host under test and nowhere else — a run that quietly seats a
/// laptop model measures the laptop.
fn local_models() -> Vec<String> {
    let output = std::process::Command::new("curl")
        .args(["-s", "--max-time", "4", "http://127.0.0.1:11434/api/tags"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let body = String::from_utf8_lossy(&output.stdout);
    // Enough JSON reading for a name list, without a dependency.
    body.split("\"name\":\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .map(str::to_string)
        .collect()
}

/// The section of a verdict a finding landed in.
fn definite_block(transcript: &str) -> String {
    let Some(start) = transcript.find("**Definite (") else {
        return String::new();
    };
    let rest = &transcript[start..];
    let end = rest
        .find("**Might be (")
        .or_else(|| rest.find("**Rejected ("))
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

#[test]
#[ignore = "needs an Ollama and several minutes; run it by hand"]
fn a_real_committee() {
    use futures_lite::future::block_on;

    let fixture = std::env::var("COMMITTEE_FIXTURE").unwrap_or_else(|_| "stock".to_string());
    let host = std::env::var("COMMITTEE_HOST")
        .unwrap_or_else(|_| "http://192.168.1.150:11434".to_string());
    let reviewers: Vec<String> = std::env::var("COMMITTEE_REVIEWERS")
        .unwrap_or_else(|_| "qwen2.5-coder:32b,deepseek-coder-v2:16b,qwen2.5-coder:14b".to_string())
        .split(',')
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();
    let judge = std::env::var("COMMITTEE_JUDGE").unwrap_or_else(|_| "phi4:14b".to_string());

    // A fresh copy of the fixture, built by the same script the user runs.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the repository root")
        .to_path_buf();
    let target = std::env::temp_dir().join(format!("committee-bench-{}", uuid::Uuid::new_v4()));
    let setup = std::process::Command::new("sh")
        .arg(root.join("committee-tests/setup.sh"))
        .arg(&target)
        .output()
        .expect("setup.sh runs");
    assert!(
        setup.status.success(),
        "setup.sh failed: {}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let working_dir = target.join(&fixture);
    assert!(working_dir.is_dir(), "no such fixture: {fixture}");

    // Cast the whole bench by hand and bench everything on this machine, so
    // the run is exactly the models named and nothing else.
    let mut roles = std::collections::BTreeMap::new();
    for tag in local_models() {
        roles.insert(tag, CommitteeRole::Benched);
    }
    let remote_models: Vec<String> = reviewers
        .iter()
        .chain(std::iter::once(&judge))
        .map(|tag| crate::ai::ollama::remote::qualify(tag, &host))
        .collect();
    for (index, model) in remote_models.iter().enumerate() {
        roles.insert(
            model.clone(),
            if index < reviewers.len() {
                CommitteeRole::Reviewer
            } else {
                CommitteeRole::Judge
            },
        );
    }

    let config = OllamaCommitteeConfig {
        reviewers: reviewers.len(),
        roles,
        ..OllamaCommitteeConfig::default()
    };

    let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let sink = std::sync::Arc::clone(&transcript);
    let started = std::time::Instant::now();
    let status = block_on(run_ollama_committee(
        "review my changes",
        &uuid::Uuid::new_v4(),
        &working_dir,
        &std::env::var("PATH").unwrap_or_default(),
        config,
        remote_models,
        move |event| {
            if let TurnEvent::Text { text } = &event {
                let mut sink = sink.lock().expect("the sink");
                sink.push_str(text);
                sink.push_str("\n\n");
                println!("{text}");
            }
        },
    ))
    .expect("the committee runs");
    let elapsed = started.elapsed();
    let transcript = transcript.lock().expect("the transcript").clone();

    // Score it. The Definite section is the only claim the run makes, so it
    // is the only one worth grading.
    let definite = definite_block(&transcript);
    let expected = planted(&fixture);
    let mut found = 0;
    println!("\n\n=== {fixture} — {:?}, exit {status:?} ===", elapsed);
    for defect in expected {
        let hit = definite.contains(defect.function);
        found += usize::from(hit);
        println!(
            "{}  {} — {}",
            if hit { "FOUND " } else { "MISSED" },
            defect.function,
            defect.defect
        );
    }
    // Anything in Definite that isn't about a planted function is either an
    // invention or a duplicate, and both are worth seeing.
    let claims = definite.lines().filter(|line| line.contains("**[")).count();
    println!(
        "\n{found}/{} planted defects in Definite, from {claims} claims there",
        expected.len()
    );

    if let Ok(out) = std::env::var("COMMITTEE_OUT") {
        std::fs::write(&out, &transcript).expect("the transcript is written");
        println!("transcript: {out}");
    }
    let _ = std::fs::remove_dir_all(&target);
}
