# Agent flow map: routing terminal prompts to the local Claude Code harness

This document maps how a prompt typed into Krust's terminal agent becomes an
agent run, why it currently hits Krust's cloud (the `401 Unauthorized: User not
in context`), where the local **Claude Code streaming harness** (this fork's
addition) plugs in, and the concrete plug points to (a) route terminal prompts
to the local harness, (b) repurpose the model picker for Claude Code + reasoning
level, and (c) give the user a `/login` path.

It is a point-in-time snapshot of the code, not a durable guarantee; verify
`file:line` references before acting on them.

> **Note:** the routing described here has since landed. The current
> end-to-end pipeline, its invariants, and the editing rules live in
> [local-harness-lifecycle.md](local-harness-lifecycle.md) — read that first;
> this file remains as the original map and rationale.

---

## TL;DR

- The `/agent <prompt>` you type in the terminal runs Krust's **cloud "Oz"
  agent**. It has no harness switch on its send path — it always calls Krust's
  multi-agent backend, which 401s under this fork's faked auth.
- The local Claude Code harness (parse → stream → inline render, already built
  and tested) is **fully wired end-to-end inside the `AgentDriver` path** and
  **is** the live target of `harness_kind(Harness::Claude)`. What's missing is
  any route from terminal input into that path — today it's only reachable via
  the `krust agent run` CLI subcommand.
- The **only mandatory cloud gate** on the local Claude path is one
  `is_logged_in()` check in `AgentDriver::new` ([driver.rs:667](../app/src/ai/agent_sdk/driver.rs)).
  Everything else on that path is Oz-only, task-id-conditional, or best-effort.
- `model_id` already reaches `claude` via the `ANTHROPIC_MODEL` env var;
  `reasoning_level` is dropped everywhere and needs wiring.
- Claude Code auth is a **separate interactive step** (`claude` OAuth `/login`,
  `setup-token`, or `ANTHROPIC_API_KEY`) that persists creds to `~/.claude`;
  headless `claude --print` runs inherit them. So `/login` = "run interactive
  `claude` once in a pane", not something we pipe into the stream.

---

## The two paths

There are two completely separate agent execution paths. The terminal UI uses
the first; our harness lives in the second.

### Path A — Oz cloud (what `/agent hello` uses today, the 401)

```
terminal input submit
  input.rs: submit_ai_query_local (14188)
    → controller.send_user_query_in_new_conversation (input.rs:14298)
  controller.rs: send_user_query_in_new_conversation (943)
    → _internal (987) → send_query (646) → send_request_input (2355)
    → ResponseStream::new (2493)
  response_stream.rs: new (175) → spawn_request (185) → spawn_generate (330)
    → generate_multi_agent_output(server_api, params, …)   ← line 356
  agent/api/impl.rs: generate_multi_agent_output (14)       ← cloud SSE → 401
```

The controller is **Oz-only on the send path**; there is no harness dispatch
here. The model chosen in the picker is stamped onto `RequestParams.model`
([controller.rs:2470](../app/src/ai/blocklist/controller.rs)) and sent to the
cloud. This whole path requires a real Krust account and cannot work offline.

### Path B — the harness driver (where the local Claude harness runs)

```
`krust agent run --harness claude …`  (CLI subcommand only, today)
  lib.rs:2913 → agent_sdk::run (mod.rs:127) → dispatch_command (139)
    → run_agent (252) → AgentCommand::Run (258)
  build_server_side_task: harness = harness_kind(args.harness)  (mod.rs:457/528)
  harness/mod.rs: harness_kind(Harness::Claude) (260)
    → HarnessKind::ThirdParty(ClaudeStreamHarness)
  create_and_run_driver (mod.rs:1467) → AgentDriver::new (1479)
    → AgentDriver::new login gate: Err(NotLoggedIn) if !is_logged_in  ← driver.rs:667
  driver.run (853) → run_internal (1994) → build_runner (2736)
  claude_stream_runner.rs: ClaudeStreamHarness::build_runner (76)
    → ClaudeStreamHarnessRunner::start (160)
      → run_claude_stream (claude_stream_exec.rs:65)
          claude --print --output-format stream-json --verbose
                 --session-id <uuid> --dangerously-skip-permissions
        → parse_stream_json_line (claude_stream.rs:22) → TurnEvent
      → LocalHarnessStream begin/apply/complete (local_harness_stream.rs:45/100/125)
        → inline AI blocks in Krust's own UI
