# Bork Agent settings page — what each feature actually does

_Settings → **AI** umbrella → **Bork Agent** subpage._

This documents the **runtime behavior** of each toggle on the Bork Agent page —
what happens when the feature is on, what data it collects, where the work runs,
and which bork-server endpoint it hits. (It is not about the settings plumbing;
flipping a toggle just persists a bool — the interesting part is what the feature
*does* while enabled.)

## The short answer

Every "Active AI" feature is a **cloud AI call**: when the feature fires, the
client gathers local context (recent commands, their output, history, a git diff,
or file contents), does some local pre-work, then **POSTs that context to a
bork-server `/ai/…` endpoint** (bearer-authed) where the LLM inference runs. The
response (a suggested command, a prompt, a code diff, a commit message) comes back
and is rendered in the UI. The inference is **not** local and **not** a serverless
"cloud function" — it's Bork's backend AI service.

Two features are exceptions:

- **Shared Block Title Generation** is currently **dead code** — its generator was
  orphaned by the "Remove block sharing" change and has no live callers. The
  toggle still renders but only sets a telemetry flag; nothing is generated.
- The **Commit & Pull Request** feature, when **off**, still works without AI
  (manual commit placeholder; `gh pr create --fill` for PRs).

## Enablement hierarchy (three nested gates)

Each feature checks all three, top to bottom ([app/src/settings/ai.rs:1878](app/src/settings/ai.rs)):

1. **Bork Agent** (page header switch) → `is_any_ai_enabled` = the global AI bool
   **and** not blocked by a remote-session org policy. Master kill-switch.
2. **Active AI** (section switch) → `is_active_ai_enabled` = `is_any_ai_enabled` **and**
   the Active-AI bool. Turning this off disables every feature below regardless of
   their own switches.
3. The **individual feature** switch.

So a feature runs only when `global AI` ∧ `Active AI` ∧ `its own toggle` are all on.

## Transport

All the simple features use the same shape ([app/src/server/server_api.rs:934](app/src/server/server_api.rs)):

```rust
POST {ChannelState::server_root_url()}/ai/<endpoint>
Authorization: Bearer <access token>
Body: JSON { …context… }
```

The passive suggestion features instead use the **streaming multi-agent pipeline**
(`generate_multi_agent_output` → `bork_multi_agent_client`,
[app/src/ai/agent/api/impl.rs:14](app/src/ai/agent/api/impl.rs)) — the same
agent-event stream the main Bork Agent conversation uses, but with a restricted
tool set and an ephemeral conversation. Secrets are redacted client-side before
sending (`redaction::redact_inputs`, impl.rs).

---

## Feature-by-feature

### Next Command
- **Setting:** `intelligent_autosuggestions_enabled_internal` → `is_intelligent_autosuggestions_enabled`
- **Code:** `NextCommandModel` — [app/src/ai/predict/next_command_model.rs](app/src/ai/predict/next_command_model.rs)
- **Trigger:** a block finishes running / the input is focused.
- **What it does:** builds a context bundle — recent blocks as `context_messages`
  (command **and** output), matching command history found locally, system/platform
  context, the prefix you've typed, the just-completed `block_context`, and any
  earlier suggestions you rejected. It **first tries a local fast-path**: rank
  likely next commands from your own rich history and validate them locally
  (`is_command_valid`, alias expansion, arg checks) with **no network**. If that
  yields nothing, it calls the cloud.
- **Endpoint:** `POST /ai/generate_input_suggestions`
  (`server_api.generate_ai_input_suggestions`, [next_command_model.rs:469](app/src/ai/predict/next_command_model.rs)).
- **Result:** a predicted command shown as the greyed inline autosuggestion / zero-state suggestion.
- **Verdict:** **cloud**, with a local history/validation fast-path that can answer without any request.

### Prompt Suggestions
- **Setting:** `prompt_suggestions_enabled_internal` → `is_prompt_suggestions_enabled`
- **Code:** `PassiveSuggestionsModel` — [app/src/ai/blocklist/passive_suggestions/maa.rs](app/src/ai/blocklist/passive_suggestions/maa.rs) (new MAA path) and [legacy.rs](app/src/ai/blocklist/passive_suggestions/legacy.rs)
- **Trigger:** a block completes (skipped for Oz environment-startup commands).
- **What it does:** sends a passive multi-agent request scoped to the
  `ToolType::SuggestPrompt` tool, carrying the completed command + its output as
  context, in an **ephemeral conversation** (deliberately kept out of your real
  history). The response is streamed back and the suggested natural-language prompt
  is extracted from the stream.
