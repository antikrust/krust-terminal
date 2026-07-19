//! The generated set is only useful if every path it advertises is actually
//! embedded, and if lookups degrade rather than panic on bad input.

use warp_assets::Assets;
use warpui_core::AssetProvider as _;

use super::*;

#[test]
fn every_advertised_path_is_a_real_embedded_asset() {
    for style in STYLES {
        for (seed, path) in style.variants {
            assert!(
                Assets.get(path).is_ok(),
                "{}/{seed} claims {path}, which is not embedded — regenerate with \
                 script/generate-avatars.mjs",
                style.id,
            );
        }
    }
}

#[test]
fn the_set_is_not_empty_and_styles_are_unique() {
    assert!(!STYLES.is_empty());
    let mut ids: Vec<&str> = STYLES.iter().map(|s| s.id).collect();
    ids.sort_unstable();
    let count = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), count, "duplicate style ids");
    assert!(
        STYLES.iter().all(|s| !s.variants.is_empty()),
        "a style with no variants would render an empty picker page"
    );
}

#[test]
fn lookups_reject_unknown_pairs_rather_than_panicking() {
    let known = STYLES.first().expect("a style");
    let (seed, path) = known.variants.first().expect("a variant");
    assert_eq!(asset_path(known.id, seed), Some(*path));

    assert_eq!(asset_path("no-such-style", seed), None);
    assert_eq!(asset_path(known.id, "no-such-seed"), None);
    // The unchosen-avatar case: a valid style with an empty seed.
    assert_eq!(asset_path(known.id, ""), None);
    assert!(style("no-such-style").is_none());
}
