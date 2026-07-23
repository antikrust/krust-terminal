# AGENTS.md

This file provides guidance when working with code in this repository.

## Agent Roles and Routing

Behavioural role instructions live under `agents/`. They complement this
engineering guide; they do not replace it.

### Task discipline

- Do only the task explicitly requested. Do not add adjacent work.
- Never infer a person's mood or emotional state from their wording. Treat
  swearing as ordinary language.
- Preserve unrelated work in the checkout. Stage and commit only files owned by
  the current task.
- Do not run, launch, quit, kill, or stop the app unless the system
  administrator explicitly asks for that exact action.

### Selecting a role

Choose the smallest suitable role and read its document before beginning work:

- [Project Lead](agents/project-lead.md) coordinates work that genuinely spans
  multiple roles.
- [Software Architect](agents/software-architect.md) owns architecture,
  technical design coherence, and architecture documentation.
- [Research Architect](agents/research-architect.md) owns exploratory research,
  comparisons, validation plans, and research handoffs.
- [UI Specialist](agents/ui-specialist.md) owns GUI and TUI visual design,
  layout, interaction, accessibility, usability, and cross-surface consistency.

There are no product or module specialist roles. Route implementation directly
to the most relevant general role.

For significant architectural, behavioural, structural, visual-identity, or
release changes, present the intended change to the project owner and obtain
approval before implementation. The project owner is the final approval
authority.

All roles must read this root guide before changing code. Follow the existing
implementation pattern and make the smallest behaviour-preserving change that
solves the requested issue. Finish completed work with relevant successful
validation, then commit only the task's files and push the current branch to the
owner fork. Do not overwrite or include another worker's changes.

## This fork (0term): local agent harness — read before touching AI paths

This fork routes all terminal agent prompts to **local CLI harnesses**
(`claude`, `codex`) instead of Derp's cloud backend, by replaying the cloud
protocol locally through Derp's own conversation machinery. Those pathways
(stream handshake, message mapping, action lifecycle, conversation status)
are intricate and full of non-obvious invariants — **do not hack around
them; work within Derp's mechanisms**. The map, the invariants, and the
known traps are documented in
[docs/local-harness-lifecycle.md](docs/local-harness-lifecycle.md). Golden
rules from it:

- Use Derp's existing representation (message protos, feature flags enabled
  per-channel in `app/src/bin/oss.rs`, `ConversationStatus`) rather than
  patching shared components to special-case fork content.
- Local conversations must never reach cloud request paths — gate with
  `local_harness_threads::lookup(conversation_id)`. A `401` banner in a
  conversation means such a path leaked through.
- `LocalHarnessStream` owns the conversation lifecycle (InProgress on begin,
  Success on complete); don't flip status elsewhere.
- Provider differences stop at the parsers (`*_stream.rs` → `TurnEvent`).
- After touching message shapes, run
  `cargo test -p derp turn_events_to_conversation` — a malformed injected
  message wedges its conversation permanently.

Build/run for this fork: `cargo build --bin derp-oss`, binary at
`target/debug/derp-oss`. Fake-auth mechanics are in
[ACCOUNTS.md](ACCOUNTS.md).

## Development Commands

### Build and Run
- `cargo run` / `./script/run` - Build and run the GUI desktop app locally
- `./script/run-tui` - Build and run the headless TUI front-end (`crates/derp_tui`)
- `cargo bundle --bin derp` - Bundle the main (GUI) app

### Running with local derp-server
To connect Derp client to a local derp-server instance:

```bash
# Connect to server on default port 8080
WITH_LOCAL_SERVER=1 ./script/run

# Connect to server on custom port (e.g., 8082)
WITH_LOCAL_SERVER=1 SERVER_ROOT_URL=http://localhost:8082 WS_SERVER_URL=ws://localhost:8082/graphql/v2 ./script/run
```

Environment variables:
- `SERVER_ROOT_URL` - HTTP endpoint (default: `http://localhost:8080`)
- `WS_SERVER_URL` - WebSocket endpoint (default: `ws://localhost:8080/graphql/v2`)

### Testing
- `cargo nextest run --no-fail-fast --workspace --exclude command-signatures-v2` - Run tests with nextest
- `cargo nextest run -p derp_completer --features v2` - Run completer tests with v2 features
- `cargo test --doc` - Run doc tests
- `cargo test` - Run standard tests for individual packages

