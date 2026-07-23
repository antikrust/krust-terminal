# Third party CLI agents settings page — what each control actually does

_Settings → **AI** umbrella → **Third party CLI agents** subpage._

Same behavior-focused treatment as [DERP-AGENT.md](DERP-AGENT.md) and
[DERP-KNOWLEDGE.md](DERP-KNOWLEDGE.md).

## The short answer

This page is **not about Derp's AI at all** — it has **zero derp-server/agent
calls** (verified: no `server_api`, no `/ai/*`, no multi-agent stream anywhere in
the CLI-agent code path). It's a **purely local terminal-UI integration** that
wraps *other vendors'* CLI coding agents (Claude Code, Codex, Gemini CLI, Amp,
Droid, OpenCode, …). Those agents talk to **their own** backends
(Anthropic/OpenAI/Google); Derp just:

1. **detects** when you run one (by matching the command you typed),
2. draws a **toolbar/footer** with quick actions over that running block, and
3. optionally shows a **"Rich Input"** editor overlay for composing prompts
   (which needs a Derp **plugin** installed *into* the agent).

Every control here is a **local settings write**. The settings sync to cloud
(`SyncToCloud::Globally(Yes)`) purely for cross-device consistency — the behavior
they drive is entirely local. The only genuinely outbound things in this whole
vertical are the **plugin install** (writing plugin files into the agent's config
dir) and telemetry.

## Where this lives in the code

| Thing | Location |
|---|---|
| Subpage enum | `AISubpage::ThirdPartyCLIAgents` — [ai_page.rs:2882](app/src/settings_view/ai_page.rs) |
| The whole page | `CLIAgentWidget` — [ai_page.rs:7388](app/src/settings_view/ai_page.rs) |
| Supported agents | `CLIAgent` enum — [cli_agent.rs:140](app/src/terminal/cli_agent.rs) |
| Detection | `CLIAgent::detect` — [cli_agent.rs:361](app/src/terminal/cli_agent.rs) |
| Session tracking / Rich Input state | [cli_agent_sessions/mod.rs](app/src/terminal/cli_agent_sessions/mod.rs) |
| Toolbar/footer UI | [terminal/view/use_agent_footer/mod.rs](app/src/terminal/view/use_agent_footer/mod.rs) |
| Rich Input editor overlay | [terminal/input/cli_agent.rs](app/src/terminal/input/cli_agent.rs) |
| Derp plugin install/update | [cli_agent_sessions/plugin_manager/](app/src/terminal/cli_agent_sessions/plugin_manager/) |

## Enablement — independent of the global AI switch

Unlike every other AI subpage, this one is **not gated on global AI**. The code is
explicit ([ai_page.rs:7412](app/src/settings_view/ai_page.rs)):

> _"The Coding Agents section is always enabled, independent of the global AI
> toggle, because these settings control third-party coding agents (Claude Code,
> Codex, Gemini CLI) rather than Derp's own AI."_

So these controls work even with Derp Agent turned off.

## The runtime mechanism

**Detection** ([`CLIAgent::detect`](app/src/terminal/cli_agent.rs)): when you run a
command, Derp extracts its first word, resolves shell aliases, and matches it
against each agent's `command_prefix` (`claude`, `codex`, `gemini`, `amp`,
`droid`, `opencode`, `copilot`, …). Your own **regex patterns** (below) extend
this to arbitrary commands. On a match, Derp knows a CLI agent is running in that
block.

**Toolbar/footer** ([use_agent_footer](app/src/terminal/view/use_agent_footer/mod.rs)):
a bar at the bottom of the active long-running block. For a detected CLI agent it
shows quick-action chips — model selector, NLD toggle, context-window usage, file
explorer, Rich Input, voice input, file attach, share session, settings,
fast-forward, handoff-to-cloud (`AgentToolbarItemKind`).

**Rich Input** ([terminal/input/cli_agent.rs](app/src/terminal/input/cli_agent.rs)):
a proper editor overlay for typing a prompt to the agent instead of poking at the
raw PTY line — multi-line editing, configurable Enter behavior, themed. The
"auto show/hide based on agent status" mode needs the **Derp plugin** installed
into the agent (e.g. `derpdotdev/claude-code-derp` for Claude Code) so Derp can
observe when the agent is idle vs. working. The plugin manager version-checks and
can auto-install it.

