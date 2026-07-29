# APP-3945: Channel-aware Bork home watching Product Spec

## Summary
Bork should hot-reload the current channel's Bork-managed files without reacting to unrelated files under `.bork*/worktrees`. This includes continuing to reload `settings.toml` correctly on platforms where settings live under `config_local_dir()` instead of `data_dir()`.

## Problem
Bork currently relies on filesystem watching for several user-visible behaviors: reloading themes, workflows, launch configs, tab configs, Bork home MCP config, Bork home skills, and public settings from `settings.toml`. The watcher surface is easy to regress because Bork-managed files are split across different directories depending on platform and channel.

The specific failure modes this work addresses are:
- changes under `.bork*/worktrees` can produce false-positive updates for Bork home watchers
- a watcher rooted only at `data_dir()` can miss `settings.toml` on Linux and Windows, where `config_local_dir()` differs from `data_dir()`
- fresh installs or hermetic test environments can fail to watch missing directories unless Bork prepares those roots before registering the watcher

## Goals
- Watch the current channel's Bork-managed directories through a single Bork-specific watcher model.
- Ignore filesystem activity under `.bork*/worktrees` so worktree contents do not trigger Bork home reload behavior.
- Continue reloading `settings.toml` when it changes on every supported platform, including platforms where settings live outside `data_dir()`.
- Preserve existing hot-reload behavior for themes, workflows, launch configs, tab configs, Bork home MCP config, and Bork home skills.

## Non-goals
- Changing where any Bork-managed file is stored.
- Changing the semantics of settings parsing, settings migration, or settings validation.
- Adding new user-facing UI for watcher state or diagnostics.
- Expanding watch coverage to arbitrary files outside Bork-managed directories.
- Changing the generic repository watcher APIs used for project repositories.

## Figma / design references
Figma: none provided

## User Experience

### Watch scope
- Bork watches the current channel's Bork-owned filesystem roots through a single singleton watcher.
- `data_dir()` remains the source of truth for channel-scoped Bork home content such as themes, workflows, launch configs, tab configs, MCP config, and skills.
- `config_local_dir()` is also watched when it is a different directory from `data_dir()`.
- When both path helpers resolve to the same directory, Bork behaves as before and does not create duplicate logical coverage.

### Settings hot reload
- When `settings.toml` changes, Bork reloads public settings from disk and applies the new values to in-memory settings models.
- This behavior must work whether `settings.toml` lives in the same directory as the rest of Bork home files or in a separate config directory.
- Creating, modifying, renaming into place, or deleting `settings.toml` must continue to flow through the existing `BorkConfigUpdateEvent::Settings` path.

### Worktree exclusion
- Files under `.bork`, `.bork-dev`, `.bork-local`, or equivalent channel-scoped Bork home directories that are nested inside `worktrees/` must not trigger Bork home reload behavior.
- Editing files inside a cloned repository stored under `.bork*/worktrees/...` must not cause Bork to reload themes, workflows, tab configs, MCP config, skills, or settings.

### Channel awareness
- Bork only reacts to files under the active channel's directories.
- A stable or dev install should not reload in response to files written into another channel's Bork home.

### Fresh-install and test-environment behavior
- If a watched Bork-owned root directory does not exist yet, Bork should create it during startup/setup before registering the watcher.
- Missing directories must not silently disable hot reload for the rest of the session.

### No regressions for existing consumers
- Editing a theme file in Bork home still updates the available theme set.
- Editing workflows, launch configs, or tab configs in Bork home still refreshes those objects.
- Editing Bork home MCP config still updates file-based MCP servers.
- Editing Bork home skills still refreshes Bork-provided skills.

## Success Criteria
- `settings.toml` hot reload works on macOS, Linux, and Windows.
- Worktree activity under `.bork*/worktrees` no longer triggers Bork home reloads.
- Themes, workflows, launch configs, tab configs, Bork MCP config, and Bork skills continue to hot reload from the current channel's Bork home.
- Bork prepares missing watch roots before attempting to register watchers.
- The watcher architecture remains centralized behind a Bork-specific singleton instead of reintroducing separate ad hoc watchers for individual consumers.

## Validation
- Unit-test the watcher filtering behavior so updates outside the kept prefix are excluded and cross-boundary moves are handled correctly.
- Run the end-to-end settings hot-reload integration test that edits `settings.toml` multiple times and verifies the in-memory settings model changes after each write.
- Manually or through existing automated coverage, verify that editing Bork home themes, skills, and MCP config still produces the expected reload behavior.
- Confirm via code review that only `data_dir()` receives the `worktrees` exclusion and `config_local_dir()` remains unfiltered.

## Open questions
- None currently.
