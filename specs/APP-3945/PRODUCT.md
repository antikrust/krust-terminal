# APP-3945: Channel-aware Derp home watching Product Spec

## Summary
Derp should hot-reload the current channel's Derp-managed files without reacting to unrelated files under `.derp*/worktrees`. This includes continuing to reload `settings.toml` correctly on platforms where settings live under `config_local_dir()` instead of `data_dir()`.

## Problem
Derp currently relies on filesystem watching for several user-visible behaviors: reloading themes, workflows, launch configs, tab configs, Derp home MCP config, Derp home skills, and public settings from `settings.toml`. The watcher surface is easy to regress because Derp-managed files are split across different directories depending on platform and channel.

The specific failure modes this work addresses are:
- changes under `.derp*/worktrees` can produce false-positive updates for Derp home watchers
- a watcher rooted only at `data_dir()` can miss `settings.toml` on Linux and Windows, where `config_local_dir()` differs from `data_dir()`
- fresh installs or hermetic test environments can fail to watch missing directories unless Derp prepares those roots before registering the watcher

## Goals
- Watch the current channel's Derp-managed directories through a single Derp-specific watcher model.
- Ignore filesystem activity under `.derp*/worktrees` so worktree contents do not trigger Derp home reload behavior.
- Continue reloading `settings.toml` when it changes on every supported platform, including platforms where settings live outside `data_dir()`.
- Preserve existing hot-reload behavior for themes, workflows, launch configs, tab configs, Derp home MCP config, and Derp home skills.

## Non-goals
- Changing where any Derp-managed file is stored.
- Changing the semantics of settings parsing, settings migration, or settings validation.
- Adding new user-facing UI for watcher state or diagnostics.
- Expanding watch coverage to arbitrary files outside Derp-managed directories.
- Changing the generic repository watcher APIs used for project repositories.

## Figma / design references
Figma: none provided

## User Experience

### Watch scope
- Derp watches the current channel's Derp-owned filesystem roots through a single singleton watcher.
- `data_dir()` remains the source of truth for channel-scoped Derp home content such as themes, workflows, launch configs, tab configs, MCP config, and skills.
- `config_local_dir()` is also watched when it is a different directory from `data_dir()`.
- When both path helpers resolve to the same directory, Derp behaves as before and does not create duplicate logical coverage.

### Settings hot reload
- When `settings.toml` changes, Derp reloads public settings from disk and applies the new values to in-memory settings models.
- This behavior must work whether `settings.toml` lives in the same directory as the rest of Derp home files or in a separate config directory.
- Creating, modifying, renaming into place, or deleting `settings.toml` must continue to flow through the existing `DerpConfigUpdateEvent::Settings` path.

### Worktree exclusion
- Files under `.derp`, `.derp-dev`, `.derp-local`, or equivalent channel-scoped Derp home directories that are nested inside `worktrees/` must not trigger Derp home reload behavior.
- Editing files inside a cloned repository stored under `.derp*/worktrees/...` must not cause Derp to reload themes, workflows, tab configs, MCP config, skills, or settings.

### Channel awareness
- Derp only reacts to files under the active channel's directories.
- A stable or dev install should not reload in response to files written into another channel's Derp home.

### Fresh-install and test-environment behavior
- If a watched Derp-owned root directory does not exist yet, Derp should create it during startup/setup before registering the watcher.
- Missing directories must not silently disable hot reload for the rest of the session.

### No regressions for existing consumers
- Editing a theme file in Derp home still updates the available theme set.
- Editing workflows, launch configs, or tab configs in Derp home still refreshes those objects.
- Editing Derp home MCP config still updates file-based MCP servers.
- Editing Derp home skills still refreshes Derp-provided skills.

## Success Criteria
- `settings.toml` hot reload works on macOS, Linux, and Windows.
- Worktree activity under `.derp*/worktrees` no longer triggers Derp home reloads.
- Themes, workflows, launch configs, tab configs, Derp MCP config, and Derp skills continue to hot reload from the current channel's Derp home.
- Derp prepares missing watch roots before attempting to register watchers.
- The watcher architecture remains centralized behind a Derp-specific singleton instead of reintroducing separate ad hoc watchers for individual consumers.

## Validation
- Unit-test the watcher filtering behavior so updates outside the kept prefix are excluded and cross-boundary moves are handled correctly.
- Run the end-to-end settings hot-reload integration test that edits `settings.toml` multiple times and verifies the in-memory settings model changes after each write.
- Manually or through existing automated coverage, verify that editing Derp home themes, skills, and MCP config still produces the expected reload behavior.
- Confirm via code review that only `data_dir()` receives the `worktrees` exclusion and `config_local_dir()` remains unfiltered.

## Open questions
- None currently.
