use uuid::Uuid;

use super::*;

fn temp_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("work-notes-{name}-{}", Uuid::new_v4()))
}

/// 60 numbered lines in paragraphs of six, about 1.3 KB.
fn paragraphs() -> String {
    (1..=60)
        .map(|n| {
            if n % 6 == 0 {
                String::new()
            } else {
                format!("line {n} of a long log, with some words")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn parts_cover_every_line_once_and_end_at_blank_lines() {
    // 180 lines in paragraphs of six; parts of about 27 lines, so the last
    // quarter of each part always holds a blank line to end at.
    let text = (1..=180)
        .map(|n| {
            if n % 6 == 0 {
                String::new()
            } else {
                format!("line {n} of a long log, with some words")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let parts = split_into_parts(&text, 1_200);
    assert!(parts.len() > 4, "{} parts", parts.len());
    let mut next = 1;
    for part in &parts {
        assert_eq!(part.first_line, next, "parts are contiguous");
        assert!(part.numbered.len() <= 1_200 + 60, "{}", part.numbered.len());
        next = part.last_line + 1;
    }
    assert_eq!(next, text.lines().count() + 1, "every line is in a part");
    for part in &parts[1..] {
        assert_eq!(
            part.first_line % 6,
            0,
            "part starts at line {}",
            part.first_line
        );
    }
    assert!(parts[0].numbered.starts_with("L1| line 1 of a long log"));
}

#[test]
fn an_overlong_line_is_split_and_keeps_its_number() {
    let text = format!("short\n{}\nend", "x".repeat(1_000));
    let parts = split_into_parts(&text, 400);
    assert!(parts
        .iter()
        .any(|part| part.first_line == 2 && part.last_line == 2));
    assert_eq!(parts.last().unwrap().last_line, 3);
}

#[test]
fn reads_each_part_then_answers_from_the_notes() {
    let dir = temp_dir("flow");
    let text = paragraphs();
    let mut notes = WorkNotes::with_limits("build log", "did it fail?", &text, &dir, 300, 5_000);
    let parts = notes.part_count();
    for index in 0..parts {
        let call = notes.next_call().unwrap();
        assert_eq!(call.label, format!("part {} of {parts}", index + 1));
        assert!(call.user.starts_with("Question: did it fail?"));
        let reply = if index == 1 {
            "error at [L8]"
        } else {
            "NOTHING"
        };
        notes.accept(Some(reply));
    }
    let call = notes.next_call().unwrap();
    assert_eq!(call.label, "answering from the notes");
    assert!(call.user.contains("error at [L8]"));
    assert!(
        !call.user.contains("NOTHING"),
        "empty parts aren't passed on"
    );
    notes.accept(Some("It failed: see [L8]."));
    assert_eq!(notes.answer(), Some("It failed: see [L8]."));
    assert!(notes.next_call().is_none());

    let file = std::fs::read_to_string(notes.notes_path()).unwrap();
    assert!(file.contains("## Answer\n\nIt failed: see [L8]."));
    assert!(file.contains("(nothing on the question)"));
    assert!(file.trim_end().ends_with(&format!(
        "Progress: {parts} of {parts} parts read · answered"
    )));
    assert_eq!(std::fs::read_to_string(notes.text_path()).unwrap(), text);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_stopped_pass_picks_up_where_it_left_off() {
    let dir = temp_dir("resume");
    let text = paragraphs();
    let mut first = WorkNotes::with_limits("log", "q", &text, &dir, 300, 5_000);
    first.accept(Some("one"));
    first.accept(Some("two"));
    drop(first);

    let resumed = WorkNotes::with_limits("log", "q", &text, &dir, 300, 5_000);
    assert_eq!(
        resumed.next_call().unwrap().label,
        format!("part 3 of {}", resumed.part_count())
    );
    assert!(resumed.render().contains("Progress: 2 of"));

    let other_question = WorkNotes::with_limits("log", "another q", &text, &dir, 300, 5_000);
    assert_eq!(
        other_question.next_call().unwrap().label,
        format!("part 1 of {}", other_question.part_count())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn notes_too_long_to_answer_from_are_condensed_in_rounds() {
    let dir = temp_dir("reduce");
    let text = paragraphs();
    let mut notes = WorkNotes::with_limits("log", "q", &text, &dir, 200, 300);
    let parts = notes.part_count();
    for index in 0..parts {
        notes.accept(Some(&format!(
            "note for part {index}: {}",
            "detail ".repeat(10)
        )));
    }
    let mut condensing = 0;
    loop {
        let call = notes.next_call().unwrap();
        if call.label == "answering from the notes" {
            break;
        }
        assert!(call.label.starts_with("condensing notes"), "{}", call.label);
        condensing += 1;
        notes.accept(Some("short"));
    }
    assert!(condensing >= 2, "{condensing} condensing calls");
    notes.accept(Some("answer"));
    assert_eq!(notes.answer(), Some("answer"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_text_with_nothing_on_the_question_answers_without_a_call() {
    let dir = temp_dir("nothing");
    let mut notes = WorkNotes::with_limits("log", "q", &paragraphs(), &dir, 300, 5_000);
    while notes.answer().is_none() {
        notes.accept(Some("**Nothing.**"));
    }
    assert_eq!(
        notes.answer(),
        Some("Nothing in the text bears on the question.")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_call_is_noted_and_the_pass_goes_on() {
    let dir = temp_dir("failed");
    let mut notes = WorkNotes::with_limits("log", "q", &paragraphs(), &dir, 300, 5_000);
    notes.accept(None);
    assert!(notes.render().contains("(this part could not be read)"));
    while notes
        .next_call()
        .is_some_and(|call| call.label.starts_with("part"))
    {
        notes.accept(Some("NOTHING"));
    }
    // The unreadable part is still reported, so the answer says it is missing.
    let call = notes.next_call().unwrap();
    assert!(call.user.contains("could not be read"));
    notes.accept(None);
    assert!(notes.answer().unwrap().contains("could not be read"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The prompts against a real model: a 900-line build log with one error
/// buried at line 612. The answer must name the error and cite its line.
#[test]
#[ignore = "hits the live local Ollama service and loads a real model"]
fn a_buried_error_is_found_and_cited_against_live_ollama() {
    use futures_lite::future::block_on;

    use crate::ai::agent_sdk::driver::harness::ollama_stream_exec::{
        ask_fresh, probe_model_caps_checked,
    };

    let log: String = (1..=900)
        .map(|n| {
            if n == 612 {
                "error[E0308]: mismatched types: expected `i32`, found `f64` at src/scroll.rs:41"
                    .to_string()
            } else {
                format!(
                    "   Compiling crate_{n} v0.{n}.0 (/work/crates/crate_{n}) in {}ms",
                    n * 7 % 900
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(log.len() > LARGE_TEXT_CHARS);
    // Trying a smaller reader: WORK_NOTES_LIVE_MODEL=granite4:7b-a1b-h.
    let model =
        std::env::var("WORK_NOTES_LIVE_MODEL").unwrap_or_else(|_| "qwen2.5-coder:14b".to_string());
    let dir = temp_dir("live");
    let path = std::env::var("PATH").unwrap_or_default();
    let session = Uuid::new_v4();
    let think = block_on(probe_model_caps_checked(&model, &path))
        .is_some_and(|caps| caps.thinking)
        .then_some(false);
    let mut notes = WorkNotes::new(
        "cargo build output",
        "Did the build fail, and why?",
        &log,
        &dir,
    );
    while let Some(next) = notes.next_call() {
        let reply = block_on(ask_fresh(
            &model,
            think,
            next.system,
            &next.user,
            &session,
            &dir,
            &path,
        ))
        .unwrap();
        notes.accept(reply.as_deref());
    }
    let answer = notes.answer().unwrap().to_string();
    let file = std::fs::read_to_string(notes.notes_path()).unwrap();
    assert!(
        answer.contains("E0308") || answer.to_lowercase().contains("mismatch"),
        "{answer}\n\n{file}"
    );
    assert!(
        answer.contains("612"),
        "the line is cited:\n{answer}\n\n{file}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn numbered_lines(lines: &[(usize, &str)]) -> Vec<(usize, String)> {
    lines
        .iter()
        .map(|(n, line)| (*n, line.to_string()))
        .collect()
}

#[test]
fn a_citation_one_line_off_is_moved_to_the_line_it_means() {
    let lines = numbered_lines(&[
        (611, "   Compiling crate_611 v0.611.0"),
        (
            612,
            "error[E0308]: mismatched types: expected `i32`, found `f64` at src/scroll.rs:41",
        ),
        (613, "   Compiling crate_613 v0.613.0"),
    ]);
    assert_eq!(
        ground_citations(
            "The build failed: mismatched types at [L613], error[E0308].",
            &lines
        ),
        "The build failed: mismatched types at [L612], error[E0308]."
    );
    // A range that holds the line is fine; one that misses it is widened.
    assert_eq!(
        ground_citations("[L610-L613] mismatched types", &lines),
        "[L610-L613] mismatched types"
    );
    let wider = numbered_lines(&[
        (608, "   Compiling crate_608 v0.608.0"),
        (609, "   Compiling crate_609 v0.609.0"),
        (610, "   Compiling crate_610 v0.610.0"),
        (611, "   Compiling crate_611 v0.611.0"),
        (612, "error[E0308]: mismatched types at src/scroll.rs:41"),
    ]);
    assert_eq!(
        ground_citations("[L608-L610] mismatched types", &wider),
        "[L608-L612] mismatched types"
    );
    assert_eq!(
        ground_citations("E0308 at [L612]", &lines),
        "E0308 at [L612]"
    );
    // Nothing nearby matches: the citation stays as the reader wrote it.
    assert_eq!(
        ground_citations("disk full at [L40]", &lines),
        "disk full at [L40]"
    );
}

#[test]
fn a_note_of_bare_citations_counts_as_nothing() {
    assert!(only_citations("[L120-L173]"));
    assert!(only_citations("[L340]-[L341]"));
    assert!(!only_citations("[L612] mismatched types"));
}
