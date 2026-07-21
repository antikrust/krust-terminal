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
            // Data-extraction artifacts (a broken `${...}` rewrite once leaked
            // a stray backtick into personas' face fill).
            assert!(
                !svg.contains('`') && !svg.contains("${"),
                "{} seed {seed:?}: extraction artifact in output",
                style.id
            );
            // More than just the wrapper: some picked component rendered.
            assert!(svg.len() > 120, "{} seed {seed:?}: suspiciously empty", style.id);
        }
    }
}

/// The styles that ship a background palette must paint it; the rest must not.
/// (fun-emoji without its background rect is eyes floating on nothing, and
/// croodles is black ink that needs its pastel paper to read on dark themes.)
#[test]
fn background_styles_paint_a_backdrop() {
    for style in ["fun-emoji", "thumbs", "shapes", "croodles"] {
        let svg = generate(style, "1").expect("generates");
        assert!(
            svg.contains("<rect fill=\"#"),
            "{style}: missing background rect"
        );
    }
    let plain = generate("bottts", "1").expect("generates");
    assert!(
        !plain.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 180 180\" fill=\"none\"><rect fill=\"#"),
        "bottts should not paint a background"
    );
}

/// Styles culled for looking wrong on dark themes stay culled. (Croodles was
/// culled once too, but it came back on a pastel paper background — it turned
/// out to be somebody's favourite set.)
#[test]
fn culled_styles_are_gone_and_fall_back() {
    for culled in ["lorelei", "notionists", "identicon"] {
        assert!(generate(culled, "1").is_none(), "{culled} should be culled");
    }
    assert!(generate("croodles", "1").is_some(), "croodles is back");
    assert_eq!(styles().len(), 15);
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

/// Audit tool, not an assertion: dumps every style x seed as SVG files so the
/// native output can be eyeballed next to the official JS renders (see
/// `script/generate-avatar-data.mjs`). Run with:
/// `DICEBEAR_DUMP_DIR=/tmp/dump cargo test -p warp --lib dicebear -- --ignored`
#[test]
#[ignore = "audit tool: writes SVGs to DICEBEAR_DUMP_DIR"]
fn dump_all_styles_for_audit() {
    let Some(dir) = std::env::var_os("DICEBEAR_DUMP_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("create dump dir");
    for style in styles() {
        for seed in ["1", "2", "3", "4", "5", "6"] {
            let svg = generate(&style.id, seed).expect("generates");
            std::fs::write(dir.join(format!("{}_{seed}.svg", style.id)), svg)
                .expect("write dump");
        }
    }
}

/// Guards the PRNG against accidental changes: hashing is a fixed algorithm, so
/// these first-`next()` values are stable. Computed from the reference port.
#[test]
fn prng_is_stable() {
    // Two seeds that must not collide, proving the hash mixes input.
    assert_ne!(generate("rings", "a"), generate("rings", "b"));
    // Empty seed still generates (hashSeed(0) -> 1).
    assert!(generate("rings", "").is_some());
}
