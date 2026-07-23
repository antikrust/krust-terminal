//! Config-file parsing. The resolution path itself reads process env and the
//! state directory, so these cover the parser — the part with actual logic.

use super::*;

#[test]
fn parses_key_value_lines() {
    let values = parse_env_file("MOLOCH_URL=https://192.168.1.50:10102\nMOLOCH_TOKEN=abc123\n");
    assert_eq!(
        values.get("MOLOCH_URL").map(String::as_str),
        Some("https://192.168.1.50:10102")
    );
    assert_eq!(
        values.get("MOLOCH_TOKEN").map(String::as_str),
        Some("abc123")
    );
}

#[test]
fn skips_comments_blanks_and_handles_export_and_quotes() {
    let values = parse_env_file(
        r#"
# the box in the study
export MOLOCH_URL="https://10.0.0.4:10102"

  MOLOCH_TOKEN = 'quoted-token'
not a pair
"#,
    );
    assert_eq!(
        values.get("MOLOCH_URL").map(String::as_str),
        Some("https://10.0.0.4:10102")
    );
    assert_eq!(
        values.get("MOLOCH_TOKEN").map(String::as_str),
        Some("quoted-token")
    );
    assert_eq!(values.len(), 2, "non-pair lines must be ignored");
}

#[test]
fn empty_values_are_treated_as_absent() {
    assert_eq!(non_empty(Some("   ".to_owned())), None);
    assert_eq!(non_empty(Some(String::new())), None);
    assert_eq!(non_empty(Some(" tok ".to_owned())), Some("tok".to_owned()));
}