```

**The gap:** terminal input (Path A) never enters Path B. The terminal's
harness + model selection lives on `AmbientAgentViewModel`
([terminal/view/ambient_agent/model.rs](../app/src/terminal/view/ambient_agent/model.rs))
and is surfaced only in **cloud-mode v2 / ambient compose** UI, feeding cloud
spawns — not the local `/agent` conversation. `is_local_to_cloud_handoff()`
even forces `selected_harness()` to `Harness::Oz`
([model.rs:497](../app/src/terminal/view/ambient_agent/model.rs)).

> Note: the doc-comment at the top of
> [claude_stream_runner.rs](../app/src/ai/agent_sdk/driver/harness/claude_stream_runner.rs)
> claiming the harness "is not yet reachable / dispatch still points at the
> interactive harness" is **stale** — the dispatch is flipped; this harness is
> live. (Fixed separately.)

---

## Cloud / auth dependencies on the local Claude path

Every touchpoint on `AgentDriver → ClaudeStreamHarness`, and whether it blocks a
purely-local run:

| # | Touchpoint | Location | Blocks local run? |
|---|---|---|---|
| 1 | **`AgentDriver::new` login gate** (`Err(NotLoggedIn)`) | driver.rs:667 | **HARD BLOCK** — the one gate to bypass. Also pre-enforced by `command_requires_auth(Run)=true` (mod.rs:1504). |
| 2 | `harness_availability.refresh()` login gate + `get_available_harnesses()` | harness_availability.rs:364,370 | Soft — falls back to `default_harnesses()`; only affects picker population. |
| 3 | `update_agent_task(InProgress)` | driver.rs:869 | Conditional — only when `task_id.is_some()` (cloud/ambient). Local run has `task_id=None`. |
| 4 | setup-event reporter | driver.rs:2005 | Soft — `noop` without task_id. |
| 5 | MCP + model + profile setup | driver.rs:2052 | Oz-only (`if matches!(task.harness, HarnessKind::Oz)`). |
| 6 | Preflight `claude auth status --json` | driver.rs:2402 | Local command, not a server call. Blocks only if `claude` itself isn't authed (see Auth). |
| 7 | `setup_harness_plugins` | driver.rs:2505+ | Soft — `requires_verified_platform_plugin()=false` for the stream harness; notification plugin best-effort. |
| 8 | `create_external_conversation` (server) | claude_code.rs:459 (interactive only) | **Avoided** — the stream runner makes no server call. |
| 9 | `save_conversation` / `cleanup` uploads | claude_stream_runner.rs:271 (no-ops) | None. |
| 10 | git/bedrock credential refresh loops | driver.rs:121 | None for local Claude. |

**Conclusion:** the local Claude stream run is effectively offline-capable once
(a) the `AgentDriver::new` `is_logged_in` gate is bypassed and (b) `claude` is
locally authenticated.

---

## The model picker

### Where it lives
`ProfileModelSelector` ([profile_model_selector.rs](../app/src/terminal/profile_model_selector.rs)),
built in [universal_developer_input.rs:400](../app/src/terminal/universal_developer_input.rs).
It branches on `is_third_party_harness` (profile_model_selector.rs:680), which
reads `ambient_agent_view_model.selected_harness()`.

### Two model sources
1. **Oz base-LLMs** — the screenshot list ("claude sonnet 5 low/medium/high/max,
   gemini, deepseek"). Shown when harness == Oz. Built by `refresh_model_menu`
   (profile_model_selector.rs:977) from
   `llm_preferences.get_base_llm_choices_for_agent_mode`. Selecting persists via
   `update_preferred_agent_mode_llm` → `LLMPreferences` → `RequestParams.model`
   on the Oz SSE path. **These are cloud models.**
2. **Harness models** (non-Oz) — `refresh_harness_model_menu`
   (profile_model_selector.rs:905) reads
   `HarnessAvailabilityModel::models_for(harness)` (harness_availability.rs:192).
   Items carry `SelectHarnessModel { model_id, reasoning_level }`; the handler
   calls `AmbientAgentViewModel::set_harness_model_selection`
   (model.rs:537), storing `harness_model_id` / `harness_reasoning_level`.

### Where the list is populated from
`HarnessAvailabilityModel::default_harnesses()`
([harness_availability.rs:48](../app/src/ai/harness_availability.rs)) hard-codes
Oz/Claude/Codex/Gemini all with **`available_models: vec![]`**. `refresh()`
overwrites them from the server's `get_available_harnesses()` — but is **gated on
`is_logged_in()`** (harness_availability.rs:364). So `models_for(Harness::Claude)`
is empty unless a logged-in server supplies it. `HarnessModelInfo =
{ id, display_name, reasoning_level: Option<String> }`.

### Selection → run plumbing
`HarnessModelConfig { model_id, reasoning_level }` is defined in
[crates/cloud_object_models/src/scheduled_ambient_agent.rs:82](../crates/cloud_object_models/src/scheduled_ambient_agent.rs),
re-exported at [ambient_agents/task.rs:6](../app/src/ai/ambient_agents/task.rs).
It flows: `merged_config.harness.model_config()` →
`AgentDriverOptions.third_party_harness_model_config` (mod.rs:1026) →
`AgentDriver::new` (driver.rs:639) → `build_harness_runner` (driver.rs:2700) →
`harness.build_runner(…, third_party_harness_model_config.as_ref())` (driver.rs:2749).

### What Claude does with it today
- `ClaudeStreamHarness::build_runner` **ignores** the param
  (`_third_party_harness_model_config`, claude_stream_runner.rs:90); `start`
  calls `stream_json_args(…, None, None)` — no `--model`.
- **But** `model_id` already reaches `claude` via env: `AgentDriver::new` calls
  `harness_model_env_vars(Harness::Claude, config)`
  ([harness/mod.rs:443](../app/src/ai/agent_sdk/driver/harness/mod.rs)), which
  inserts `ANTHROPIC_MODEL = model_id` into `resolved_env_vars`, applied to the
  subprocess (claude_stream_runner.rs:170). **`reasoning_level` is dropped.**
- `claude` has `--model` for the model; there is **no first-class reasoning-level
  CLI flag** — thinking budget is env/prompt driven (e.g. `MAX_THINKING_TOKENS`).
  So map `reasoning_level` → thinking-budget env.

---

## Claude Code auth & a `/login` path

### How `claude` persists auth
- Cred root: `$CLAUDE_CONFIG_DIR` else `~/.claude`
  ([claude_transcript.rs:86](../app/src/ai/agent_sdk/driver/harness/claude_transcript.rs)).
  OAuth tokens / API-key creds are written and owned by the `claude` CLI itself
  (e.g. `~/.claude/.credentials.json` or the OS keychain).
- Krust writes only non-credential config — `~/.claude.json` (onboarding/trust
  flags) and `~/.claude/settings.json`
  ([claude_code.rs:610](../app/src/ai/agent_sdk/driver/harness/claude_code.rs)).
  **Krust never authenticates `claude`.**
- Preflight `claude auth status --json` (claude_stream_runner.rs:70) runs
  **locally** in the terminal, not a server call.

### Headless auth reality
`claude --print` is non-interactive, so `/login` cannot run inside the stream
harness. Claude must be authed by a **prior interactive** login; creds persist in
`~/.claude` and headless runs inherit them — explicitly noted at
[local_harness_launch.rs:207](../app/src/pane_group/pane/local_harness_launch.rs)
("Local child harness panes inherit the user's existing local auth/session
state").

### Cleanest `/login` path
The controller already emits
`BlocklistAIControllerEvent::ExecuteLocalHarnessCommand { command }`
([controller.rs:1807](../app/src/ai/blocklist/controller.rs)), consumed at
[terminal/view.rs:5311](../app/src/terminal/view.rs) via
`execute_command_or_set_pending(command, ctx)` — it runs an arbitrary command in
the user's terminal session. A `/login` slash command / action can emit this with
`command = "claude"` (bare, interactive, **no `--print`**), letting the user
complete OAuth in a visible pane. Creds land in `~/.claude`; subsequent headless
stream runs authenticate; `claude auth status --json` then passes preflight.

---

## Plug points (the rewire plan)

### (a) Route terminal prompts to the local harness
- **Best seam:** branch in `BlocklistAIController::send_query`
  ([controller.rs:646](../app/src/ai/blocklist/controller.rs)) /
  `send_request_input` (controller.rs:2355): when the effective harness is
  Claude, instead of `ResponseStream::new → generate_multi_agent_output`
  (the Oz cloud path), drive a local Claude stream and inject via
  `LocalHarnessStream` (local_harness_stream.rs:45/100/125) — already reachable
  from the `blocklist` module.
- **Runner core is liftable:** `run_claude_stream` (claude_stream_exec.rs:65) +
  `LocalHarnessStream`, already composed in `ClaudeStreamHarnessRunner::start`.
  Driving them directly from the controller sidesteps the `AgentDriver::new`
  login gate (driver.rs:667) entirely.
- **Precedent:** `ExecuteLocalHarnessCommand` (controller.rs:1807 → view.rs:5311)
  already runs local harness commands from the controller.
- Alternative seam: branch at input `submit_ai_query_local`
  ([input.rs:14188](../app/src/terminal/input.rs)) on `selected_harness`.

### (b) Repurpose the model picker
- Populate the Claude entry's `available_models` in `default_harnesses()`
  (harness_availability.rs:57) with `HarnessModelInfo`s (model × reasoning, e.g.
  sonnet/opus/haiku × low/medium/high) so `models_for(Harness::Claude)` is
  non-empty **offline**, bypassing the login-gated `refresh()`.
- Default the terminal `AmbientAgentViewModel` harness to Claude via `set_harness`
  (model.rs:505) so `is_third_party_harness` (profile_model_selector.rs:680)
  renders the harness menu.
- Translate model + reasoning into the `claude` invocation: have
  `ClaudeStreamHarness::build_runner` consume the config param
  (claude_stream_runner.rs:90), add `--model` to `stream_json_args`
  (claude_stream_exec.rs:28), and extend `harness_model_env_vars`
  (harness/mod.rs:443) to map `reasoning_level` → thinking-budget env. Today only
  `model_id → ANTHROPIC_MODEL` is wired.

### (c) Add a `/login` path
- Add a slash command / controller action that runs **interactive** `claude`
  (no `--print`) in a terminal session or new pane, reusing
  `ExecuteLocalHarnessCommand` (controller.rs:1807 → view.rs:5311) or
  `launch_local_harness_child`
  ([terminal_pane.rs:1774](../app/src/pane_group/pane/terminal_pane.rs)), with
  `command = "claude"`. Creds persist to `~/.claude`; headless runs inherit them;
  `claude auth status --json` confirms.

---

## Implementation status (chosen approach: keep AgentDriver)

**Decided:** route terminal prompts through the existing `AgentDriver` run
lifecycle, bound to the visible terminal. Repurpose the model picker to Claude
models + reasoning levels. (The `is_logged_in()` gate needs no change — the
fork's faked credentials already satisfy it; the 401 was purely the Oz cloud
call, which the local path never makes.)

### Done — core routing (compiles; terminal prompts now run local Claude inline)
- **`AgentDriver::new_from_view`** ([driver.rs](../app/src/ai/agent_sdk/driver.rs)):
  refactored `new` into `new_inner(options, existing_terminal_driver, ctx)`; `new`
  passes `None` (headless terminal, CLI path), `new_from_view` passes a
  `TerminalDriver::create_from_existing_view(view)` so the run renders inline in
  the user's visible terminal.
- **Trigger** — `Input::submit_ai_query_local` ([input.rs](../app/src/terminal/input.rs)):
  when `selected_harness() == Claude`, emits `Event::RunLocalClaudeQuery
  { prompt, model_id, reasoning_level }` and returns instead of calling the Oz
  controller.
- **Handler** — `TerminalView::run_local_claude_query` ([view.rs](../app/src/terminal/view.rs)),
  dispatched from `handle_input_event`: builds `AgentDriverOptions` + `Task`,
  constructs the driver via `new_from_view(options, ctx.handle().upgrade(ctx))`,
  and spawns `driver.run(task)` **without** `terminate_app`.
- **Default harness** — terminal `AmbientAgentViewModel` now defaults to
  `Harness::Claude` (model.rs:320) instead of Oz, so queries route local by
  default.
- **Replies render (2026-07-16)** — `LocalHarnessStream::begin` now performs
  the same handshake the Oz server stream does, without which every injected
  message was silently dropped ("Task not found" in the log) and no reply ever
  rendered: (1) register the exchange, (2)
  `initialize_output_for_response_stream` (StreamInit) so the exchange has a
  live output (else `OutputNeverInitialized`), (3) a client-minted `CreateTask`
  action to upgrade the conversation's optimistic root task (else
  `TaskNotInitialized` — and the upstream `AddMessagesToTask` handler removes
  the task from the store before adding and doesn't restore it on failure, so
  one bad injection wedges the conversation). Round-trip regression test:
  `local_harness_stream_tests.rs`. Failed turns (`is_error` results) now render
  an error block instead of ending invisibly, and `run_claude_stream` captures
  the CLI's stderr + exit code to the log on failure instead of nulling it.
- **Replies visible (2026-07-16, part 2)** — completing in the model wasn't
  enough: with `FeatureFlag::AgentView` on, the GUI displays whatever
  conversation the `AgentViewController` was *entered* for, and the local run's
  fresh conversation was never bound (the Oz send path surfaces its
  conversation via the controller; symptom: input footer shows busy, then
  stops, no reply). Fix: `run_local_claude_query`'s drain calls
  `enter_agent_view_for_conversation(None, Input, stream.conversation_id())`
  when the stream begins, and `LocalHarnessStream::begin` also calls
  `mark_active_conversation_id` (pane title / details panel / active-pointer
  parity with the cloud controller's post-send bookkeeping).

- **Noise + double-submit cleanup (2026-07-16, part 3)** — verified working
  end-to-end in the GUI (reply rendered). Cleanups: (a) the banner →
  agent-view handoff replayed the first submission, spawning two identical
  claude runs; `run_local_claude_query` now drops an identical prompt within a
  3s window (`last_local_claude_submission`). (b)
  `mark_response_stream_completed_successfully` no longer fetches server
  metadata for conversations whose server token is the local placeholder
  (empty) — this was the post-run "Unauthorized: User not in context" warn.
  (c) the CloudObjects update websocket is disabled via
  `CLOUD_OBJECTS_LISTENER_ENABLED = false` (listener.rs) — it can never
  authenticate under faked credentials and retried forever.

- **Multi-turn threads (2026-07-16, part 4)** — follow-up prompts previously
  started a fresh conversation (and a fresh claude session) per submit, so the
  previous exchange vanished from view and claude had no memory. Each
  `TerminalView` now keeps `local_claude_thread: (claude session id,
  conversation id)`: follow-ups run `claude --resume <session>` and register
  their exchange on the existing conversation via
  `LocalHarnessStream::begin_follow_up` (same handshake minus the `CreateTask`
  upgrade, which must not run twice — re-upgrading a server-backed root task
  fails and loses the task). A resumed run forks a fresh session id, which
  claude reports in `SessionInit`; the drain tracks it so the next follow-up
  resumes the right session. Fresh conversations still enter the agent view;
  follow-ups leave the binding alone.

### Remaining (each needs an app-rebuild to validate)
1. **Model picker** — populate the Claude entry's `available_models` in
   `default_harnesses()` (harness_availability.rs:57) with Claude models ×
   reasoning. Today the local run uses `claude`'s default model (`model_id` is
   whatever the picker holds, else none). Exact Claude model IDs / aliases the
   installed CLI accepts need confirming.
2. **Reasoning** — extend `harness_model_env_vars` (harness/mod.rs:443) to map
   `reasoning_level` → thinking-budget env (e.g. `MAX_THINKING_TOKENS`); add
   `--model` to `stream_json_args` and consume the config in
   `ClaudeStreamHarness::build_runner`.
3. **`/login`** — slash command running interactive `claude` via
   `ExecuteLocalHarnessCommand` (see auth section).
4. **Runtime unknowns to validate:** whether the initial-prompt-on-enter path
   routes through `submit_ai_query_local` (my branch) or bypasses it; the
   `AgentDriver` singleton lifecycle across repeated queries; and whether any
   `driver.run` setup step still assumes a cloud/CLI context for a local run.

## Open design questions (to decide before building)

1. **Where to branch — controller vs input?** Branching in the controller
   (`send_query`) keeps one submit path and reuses conversation/exchange
   bookkeeping, but the controller is Oz-shaped; we'd add a local fork inside it.
   Branching at input is cleaner separation but duplicates some submit plumbing.
2. **Bypass the login gate, or fake login harder?** We already fake auth
   globally; the `AgentDriver::new` gate is a second, independent check. If we
   route through the controller (not `AgentDriver`), we avoid it entirely — but
   if we keep using `AgentDriver`, we'd relax driver.rs:667.
3. **Reasoning level mapping.** Confirm the exact env/flag `claude` honors for
   thinking budget per level (low/medium/high) — `MAX_THINKING_TOKENS` is the
   likely knob; validate against the installed `claude` version.
4. **Model list contents.** Which Claude models/levels to show in the repurposed
   picker, and whether to hide the Oz list entirely when the harness is Claude.
