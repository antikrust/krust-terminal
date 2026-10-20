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

/// Reads every part, answering each with `reply(part index)` and every
/// finding check with YES.
fn read_all<'a>(notes: &mut WorkNotes, reply: impl Fn(usize) -> &'a str) {
    let mut part = 0;
    while let Some(call) = notes.next_call() {
        if call.label.contains("checking finding") {
            notes.accept(Some("YES"));
        } else if call.label.starts_with("part") {
            notes.accept(Some(reply(part)));
            part += 1;
        } else {
            break;
        }
    }
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
        if index == 1 {
            // A finding is checked against the lines it cites first.
            let check = notes.next_call().unwrap();
            assert_eq!(
                check.label,
                format!("part 2 of {parts}: checking finding 1 of 1")
            );
            assert!(check.user.contains("A reader's finding: error at [L8]"));
            assert!(check.user.contains("L8| line 8 of a long log"));
            notes.accept(Some("YES, line 8 shows it"));
        }
    }
    let call = notes.next_call().unwrap();
    assert_eq!(call.label, "answering from the notes");
    assert!(call.user.contains("error at [L8]"));
    assert!(
        !call.user.contains("NOTHING"),
        "empty parts aren't passed on"
    );
    notes.accept(Some("It failed: see [L8]."));
    // The line the answer rests on is checked against the text.
    let check = notes.next_call().unwrap();
    assert_eq!(check.label, "checking the answer against the text (1 of 1)");
    assert!(check.user.starts_with("Claim: It failed: see [L8]."));
    assert!(check.user.contains("L8| line 8 of a long log"));
    assert!(check.user.contains("L4| line 4"));
    assert!(!check.user.contains("L13|"));
    notes.accept(Some("TRUE: line 8 is there."));
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

