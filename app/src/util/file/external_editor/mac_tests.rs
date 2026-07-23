use super::is_derp_bundle;

#[test]
fn is_derp_bundle_recognises_derp_channels() {
    assert!(is_derp_bundle("baby.derp.Derp"));
    assert!(is_derp_bundle("baby.derp.DerpDev"));
    assert!(is_derp_bundle("baby.derp.DerpPreview"));
    assert!(is_derp_bundle("baby.derp.DerpOss"));
}

#[test]
fn is_derp_bundle_rejects_other_apps() {
    assert!(!is_derp_bundle("com.microsoft.VSCode"));
    assert!(!is_derp_bundle("com.apple.TextEdit"));
    assert!(!is_derp_bundle("dev.zed.Zed"));
    assert!(!is_derp_bundle("invalid"));
    assert!(!is_derp_bundle(""));
}