---

## Control-by-control

### Show coding agent toolbar  (toggle)
- **Setting:** `should_render_cli_agent_footer` → `ToggleCLIAgentToolbar` (toml `agents.third_party.should_render_cli_agent_toolbar`, default **on**)
- **Render:** [ai_page.rs:7415](app/src/settings_view/ai_page.rs)
- **Effect:** master switch for the footer/toolbar. When on, running a detected
  CLI agent (or a command matching one of your patterns) shows the toolbar. All
  the settings below only appear when this is on.
- **Verdict:** local UI toggle.

### Rich Input settings  _(shown only when `FeatureFlag::CLIAgentRichInput` is on)_
Four local toggles ([ai_page.rs:7474](app/src/settings_view/ai_page.rs)), all
`agents.third_party.*`:

| Toggle | Setting (default) | Effect |
|---|---|---|
| **Auto show/hide Rich Input based on agent status** | `auto_toggle_rich_input` (**on**) | Opens/closes the Rich Input editor as the agent goes idle/busy. **Requires the Derp plugin** for the agent (tooltip says so). |
| **Auto open Rich Input when a coding agent session starts** | `auto_open_rich_input_on_cli_agent_start` (off) | Pops the editor open the moment a CLI-agent session is detected. |
| **Auto dismiss Rich Input after prompt submission** | `auto_dismiss_rich_input_after_submit` (off) | Closes the editor after you send a prompt. |
| **Submit Rich Input with Ctrl+Enter** | `submit_on_ctrl_enter` (off) | Enter inserts a newline; Ctrl+Enter submits (vs. Enter submitting). |

(Internally Rich Input is the "composer" — note the `composer` toml keys.)

### Commands that enable the toolbar  (regex list + per-command agent)
- **Setting:** `cli_agent_footer_enabled_commands` (a `ToolbarCommandMap`, toml `agents.third_party.cli_agent_toolbar_enabled_commands`)
- **Render:** [ai_page.rs:7544](app/src/settings_view/ai_page.rs)
- **Effect:** add **regex patterns** so the toolbar also shows for commands Derp
  wouldn't otherwise recognize as an agent. Each row has:
  - a **remove** button → `RemoveCLIAgentToolbarEnabledCommand`,
  - an **agent dropdown** → `SetCLIAgentForCommand { pattern, agent }`
    ([settings/ai.rs:2277](app/src/settings/ai.rs)) — tells Derp *which* agent a
    matching command is (so it uses the right plugin / chips), or `Unknown` for a
    generic match.
  - Add new patterns via the inline command editor.
- **Verdict:** local config map; drives detection.

### Toolbar layout editor  _(shown only when `FeatureFlag::AgentToolbarEditor` is on)_
- **Render:** `render_toolbar_layout_editor` — [ai_page.rs:7661](app/src/settings_view/ai_page.rs)
- **Effect:** rearrange the toolbar chips. The layout is `CLIAgentToolbarChipSelection`
  — `Default`, or `Custom { left, right }` arrangements of `AgentToolbarItemKind`.
- **Verdict:** local UI layout preference.

---

## Data / cloud view

| Aspect | Reality |
|---|---|
| Derp AI / derp-server calls | **None** — verified across the whole CLI-agent path |
| Where the CLI agent's requests go | To its **own vendor** (Anthropic/OpenAI/Google/…), not Derp |
| The settings on this page | Local writes; background-synced to derp-server for cross-device consistency (`SyncToCloud::Globally(Yes)`) |
| Genuinely outbound from this vertical | The **plugin install** (writes plugin files into the agent's config dir, e.g. `~/.claude`; may fetch the plugin) + telemetry |

**Contrast with the other pages:** Derp Agent and Knowledge flip flags that make
*derp-server* do AI work. This page does no AI of its own at all — it's terminal
UX layered over agents that belong to other vendors.
