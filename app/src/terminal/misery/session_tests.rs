use super::super::Phase;
use super::*;

#[test]
fn directory_name_takes_the_basename() {
    assert_eq!(
        directory_name(Some("/Volumes/Instruments/warp")),
        Some("warp".to_owned())
    );
    assert_eq!(
        directory_name(Some("/Volumes/Instruments/warp/")),
        Some("warp".to_owned()),
        "a trailing slash should not swallow the name"
    );
    assert_eq!(directory_name(None), None);
    assert_eq!(directory_name(Some("/")), None, "root has no useful name");
}

#[test]
fn listing_skips_dotfiles() {
    let dir = std::env::temp_dir().join(format!("misery-listing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("visible.txt"), b"").unwrap();
    std::fs::write(dir.join(".hidden"), b"").unwrap();

    let files = list_directory(dir.to_str().unwrap());

    assert!(files.contains(&"visible.txt".to_owned()));
    assert!(
        !files.iter().any(|f| f.starts_with('.')),
        "dotfiles should be skipped: {files:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn listing_a_missing_directory_is_not_an_error() {
    assert!(list_directory("/definitely/not/a/real/path/here").is_empty());
}

#[test]
fn directory_listing_is_cached_per_path() {
    let dir = std::env::temp_dir().join(format!("misery-cache-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("first.txt"), b"").unwrap();
    let path = dir.to_str().unwrap().to_owned();

    let mut session = MiserySession::new();
    let first = session.nearby_files(Some(&path));
    assert!(first.contains(&"first.txt".to_owned()));

    // A file added after the first listing should not appear, proving we
    // didn't re-stat the directory.
    std::fs::write(dir.join("second.txt"), b"").unwrap();
    let second = session.nearby_files(Some(&path));
    assert_eq!(
        first, second,
        "second call should have been served by cache"
    );

    // A different path must bypass the cache.
    let other = session.nearby_files(Some("/"));
    assert_ne!(other, first);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn no_working_directory_yields_no_files() {
    let mut session = MiserySession::new();
    assert!(session.nearby_files(None).is_empty());
}

#[test]
fn submitting_records_the_command_for_repetition() {
    let mut session = MiserySession::new();
    // Generation is probabilistic, but note_command must run regardless of
    // whether a line came back.
    for _ in 0..5 {
        session.on_command_submitted("ls -la", None, true);
    }
    let parsed = super::super::classify::parse("ls -la").unwrap();
    assert!(
        session.engine.history(&parsed, Phase::Submit).repeat_count > 0,
        "repeats should have been recorded"
    );
}

#[test]
fn unparseable_command_is_handled() {
    let mut session = MiserySession::new();
    assert!(session.on_command_submitted("", None, true).is_none());
    assert!(session.on_command_submitted("   ", None, true).is_none());
}

#[test]
fn completed_command_survives_a_missing_start_time() {
    let mut session = MiserySession::new();
    // No `started_at` means no duration, so duration templates must be
    // filtered out rather than panicking.
    for _ in 0..50 {
        session.on_command_completed(
            CompletedCommand {
                command: "ls",
                working_directory: None,
                exit_code: 0,
                started_at: None,
                output_lines: 10,
            },
            true,
        );
    }
}

#[test]
fn enormous_output_line_count_does_not_overflow() {
    let mut session = MiserySession::new();
    // `num_output_lines` is a u64 on the block; the context stores u32.
    for _ in 0..50 {
        session.on_command_completed(
            CompletedCommand {
                command: "cat huge.log",
                working_directory: None,
                exit_code: 0,
                started_at: None,
                output_lines: u64::MAX,
            },
            true,
        );
    }
}
