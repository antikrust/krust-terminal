use serde_json::json;
use uuid::Uuid;

use super::*;

const RECORD: &str = "# Conversation\nthe port is 8123\nsomething else\nThe PORT moved to 9000\n";

#[test]
fn search_finds_lines_in_any_case_with_their_numbers() {
    let found = search_text(RECORD, "port");
    assert!(found.contains("L2| the port is 8123"));
    assert!(found.contains("L4| The PORT moved to 9000"));
    assert!(!found.contains("something else"));
    assert!(found.contains("2 of 2 matching lines"));
}

#[test]
fn search_says_when_nothing_matches() {
    assert!(search_text(RECORD, "nothing like this").starts_with("No line"));
}

#[test]
fn search_lists_a_limited_number_of_matches() {
    let text = "match\n".repeat(LOOKUP_MATCHES + 5);
    let found = search_text(&text, "match");
    assert!(found.contains(&format!("{LOOKUP_MATCHES} of {} matching", LOOKUP_MATCHES + 5)));
}

#[test]
fn read_goes_from_an_offset_and_says_how_to_read_on() {
    let read = read_text(RECORD, 1, 2);
    assert!(read.starts_with("L2| the port is 8123"));
    assert!(read.contains("L3| something else"));
    assert!(!read.contains("L4|"));
    assert!(read.contains("offset 3 reads on"));
}

#[test]
fn read_to_the_end_has_no_read_on_note() {
    assert!(!read_text(RECORD, 2, 10).contains("reads on"));
}

#[test]
fn read_past_the_end_says_so() {
    assert!(read_text(RECORD, 100, 10).contains("has 4 lines"));
}

#[test]
fn a_read_never_comes_back_empty_for_one_huge_line() {
    let text = format!("{}\nnext\n", "x".repeat(LOOKUP_CHARS * 2));
    assert!(read_text(&text, 0, 10).starts_with("L1| "));
}

#[test]
fn a_long_line_is_shortened() {
    let line = numbered_line(0, &"x".repeat(LOOKUP_LINE_CHARS * 2));
    assert!(line.len() < LOOKUP_LINE_CHARS + 10);
}

#[test]
fn a_session_with_no_conversation_has_no_record() {
    let session = Uuid::new_v4();
    assert!(lookup_pointer(&session).is_none());
    assert!(lookup(&session, &json!({ "query": "port" })).is_err());
}