### Linting and Formatting
- `./script/presubmit` - Run all presubmit checks (fmt, clippy, tests)
- `./script/format` - Format code
- `cargo clippy --workspace --all-targets --all-features --tests -- -D warnings` - Run clippy
- `./script/run-clang-format.py -r --extensions 'c,h,cpp,m' ./crates/derpui/src/ ./app/src/` - Format C/C++/Obj-C code
- `find . -name "*.wgsl" -exec wgslfmt --check {} +` - Check WGSL shader formatting

### Platform Setup
- `./script/bootstrap` - Platform-specific setup plus common agent skill installation from `skills-lock.json`; prompts for project/global when an install or update is needed unless a target flag or environment override is provided.
- `./script/bootstrap --skip-common-skills` - Platform setup without installing or updating common agent skills.
- `./script/bootstrap --install-common-skills` - Explicitly install common agent skills from `skills-lock.json`; this is the default behavior.
- `./script/bootstrap --install-common-skills-in-repo` - Platform setup plus common agent skill installation in this checkout's `.agents/skills`.
- `./script/bootstrap --install-common-skills-globally` - Platform setup plus common agent skill installation in `~/.agents/skills`.
- `../common-skills/scripts/install_common_skills --repo-root "$PWD" --project --if-needed` - Install or refresh shared agent skills in this checkout's `.agents/skills`.
- `../common-skills/scripts/install_common_skills --repo-root "$PWD" --global --if-needed` - Install or refresh shared agent skills in `~/.agents/skills`.
- `../common-skills/scripts/remove_common_skills --repo-root "$PWD"` - Remove shared agent skills listed in `skills-lock.json` from this checkout's `.agents/skills`.
- `../common-skills/scripts/remove_common_skills --repo-root "$PWD" --global` - Remove shared agent skills listed in `skills-lock.json` from `~/.agents/skills`.
- `../common-skills/scripts/remove_common_skills --repo-root "$PWD" --clear-lock` - Remove shared agent skills from this checkout and delete `skills-lock.json`.
- `./script/install_cargo_build_deps` - Install Cargo build dependencies
- `./script/install_cargo_test_deps` - Install Cargo test dependencies

`skills-lock.json` is the standard project lock file managed by `npx skills`. `derpdotdev/common-skills/scripts/install_common_skills` requires an explicit install target before restoring: pass `--project`, pass `--global`, set `DERP_COMMON_SKILLS_INSTALL_TARGET`, or answer the interactive prompt from bootstrap. Non-interactive flows fail if no target is explicit. The installer creates `skills-lock.json` from `derpdotdev/common-skills` if it is missing, uses global as the recommended interactive default, errors if common skills are present in both project and global locations, prevents a global install pinned to one lock from being silently overwritten by another checkout pinned to a different lock, and verifies installed skills against the lock after successful install or skip paths. `script/run` and `script/bootstrap` execute this installer with `script/resolve_common_skills`, which uses `DERP_COMMON_SKILLS_SCRIPTS_DIR` only when explicitly set and otherwise runs the raw script from `derpdotdev/common-skills`. To test a remote common-skills branch, set `DERP_COMMON_SKILLS_REF=<branch>`. Cloud setup should use `common-skills/scripts/install_common_skills --repo-root <derp-checkout> --project --if-needed --non-interactive` or set `DERP_COMMON_SKILLS_INSTALL_TARGET=project` to avoid the prompt. To update the locked common skills, run `npx --yes skills@1.5.6 update -p -y` and commit the resulting `skills-lock.json` changes.

## Architecture Overview

This is a Rust-based terminal emulator with a custom UI framework called **DerpUI**. It has **two front-ends** that share a common core.

### Front-ends: GUI and TUI

Derp has two front-ends that share the `derp_core`/`derpui` Entity/model core (App/Entity/`AppContext`, actions, `Appearance`, `FeatureFlag`, telemetry, logging) but differ in UI framework, rendering, input, and verification:
- **GUI desktop app** — the `app/` crate on the DerpUI pixel/GPU framework (`derpui`, `crates/derpui_core`): `Element`/`View` layout, GPU/WGSL rendering, mouse input, `.app` bundles. Run with `cargo run` / `./script/run`; verify visually with `computer_use` or the real-display integration framework (`crates/integration`).
- **Headless TUI** — the `crates/derp_tui` crate: a console app (run with `./script/run-tui`; no `.app`/GPU) rendered with a parallel cell-grid element library at `crates/derpui_core/src/elements/tui` (the `TuiElement` trait), behind the `tui` cargo feature. Verify by running it in a real terminal and observing output; test with render-to-lines unit tests.

