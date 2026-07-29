//! The committee's own test bench: a whole run, on real models, against a
//! fixture whose right answer is known, scored automatically.
//!
//! Every stage of the pipeline has unit tests. None of them can answer the
//! only question that matters — *does a bench of small local models actually
//! find the bugs?* — because that question is about model behaviour, and
//! model behaviour is exactly what a unit test stubs out. Two runs of the
//! stock fixture went by before anyone noticed the fixture was telling the
//! models the answer.
//!
//! It is `#[ignore]`d: it needs an Ollama serving it and takes minutes.
//!
//! ```text
//! COMMITTEE_HOST=http://192.168.1.150:11434 \
//! COMMITTEE_REVIEWERS=qwen2.5-coder:7b,deepseek-coder-v2:16b,qwen2.5-coder:14b \
//! COMMITTEE_JUDGE=phi4:14b \
//! COMMITTEE_FIXTURE=stock \
//! COMMITTEE_ROUNDS=notes \
//! cargo test -p bork --lib -- --ignored --nocapture a_real_committee
//! ```
//!
//! `COMMITTEE_HOST` is the load-bearing one. Everything it names runs over
//! there and every model on this machine is benched, so a run can't quietly
//! seat a laptop model and measure the laptop.
//!
//! Every run ends with one `BENCH ...` line holding its numbers, so a
//! session's worth of them can be read back with `grep BENCH`.

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

/// What the bench asks of a fixture, and how it marks the answer.
enum Task {
    /// Review the working tree's diff. Marked against what was planted.
    Review(&'static [Planted]),
    /// Ask for a program, save the winner, run something over it. Marked by
    /// that command's exit code, which is why every `Make` fixture ships a
    /// grader the committee never sees.
    Make {
        /// `None` reads the fixture's `PROMPT.txt` — the panel can't open
        /// files, so a file it must work on has to travel in the prompt.
        ask: Option<&'static str>,
        save_as: &'static str,
        grade: &'static [&'static str],
    },
}

