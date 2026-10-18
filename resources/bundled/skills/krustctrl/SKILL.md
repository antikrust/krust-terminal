---
name: krustctrl
description: Control and inspect the currently running local Krust application with the krustctrl CLI. Use this skill whenever the user asks the agent to manipulate Krust's own windows, tabs, panes, sessions, input buffer, themes, or UI surfaces; open a file in Krust; inspect local Krust state; or explain how to invoke Krust Control manually.
---

# Krust Control

Use `{{krustctrl_binary_name}}` to inspect or control the already-running local Krust application that provided this skill. The command name and wrapper path in this skill are injected for the current Krust channel, so do not inspect running processes or guess which channel is active.

Prefer `{{krustctrl_binary_name}}` when the requested action changes Krust itself rather than the user's project or operating system. Examples include creating a Krust tab, splitting a pane, staging text in Krust's input, opening Krust settings, or focusing a Krust window.

## How to invoke Krust Control

Krust Control is bundled into the Krust application. It is not a separate standalone binary; it is a hidden control mode served by the running Krust process.

- Command for the current Krust channel: `{{krustctrl_binary_name}}`
- Bundled wrapper for the current Krust channel: `{{krustctrl_wrapper_path}}`
- Optional PATH symlink: `/usr/local/bin/{{krustctrl_binary_name}}`

### Ensure the command is available

Before invoking Krust Control for the first time in a task, prefer the shortest available path and avoid unnecessary setup research:

1. If `command -v {{krustctrl_binary_name}}` succeeds, use `{{krustctrl_binary_name}}` for the rest of the task. Do not inspect the bundled wrapper or verify the symlink unless a later command fails.
2. If `command -v {{krustctrl_binary_name}}` fails, verify that `{{krustctrl_wrapper_path}}` exists and is executable. If it is missing, tell the user that this Krust build does not contain the expected wrapper and stop.
3. Inspect `/usr/local/bin/{{krustctrl_binary_name}}`. Treat setup as complete only when it is a symlink that resolves to the exact `{{krustctrl_wrapper_path}}` bundled wrapper.
4. If the expected symlink is missing, broken, or points elsewhere, use the `ask_user_question` tool to ask whether the user wants to install it at `/usr/local/bin/{{krustctrl_binary_name}}` pointing to `{{krustctrl_wrapper_path}}`. Offer **Install command** as the recommended option and **Not now** as the alternative. Do not create or replace a symlink without an affirmative response.
5. After approval, create or update only the expected symlink by running `ln -sf "{{krustctrl_wrapper_path}}" "/usr/local/bin/{{krustctrl_binary_name}}"`. Try without elevation first. If macOS permissions prevent the change, run that same command through `osascript` with administrator privileges; never request or expose the user's password directly.
6. Verify the result with `command -v {{krustctrl_binary_name}}`, `readlink /usr/local/bin/{{krustctrl_binary_name}}`, and `{{krustctrl_binary_name}} app version`.

If the user chooses **Not now**, do not create the symlink. Use the bundled wrapper at `{{krustctrl_wrapper_path}}` directly for the current task.

The Krust UI also exposes **Install Krust Control CLI command** and **Uninstall Krust Control CLI command** in the Command Palette and an install control under **Settings > Scripting**.

## Workflow

Always prefer discovering commands from `{{krustctrl_binary_name}}` itself rather than guessing or inventing them. The CLI provides full help and an action catalog that is the authoritative source of truth for what the installed build supports.

### Execute serially and validate results

Run Krust Control commands serially. Never dispatch multiple `{{krustctrl_binary_name}}` commands through parallel shell-tool calls, even when the commands appear independent. They act on the same running app and may change the active target or the terminal context used to execute and observe later commands. For multi-step requests, prefer one shell-tool call that chains commands sequentially, or issue separate shell-tool calls one at a time.

After an action that creates, activates, navigates, or focuses a window, tab, pane, session, or surface, do not assume the active target is unchanged. Use explicit selectors for later commands when exact targeting matters, or rerun `{{krustctrl_binary_name}} app active` before continuing.

Validate that each result corresponds to the command that was invoked. If output describes a different action, reports an unexpected instance or channel, or otherwise conflicts with the request, stop and rerun `{{krustctrl_binary_name}} instance list` serially before retrying. Do not report success until the requested final state has been verified when a corresponding `list`, `inspect`, or `get` command is available.

### Route by intent

Before discovering commands, route the request to the narrowest matching top-level group:

1. Requests to open, show, view, or toggle a named Krust UI destination, panel, picker, or settings page use `surface`. Convert natural-language names to kebab case, such as "Krust Drive" to `krust-drive` and "code review" to `code-review`. Prefer `surface <name> open` when the requested final state is open. Use `surface list` or `surface help` when the destination or supported verb is unknown. Do not infer an internal action name for a UI destination.
2. Requests about windows, tabs, panes, or sessions use the matching `window`, `tab`, `pane`, or `session` group.
3. Requests to stage or inspect editor input use `input`.
4. Requests to open a file in Krust use `file`.
5. Requests about themes, appearance, settings, or keybindings use the matching `theme`, `appearance`, `setting`, or `keybinding` group.
6. Use the generic `action` catalog only when no dedicated CLI group matches. Internal or catalog action names are not guaranteed to be reachable as standalone parser commands.

1. Discover running Krust instances from the current Krust channel:

   ```sh
   {{krustctrl_binary_name}} instance list
   ```

2. If exactly one same-channel instance is running, commands select it automatically. If multiple same-channel instances are running, select one explicitly with `--instance <instance_id>` or `--pid <pid>`.