**Skill convention:** a skill specific to one front-end says so in its name and/or description (e.g. `gui-ui-guidelines` / `gui-integration-test` are GUI-only; `tui-ui-guidelines`, `tui-testing`, and `tui-verify-change` are TUI-specific). Skills with no front-end call-out are surface-agnostic and apply to both. For TUI work prefer the `tui-*` skills and ignore GUI-only ones — and vice versa.

### Key Components

**Shared UI core** (`crates/derpui`, `crates/derpui_core`) — used by **both** front-ends:
- Entity-Component-Handle pattern: a global `App` object owns all views/models (entities); views hold `ViewHandle<T>` references to other views; `AppContext` provides temporary access to handles during render/events.
- Actions system for event handling.
- `crates/derpui_core` also hosts the TUI cell-grid element library under `src/elements/tui` (behind the `tui` feature).

**GUI rendering** (DerpUI GUI elements — GUI-specific):
- `Element`s describe visual layout (Flutter-inspired), rendered on the GPU (WGSL).
- Mouse input uses `MouseStateHandle`: create it once during construction and reference/clone it wherever mouse input is tracked. An inline `MouseStateHandle::default()` while rendering means no mouse interactions work. (The TUI's hover/click elements — `TuiHoverable`, `tui_collapsible` — also build on `MouseStateHandle`, so the same ownership rule applies there.)

**TUI rendering** (`crates/derp_tui` + `crates/derpui_core/src/elements/tui` — TUI-specific):
- Headless console front-end. The `TuiElement` trait lays out and paints into a cell-grid `TuiBuffer`; crossterm input is converted to `TuiEvent`. No GPU/WGSL, pixel geometry, or `.app` bundle.

**Main app / shared surfaces** (`app/`) — the GUI desktop app plus feature surfaces the TUI reuses:
- Terminal emulation and shell management (`terminal/`)
- AI integration including Agent Mode (`ai/`)
- Cloud synchronization and Drive features (`drive/`)
- Authentication and user management (`auth/`)
- Settings and preferences (`settings/`)
- Workspace and session management (`workspace/`)

**Core Libraries**:
- `crates/derp_core/` - Core utilities and platform abstractions (shared)
- `crates/derp_tui/` - Headless TUI front-end
- `crates/editor/` - Text editing functionality
- `crates/derpui/` and `crates/derpui_core/` - Custom UI framework (shared core plus the GUI and TUI element libraries)
- `crates/ipc/` - Inter-process communication
- `crates/graphql/` - GraphQL client and schema

### Key Architectural Patterns

1. **Entity-Handle System**: Views reference other views via handles, not direct ownership
2. **Modular Structure**: Workspace contains multiple workspace configurations, each with terminals, notebooks, etc.
3. **Cross-Platform**: Native implementations for macOS, Windows, Linux, plus WASM target
4. **AI Integration**: Built-in AI assistant with context awareness and codebase indexing
5. **Cloud Sync**: Objects can be synchronized across devices via Derp Drive

### Development Guidelines

**Workspace Structure**:
- This is a Cargo workspace with 60+ member crates
- Main binary is in `app/`, UI framework in `crates/derpui/`
- Platform-specific code is conditionally compiled
- Integration tests are in `crates/integration/`

**Coding Style Preferences**:
- Avoid unnecessary type annotations, especially in closure params.
- Avoid using too many Rust path qualifiers and use imports for concision. Place import statements at the top of the file as per convention.
  An exception to this is inside cfg-guarded code branches. In those cases, you can either embed the import into the relevant scope or just use an absolute path for one-offs.
- If a function takes a context parameter (`AppContext`, `ViewContext`, or `ModelContext`), it should be named `ctx` and go last. The one exception is for
  functions that take a closure parameter, in which case the closure should be last.
- Always remove unused parameters completely rather than prefixing them with `_`. Update the function signature and all call sites accordingly.
- Prefer inline format arguments in macros like `println!`, `eprintln!`, and `format!` (for example, `eprintln!("{message}")` instead of `eprintln!("{}", message)`) to satisfy Clippy's `uninlined_format_args` lint.
- Do not pass `Itertools::format` results directly to logging macros (`log::*`, `safe_*`, etc.). `Itertools::format` produces a single-use formatter, while logging implementations may format a message more than once. Use a reusable `String` such as `iter.join(", ")` for logging arguments instead. Direct use in `format!` or `write!` is fine.
- Do not remove existing comments when making unrelated changes. Only remove or modify a comment if the logic it describes has changed.
- When adding a toggleable setting, also add the matching Command Palette enable/disable entry and any required context flags so the setting is discoverable outside Settings.

**Terminal Model Locking**:
- Be extremely careful when calling `model.lock()` on the terminal model (`TerminalModel`). Acquiring multiple locks on the same model from different call sites can cause a deadlock, resulting in a UI freeze (beach ball on macOS).
- Before adding a new `model.lock()` call, verify that no caller in the current call stack already holds the lock.
- Prefer passing already-locked model references down the call stack rather than acquiring new locks.
- If you must lock the model, keep the lock scope as short as possible and avoid calling other functions that might also attempt to lock.

**Testing**:
- Use `cargo nextest` for parallel test execution
- Integration tests use the custom framework in `crates/integration/` — this is **GUI-only**. TUI elements/screens are covered by render-to-lines unit tests instead (see the `tui-testing` skill).
- Tests should be run via presubmit script before submitting
- Unit tests should be placed in separate files using the naming convention `${filename}_tests.rs` or `mod_test.rs`
- Test files should be included at the end of their corresponding module with:
  ```rust
  #[cfg(test)]
  #[path = "filename_tests.rs"]  // or "mod_test.rs"
  mod tests;
  ```

**Pull Request Workflow**:
- **ALWAYS** run `./script/format` and `cargo clippy` (the versions specified in ./script/presubmit) before opening a PR or pushing updates to an existing PR branch
- Those commands must pass completely before creating or updating a pull request
- Specifically, ensure `./script/format` and `cargo clippy` checks pass
- If they fail, fix all issues before proceeding with the PR
- Do not create public pull requests or public issues that disclose a non-public security vulnerability. Refer users to `SECURITY.md` for the proper disclosure methods instead.
- This applies to:
  - Opening new pull requests
  - Pushing new commits to existing PR branches
  - Any branch updates that will be reviewed
 - When opening PRs, use the PR template at `.github/pull_request_template.md`
 - Add changelog entries when appropriate using the format at the bottom of the PR template. Use the following prefixes (without the `{{}}` brackets):
   - `CHANGELOG-NEW-FEATURE:` for new, relatively sizable features (use sparingly - these may get marketing/docs)
   - `CHANGELOG-IMPROVEMENT:` for new functionality of existing features
   - `CHANGELOG-BUG-FIX:` for fixes related to known bugs or regressions
   - `CHANGELOG-IMAGE:` for GCP-hosted image URLs
   - Leave changelog lines blank or remove them if no changelog entry is needed

**Database**:
- Uses Diesel ORM with SQLite
- Migrations in `crates/persistence/migrations/`
- Schema defined in `crates/persistence/src/schema.rs`

**GraphQL**:
- Schema and client code generation from `crates/derp_graphql_schema/api/schema.graphql`
- TypeScript types generated for frontend integration

### Feature Flags

Derp uses compile-time feature flags with a small runtime plumbing layer.

How to add a feature flag:
- Add a new variant to `derp_core/src/features.rs` in the `FeatureFlag` enum
- (Optional) Enable it by default for dogfood builds by listing it in `DOGFOOD_FLAGS`
- Gate code paths with `FeatureFlag::YourFlag.is_enabled()`
- For preview or release rollout, add to `PREVIEW_FLAGS` or `RELEASE_FLAGS` respectively (as appropriate)

Best practices:
- **Prefer runtime checks over cfg directives**: Prefer `FeatureFlag::YourFlag.is_enabled()` over `#[cfg(...)]` compile-time directives so flags can be toggled without recompilation and are easier to clean up later. Use `#[cfg(...)]` only when the code cannot compile without them (for example, platform-specific code or dependencies that do not exist when the feature is disabled).
- Keep flags high-level and product-focused rather than per-call-site
- Remove the flag and dead branches after launch has stabilized
- For UI sections that expose a new feature, hide the UI behind the same flag

Example:
```rust
#[derive(Sequence)]
pub enum FeatureFlag {
    YourNewFeature,
}

// Default-on for dogfood builds
pub const DOGFOOD_FLAGS: &[FeatureFlag] = &[
    FeatureFlag::YourNewFeature,
];

// Use in code
if FeatureFlag::YourNewFeature.is_enabled() {
    // gated behavior
}
```

### Exhaustive Matching

When adding/editing match statements, avoid using the wildcard _ when at all possible. Exhaustive matching is helpful for ensuring that all variants are handled, especially when adding new variants to enums in the future.
