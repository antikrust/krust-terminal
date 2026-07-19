use super::*;

/// A slice of the real listing page, captured 2026-07-19. If these tests start
/// failing, ollama.com changed its markup and the live path is degrading to
/// [`bundled`] — regenerate the fixture and fix the parser.
const SAMPLE: &str = include_str!("test_data/library_sample.html");

fn parsed() -> Vec<CatalogEntry> {
    parse_library_html(SAMPLE)
}

fn find<'a>(entries: &'a [CatalogEntry], name: &str) -> &'a CatalogEntry {
    entries
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("{name} missing from parsed catalog"))
}

#[test]
fn parses_every_entry_in_the_sample() {
    let entries = parsed();
    assert_eq!(entries.len(), 6, "got: {:?}", names(&entries));
}

fn names(entries: &[CatalogEntry]) -> Vec<&str> {
    entries.iter().map(|e| e.name.as_str()).collect()
}

#[test]
fn extracts_name_description_and_sizes() {
    let entries = parsed();
    let llama = find(&entries, "llama3.1");

    assert!(
        llama.description.starts_with("Llama 3.1"),
        "description: {}",
        llama.description
    );
    let tags: Vec<&str> = llama.sizes.iter().map(|s| s.tag.as_str()).collect();
    assert_eq!(tags, vec!["8b", "70b", "405b"]);
}

#[test]
fn extracts_capability_tags_without_swallowing_other_spans() {
    let entries = parsed();
    let llama = find(&entries, "llama3.1");

    assert!(llama.capabilities.contains(&"tools".to_string()));
    // Pull counts, tag counts and "Updated" all live in spans too, and must
    // not be mistaken for capabilities.
    for junk in ["Pulls", "Tags", "Updated"] {
        assert!(
            !llama.capabilities.iter().any(|c| c.contains(junk)),
            "capability list polluted with {junk:?}: {:?}",
            llama.capabilities
        );
    }
}

#[test]
fn recognises_embedding_only_models() {
    let entries = parsed();
    assert!(find(&entries, "nomic-embed-text").is_embedding_only());
    assert!(!find(&entries, "llama3.1").is_embedding_only());
}

#[test]
fn sizes_are_sorted_smallest_first() {
    for entry in parsed() {
        let mut sorted = entry.sizes.clone();
        sorted.sort_by(|a, b| a.params.billions.total_cmp(&b.params.billions));
        assert_eq!(entry.sizes, sorted, "{} sizes out of order", entry.name);
    }
}

#[test]
fn builds_pull_tags_in_ollama_form() {
    let entries = parsed();
    let coder = find(&entries, "qwen2.5-coder");
    let smallest = coder.smallest_size().expect("has sizes");

    // Sub-billion sizes are written `0.5b` by Ollama, never `500m`, so the
    // tag must be the library's literal text rather than a reconstruction.
    assert_eq!(coder.tag_for(smallest), "qwen2.5-coder:0.5b");

    for size in &coder.sizes {
        let tag = coder.tag_for(size);
        assert!(tag.ends_with('b'), "not a pullable tag: {tag}");
        assert!(!tag.contains('m'), "millions form is not a real tag: {tag}");
    }
}

#[test]
fn deeper_library_links_do_not_become_entries() {
    // Tag and blob links share the /library/ prefix.
    let html = r#"
        <a href="/library/mistral"><span>7b</span></a>
        <a href="/library/mistral/tags"><span>ignored</span></a>
        <a href="/library/mistral:7b?foo=1"><span>ignored</span></a>
    "#;
    let entries = parse_library_html(html);
    assert_eq!(names(&entries), vec!["mistral"]);
}

#[test]
fn duplicate_links_to_the_same_model_collapse() {
    let html = r#"
        <a href="/library/mistral"><span>7b</span></a>
        <a href="/library/mistral"><span>7b</span></a>
    "#;
    assert_eq!(parse_library_html(html).len(), 1);
}

#[test]
fn unparseable_markup_yields_nothing_rather_than_garbage() {
    // The signal callers use to fall back to the bundled list.
    assert!(parse_library_html("").is_empty());
    assert!(parse_library_html("<html><body>redesigned</body></html>").is_empty());
}

#[test]
fn html_entities_are_decoded() {
    let html = r#"<a href="/library/x"><p>Tools &amp; agents</p><span>7b</span></a>"#;
    let entries = parse_library_html(html);
    assert_eq!(entries[0].description, "Tools & agents");
}

#[test]
fn bundled_fallback_is_usable_on_its_own() {
    let bundled = bundled();
    assert!(bundled.len() >= 5);
    for entry in &bundled {
        assert!(!entry.name.is_empty());
        assert!(!entry.description.is_empty(), "{} has no blurb", entry.name);
        assert!(
            entry.smallest_size().is_some(),
            "{} has no sizes, so nothing can be pulled",
            entry.name
        );
    }
}

#[test]
fn bundled_covers_the_models_the_local_harness_already_uses() {
    // The built-in Ollama coding harness hardcodes these two; the catalog
    // should not present a world where they don't exist.
    let bundled = bundled();
    for required in ["codellama", "deepseek-coder"] {
        assert!(
            bundled.iter().any(|e| e.name == required),
            "{required} missing from the offline catalog"
        );
    }
}
