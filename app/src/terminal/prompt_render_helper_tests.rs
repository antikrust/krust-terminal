use super::terminal_prompt_text;
use crate::settings::TerminalPromptStyle;

const HOME: Option<&str> = Some("/Users/blah");

#[test]
fn directory_style_shows_only_the_last_component() {
    assert_eq!(
        terminal_prompt_text(
            TerminalPromptStyle::Directory,
            Some("blah"),
            Some("mac"),
            HOME,
            Some("/Users/blah/committee-ops/dest"),
        ),
        "dest $"
    );
}

#[test]
fn home_and_root_keep_a_name_rather_than_going_blank() {
    // `~` and `/` have no meaningful last component; showing an empty name before the
    // sigil would read as a broken prompt.
    assert_eq!(
        terminal_prompt_text(
            TerminalPromptStyle::Directory,
            Some("blah"),
            Some("mac"),
            HOME,
            Some("/Users/blah"),
        ),
        "~ $"
    );
    assert_eq!(
        terminal_prompt_text(
            TerminalPromptStyle::Directory,
            Some("blah"),
            Some("mac"),
            HOME,
            Some("/"),
        ),
        "/ $"
    );
}

#[test]
fn path_style_abbreviates_home_to_tilde() {
    assert_eq!(
        terminal_prompt_text(
            TerminalPromptStyle::Path,
            Some("blah"),
            Some("mac"),
            HOME,
            Some("/Users/blah/committee-ops/dest"),
        ),
        "~/committee-ops/dest $"
    );
    // Outside home the path is shown as-is.
    assert_eq!(
        terminal_prompt_text(
            TerminalPromptStyle::Path,
            Some("blah"),
            Some("mac"),
            HOME,
            Some("/etc/nginx"),
        ),
        "/etc/nginx $"
    );
}

#[test]
fn root_gets_a_hash_in_every_style() {
    for style in [
        TerminalPromptStyle::Sigil,
        TerminalPromptStyle::Directory,
        TerminalPromptStyle::Path,
        TerminalPromptStyle::UserHost,
    ] {
        let prompt = terminal_prompt_text(style, Some("root"), Some("mac"), HOME, Some("/etc"));
        assert!(
            prompt.ends_with('#'),
            "{style:?} should end in # for root, got {prompt:?}"
        );
    }
}

#[test]
fn sigil_style_is_just_the_sigil() {
    assert_eq!(
        terminal_prompt_text(
            TerminalPromptStyle::Sigil,
            Some("blah"),
            Some("mac"),
            HOME,
            Some("/Users/blah/anywhere"),
        ),
        "$"
    );
}

#[test]
fn user_host_style_joins_who_and_where() {
    assert_eq!(
        terminal_prompt_text(
            TerminalPromptStyle::UserHost,
            Some("blah"),
            Some("mac"),
            HOME,
            Some("/Users/blah/dest"),
        ),
        "blah@mac ~/dest $"
    );
}

#[test]
fn a_prompt_survives_a_session_that_has_reported_nothing_yet() {
    // Before the shell reports anything there is still a usable prompt, rather than a
    // stray separator or an empty line.
    assert_eq!(
        terminal_prompt_text(TerminalPromptStyle::Directory, None, None, None, None),
        "$"
    );
    assert_eq!(
        terminal_prompt_text(TerminalPromptStyle::UserHost, None, None, None, None),
        "$"
    );
}
