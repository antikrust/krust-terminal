# APP-3945: Channel-aware Derp home watching Technical Spec

## Problem
Derp's hot-reload behavior for user-managed files depends on several consumers observing filesystem changes from the current channel's Derp home. This work centralizes those updates behind `DerpDataDirectoryWatcher`, but it must also preserve a platform-specific requirement: public settings live under `config_local_dir()` while most other Derp home content lives under `data_dir()`.

The technical problem is to keep one Derp-specific watcher abstraction while:
- filtering `.derp*/worktrees` only for the data-directory tree
- watching `config_local_dir()` in addition to `data_dir()` when those roots differ
- preserving the existing downstream event contracts used by `DerpConfig`, Derp home MCP watching, and Derp home skill watching
- avoiding new generic filtering APIs in `repo_metadata::DirectoryWatcher`

## Relevant code
- `app/src/derp_data_directory_watcher.rs` — Derp-specific singleton watcher that owns a `BulkFilesystemWatcher`, registers Derp home roots, and emits `DerpDataDirectoryWatcherEvent::FilesChanged`.
- `app/src/lib.rs` — startup wiring that prepares the Derp watch roots before registering `DerpDataDirectoryWatcher`.
- `app/src/user_config/native.rs` — `DerpConfig` subscription that maps watcher updates to `Themes`, `Workflows`, `LaunchConfigs`, `TabConfigs`, and `Settings` events.
- `app/src/settings/init.rs` — settings-file hot-reload pipeline that reacts to `DerpConfigUpdateEvent::Settings`.
- `app/src/ai/mcp/file_mcp_watcher.rs` — Derp home MCP watcher subscriber that depends on `DerpDataDirectoryWatcher`.
- `app/src/ai/mcp/mod.rs` — helper that resolves the Derp MCP home config path through the shared Derp data directory helpers.
- `app/src/ai/skills/file_watchers/skill_watcher.rs` — Derp home skill watcher subscriber that depends on `DerpDataDirectoryWatcher`.
- `app/src/ai/skills/file_watchers/utils.rs` — skill path parsing helpers, including the Derp-home special case.
- `app/src/ai/skills/skill_utils.rs` — helper for resolving a skill root from a changed file path.
- `crates/ai/src/skills/skill_provider.rs` — provider/scope classification for channel-aware Derp home skill paths.
- `crates/integration/src/test/settings_file_hot_reload.rs` — end-to-end settings hot-reload coverage.

## Current state
`DerpDataDirectoryWatcher` is a Derp-specific singleton that owns its own `BulkFilesystemWatcher`, similar to `HomeDirectoryWatcher`. It does not depend on `DirectoryWatcher`, and it does not require any per-directory filter plumbing in `repo_metadata`.

At startup:
- setup code prepares `data_dir()` before `DerpDataDirectoryWatcher` is registered
- `DerpDataDirectoryWatcher` registers `data_dir()` recursively with a `WatchFilter` that excludes `<data_dir>/worktrees`
- setup code prepares `config_local_dir()` when it differs from `data_dir()`
- `DerpDataDirectoryWatcher` registers `config_local_dir()` recursively when it differs from `data_dir()`

The watcher receives `BulkFilesystemWatcherEvent` values, converts them into `RepositoryUpdate`, and emits `DerpDataDirectoryWatcherEvent::FilesChanged` directly. It does not apply a second worktree filter after registration-time filtering.

`DerpConfig` subscribes to that event stream and reloads themes, workflows, launch configs, and tab configs on background tasks when the update touches the relevant paths. Settings continue to flow through `DerpConfigUpdateEvent::Settings`.

`FileMCPWatcher` subscribes to the same singleton for Derp home MCP updates while continuing to use the existing home-directory and repository watching paths for non-Derp providers. The duplicated startup parse path and the duplicated single-config incremental update path are now shared with the non-Derp logic.

`SkillWatcher` subscribes to the same singleton for Derp home skill updates while continuing to use the existing home-directory and repository watching paths for non-Derp providers and project skills. The initial Derp home skill load and repository-scan load now share the same directory-read helper.

Because Derp home skills now live under the channel-aware `data_dir()/skills`, helper code also needs to recognize that path when determining provider, scope, and enclosing skill directory.

## Chosen design

### 1. Dedicated Derp watcher ownership
`DerpDataDirectoryWatcher` owns a `BulkFilesystemWatcher` directly instead of layering on top of `DirectoryWatcher`.

This keeps the abstraction boundary simple:
- project repositories still use `DirectoryWatcher`
- Derp-owned home/config directories use `DerpDataDirectoryWatcher`

That separation avoids broadening the generic repo watcher API for a Derp-specific use case.

### 2. Watch root preparation and registration
Each Derp-owned root is prepared before watcher registration, but that preparation happens outside the watcher constructor:
- call `create_dir_all()` during startup/setup
- keep watcher registration separate from root creation
- log failures before registration rather than implicitly recovering inside watcher construction

