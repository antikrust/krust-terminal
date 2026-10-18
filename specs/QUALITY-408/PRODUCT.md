# Separate Preview Config Directory on macOS

## Summary

On macOS, give the Preview channel its own config directory (`~/.krust-preview`) instead of sharing `~/.krust` with Stable. On first launch, migrate existing config by symlinking each top-level entry from `~/.krust` into `~/.krust-preview`, so existing Preview users keep all their customizations.

## Problem

Stable and Preview currently share the `~/.krust` directory on macOS. This means:

- Settings changes made in one channel affect the other.
- Preview experiments (themes, keybindings, tab configs, etc.) can break the Stable experience.
- There is no safe way to test config-level changes on Preview without risking Stable user data.

Other channels (Dev, Integration, Local) already use separate directories (`.krust-dev`, `.krust-integration`, `.krust-local`). Preview is the only non-Stable channel that still shares with Stable.

## Goals

1. Preview uses `~/.krust-preview` as its config directory on macOS.
2. Existing Preview users retain all their customizations after the change.
3. Stable users see no change whatsoever.
4. The migration is automatic and requires no user action.

## Non-goals

- Changing behavior on Linux or Windows (these platforms already use separate directories via `ProjectDirs`).
- Migrating the SQLite database (already stored separately in `~/Library/Application Support/` or the App Group container).
- Changing Dev, Integration, or Local channel directories.
- Achieving full bidirectional sync between Stable and Preview configs going forward.

## Figma

Figma: none needed (no UI changes).

## User Experience

### First launch after upgrade (Preview channel)

When a Preview user launches Krust for the first time after this change:

1. Krust detects that `~/.krust-preview` does not exist.
2. Krust creates `~/.krust-preview`.
3. For each top-level entry in `~/.krust` (files and directories), Krust creates a symbolic link in `~/.krust-preview` pointing to the corresponding entry in `~/.krust`.
4. Preview proceeds using `~/.krust-preview` as its config directory.

After migration, the directory looks like:

```
~/.krust-preview/
  keybindings.yaml  -> ~/.krust/keybindings.yaml
  themes/           -> ~/.krust/themes/
  workflows/        -> ~/.krust/workflows/
  tab_configs/      -> ~/.krust/tab_configs/
  ...etc
```

### Subsequent launches (Preview)

No migration occurs. Preview reads and writes from `~/.krust-preview`.

### Stable users

Zero change. Stable continues to use `~/.krust`.

### settings.toml is not symlinked

The `settings.toml` file (used by the Settings File feature) is intentionally kept separate between Stable and Preview. The migration uses an explicit exclude list (`MIGRATION_EXCLUDED_FILES`) to skip `settings.toml` during symlinking. This handles the case where a user runs Stable first (which creates `settings.toml` in `~/.krust`) and later updates Preview — without the filter, the migration would symlink the file, causing both channels to share settings.

### Behavior after migration

- **Reading config**: Preview reads through symlinks, so existing Stable config is visible.
- **Modifying a shared file**: If Preview modifies a symlinked file (e.g., `keybindings.yaml`), the change affects Stable too, since the symlink points to the same file. This is the intended tradeoff — users who customize on Preview are likely the same user customizing on Stable.
- **Creating new entries in a symlinked directory**: If Preview creates a new file inside a symlinked directory (e.g., a new tab config in `tab_configs/`), the file is created in `~/.krust/tab_configs/` because the directory symlink resolves there. This is an acceptable limitation of the symlink approach.
- **Breaking the symlink**: If the user wants full independence for a specific config, they can delete the symlink in `~/.krust-preview/` and replace it with a real file or directory. Preview will use the local copy from that point on.

### Edge cases

- **`~/.krust` does not exist**: No migration needed. Preview creates `~/.krust-preview` fresh, and Krust's existing directory-creation logic populates it.
- **`~/.krust-preview` already exists**: Migration is skipped entirely. This handles the case where the user has manually created the directory or is launching for a second time.
- **Entries in `~/.krust` are themselves symlinks** (e.g., `keybindings.yaml -> ~/dotfiles/...`): The migration creates a symlink to the symlink. The chain resolves correctly. No special handling needed.
- **`.DS_Store` and hidden OS files**: These should be skipped during migration to avoid unnecessary clutter.
- **Simultaneous Stable and Preview launch**: The directory creation (`create_dir`) fails atomically if the directory already exists, so concurrent launches are safe. The migration will run in one process and be skipped in the other.
- **Permissions errors**: If symlink creation fails for a specific entry, log a warning and continue with the remaining entries. Partial migration is better than no migration.

## Alternatives Considered

### 1. Full copy instead of symlinks
Copy all files and directories from `~/.krust` to `~/.krust-preview`.

- Pro: Full independence from day one.
- Con: Doubles disk usage for themes, workflows, etc. Changes in Stable no longer propagate to Preview, which could confuse users who expect their config to be shared.

### 2. Symlink the entire directory
Create `~/.krust-preview` as a symlink to `~/.krust`.

- Pro: Simplest implementation.
- Con: Not actually a separate directory. Any new entries created by Preview (e.g., new directories Krust adds in future versions) would appear in `~/.krust`. Defeats the purpose of separation.

### 3. No migration (clean start)
Simply start using `~/.krust-preview` with no migration.

- Pro: Simplest. No migration code.
- Con: Users lose all custom keybindings, themes, workflows, launch configs, tab configs, and settings. This is a bad experience.

### 4. Deep symlink (per-file in each subdirectory)
For each directory like `themes/`, create the directory in `.krust-preview` and symlink each individual file.

- Pro: New files created inside directories stay in `.krust-preview` rather than leaking into `.krust`.
- Con: Significantly more complex. The improvement is marginal — most users don't create new config files frequently, and those who do are likely sophisticated enough to understand the symlink behavior.

**Decision**: Symlink top-level contents (option in the main proposal) is the best balance of simplicity, backwards compatibility, and user experience.

## Success Criteria

1. After upgrading, a Preview user's keybindings, themes, workflows, launch configs, tab configs, MCP config, and settings are all accessible in Preview without manual action.
2. A Stable user's `~/.krust` directory is unchanged after the upgrade.
3. `~/.krust-preview` is created on first launch of Preview and contains symlinks to each top-level entry in `~/.krust`.
4. If `~/.krust` does not exist, `~/.krust-preview` is created empty (normal startup logic applies).
5. If `~/.krust-preview` already exists, no migration runs.
6. The migration only runs on macOS.
7. Partial migration failures (e.g., one symlink fails) do not prevent Krust from launching.

## Validation

- **Manual testing on macOS Preview build**:
  - Fresh install with no `~/.krust`: Preview creates `~/.krust-preview`, normal setup.
  - Existing `~/.krust` with custom keybindings/themes: After upgrade, verify symlinks exist and config loads correctly.
  - Launch Stable after Preview migration: Stable still uses `~/.krust` and is unaffected.
- **Integration test**: Verify the migration function creates expected symlinks given a mock `~/.krust` directory.
