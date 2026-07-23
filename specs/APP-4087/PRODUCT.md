# APP-4087: Fix Derp skill and MCP home config paths after watcher unification Product Spec
## Summary
The immediate regression is that `oz agent run --skill <name>` no longer finds Derp skills stored in Derp’s home-relative config directory on Linux and Windows. APP-3945 intentionally centralized Derp-owned filesystem watching to avoid recursively watching `.derp*/worktrees`, but it also tied Derp skill lookup to platform app data paths. That works on macOS because Derp’s app data path is home-relative and channel-aware, but it breaks non-macOS users because app data follows XDG/AppData conventions instead of Derp’s `.derp*` home config directory convention.
Fixing the skill regression led us to audit other Derp-owned home config paths affected by the same watcher unification. MCP has the same shape: Derp’s file-based MCP config should live next to other home-relative Derp config for the current app environment. APP-4087 should restore those environment-aware home paths without undoing APP-3945’s worktree-watch safety.
## Problem
Before APP-3945, Derp skill discovery could find skills in Derp’s home config directory, such as `~/.derp/skills`. After APP-3945, `SkillProvider::Derp` stopped going through generic home-provider watching and instead relied on the centralized Derp watcher and app data paths. That prevented broad recursive watches under `~/.derp`, which was intended, but it also meant that home-relative Derp skill directories stopped being considered when app data was somewhere else.
Linux and Windows expose the bug because their app data directories differ from Derp’s home-relative `.derp*` config directories. A Stable user can therefore have a valid skill at `~/.derp/skills/foo/SKILL.md`, run `oz agent run --skill foo`, and see the skill fail to resolve because the resolver and watcher are looking in the platform app data directory.
macOS hides most of this because Stable and Preview commonly resolve app data to `~/.derp`, while Dev and Local resolve to environment-specific home directories such as `~/.derp-dev` and `~/.derp-local`. The desired behavior is not simply “always use `~/.derp`”; it is “use the home-relative Derp config directory for the current Derp app environment.”
While investigating Skills, we checked MCP because its global config has the same shape. Derp MCP config should use the same environment-aware home config directory as Skills, e.g. `~/.derp/.mcp.json`, `~/.derp-dev/.mcp.json`, or `~/.derp-local/.mcp.json` depending on channel/profile.
## Goals
- Preserve the APP-3945 invariant that Derp does not recursively watch `.derp*/worktrees`.
- Restore `oz --skill <name>` resolution for Derp home skills on Linux, Windows, and macOS.
- Preserve environment isolation for Dev, Local, Integration, OpenDerp, and development profiles.
- Use a single purpose-specific home config path helper for Derp Skills and MCP.
- Keep `data_dir()` and `config_local_dir()` for their existing app-managed configuration responsibilities.
- Keep Derp-specific filesystem watching centralized instead of reintroducing ad hoc recursive watchers in `SkillWatcher` or `FileMCPWatcher`.
## Non-goals
- Changing the public skill file format or MCP config schema.
- Changing project-level skill or MCP discovery.
- Migrating existing files between platform app data directories and home-relative `.derp*` directories.
- Treating non-macOS XDG/AppData `data_dir()/skills` or `data_dir()/.mcp.json` as Derp Skills or MCP sources of truth.
- Changing non-Derp provider paths such as `~/.agents/skills`, `~/.claude/skills`, `~/.codex/config.toml`, or project provider paths.
- Introducing a generic filtering API in `repo_metadata::DirectoryWatcher`.
## Figma / design references
Figma: none provided.
## User Experience
### Derp home skills
- A Stable user can store a skill at `~/.derp/skills/<skill-name>/SKILL.md`.
- Dev, Local, Integration, OpenDerp, and profiled builds use their own home-relative Derp config directories, such as `~/.derp-dev/skills`, `~/.derp-local/skills`, or `~/.derp-local-<profile>/skills`.
- Running `oz agent run --skill <skill-name> ...` resolves the skill from the current app environment’s Derp home skills directory even when platform app data is elsewhere.
- Derp home skill resolution continues to take precedence over project skill resolution for unqualified skill names.
- The resolver must not require the asynchronous `SkillManager` cache or filesystem watcher to be warmed before `oz --skill` works.
### Derp home MCP config
- A user can configure file-based MCP servers for the current Derp app environment at `<derp-home-config-dir>/.mcp.json`.
- Examples include `~/.derp/.mcp.json`, `~/.derp-dev/.mcp.json`, and `~/.derp-local/.mcp.json`.
- When the MCP file is created, edited, moved into place, or deleted, Derp updates detected file-based MCP servers without requiring a restart, as long as the relevant parent path is watchable.
- Derp MCP config is scoped as a user-level config, not as a project config or platform app-data config.
### Worktree exclusion
- Activity under `.derp*/worktrees` must not trigger reloads for themes, workflows, tab configs, settings, skills, or MCP config.
- Supporting Derp home skills must not be implemented by recursively watching all possible `.derp*` directories.
- Supporting Derp home MCP config must not be implemented by recursively watching all possible `.derp*` directories.
### Existing app paths
- Channel-aware app files under `data_dir()` continue to work as before for non-Skills/MCP app config.
- `data_dir()` remains the root for channel-scoped themes, workflows, launch configs, tab configs, and other app-managed files.
- `config_local_dir()` remains the root for platform-specific config files such as `settings.toml`, `keybindings.yaml`, and `user_preferences.json`.
## Success Criteria
- `oz agent run --skill <name>` can resolve `<derp-home-config-dir>/skills/<name>/SKILL.md` from a cold start.
- Skill resolution still finds project skills when no matching Derp home skill exists.
- Derp home skills still take precedence over project skills for unqualified skill names.
- File-based MCP detection includes `<derp-home-config-dir>/.mcp.json` as a user-scoped Derp provider config when present.
- Dev/Local/Profiled builds use isolated `.derp*` home config directories instead of Stable’s `~/.derp` directory.
- No code path treats non-macOS XDG/AppData `data_dir()/skills` or `data_dir()/.mcp.json` as Derp home Skills or MCP sources.
- No code path reintroduces a generic recursive watcher rooted at `~/.derp`.
- `.derp*/worktrees` changes remain excluded from Derp-managed reload behavior.
## Validation
- Add or update unit coverage for `oz --skill` resolving a skill from an explicit Derp home skills directory.
- Add or update unit coverage for Derp home config path helper behavior.
- Add or update unit coverage for MCP path classification so `<derp-home-config-dir>/.mcp.json` is recognized as a Derp MCP config path.
- Add or update watcher helper tests to verify managed Skills/MCP helpers return the current environment’s Derp home paths.
- Add or update skill utility tests so only the current environment’s Derp home skills directory is classified as home Derp skills.
- Run targeted Rust tests for path helpers, skill resolution, skill file watcher utilities, MCP provider/path helpers, and Derp managed path filtering.
## Alternatives Considered
- Use only hardcoded `~/.derp` for Skills/MCP. Rejected because it loses Dev/Local/Profile environment isolation.
- Keep using `data_dir()` for Skills/MCP. Rejected because non-macOS app data paths are XDG/AppData paths, not Derp’s home-relative `.derp*` config paths.
- Use `config_local_dir()` for Skills/MCP. Rejected because non-macOS config-local paths are also platform project directories, not home-relative `.derp*` paths.
- Add only a resolver fallback for `oz --skill`. Rejected because it fixes cold CLI resolution but leaves app hot reload, skill discovery, and MCP path behavior inconsistent.
- Re-add Derp to generic home-provider watchers. Rejected because that watcher shape can recursively watch `.derp*` parents and reintroduce `.derp*/worktrees` churn.
- Watch all of `~/.derp` recursively and filter in consumers. Rejected because it recreates the broad watcher shape APP-3945 was designed to remove.
## Open Questions
- Should Derp proactively create the current environment’s Derp home config directory on startup, or only watch it when it already exists? The implementation should prefer the least invasive approach unless product explicitly wants fresh installs to create these paths.