/// A finding the lines it cites don't bear out is dropped before it becomes
/// a note; one they do is kept, as are lines that cite nothing. Past six
/// cited findings a reader is listing noise: the rest go unchecked, dropped.
#[test]
fn a_finding_the_lines_dont_bear_out_is_dropped() {
    let dir = temp_dir("findings");
    let text = paragraphs();
    let mut notes = WorkNotes::with_limits("log", "did it fail?", &text, &dir, 600, 5_000);
    notes.accept(Some(
        "Two things:\n[L2] the write fails here\n  because the disk is full\n[L3] line 3 of a long log",
    ));
    let first = notes.next_call().unwrap();
    assert!(
        first.label.ends_with("checking finding 1 of 2"),
        "{}",
        first.label
    );
    assert!(first
        .user
        .contains("A reader's finding: [L2] the write fails here\n  because the disk is full"));
    notes.accept(Some("NO — line 2 says nothing of a write"));
    assert!(notes
        .next_call()
        .unwrap()
        .label
        .ends_with("checking finding 2 of 2"));
    notes.accept(Some("Yes."));
    assert!(notes.next_call().unwrap().label.starts_with("part 2 of"));
    let file = notes.render();
    assert!(
        file.contains("Two things:\n[L3] line 3 of a long log"),
        "{file}"
    );
    assert!(
        file.contains("Dropped, as the lines didn't bear them out:\n- [L2] the write fails here")
    );

    // Seven findings on lines of the second part.
    let numbers: Vec<usize> = notes.parts[1]
        .lines
        .iter()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(number, _)| *number)
        .take(7)
        .collect();
    assert_eq!(numbers.len(), 7);
    let many = numbers
        .iter()
        .map(|n| format!("[L{n}] line {n} of a long log"))
        .collect::<Vec<_>>()
        .join("\n");
    notes.accept(Some(&many));
    let mut checks = 0;
    while notes
        .next_call()
        .is_some_and(|call| call.label.contains("checking finding"))
    {
        checks += 1;
        notes.accept(Some("YES"));
    }
    assert_eq!(checks, 6);
    let file = notes.render();
    let last = numbers[6];
    assert!(
        file.contains(&format!("- [L{last}] line {last} of a long log")),
        "{file}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// bonsai-2:27b's finding on the Blunt file: the label's line, then why,
/// citing the line the misspelled name is defined on. One finding, not two.
#[test]
fn a_finding_goes_on_until_the_next_line_that_opens_with_a_label() {
    let reply = "[L6197] state.biasDcX = shaepd;\nThis line assigns a variable named `shaepd`, a misspelling of \
                 `shaped` (defined on L6170).\n- [L6201] another\n1. [L6203] and one more\nL6210| a fifth";
    let findings = findings_in(reply);
    assert_eq!(findings.len(), 4, "{findings:?}");
    assert!(findings[0].ends_with("(defined on L6170)."));
    assert!(starts_with_label("**[L12]** typo"));
    assert!(!starts_with_label("This line (L12) has a typo"));
    assert!(!starts_with_label("6197 is the line"));
}

/// llama3.1:8b's answer on the Blunt file named a line "missing a
/// semicolon" that has one. Checked against the text, a claim like that is
/// dropped with the notes behind it, and the question answered again.
#[test]
fn an_answer_the_text_contradicts_is_dropped_and_given_again() {
    let dir = temp_dir("check");
    let text = paragraphs();
    let mut notes = WorkNotes::with_limits("log", "did it fail?", &text, &dir, 300, 5_000);
    read_all(&mut notes, |index| match index {
        0 => "[L2] the semicolon is missing here",
        1 => "[L8] the error that stops the build",
        _ => "NOTHING",
    });
    assert_eq!(notes.next_call().unwrap().label, "answering from the notes");
    notes.accept(Some("Line 2 is missing a semicolon.\nAlso see [L8]."));
    assert_eq!(
        notes.next_call().unwrap().label,
        "checking the answer against the text (1 of 2)"
    );
    notes.accept(Some("FALSE — line 2 ends as it should."));
    assert!(notes
        .next_call()
        .unwrap()
        .user
        .contains("Claim: Also see [L8]."));
    notes.accept(Some("TRUE"));

    // Answered again, without the note on line 2.
    let again = notes.next_call().unwrap();
    assert_eq!(again.label, "answering from the notes");
    assert!(!again.user.contains("semicolon"), "{}", again.user);
    assert!(again.user.contains("[L8] the error"));
    notes.accept(Some("It failed at [L8]."));
    notes.accept(Some("TRUE"));
    assert_eq!(notes.answer(), Some("It failed at [L8]."));
    let file = std::fs::read_to_string(notes.notes_path()).unwrap();
    assert!(file.contains("## Checked against the text, and dropped"));
    assert!(file
        .contains("- L2: \"Line 2 is missing a semicolon.\" — FALSE — line 2 ends as it should."));

    // When nothing is left once the rejected notes go, that is the answer.
    let dir2 = temp_dir("check-nothing-left");
    let mut emptied = WorkNotes::with_limits("log", "q", &text, &dir2, 300, 5_000);
    read_all(&mut emptied, |_| "[L2] a semicolon is missing");
    emptied.accept(Some("See [L2]."));
    emptied.accept(Some("FALSE: it isn't missing"));
    assert!(emptied
        .answer()
        .unwrap()
        .starts_with("Nothing found in the text bears on the question."));

    // A second answer the text contradicts stands, with what failed said.
    let dir3 = temp_dir("check-twice");
    let mut twice = WorkNotes::with_limits("log", "q", &text, &dir3, 300, 5_000);
    read_all(&mut twice, |_| {
        "[L2] a semicolon is missing\n[L3] one is missing here too"
    });
    twice.accept(Some("See [L2]."));
    twice.accept(Some("FALSE: it isn't missing"));
    assert_eq!(twice.next_call().unwrap().label, "answering from the notes");
    twice.accept(Some("See [L3]."));
    twice.accept(Some("FALSE: nor here"));
    assert_eq!(
        twice.answer(),
        Some("See [L3].\n\n(Checked against the text, this did not hold up: L3 — FALSE: nor here)")
    );
    for dir in [dir, dir2, dir3] {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn an_answers_cited_lines_are_found_however_it_writes_them() {
    assert_eq!(cited_lines("see [L12]"), vec![(12, 12)]);
    assert_eq!(cited_lines("in [L12-L14]"), vec![(12, 14)]);
    assert_eq!(
        cited_lines("Line 7550 is missing a semicolon"),
        vec![(7550, 7550)]
    );
    assert_eq!(
        cited_lines("lines 610–615 and line 9"),
        vec![(610, 615), (9, 9)]
    );
    assert_eq!(
        cited_lines("the pipeline 3 times"),
        Vec::<(usize, usize)>::new()
    );
    assert!(rejects("FALSE — it has one"));
    assert!(rejects("false."));
    assert!(!rejects("TRUE: the lines show it, not false"));
    assert!(!rejects("The lines can't settle it."));
    let claims = answer_claims("It is [L5].\nAnd [L5] again, and line 99999.", 60);
    assert_eq!(claims.len(), 1);
    assert_eq!(excerpt_range(2, 2, 60), (1, 6));
    assert_eq!(excerpt_range(100, 400, 1_000), (96, 111));
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

/// A text from a file, read by a real model, for trying readers on real
/// material: WORK_NOTES_FILE is the text, WORK_NOTES_QUESTION what is asked
/// of it, and WORK_NOTES_EXPECT_LINE the line the answer must name.
/// Optionally WORK_NOTES_EXPECT_WORDS (comma-separated; the answer must say
/// one), WORK_NOTES_TITLE, WORK_NOTES_DIR (where the notes are kept, one
/// folder per model; a temporary one otherwise), WORK_NOTES_PART_CHARS (the
/// most text one reading call sees, for trying other sizes) and
/// WORK_NOTES_LIVE_MODEL.
/// Each reply is printed as it comes, then how the pass went.
#[test]
#[ignore = "hits the live local Ollama service; set WORK_NOTES_FILE to use it"]
fn the_file_named_in_the_environment_is_read_against_live_ollama() {
    use std::time::Instant;

    use futures_lite::future::block_on;

    use crate::ai::agent_sdk::driver::harness::ollama_stream_exec::{
        ask_fresh, probe_model_caps_checked,
    };

    let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let file = PathBuf::from(env("WORK_NOTES_FILE").expect("WORK_NOTES_FILE names the text"));
    let question = env("WORK_NOTES_QUESTION").expect("WORK_NOTES_QUESTION is what to ask of it");
    let line: usize = env("WORK_NOTES_EXPECT_LINE")
        .and_then(|line| line.parse().ok())
        .expect("WORK_NOTES_EXPECT_LINE is the line the answer must name");
    let words: Vec<String> = env("WORK_NOTES_EXPECT_WORDS")
        .map(|words| {
            words
                .split(',')
                .map(|word| word.trim().to_lowercase())
                .filter(|word| !word.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let title = env("WORK_NOTES_TITLE")
        .unwrap_or_else(|| file.file_name().unwrap().to_string_lossy().into_owned());
    let model = env("WORK_NOTES_LIVE_MODEL").unwrap_or_else(|| "qwen2.5-coder:14b".to_string());
    let kept = env("WORK_NOTES_DIR").map(PathBuf::from);
    let dir = kept.clone().unwrap_or_else(|| temp_dir("file"));
    let text = std::fs::read_to_string(&file).unwrap();

    let path = std::env::var("PATH").unwrap_or_default();
    let session = Uuid::new_v4();
    let think = block_on(probe_model_caps_checked(&model, &path))
        .is_some_and(|caps| caps.thinking)
        .then_some(false);
    let part_chars = env("WORK_NOTES_PART_CHARS")
        .and_then(|chars| chars.parse().ok())
        .unwrap_or(PART_CHARS);
    let mut notes =
        WorkNotes::with_limits(&title, &question, &text, &dir, part_chars, REDUCE_CHARS);
    let started = Instant::now();
    eprintln!(
        "{} lines in {} parts, read by {model}; notes in {}",
        notes.total_lines(),
        notes.part_count(),
        notes.notes_path().display()
    );
    if let Some(other) = env("WORK_NOTES_REUSE").map(PathBuf::from) {
        let carried = carry_over_notes(&mut notes, &other, part_chars);
        eprintln!(
            "{carried} of {} parts are the same in {}: their notes are carried over, not read again",
            notes.part_count(),
            other.display()
        );
    }
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
        eprintln!(
            "[{:>5}s] {}: {}",
            started.elapsed().as_secs(),
            next.label,
            reply.as_deref().map_or("(no reply)".to_string(), one_line)
        );
        notes.accept(reply.as_deref());
    }

    let answer = notes.answer().unwrap().to_string();
    let holder = notes
        .parts
        .iter()
        .position(|part| (part.first_line..=part.last_line).contains(&line))
        .expect("the expected line is in the text");
    let noted = notes
        .saved
        .notes
        .iter()
        .filter(|note| note.as_deref().is_some_and(|note| !note.trim().is_empty()))
        .count();
    let holder_note = notes.saved.notes[holder].clone().unwrap_or_default();
    let named = names_line(&answer, line);
    let in_a_range = !named && a_range_holds(&answer, line);
    let lower = answer.to_lowercase();
    let said = words.is_empty() || words.iter().any(|word| lower.contains(word.as_str()));
    let part = &notes.parts[holder];
    eprintln!(
        "\nThe part holding L{line} (lines {}–{}) noted: {}\n\n## Answer\n\n{answer}\n",
        part.first_line,
        part.last_line,
        if holder_note.trim().is_empty() {
            "nothing"
        } else {
            holder_note.trim()
        }
    );
    eprintln!(
        "RESULT model={model} names_line={} line_in_a_range={in_a_range} says_what={said} \
         part_noted_line={} parts_with_notes={noted}/{} seconds={}",
        named,
        names_line(&holder_note, line) || a_range_holds(&holder_note, line),
        notes.part_count(),
        started.elapsed().as_secs()
    );
    if kept.is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(
        named && said,
        "the answer names L{line} and says what is wrong:\n{answer}"
    );
}

/// For WORK_NOTES_REUSE: another run's notes folder — the same model and
/// question, on a text that differs from this one in a few lines. A part
/// whose text is the same there got its note from the same prompt at
/// temperature 0, so that note is taken instead of reading the part again.
/// Returns how many were carried over.
fn carry_over_notes(notes: &mut WorkNotes, other: &Path, part_chars: usize) -> usize {
    let Ok(entries) = std::fs::read_dir(other) else {
        return 0;
    };
    let mut carried = 0;
    for entry in entries.flatten() {
        let state = entry.path();
        if state.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let (Some(saved), Ok(text)) = (
            std::fs::read_to_string(&state)
                .ok()
                .and_then(|json| serde_json::from_str::<Saved>(&json).ok()),
            std::fs::read_to_string(state.with_extension("txt")),
        ) else {
            continue;
        };
        // The same question and part size, or the notes answer something else.
        if saved.key != notes_key(&notes.question, &text, part_chars) {
            continue;
        }
        let parts = split_into_parts(&text, part_chars);
        for (index, part) in notes.parts.iter().enumerate() {
            if notes.saved.notes[index].is_some() {
                continue;
            }
            let same = parts
                .iter()
                .position(|theirs| theirs.numbered == part.numbered)
                .and_then(|theirs| saved.notes.get(theirs).cloned().flatten());
            if let Some(note) = same {
                notes.saved.notes[index] = Some(note);
                carried += 1;
            }
        }
    }
    if notes.saved.answer.is_none() && notes.saved.notes.iter().all(Option::is_some) {
        notes.start_reducing(0);
    }
    notes.save();
    carried
}

/// A reply on one line, cut to 240 characters, for a progress log.
fn one_line(reply: &str) -> String {
    let flat = reply.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(240) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    }
}

/// Whether `text` names line `line`: its number on its own (`6197`,
/// `L6197`, `line 6197`), not inside a longer number.
fn names_line(text: &str, line: usize) -> bool {
    let digits = line.to_string();
    text.match_indices(&digits).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + digits.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_digit()) && !after.is_some_and(|c| c.is_ascii_digit())
    })
}

/// Whether a range `text` cites (`[L6190-L6205]`) holds `line`.
fn a_range_holds(text: &str, line: usize) -> bool {
    citations(text).windows(2).any(|pair| {
        let ((first_at, first), (second_at, second)) = (&pair[0], &pair[1]);
        text.get(first_at.end..second_at.start.saturating_sub(1)) == Some("-")
            && (*first.min(second)..=*first.max(second)).contains(&line)
    })
}

#[test]
fn a_line_is_named_only_by_its_own_number() {
    assert!(names_line("the typo at [L6197]", 6197));
    assert!(names_line("line 6197: shaepd", 6197));
    assert!(!names_line("L61970", 6197));
    assert!(!names_line("16197", 6197));
    assert!(a_range_holds("in [L6190-L6205]", 6197));
    assert!(!a_range_holds("in [L6190-L6195]", 6197));
    assert!(!a_range_holds("[L6190] and [L6205]", 6197));
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

/// granite4 named the misspelled identifier and cited a line 23 lines from
/// the one that has it: a quoted token found on one line only is cited.
#[test]
fn a_quoted_token_on_one_line_of_the_part_is_cited() {
    let lines = numbered_lines(&[
        (6172, "    float u = dryIn * drv;"),
        (
            6174,
            "    const double F = au + std::log1p(std::exp(-2.0 * au));",
        ),
        (6196, "    x = shaped - state.biasDcX;"),
        (6197, "    state.biasDcX = shaepd;"),
    ]);
    assert_eq!(
        cite_quoted_lines(
            "[L6174] references a variable named \"shaepd\", which is undefined",
            &lines
        ),
        "[L6174] references a variable named \"shaepd\" [L6197], which is undefined"
    );
    // Already cited, on more than one line, or on none: left as it is.
    for note in [
        "[L6197] `shaepd` is a typo",
        "[L6174] uses `state` here",
        "[L6174] `nowhere` is undefined",
        "[L6174] it's fine, isn't it",
    ] {
        assert_eq!(cite_quoted_lines(note, &lines), note);
    }
}

/// A finding that cites two lines far apart is checked against both.
#[test]
fn a_finding_on_two_far_lines_is_checked_against_both() {
    let dir = temp_dir("far");
    let mut notes = WorkNotes::with_limits("log", "q", &paragraphs(), &dir, 2_000, 5_000);
    notes.accept(Some("[L2] and [L40] differ"));
    let call = notes.next_call().unwrap();
    assert!(
        call.user.contains("Lines 1–6 and 36–44 of log"),
        "{}",
        call.user
    );
    assert!(call.user.contains("L2| line 2 of a long log"));
    assert!(call.user.contains("\n…\nL36| "));
    assert!(call.user.contains("L40| line 40 of a long log"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_note_of_bare_citations_counts_as_nothing() {
    assert!(only_citations("[L120-L173]"));
    assert!(only_citations("[L340]-[L341]"));
    assert!(!only_citations("[L612] mismatched types"));
}
