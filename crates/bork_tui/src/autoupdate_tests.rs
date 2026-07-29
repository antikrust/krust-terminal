use std::fs;
use std::path::{Path, PathBuf};

use super::{InstallLayout, InstallLock};

/// Creates a unique, empty temp directory for a test.
fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bork-tui-autoupdate-test-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn detects_managed_install_layout() {
    let exe = Path::new("/home/user/.bork/tui/versions/v0.2026.01.01.00.00.dev_00/bork-tui-dev");
    let layout = InstallLayout::from_canonical_exe_path(exe).unwrap();

    assert_eq!(layout.root, Path::new("/home/user/.bork/tui"));
    assert_eq!(
        layout.versions_dir,
        Path::new("/home/user/.bork/tui/versions")
    );
    assert_eq!(
        layout.current_link,
        Path::new("/home/user/.bork/tui/current")
    );
    assert_eq!(
        layout.running_version_dir,
        Path::new("/home/user/.bork/tui/versions/v0.2026.01.01.00.00.dev_00")
    );
    assert_eq!(layout.binary_name, "bork-tui-dev");
}

#[test]
fn rejects_unmanaged_exe_paths() {
    // Legacy flat install: the binary sits directly under the install root.
    assert_eq!(
        InstallLayout::from_canonical_exe_path(Path::new("/home/user/.bork/tui/bork-tui-dev")),
        None
    );
    // A plain cargo build in target/.
    assert_eq!(
        InstallLayout::from_canonical_exe_path(Path::new("/repo/target/debug/bork-tui-dev")),
        None
    );
}

#[test]
fn install_lock_is_exclusive_and_released_on_drop() {
    let root = temp_root("lock");

    let lock = InstallLock::acquire(&root).unwrap();
    assert!(lock.is_some());

    // A second acquisition fails while the lock is held.
    assert!(InstallLock::acquire(&root).unwrap().is_none());

    // Dropping the lock releases it.
    drop(lock);
    assert!(InstallLock::acquire(&root).unwrap().is_some());

    let _ = fs::remove_dir_all(&root);
}