`data_dir()` is registered with a watcher-level `WatchFilter` that excludes `<data_dir>/worktrees`.

`config_local_dir()` is registered with `WatchFilter::accept_all()` when it is distinct from `data_dir()`.

### 3. Update normalization
`BulkFilesystemWatcherEvent` is converted into `RepositoryUpdate` so existing subscribers can keep using the same helper logic and event vocabulary.

`filter_repository_update()` is retained as a downstream helper to:
- keep only the paths relevant to a downstream consumer
- convert cross-boundary moves into add/delete updates when a move crosses the watched prefix boundary

### 4. Downstream consumers
`DerpConfig` remains subscription-based and does not reintroduce a direct `notify` watcher.

It continues to:
- reload themes, workflows, launch configs, and tab configs when updates touch the relevant directories
- perform those file-backed reloads via `ctx.spawn(...)` so disk reads happen off the model thread
- emit `DerpConfigUpdateEvent::Settings` when updates touch `user_preferences_toml_file_path()`

`FileMCPWatcher` subscribes to Derp watcher events for `data_dir()/.mcp.json` and keeps the existing logic for non-Derp providers. Derp remains on the Derp-specific watcher source, but shared helpers now cover:
- the startup parse path for a single config file
- the single-config incremental update path used by both Derp and non-Derp providers

`SkillWatcher` subscribes to Derp watcher events for `data_dir()/skills` and keeps the existing logic for non-Derp home providers and project repositories. The Derp home initialization path and repository scan path share the same helper for reading skill directories and emitting updates.

### 5. Channel-aware helper paths
The helper path logic is updated so Derp home skills and MCP config resolve through the active channel's data directory:
- Derp MCP home config resolves to `derp_data_mcp_config_file_path()`
- Derp home skills resolve to `data_dir()/skills`
- provider and scope classification recognize those paths as Derp home paths rather than project paths

## End-to-end flow
1. Derp startup prepares `data_dir()` and, when distinct, `config_local_dir()` before constructing `DerpDataDirectoryWatcher`.
2. `DerpDataDirectoryWatcher` registers `data_dir()` with a watcher-level filter that excludes `<data_dir>/worktrees`.
3. If `config_local_dir()` differs from `data_dir()`, `DerpDataDirectoryWatcher` registers that root with no extra filter.
4. The underlying `BulkFilesystemWatcher` emits `BulkFilesystemWatcherEvent`.
5. `DerpDataDirectoryWatcher` converts that event into `RepositoryUpdate` and emits `FilesChanged(update)`.
6. `DerpConfig`, `FileMCPWatcher`, and `SkillWatcher` inspect the update and react only when their owned paths were touched.
7. For themes, workflows, launch configs, and tab configs, `DerpConfig` reloads from disk on background tasks before applying the new state on the model thread.
8. For `settings.toml`, `DerpConfig` emits `DerpConfigUpdateEvent::Settings`, and `settings::init()` reloads public settings from disk.

## Risks and mitigations
- Distinct-root platforms regress settings hot reload.
  - Mitigation: register `config_local_dir()` separately when it differs from `data_dir()`.
- Worktree events leak back into Derp home consumers.
  - Mitigation: apply a watcher-level filter to `data_dir()` and let downstream consumers do any additional prefix filtering they need.
- Fresh installs fail to register watchers because the root directory is missing.
  - Mitigation: prepare each watch root during startup before registration.
- Path comparisons miss updates because watcher event paths and logical config paths may differ by symlink or canonical form.
  - Mitigation: `DerpConfig` continues checking both raw and canonicalized paths in `update_touches_dir()` and `update_touches_path()`.
- Derp home skill or MCP paths are misclassified as project paths.
  - Mitigation: centralize Derp-home path handling in the MCP and skills helper utilities.

## Testing and validation
- `cargo test -p derp --lib filter_repository_update_by_prefix_keeps_only_matching_paths`
  - validates the downstream filtering helper that skills consumers rely on
- `cargo test -p integration --test integration settings_file_hot_reload -- --nocapture`
  - verifies the end-to-end settings hot-reload behavior by rewriting `settings.toml` and asserting in-memory settings updates
- Code review validation:
  - `data_dir()` is watched with the `worktrees` exclusion
  - `config_local_dir()` is watched only when distinct
  - `DerpDataDirectoryWatcher` owns its own filesystem watcher instead of extending `DirectoryWatcher`
  - watch-root preparation happens before watcher registration instead of inside the watcher constructor
  - Derp home MCP and skill helpers resolve through the channel-aware data directory

## Follow-ups
- Add a focused test for `DerpDataDirectoryWatcher` root registration when `data_dir()` and `config_local_dir()` differ.
- Consider whether the remaining Derp-home helper changes in the MCP and skills layers can be reduced further without regressing path classification.
