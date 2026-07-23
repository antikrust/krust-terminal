use std::path::PathBuf;

use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};

use super::*;

/// A scratch subkey under HKCU that tests create/destroy to avoid touching
/// the real `Software\Microsoft\Windows\CurrentVersion\Run` hive.
struct ScratchSubkey {
    path: String,
}

impl ScratchSubkey {
    fn new(name: &str) -> Self {
        let suffix = format!(
            "{}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            name,
        );
        let path = format!(r"Software\Derp\LoginItemTests\{suffix}");
        RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(&path)
            .expect("create scratch subkey");
        Self { path }
    }

    fn read(&self, value_name: &str) -> Option<String> {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(&self.path, KEY_READ)
            .ok()?;
        key.get_value::<String, _>(value_name).ok()
    }
}

impl Drop for ScratchSubkey {
    fn drop(&mut self) {
        let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(&self.path);
    }
}

#[test]
fn register_writes_quoted_path() {
    let scratch = ScratchSubkey::new("register_writes_quoted_path");
    let exe = PathBuf::from(r"C:\Program Files\Derp\derp.exe");
    register_in(HKEY_CURRENT_USER, &scratch.path, "Derp", &exe).unwrap();
    assert_eq!(
        scratch.read("Derp").as_deref(),
        Some(r#""C:\Program Files\Derp\derp.exe""#)
    );
}

#[test]
fn register_overwrites_previous_path() {
    let scratch = ScratchSubkey::new("register_overwrites_previous_path");
    register_in(
        HKEY_CURRENT_USER,
        &scratch.path,
        "Derp",
        &PathBuf::from(r"C:\old\derp.exe"),
    )
    .unwrap();
    register_in(
        HKEY_CURRENT_USER,
        &scratch.path,
        "Derp",
        &PathBuf::from(r"C:\new\derp.exe"),
    )
    .unwrap();
    assert_eq!(
        scratch.read("Derp").as_deref(),
        Some(r#""C:\new\derp.exe""#)
    );
}

#[test]
fn unregister_is_idempotent() {
    let scratch = ScratchSubkey::new("unregister_is_idempotent");
    // Never registered: unregister should be Ok.
    unregister_in(HKEY_CURRENT_USER, &scratch.path, "Derp").unwrap();
    // Register, then unregister twice.
    register_in(
        HKEY_CURRENT_USER,
        &scratch.path,
        "Derp",
        &PathBuf::from(r"C:\derp.exe"),
    )
    .unwrap();
    unregister_in(HKEY_CURRENT_USER, &scratch.path, "Derp").unwrap();
    unregister_in(HKEY_CURRENT_USER, &scratch.path, "Derp").unwrap();
    assert!(scratch.read("Derp").is_none());
}

#[test]
fn unregister_leaves_other_values_alone() {
    let scratch = ScratchSubkey::new("unregister_leaves_other_values_alone");
    register_in(
        HKEY_CURRENT_USER,
        &scratch.path,
        "Derp",
        &PathBuf::from(r"C:\derp.exe"),
    )
    .unwrap();
    register_in(
        HKEY_CURRENT_USER,
        &scratch.path,
        "DerpPreview",
        &PathBuf::from(r"C:\derp-preview.exe"),
    )
    .unwrap();

    unregister_in(HKEY_CURRENT_USER, &scratch.path, "Derp").unwrap();

    assert!(scratch.read("Derp").is_none());
    assert_eq!(
        scratch.read("DerpPreview").as_deref(),
        Some(r#""C:\derp-preview.exe""#)
    );
}

#[test]
fn unregister_missing_subkey_is_ok() {
    unregister_in(
        HKEY_CURRENT_USER,
        r"Software\Derp\LoginItemTests\does-not-exist",
        "Derp",
    )
    .unwrap();
}