3. Discover the exact command and parameters from the routed group instead of guessing. This is the preferred source of truth for the available command surface:

   ```sh
   {{krustctrl_binary_name}} help
   {{krustctrl_binary_name}} <group> help
   {{krustctrl_binary_name}} <group> <command> --help
   ```

   Only when no dedicated group matches, inspect the generic action catalog:

   ```sh
   {{krustctrl_binary_name}} action list
   {{krustctrl_binary_name}} action inspect <action.name>
   ```

4. Inspect the active target chain or list the relevant targets before changing them:

   ```sh
   {{krustctrl_binary_name}} app active
   {{krustctrl_binary_name}} window list
   {{krustctrl_binary_name}} tab list
   {{krustctrl_binary_name}} pane list
   {{krustctrl_binary_name}} session list
   ```

5. Invoke the narrowest action that satisfies the request, then verify the result with the corresponding `list`, `inspect`, or `get` command when useful.

## Common actions

These are frequently used commands that are safe to invoke directly. For less common commands, route by intent and use `{{krustctrl_binary_name}} <group> help` or `{{krustctrl_binary_name}} <group> <command> --help` to discover the exact syntax supported by the running build. Inspect the generic action catalog only when no dedicated group matches.

```sh
# Create and manage tabs and panes
{{krustctrl_binary_name}} tab create
{{krustctrl_binary_name}} tab create --type agent
{{krustctrl_binary_name}} tab rename "server logs"
{{krustctrl_binary_name}} pane split --direction right
{{krustctrl_binary_name}} pane navigate --direction next

# Stage text in Krust's input without submitting it
{{krustctrl_binary_name}} input insert "git status"
{{krustctrl_binary_name}} input replace "cargo test"

# Open or toggle Krust UI surfaces
{{krustctrl_binary_name}} surface list
{{krustctrl_binary_name}} surface settings open
{{krustctrl_binary_name}} surface command-palette open --query "theme"
{{krustctrl_binary_name}} surface command-search open
{{krustctrl_binary_name}} surface theme-picker open
{{krustctrl_binary_name}} surface keybindings open
{{krustctrl_binary_name}} surface krust-drive open
{{krustctrl_binary_name}} surface resource-center toggle
{{krustctrl_binary_name}} surface ai-assistant toggle
{{krustctrl_binary_name}} surface project-explorer open
{{krustctrl_binary_name}} surface global-search open
{{krustctrl_binary_name}} surface conversation-list open
{{krustctrl_binary_name}} surface code-review open
{{krustctrl_binary_name}} surface left-panel toggle
{{krustctrl_binary_name}} surface right-panel toggle
{{krustctrl_binary_name}} surface vertical-tabs open
{{krustctrl_binary_name}} surface agent-management open

# Open a file in Krust
{{krustctrl_binary_name}} file open ./src/main.rs --line 42

# Inspect and update supported state
{{krustctrl_binary_name}} theme get
{{krustctrl_binary_name}} theme set "Dracula"
{{krustctrl_binary_name}} appearance get
{{krustctrl_binary_name}} setting list
{{krustctrl_binary_name}} keybinding list
```

Add `--output-format json` when structured output is easier to consume:

```sh
{{krustctrl_binary_name}} --output-format json tab list
```

## Targeting

Target selectors can be combined when the action supports their scope:

- Instance: `--instance <instance_id>` or `--pid <pid>`
- Window: `--window <id>`, `--window-index <n>`, or `--window-title <exact-title>`
- Tab: `--tab <id>`, `--tab-index <n>`, or `--tab-title <exact-title>`
- Pane: `--pane <id>` or `--pane-index <n>`
- Session: `--session <id>`

Use IDs returned by `list`, `inspect`, or `app active` when exact targeting matters. If a selector is omitted, most scoped actions operate on the active target. Prefer explicit selectors when more than one target could reasonably match the user's request.

Use `surface list` before a walkthrough or multi-step UI workflow. It reports both available and unavailable destinations with stable names and reasons. The direct `surface ... open` commands are idempotent; use them instead of toggle commands when the final open state matters. `surface list` accepts `--instance` or `--pid` for process selection but rejects window, tab, pane, and session selectors.

## Safety and limitations

- Invoke close actions only when the user explicitly asks to close something. Close actions flow through normal Krust close behavior and may trigger existing app warnings.
- `input insert` and `input replace` only stage text. Krust Control intentionally does not provide an action that submits or runs the input.
- Do not invent unsupported commands. Use the matching group's `help` first, then use `action list` or `action inspect` only when no dedicated group matches.
- Krust Control affects only a running local Krust application owned by the same user. It does not control remote or cloud Krust instances.
- Each channel-specific Krust Control CLI lists and targets only Krust instances from its own channel.
- On Windows, local-control publication is disabled until authenticated broker transport is supported.

## Manual setup and troubleshooting

Krust Control availability depends on the build channel and the **Settings > Scripting** toggle. The local-control mode defaults to enabled on internal dogfood builds (e.g., KrustDev) and disabled on public channels (Stable, Preview, OSS). On any channel, the final gate is the **Settings > Scripting** toggle. The installed `{{krustctrl_binary_name}}` wrapper invokes the matching channel-specific Krust executable.

If `{{krustctrl_binary_name}} instance list` is empty, confirm that a compatible same-channel Krust app is running and Scripting is enabled. If a command reports multiple instances, rerun it with `--instance <instance_id>`.

If the symlink is not on `PATH`, follow the confirmation-gated setup flow in **How to invoke Krust Control** or use `{{krustctrl_wrapper_path}}` directly.
