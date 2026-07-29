use super::is_bork_bundle;

#[test]
fn is_bork_bundle_recognises_bork_channels() {
    assert!(is_bork_bundle("bork.baby.Bork"));
    assert!(is_bork_bundle("bork.baby.BorkDev"));
    assert!(is_bork_bundle("bork.baby.BorkPreview"));
    assert!(is_bork_bundle("bork.baby.Bork"));
}

#[test]
fn is_bork_bundle_rejects_other_apps() {
    assert!(!is_bork_bundle("com.microsoft.VSCode"));
    assert!(!is_bork_bundle("com.apple.TextEdit"));
    assert!(!is_bork_bundle("dev.zed.Zed"));
    assert!(!is_bork_bundle("invalid"));
    assert!(!is_bork_bundle(""));
}
