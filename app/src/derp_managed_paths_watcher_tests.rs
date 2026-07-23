use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use dirs::home_dir;
use repo_metadata::{RepositoryUpdate, TargetFile};

use super::{
    filter_repository_update_by_prefix, derp_home_mcp_config_file_path, derp_home_skills_dir,
    derp_managed_mcp_config_path, derp_managed_skill_dirs,
};

#[test]
fn derp_managed_skill_dirs_contains_only_derp_home_path() {
    let dirs = derp_managed_skill_dirs();
    match derp_home_skills_dir() {
        Some(derp_home_skills_dir) => assert_eq!(dirs, vec![derp_home_skills_dir]),
        None => assert!(dirs.is_empty()),
    }
}

#[test]
fn derp_managed_mcp_config_path_contains_only_derp_home_path() {
    match (
        home_dir(),
        derp_home_mcp_config_file_path(),
        derp_managed_mcp_config_path(),
    ) {
        (Some(home_dir), Some(derp_home_mcp_config_path), Some(path)) => {
            assert_eq!(path.root_path, home_dir);
            assert_eq!(path.config_path, derp_home_mcp_config_path);
        }
        (_, _, None) => {}
        _ => panic!("Expected Derp MCP path when home directory is available"),
    }
}

#[test]
fn filter_repository_update_by_prefix_keeps_only_matching_paths() {
    let skills_dir = PathBuf::from("/tmp/.derp-local/skills");
    let other_dir = PathBuf::from("/tmp/.derp-local/worktrees/repo");
    let skill_file = skills_dir.join("deploy").join("SKILL.md");
    let other_file = other_dir.join("README.md");

    let update = RepositoryUpdate {
        added: HashSet::from([
            TargetFile::new(skill_file.clone(), false),
            TargetFile::new(other_file.clone(), false),
        ]),
        modified: HashSet::new(),
        deleted: HashSet::new(),
        moved: HashMap::new(),
        commit_updated: false,
        index_lock_detected: false,
        remote_ref_updated: false,
    };

    let filtered =
        filter_repository_update_by_prefix(&update, &skills_dir).expect("expected update");

    assert!(filtered.contains_added_or_modified(&TargetFile::new(skill_file, false)));
    assert!(!filtered.contains_added_or_modified(&TargetFile::new(other_file, false)));
}

#[test]
fn filter_repository_update_by_prefix_converts_cross_boundary_moves() {
    let skills_dir = PathBuf::from("/tmp/.derp-local/skills");
    let skill_file = skills_dir.join("deploy").join("SKILL.md");
    let ignored_file = PathBuf::from("/tmp/.derp-local/worktrees/repo/SKILL.md");

    let update = RepositoryUpdate {
        added: HashSet::new(),
        modified: HashSet::new(),
        deleted: HashSet::new(),
        moved: HashMap::from([(
            TargetFile::new(skill_file.clone(), false),
            TargetFile::new(ignored_file, false),
        )]),
        commit_updated: false,
        index_lock_detected: false,
        remote_ref_updated: false,
    };

    let filtered =
        filter_repository_update_by_prefix(&update, &skills_dir).expect("expected update");

    assert!(filtered.contains_added_or_modified(&TargetFile::new(skill_file, false)));
    assert!(filtered.moved.is_empty());
    assert!(filtered.deleted.is_empty());
}
