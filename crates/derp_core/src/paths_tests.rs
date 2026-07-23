use dirs::home_dir;

use super::*;

#[test]
fn test_data_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    // ChannelState, by default, is configured for Channel::Oss.
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(data_dir(), home_dir.join(".derp-oss"));
        } else if #[cfg(any(target_os = "linux", target_os = "freebsd"))] {
            assert_eq!(data_dir(), home_dir.join(".local/share/derp-oss"));
        } else if #[cfg(windows)] {
            assert_eq!(data_dir(), home_dir.join("AppData\\Roaming\\derp\\DerpOss\\data"));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_config_local_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    // ChannelState, by default, is configured for Channel::Oss.
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(config_local_dir(), home_dir.join(".derp-oss"));
        } else if #[cfg(any(target_os = "linux", target_os = "freebsd"))] {
            assert_eq!(config_local_dir(), home_dir.join(".config/derp-oss"));
        } else if #[cfg(windows)] {
            assert_eq!(config_local_dir(), home_dir.join("AppData\\Local\\derp\\DerpOss\\config"));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_derp_home_config_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    let expected_dir_name = match ChannelState::data_profile() {
        Some(data_profile) => format!(".derp-oss-{data_profile}"),
        None => ".derp-oss".to_string(),
    };

    assert_eq!(
        derp_home_config_dir(),
        Some(home_dir.join(expected_dir_name))
    );
}

#[test]
fn test_derp_home_skills_and_mcp_paths() {
    let Some(config_dir) = derp_home_config_dir() else {
        panic!("Should be able to compute Derp home config directory");
    };

    assert_eq!(derp_home_skills_dir(), Some(config_dir.join("skills")));
    assert_eq!(
        derp_home_mcp_config_file_path(),
        Some(config_dir.join(".mcp.json"))
    );
}
#[test]
fn test_cache_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    // ChannelState, by default, is configured for Channel::Oss.
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(cache_dir(), home_dir.join("Library/Application Support/derp.baby.DerpOss"));
        } else if #[cfg(any(target_os = "linux", target_os = "freebsd"))] {
            assert_eq!(cache_dir(), home_dir.join(".cache/derp-oss"));
        } else if #[cfg(windows)] {
            assert_eq!(cache_dir(), home_dir.join("AppData\\Local\\derp\\DerpOss\\cache"));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_state_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    cfg_if::cfg_if! {
        // ChannelState, by default, is configured for Channel::Oss.
        if #[cfg(target_os = "macos")] {
            assert_eq!(state_dir(), home_dir.join("Library/Application Support/derp.baby.DerpOss"));
        } else if #[cfg(any(target_os = "linux", target_os = "freebsd"))] {
            assert_eq!(state_dir(), home_dir.join(".local/state/derp-oss"));
        } else if #[cfg(windows)] {
            assert_eq!(state_dir(), home_dir.join("AppData\\Local\\derp\\DerpOss\\data"));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_tui_state_dir_is_tui_subdir_of_gui_state_base() {
    let tui_dir = tui_state_dir();
    assert_eq!(tui_dir.file_name(), Some(std::ffi::OsStr::new("tui")));

    // The TUI state dir must be a direct `tui` child of the same base
    // directory that holds the GUI's SQLite database (the secure state dir
    // when available, otherwise the plain state dir), so the two front-ends
    // keep sibling — never shared — databases.
    let gui_state_base = secure_state_dir().unwrap_or_else(state_dir);
    assert_eq!(tui_dir.parent(), Some(gui_state_base.as_path()));
}

#[test]
fn test_project_path_for_derp_app_id() {
    let project_dirs = project_dirs_for_app_id(AppId::new("dev", "derp", "Derp"), None)
        .expect("should be able to compute project dirs");
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(project_dirs.project_path(), "derp.baby.Derp");
        } else if #[cfg(any(target_os = "linux", target_os = "freebsd"))] {
            assert_eq!(project_dirs.project_path(), "derp-terminal");
        } else if #[cfg(windows)] {
            assert_eq!(project_dirs.project_path(), "derp\\Derp");
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_project_path_for_derp_dev_app_id() {
    let project_dirs = project_dirs_for_app_id(AppId::new("dev", "derp", "DerpDev"), None)
        .expect("should be able to compute project dirs");
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(project_dirs.project_path(), "derp.baby.DerpDev");
        } else if #[cfg(any(target_os = "linux", target_os = "freebsd"))] {
            assert_eq!(project_dirs.project_path(), "derp-terminal-dev");
        } else if #[cfg(windows)] {
            assert_eq!(project_dirs.project_path(), "derp\\DerpDev");
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_project_path_for_oss_app_id() {
    let project_dirs = project_dirs_for_app_id(AppId::new("dev", "derp", "DerpOss"), None)
        .expect("should be able to compute project dirs");
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(project_dirs.project_path(), "derp.baby.DerpOss");
        } else if #[cfg(any(target_os = "linux", target_os = "freebsd"))] {
            assert_eq!(project_dirs.project_path(), "derp-oss");
        } else if #[cfg(windows)] {
            assert_eq!(project_dirs.project_path(), "derp\\DerpOss");
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}
