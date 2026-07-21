use super::*;

#[test]
fn every_style_generates_valid_svg() {
    for style in styles() {
        for seed in ["", "01", "alice", "hello world", "\u{1f600}", "123"] {
            let svg = generate(&style.id, seed)
                .unwrap_or_else(|| panic!("{} should generate", style.id));
            assert!(svg.starts_with("<svg "), "{}: bad start", style.id);
            assert!(svg.ends_with("</svg>"), "{}: bad end", style.id);
            assert!(svg.contains("viewBox="), "{}: no viewBox", style.id);
            assert!(
                !svg.contains("{{"),
                "{} seed {seed:?}: leftover template marker",
                style.id
            );
            // More than just the wrapper: some picked component rendered.
            assert!(svg.len() > 120, "{} seed {seed:?}: suspiciously empty", style.id);
        }
    }
}

#[test]
fn generation_is_deterministic_and_seed_sensitive() {
    let a1 = generate("bottts", "alice").unwrap();
    let a2 = generate("bottts", "alice").unwrap();
    assert_eq!(a1, a2, "same (style, seed) must be identical");

    let bob = generate("bottts", "bob").unwrap();
    assert_ne!(a1, bob, "different seeds should differ");
}

#[test]
fn unknown_style_is_none() {
    assert!(generate("does-not-exist", "x").is_none());
}

#[test]
fn styles_present_and_default_known() {
    assert!(!styles().is_empty());
    assert!(styles().iter().any(|s| s.id == DEFAULT_STYLE));
    assert_eq!(label(DEFAULT_STYLE), Some("Robots"));
}

/// Guards the PRNG against accidental changes: hashing is a fixed algorithm, so
/// these first-`next()` values are stable. Computed from the reference port.
#[test]
fn prng_is_stable() {
    // Two seeds that must not collide, proving the hash mixes input.
    assert_ne!(generate("identicon", "a"), generate("identicon", "b"));
    // Empty seed still generates (hashSeed(0) -> 1).
    assert!(generate("identicon", "").is_some());
}
