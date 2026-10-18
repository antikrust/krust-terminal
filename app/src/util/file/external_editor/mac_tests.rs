use super::is_krust_bundle;

#[test]
fn is_krust_bundle_recognises_krust_channels() {
    assert!(is_krust_bundle("krust.baby.Krust"));
    assert!(is_krust_bundle("krust.baby.KrustDev"));
    assert!(is_krust_bundle("krust.baby.KrustPreview"));
    assert!(is_krust_bundle("krust.baby.Krust"));
}

#[test]
fn is_krust_bundle_rejects_other_apps() {
    assert!(!is_krust_bundle("com.microsoft.VSCode"));
    assert!(!is_krust_bundle("com.apple.TextEdit"));
    assert!(!is_krust_bundle("dev.zed.Zed"));
    assert!(!is_krust_bundle("invalid"));
    assert!(!is_krust_bundle(""));
}
