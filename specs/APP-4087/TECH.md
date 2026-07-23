# APP-4087: Fix Derp skill and MCP home config paths after watcher unification Technical Spec
## Problem
APP-3945 introduced `DerpDataDirectoryWatcher` to centralize Derp-specific filesystem watching and avoid recursively watching `.derp*/worktrees`. The follow-up problem is that `SkillProvider::Derp` and `MCPProvider::Derp` were tied to app data paths. On macOS those paths are home-relative and channel-aware, but on Linux and Windows they are XDG/AppData project directories.
The technical goal is to preserve centralized watching while separating three path concepts:
- `data_dir()` for channel/platform app data such as themes, workflows, launch configs, and tab configs
- `config_local_dir()` for platform-specific local config such as settings and preferences
- a new Derp home config directory helper for user-facing Derp Skills and MCP, preserving `.derp*` home-relative names and channel/profile isolation across all OSes
## Relevant code
- `crates/derp_core/src/paths.rs` — owns app data/config paths and the new Derp home config directory helpers.
- `app/src/derp_managed_paths_watcher.rs` — singleton watcher for safe Derp-managed roots and app-local wrappers around the core helpers.
- `app/src/lib.rs` — startup registration for the Derp watcher and watch root preparation.
- `app/src/ai/skills/file_watchers/skill_watcher.rs` — subscribes to Derp watcher events and filters Derp home skill updates.
- `app/src/ai/skills/resolve_skill_spec.rs` — resolves `oz --skill` specs and scans home/global skill directories on cold start.
- `app/src/ai/skills/file_watchers/utils.rs` — classifies skill paths and detects home provider skill paths.
- `app/src/ai/skills/skill_utils.rs` — maps changed files to `SKILL.md` paths.
- `crates/ai/src/skills/skill_provider.rs` — defines provider paths, `home_skills_path`, provider classification, and scope classification.
- `app/src/ai/mcp/file_mcp_watcher.rs` — subscribes to Derp watcher events and handles Derp home MCP updates.
- `app/src/ai/mcp/mod.rs` — defines `MCPProvider::home_config_path`, `home_config_file_path`, and `mcp_provider_from_file_path`.
- `app/src/user_config/native.rs` — consumes Derp watcher updates for themes, workflows, launch configs, tab configs, and settings.
## Path helper design
Add purpose-specific helpers in `derp_core::paths`:
- `derp_home_config_dir_name() -> String`
- `derp_home_config_dir() -> Option<PathBuf>`
- `derp_home_skills_dir() -> Option<PathBuf>`
- `derp_home_mcp_config_file_path() -> Option<PathBuf>`
`derp_home_config_dir_name()` uses `ChannelState::channel()` and `ChannelState::data_profile()`:
- Stable and Preview: `.derp`
- Dev: `.derp-dev`
- Local: `.derp-local`
- Integration: `.derp-integration`
- OpenDerp: `.openderp`
- Debug data profile: append `-<profile>` to the base name
`derp_home_config_dir()` joins that name to `dirs::home_dir()`. This intentionally differs from non-macOS `data_dir()` and `config_local_dir()`, which use `ProjectDirs` and therefore produce XDG/AppData paths.
## Watch roots
`DerpManagedPathsWatcher` registers:
- `data_dir()` recursively, with a filter excluding `<data_dir>/worktrees`
- `config_local_dir()` recursively when distinct from `data_dir()`
- `derp_home_skills_dir()` recursively when it exists and is not already covered by `data_dir()` or `config_local_dir()`
- `derp_home_config_dir()` non-recursively with a filter that only accepts `derp_home_mcp_config_file_path()` when it exists and is not already covered by `data_dir()` or `config_local_dir()`
The implementation must not recursively watch all of `~/.derp` or all possible `.derp*` directories.
If the Derp home config paths do not exist, the watcher should not fail the session. Startup disk parsing and cold resolution should still work when files exist, and future creation behavior can be handled separately if needed.
## Skill watcher consumption
`SkillWatcher` subscribes to `DerpManagedPathsWatcher`.
Initial Derp skill loading reads from `derp_managed_skill_dirs()`, which resolves to `derp_core::paths::derp_home_skills_dir()` when home exists.
Incremental handling filters updates by the current environment’s Derp home skills directory:
```text
for skills_dir in derp_managed_skill_dirs() {
    if let Some(filtered_update) = filter_repository_update_by_prefix(update, &skills_dir) {
        handle_repository_update(filtered_update)
    }
}
```
`SkillProvider::Derp` remains excluded from generic home-provider watching. The generic home-provider watcher must not be used to watch `.derp*` parents.
## Skill resolver cold-start behavior
`resolve_skill_spec` checks home/global skill directories from disk after cached home matches and before project resolution.
The resolver fallback scans home provider paths in provider precedence order. For `SkillProvider::Derp`, `home_skills_path(SkillProvider::Derp)` resolves to `derp_core::paths::derp_home_skills_dir()`.
Full-path skill specs keep existing root-relative behavior and should not start accepting arbitrary absolute paths.
## Skill path classification
Skill path classification helpers classify the current environment’s Derp home skills directory as the home Derp skills path:
- `extract_skill_parent_directory`
- `is_home_provider_path`
- `skill_path_from_file_path`
- `get_provider_for_path`
- `get_scope_for_path`
They do not classify non-macOS XDG/AppData `data_dir()/skills` as a Derp home skill path.
## MCP watcher consumption
`FileMCPWatcher` subscribes to `DerpManagedPathsWatcher`.
Startup parsing parses the single `derp_managed_mcp_config_path()` if home exists.
Incremental handling evaluates the single current-environment config path:
```text
let mcp_path = derp_managed_mcp_config_path()
let was_deleted = update deletes or moves out mcp_path.config_path
let was_added = update adds/modifies or moves into mcp_path.config_path
handle_single_config_update(mcp_path.root_path, MCPProvider::Derp, mcp_path.config_path, was_deleted, was_added)
```
The config path is `derp_core::paths::derp_home_mcp_config_file_path()`. The logical root path remains `dirs::home_dir()` so `FileBasedMCPManager` treats it as user-scoped MCP config.
## MCP path classification
`home_config_file_path(MCPProvider::Derp)` returns `derp_core::paths::derp_home_mcp_config_file_path()`.
`mcp_provider_from_file_path` recognizes the exact Derp home MCP path first, then continues to fall back to project-config suffix matching for project configs.
## Preserve user config behavior
`DerpConfig` keeps consuming `DerpManagedPathsWatcher` for themes, workflows, launch configs, tab configs, and settings. Its filtering remains path-specific:
- data-dir content is still checked against `themes_dir()`, `workflows_dir()`, `launch_configs_dir()`, and `tab_configs_dir()`
- settings still uses `user_preferences_toml_file_path()` under `config_local_dir()`
## End-to-end flow
1. Startup prepares the standard channel-aware watch roots.
2. `DerpManagedPathsWatcher` registers `data_dir()` with `worktrees` excluded and registers `config_local_dir()` when distinct.
3. `DerpManagedPathsWatcher` registers safe Derp home roots if not already covered: the Skills directory recursively and the config directory narrowly for `.mcp.json`.
4. The watcher emits `FilesChanged(update)` for all registered managed roots.
5. `DerpConfig` filters the update for user config paths and reloads relevant config.
6. `SkillWatcher` filters the update against the current environment’s Derp home skills directory and handles skill add/update/delete semantics.
7. `FileMCPWatcher` checks the current environment’s Derp home MCP config path and emits user-scoped MCP events as appropriate.
8. `oz --skill <name>` resolves from cached home skills first, then scans home/global skill paths from disk, then falls back to project/repo resolution.
## Risks and mitigations
- Risk: recursively watching `.derp*` parents reintroduces worktree events.
  - Mitigation: register the exact Skills directory for recursive watching and only watch the config directory non-recursively filtered to `.mcp.json`.