fn task(fixture: &str) -> Task {
    match fixture {
        "stock" => Task::Review(&[
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
        ]),
        "payday" => Task::Review(&[
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
        ]),
        // Nothing is planted. Every other fixture measures whether the
        // committee finds defects; this measures whether it invents them,
        // which is the property that decides whether it can be left running.
        // The diff is a real refactor — a helper extracted, `partition` for
        // `split`, a dict literal for copy-and-update — proved equivalent
        // over four thousand random inputs.
        "tidy" => Task::Review(&[]),
        "orders" => Task::Review(&[
            Planted {
                function: "render_line",
                defect: "pence printed as pounds — £1250.00 for £12.50",
            },
            Planted {
                function: "checkout",
                defect: "a percentage handed to something wanting a fraction",
            },
        ]),
        // Graded by a file the committee never sees: three of twelve cases
        // fail before it starts and none should after, however it gets there.
        "durations" => Task::Make {
            ask: None,
            save_as: "durations.py",
            grade: &["python3", "test_durations.py"],
        },
        // Graded by a file the committee never sees, like the edit test.
        // This asked for a terminal animation once, marked by whether the
        // result parsed, and passed a script that referenced a local from
        // another function and called two curses methods that don't exist.
        // A program that has to be watched cannot be marked.
        "arcade" => Task::Make {
            ask: Some(
                "write a python3 file called wrap.py containing a function \
                 `wrap(text, width)` that breaks `text` into a list of lines of at most \
                 `width` characters. Break only at spaces and never split a word; a word \
                 longer than `width` gets a line to itself. Collapse runs of whitespace, and \
                 return an empty list for empty text. Standard library only.",
            ),
            save_as: "wrap.py",
            grade: &["python3", "test_wrap.py"],
        },
        other => panic!("no bench task for fixture {other}"),
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

/// The **Definite** section of a verdict — the only claim a run makes, and
/// so the only part worth marking.
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

/// A run, and everything it collected.
struct Run {
    working_dir: std::path::PathBuf,
    transcript: std::sync::Arc<std::sync::Mutex<String>>,
    session: uuid::Uuid,
    config: OllamaCommitteeConfig,
    remote_models: Vec<String>,
    path: String,
}

impl Run {
    fn say(&self, prompt: &str) {
        use futures_lite::future::block_on;

        let sink = std::sync::Arc::clone(&self.transcript);
        block_on(run_ollama_committee(
            prompt,
            &self.session,
            &self.working_dir,
            &self.path,
            self.config.clone(),
            self.remote_models.clone(),
            move |event| {
                if let TurnEvent::Text { text } = &event {
                    if let Ok(mut sink) = sink.lock() {
                        sink.push_str(text);
                        sink.push_str("\n\n");
                    }
                    println!("{text}");
                }
            },
        ))
        .expect("the committee runs");
    }

    fn transcript(&self) -> String {
        self.transcript.lock().expect("the transcript").clone()
    }
}

#[test]
#[ignore = "needs an Ollama and several minutes; run it by hand"]
fn a_real_committee() {
    let fixture = std::env::var("COMMITTEE_FIXTURE").unwrap_or_else(|_| "stock".to_string());
    let host = std::env::var("COMMITTEE_HOST")
        .unwrap_or_else(|_| "http://192.168.1.150:11434".to_string());
    let reviewers: Vec<String> = std::env::var("COMMITTEE_REVIEWERS")
        .unwrap_or_else(|_| "qwen2.5-coder:7b,deepseek-coder-v2:16b,qwen2.5-coder:14b".to_string())
        .split(',')
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();
    let judge = std::env::var("COMMITTEE_JUDGE").unwrap_or_else(|_| "phi4:14b".to_string());
    // Roles are keyed by model tag, so a model named as both a reviewer and
    // the judge silently becomes only the judge: a bench asked for three
    // reviewers quietly seats two, and the run measures something nobody
    // chose. It cost one matrix before anyone noticed.
    assert!(
        !reviewers.contains(&judge),
        "{judge} is named as a reviewer and as the judge; a model holds one seat"
    );
    let rounds: Vec<PanelRoundKind> = std::env::var("COMMITTEE_ROUNDS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|name| PanelRoundKind::from_str(name.trim()))
        .collect();

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

    let run = Run {
        working_dir: working_dir.clone(),
        transcript: std::sync::Arc::new(std::sync::Mutex::new(String::new())),
        session: uuid::Uuid::new_v4(),
        config: OllamaCommitteeConfig {
            reviewers: reviewers.len(),
            roles,
            panel_plan: rounds.clone(),
            ..OllamaCommitteeConfig::default()
        },
        remote_models,
        path: std::env::var("PATH").unwrap_or_default(),
    };

    let started = std::time::Instant::now();
    let task = task(&fixture);
    let (found, wanted, extra) = match &task {
        Task::Review(planted) => {
            run.say("review my changes");
            let definite = definite_block(&run.transcript());
            println!("\n\n=== {fixture} ===");
            let mut found = 0;
            for defect in *planted {
                let hit = definite.contains(defect.function);
                found += usize::from(hit);
                println!(
                    "{}  {} — {}",
                    if hit { "FOUND " } else { "MISSED" },
                    defect.function,
                    defect.defect
                );
            }
            // Everything else in Definite is an invention or a double, and
            // the verdict's own headline counts both as findings.
            let claims = definite.lines().filter(|line| line.contains("**[")).count();
            (found, planted.len(), claims.saturating_sub(found))
        }
        Task::Make {
            ask,
            save_as,
            grade,
        } => {
            let prompt = match ask {
                Some(literal) => literal.to_string(),
                None => std::fs::read_to_string(working_dir.join("PROMPT.txt"))
                    .expect("the fixture's PROMPT.txt"),
            };
            run.say(&prompt);
            // Exactly what the user types: the panel's winner written out
            // under a name, through the same `save` path.
            run.say(&format!("save {save_as}"));

            println!("\n\n=== {fixture} ===");
            let written = working_dir.join(save_as);
            if !written.exists() {
                println!("FAILED  nothing was saved as {save_as}");
                (0, 1, 0)
            } else {
                let marked = std::process::Command::new(grade[0])
                    .args(&grade[1..])
                    .current_dir(&working_dir)
                    .output()
                    .expect("the grader runs");
                let report = String::from_utf8_lossy(&marked.stdout);
                let complaint = String::from_utf8_lossy(&marked.stderr);
                println!("{report}");
                if !complaint.trim().is_empty() {
                    println!("{complaint}");
                }
                let passed = marked.status.success();
                println!(
                    "{}  {save_as} — {}",
                    if passed { "PASSED" } else { "FAILED" },
                    grade.join(" ")
                );
                (usize::from(passed), 1, 0)
            }
        }
    };

    let seconds = started.elapsed().as_secs();
    let rounds: Vec<&str> = rounds.iter().map(|round| round.as_str()).collect();
    // The commit the pipeline was at. A score is only a fact about the code
    // that produced it, and this one moved thirty times in a night — a table
    // of numbers with no commit against them is a table nobody can tell is
    // out of date, which is worse than no table.
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    // One line per run, so a session's worth greps back into a table.
    println!(
        "\nBENCH fixture={fixture} reviewers={} judge={judge} rounds={} found={found}/{wanted} \
         extra={extra} seconds={seconds} commit={commit}",
        reviewers.join("+"),
        if rounds.is_empty() {
            "none".to_string()
        } else {
            rounds.join("+")
        },
    );

    if let Ok(out) = std::env::var("COMMITTEE_OUT") {
        std::fs::write(&out, run.transcript()).expect("the transcript is written");
        println!("transcript: {out}");
    }
    let _ = std::fs::remove_dir_all(&target);
}