- **Endpoint:** streaming multi-agent pipeline (`generate_multi_agent_output`);
  legacy path is `POST /ai/generate_am_query_suggestions`
  ([legacy.rs:285](app/src/ai/blocklist/passive_suggestions/legacy.rs)).
- **Result:** an inline suggested-prompt banner in the input.
- **Verdict:** **cloud** (streamed).

### Suggested Code Banners
- **Setting:** `code_suggestions_enabled_internal` → `is_code_suggestions_enabled`
- **Code:** same `PassiveSuggestionsModel` (maa.rs).
- **Trigger:** a block completes.
- **What it does:** first **locally** detects file paths relevant to the command +
  output (`detect_relevant_file_paths_for_block`, on a blocking thread), reads
  those files, then sends a passive multi-agent request asking the model to
  propose code diffs / queries grounded in that content.
- **Endpoint:** same streaming multi-agent pipeline as Prompt Suggestions.
- **Result:** inline code-diff / query banners in the blocklist.
- **Verdict:** **cloud**, with local relevant-file detection + file reads feeding the request.

### Shared Block Title Generation  ✅ removed
- **What it did historically:** POST a command + output to bork-server to generate
  a short title for a block being shared to a bork.baby permalink.
- The generator was orphaned by the "Remove block sharing" change (no live callers),
  and the toggle only set a telemetry flag — it generated nothing.
- **Now removed** in this branch: the setting, toggle UI, `ToggleSharedTitleGeneration`
  action, telemetry event, context flag, and the `SharedBlockTitleGeneration` feature
  flag (+ cargo feature) are all gone. Verified with `cargo check -p bork` (clean).

### Commit & Pull Request Generation
- **Setting:** `git_operations_autogen_enabled_internal` → `is_git_operations_autogen_enabled`
- **Code:** git dialog — [app/src/code_review/git_dialog/mod.rs](app/src/code_review/git_dialog/mod.rs), request in [app/src/code_review/git_actions.rs](app/src/code_review/git_actions.rs)
- **Gate:** `should_send_git_ops_ai_request` = feature flag **and** this toggle
  **and** the team's Git-Operations-AI tier policy ([git_dialog/mod.rs:118](app/src/code_review/git_dialog/mod.rs)).
- **What it does (on):** sends the **git diff** (plus branch name and any existing
  commit messages) with an `output_type` of `CommitMessage` / `PrTitle` /
  `PrDescription` and fills the corresponding field in the commit / PR dialog.
- **Endpoint:** `POST /ai/generate_code_review_content`
  (request/response types in [app/src/ai/generate_code_review_content/api.rs](app/src/ai/generate_code_review_content/api.rs);
  impl at [app/src/server/server_api/ai.rs:2919](app/src/server/server_api/ai.rs)).
- **What it does (off):** the commit dialog opens with a manual-type placeholder
  and PR creation runs `gh pr create --fill` — **no AI, no request**.
- **Verdict:** **cloud** when enabled; fully functional (non-AI) when disabled.

### Natural Language Autosuggestions  _(part of Active AI, hidden unless its flag is on)_
- **Setting:** `natural_language_autosuggestions_enabled_internal` → `is_natural_language_autosuggestions_enabled` (only shown when `FeatureFlag::PredictAMQueries` is enabled — that's why it isn't in the current screenshot).
- **Endpoint:** `POST /ai/predict_am_queries` (`server_api.predict_am_queries`, [server_api.rs](app/src/server/server_api.rs)).
- **Verdict:** **cloud**.

---

## Data that leaves the machine (privacy view)

| Feature | Sent to bork-server |
|---|---|
| Next Command | recent command **blocks incl. output**, matching history, typed prefix, system/platform context |
| Prompt Suggestions | the completed command + its output |
| Suggested Code Banners | the completed command + output **and the contents of locally-detected relevant files** |
| Commit & PR Generation | the **git diff**, branch name, existing commit messages |
| Natural Language Autosuggestions | recent command/query context |

Secrets are redacted client-side before the multi-agent requests are sent
(`redaction::redact_inputs`).

---

## Note: credit-usage read removed from this page

Previously, opening **any** AI subpage (including Bork Agent) fired
`refresh_request_usage_async`, a bork-server read to populate the AI-credit
counters. Those counters are only rendered by `UsageWidget`, which lives on the
**Profiles** subpage — so the read was wasted on the Bork Agent / Knowledge /
Third-party-CLI pages. `on_page_selected` is now scoped to fire that read only when
the active subpage is Profiles ([app/src/settings_view/ai_page.rs](app/src/settings_view/ai_page.rs), `on_page_selected`).