- Risk: path helper changes accidentally reclassify project `.derp/skills` as home skills.
  - Mitigation: only classify the exact current-environment Derp home skills prefix as a home path; continue using suffix matching for project provider paths.
- Risk: `FileBasedMCPManager` stores Derp home MCP under the wrong logical root.
  - Mitigation: the managed MCP config helper carries both logical root and file path, with `root_path = home_dir`.
- Risk: cold `oz --skill` still races async skill loading.
  - Mitigation: resolver scans home/global skill directories directly before project fallback.
## Alternatives Considered
- Use only hardcoded `~/.derp` for Skills/MCP. Rejected because it loses Dev/Local/Profile environment isolation.
- Keep using `data_dir()` for Skills/MCP. Rejected because non-macOS app data paths are XDG/AppData paths, not Derp’s home-relative `.derp*` config paths.
- Use `config_local_dir()` for Skills/MCP. Rejected because non-macOS config-local paths are also platform project directories, not home-relative `.derp*` paths.
- Fix only `resolve_skill_spec`. Rejected as incomplete because `SkillWatcher` would still not initial-load or hot-reload Derp home skills, and MCP would keep the same platform mismatch.
- Let `SkillWatcher` register a home provider watcher for Derp again. Rejected because `DirectoryWatcher` would watch a `.derp*` parent recursively for skills, which can reintroduce worktree events.
- Add separate filesystem watchers in `SkillWatcher` and `FileMCPWatcher`. Rejected because the central watcher was introduced specifically to make Derp-managed path filtering and exclusions auditable in one place.
## Testing and validation
- Add or update `derp_core::paths` tests for Derp home config directory, Skills directory, and MCP config path helpers.
- Add or update `resolve_skill_spec` tests for resolving a simple skill name from a Derp home skills directory without relying on `SkillManager`.
- Add or update watcher helper tests for current-environment Derp home Skills and MCP helper paths.
- Add or update skill utility tests showing the current-environment Derp home skills directory classifies as a Derp home skill path.
- Add or update MCP helper tests showing the current-environment Derp home MCP file classifies as `MCPProvider::Derp`.
- Run targeted commands such as:
  - `cargo test -p derp_core --lib paths`
  - `cargo test -p ai skills::skill_provider --lib`
  - `cargo test -p derp --lib resolve_from_root_path_by_directory_scan`
  - `cargo test -p derp --lib file_watchers::utils`
  - `cargo test -p derp --lib mcp`
  - `cargo test -p derp --lib derp_managed_paths_watcher`
## Follow-ups
- Consider adding lifecycle support for creating the current environment’s Derp home config directory after startup without proactively creating it.
- Consider updating public docs if they should explicitly describe Dev/Local/Profile-specific `.derp*` Skills and MCP locations.
