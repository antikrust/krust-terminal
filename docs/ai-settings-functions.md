# `AISettings` gating functions

This document catalogs every public `is_..._enabled` / `is_..._disabled` gating
function on the `AISettings` struct in `app/src/settings/ai.rs`. It exists because
this repo is a fork of Derp being stripped of account/cloud-gated features for
solo local use: some of these gates only ever unlock a network call to Derp's
own servers (account, quota, and org-policy dependent) and have no local
equivalent, while others gate purely on-device behavior (local models, local
file/process spawning, or simple local booleans) that can keep working with no
account and no cloud backend at all. A few are genuinely hybrid, falling back
to a local path when the cloud path isn't available. Knowing which is which
matters for deciding what can be routed through local CLI harnesses (Claude
Code, Codex, etc.) versus what would need a real Derp backend to do anything.

This is a point-in-time snapshot of one commit (`5f52cf1`) of `app/src/settings/ai.rs`.
Line numbers and call sites will drift as the code changes; treat this as a
starting map for re-verification, not a durable guarantee.

---

## `is_ai_disabled_due_to_remote_session_org_policy` (line 1782)

Returns `true` (disabling AI) when the focused terminal contains remote
(SSH-style) blocks and the current team's org policy hasn't explicitly allowed
AI in remote sessions. **Hybrid**: the check itself is a local boolean
read off `FocusedTerminalInfo` and `UserWorkspaces::is_ai_allowed_in_remote_sessions()`
(`app/src/workspaces/user_workspaces.rs:1517`), but the org policy value it
reads was itself synced down from Derp's cloud team-management backend, so
without a Derp account/team this simply defaults to `true` (allowed) via the
`.unwrap_or(true)` fallback when there's no current team.

## `is_any_ai_enabled` (line 1798)

The master AI kill switch: `*self.is_any_ai_enabled && !self.is_ai_disabled_due_to_remote_session_org_policy(app)`.
**Local.** Notably, this previously also required `!is_anonymous_or_logged_out`
(i.e. being logged into a Derp account) — that check has just been removed
from this function as part of this fork's work, so login is no longer a
precondition for any AI feature gated behind this function.

## `is_active_ai_enabled` (line 1850)

Composes `is_any_ai_enabled(app) && *self.is_active_ai_enabled_internal && AppExecutionMode::allows_active_ai()`.
**Local.** `allows_active_ai()` (`crates/derp_core/src/execution_mode.rs:56`) just
checks `self.is_app()` — i.e. "is this the real desktop app," unrelated to
auth or cloud connectivity.

## `is_prompt_suggestions_enabled` (line 1856)

Gates "passive suggestions" — ghost-text prompt hints shown after a command
finishes. **Cloud.** Two implementations exist:
`app/src/ai/blocklist/passive_suggestions/maa.rs:196-202` calls
`generate_multi_agent_output` via `ServerApiProvider::as_ref(ctx).get()`, and
`app/src/ai/blocklist/passive_suggestions/legacy.rs:283` builds a
`GenerateAMQuerySuggestionsRequest` and sends it through the same
`ServerApiProvider`. No local generation path exists for this.

## `is_rule_suggestions_enabled` (line 1860)

Gates whether the agent suggests new "rules" (memory-like facts) after a
conversation. **Cloud.** Checked at `app/src/ai/blocklist/block.rs:2533`,
and the suggestions are read back from `CloudModel::as_ref(ctx).get_all_objects_of_type::<_, CloudAIFactModel>()`
(`app/src/ai/blocklist/block.rs:2543-2544`) — `CloudAIFactModel` lives in
Derp's cloud object store (`app/src/ai/facts/mod.rs:1`, `crate::cloud_object::model::persistence::CloudModel`).

## `is_code_suggestions_enabled` (line 1864)

Sibling of `is_prompt_suggestions_enabled`, gating the "code suggestions"
flavor of passive ghost-text hints. **Cloud.** Same two call sites/machinery
as prompt suggestions (`maa.rs` / `legacy.rs`), both routed through
`ServerApiProvider`.

## `is_natural_language_autosuggestions_enabled` (line 1868)

Gates ghosted-text autosuggestions that predict a natural-language "Agent
Mode" query while typing (distinct from the input-mode classifier below).
**Cloud**, confirmed at the call site: `app/src/terminal/input.rs:10019`
guards `predict_am_query`, which builds a `PredictAMQueriesRequest` and calls
`server_api.predict_am_queries(&request).await` (`app/src/terminal/input.rs:13712-13722`).
Also requires `FeatureFlag::PredictAMQueries`.

## `is_shared_block_title_generation_enabled` (line 1872)

Gates auto-generating a title when a user opens the "share block" dialog.
**Cloud.** `app/src/terminal/share_block_modal.rs:1160-1163` (`should_send_title_gen_request`)
guards a call to `block_client.generate_shared_block_title(request)`
(`app/src/terminal/share_block_modal.rs:459-460`), where `BlockClient` is a
trait implemented against Derp's server API (`app/src/server/server_api/block.rs:28,43`).
Sharing a block is itself an inherently cloud feature (it produces a public
URL), so this has no local equivalent.

## `is_git_operations_autogen_enabled` (line 1876)

Gates AI-autogenerating commit messages and PR titles/descriptions in the git
review dialog. **Cloud.** `app/src/code_review/git_dialog/mod.rs:119-122`
(`should_send_git_ops_ai_request`) guards `generate_commit_message`
(`app/src/code_review/diff_state/mod.rs:787` → `app/src/code_review/git_actions.rs:83-102`),
which calls `ai_client.generate_code_review_content(...)` where `AIClient` is
implemented by `ServerApi` (`app/src/server/server_api/ai.rs:1122`, `impl AIClient for ServerApi` at line 1603).
When disabled, call sites fall back to manual entry / `gh pr create --fill`.

## `is_intelligent_autosuggestions_enabled` (line 1880)

Gates "next command" autosuggestions in the terminal. **Hybrid.** Consumed at
`app/src/ai/predict/next_command_model.rs:60-63` (`is_next_command_enabled`,
also requires `UserWorkspaces::is_next_command_enabled()`). The module
supports two sources: a purely local suggestion derived from shell history
matching (tracked via the `is_from_ai: bool` flag on `NextCommandSuggestionState`,
`app/src/ai/predict/next_command_model.rs:80-88`), and an LLM-backed suggestion
sent through `ServerApi` (imported at `app/src/ai/predict/next_command_model.rs:31`)
via a `GenerateAIInputSuggestionsRequest`. History-based prediction keeps
working with no account; the AI-backed variant does not.

## `is_voice_input_enabled` (line 1884)

Gates the microphone/voice-dictation button on the input editor. Also
requires the `voice_input` Cargo feature to be compiled in
(`cfg!(feature = "voice_input")`) and `UserWorkspaces::is_voice_enabled()`.
**Cloud** for the actual transcription: `app/src/editor/view/voice.rs:18`
imports `crate::server::server_api::TranscribeError`, and the transcription
result handling at `app/src/editor/view/voice.rs:486-501` matches on
`TranscribeError::QuotaLimit`, confirming audio is sent to Derp's servers to
be transcribed (local mic capture, but no local speech-to-text path).

## `is_ai_autodetection_enabled` (line 1895)

Gates whether the terminal input box auto-detects "this looks like natural
language, switch to AI mode" versus staying in shell mode (used outside the
`AgentView` feature flag path — see `is_nld_in_terminal_enabled` for the
`AgentView` variant). **Local.** The actual classification is done by
`InputClassifierModel` (`app/src/input_classifier.rs`), which loads an
on-device ONNX tiny-BERT model bundled in the repo
(`crates/input_classifier/models/onnx/bert_tiny_v{1,2,3}.onnx`) when the
corresponding `nld_classifier_v*` Cargo feature is compiled in, and otherwise
falls back to a pure `HeuristicClassifier` (`app/src/input_classifier.rs:12,54-58`).
No network call is involved in the classification step itself. (Note: once
input is classified/switched into AI mode, what the user's query then does —
e.g. full agent chat — is governed by other, generally cloud, gates elsewhere;
this toggle only covers the mode-switch decision.)

## `is_nld_in_terminal_enabled` (line 1904)

The `FeatureFlag::AgentView`-specific counterpart to `is_ai_autodetection_enabled`,
gating natural-language detection in the plain terminal (as opposed to the
fullscreen agent view). **Local**, for the same reason as above — both flow
through the identical `GuiInputModePolicy::initial_config` branch
(`app/src/ai/blocklist/agent_view/gui_input_mode_policy.rs:38-43`) and the
same on-device classifier.

## `is_memory_enabled` (line 1908)

Gates whether "memory" (learned facts about the user, shown in the Rules/Memory
UI) is active. **Cloud.** Consumed in `app/src/ai/agent/api.rs:208` when
building agent request context, and the underlying storage type is
`CloudAIFactModel` / `AIMemory` from `app/src/ai/facts/mod.rs:1`
(`cloud_object_models`). The Rules view also shows a "disabled" banner when
this is off (`app/src/ai/facts/view/rule.rs:966`).

## `is_derp_drive_context_enabled` (line 1912)

Gates whether Derp Drive objects (shared notebooks, workflows, env-var
collections, etc.) are pulled in as agent context. **Cloud.** Consumed
alongside `is_memory_enabled` at `app/src/ai/agent/api.rs:209`. Derp Drive
items are backed by `CloudObject`/`cloud_object` machinery
(`app/src/drive/mod.rs:19,26-27`, `CloudObjectTypeAndId`, `CloudViewModel`) —
Drive is fundamentally a cloud-synced workspace feature with no local
equivalent.

## `is_file_based_mcp_enabled` (line 1916)

Gates whether third-party MCP servers *discovered from local config files*
(e.g. project `.mcp.json`-style configs) are auto-spawned. **Local.** Also
requires `FeatureFlag::FileBasedMcp`. Consumed in
`app/src/ai/mcp/file_based_manager.rs:317,357,423` to decide auto-start
eligibility for `TemplatableMCPServerInstallation`s; the servers themselves are
spawned as local child processes (`FileBasedMCPManagerEvent::SpawnServers`,
`app/src/ai/mcp/file_based_manager.rs:335-343`). Global first-party ("Derp")
servers always auto-spawn regardless of this toggle; only global third-party
servers are gated by it, and project-scoped servers always require explicit
opt-in regardless. A code comment (`app/src/settings/ai.rs:1917-1929`) notes
this toggle is intentionally *not* force-enabled in Cloud Mode, to avoid
letting a checked-in repo config run arbitrary commands during autonomous/cloud
agent runs.

## `is_orchestration_enabled` (line 1931)

Gates whether the agent is allowed to orchestrate/spawn child agent runs at
all. **Hybrid**, and directly relevant to this fork: consumed at
`app/src/ai/agent/api.rs:335-341` alongside a `BlocklistAIPermissions::get_run_agents_setting`
check and a same-session-type restriction. Whether a spawned child agent is
itself cloud-backed (Oz) or a **local CLI harness** (Claude Code, Codex, etc.)
is a separate, orthogonal decision handled by `app/src/ai/local_harness_setup.rs`
(`LocalHarnessSetupState`, `Harness::{Claude,Codex,OpenCode,Gemini}` at
lines 15-40) — so this toggle is the on/off switch for orchestration in
general, and does not itself force a cloud dependency, but the top-level
agent driving the orchestration is typically the main cloud-backed agent.

## `is_cloud_handoff_enabled` (line 1938)

Gates whether a local conversation can be "handed off" to continue running in
Derp's cloud. **Cloud by definition** — the whole feature is
local-to-cloud handoff. Requires `FeatureFlag::OzHandoff`,
`FeatureFlag::HandoffLocalCloud`, the `local_fs` Cargo feature, non-wasm
target, `PrivacySettings::is_cloud_conversation_storage_enabled`, and that the
current team hasn't disabled cloud conversation storage via
`UserWorkspaces::get_cloud_conversation_storage_enablement_setting()`
(`app/src/settings/ai.rs:1938-1956`, `app/src/workspaces/user_workspaces.rs:1506-1514`).
Has no local-only equivalent since the destination is inherently the cloud.

## `is_ampersand_handoff_enabled` (line 1957)

`is_cloud_handoff_enabled(app) && !*self.should_force_disable_ampersand_handoff`
— gates the `&`-prefix compose shortcut for triggering a cloud handoff
(`app/src/terminal/input.rs:4369`, `app/src/ai/blocklist/agent_view/agent_message_bar.rs:609`).
**Cloud**, by direct composition with `is_cloud_handoff_enabled`.

## `is_auto_handoff_on_sleep_enabled` (line 1961)

Gates automatically handing an active conversation off to the cloud when the
machine goes to sleep, so it keeps running. **Cloud**, again by composition
with `is_cloud_handoff_enabled`, plus a platform-support check
(`auto_handoff_on_sleep_enabled.is_supported_on_current_platform()`).
Consumed in `app/src/workspace/auto_handoff.rs:245,268,408`.

---

## Summary

| Function | Classification |
|---|---|
| `is_ai_disabled_due_to_remote_session_org_policy` | Hybrid (local check, cloud-synced org data) |
| `is_any_ai_enabled` | Local (recently de-gated from requiring login) |
| `is_active_ai_enabled` | Local |
| `is_prompt_suggestions_enabled` | Cloud |
| `is_rule_suggestions_enabled` | Cloud |
| `is_code_suggestions_enabled` | Cloud |
| `is_natural_language_autosuggestions_enabled` | Cloud |
| `is_shared_block_title_generation_enabled` | Cloud |
| `is_git_operations_autogen_enabled` | Cloud |
| `is_intelligent_autosuggestions_enabled` | Hybrid (local history + cloud LLM) |
| `is_voice_input_enabled` | Cloud (transcription) |
| `is_ai_autodetection_enabled` | Local (on-device ONNX/heuristic classifier) |
| `is_nld_in_terminal_enabled` | Local (same classifier, AgentView path) |
| `is_memory_enabled` | Cloud |
| `is_derp_drive_context_enabled` | Cloud |
| `is_file_based_mcp_enabled` | Local |
| `is_orchestration_enabled` | Hybrid (enables child agents; cloud vs. local harness decided elsewhere) |
| `is_cloud_handoff_enabled` | Cloud |
| `is_ampersand_handoff_enabled` | Cloud |
| `is_auto_handoff_on_sleep_enabled` | Cloud |

No function above was left unclassified. The closest calls to flag as
lower-confidence than the rest:

- `is_ai_disabled_due_to_remote_session_org_policy` — classified "hybrid"
  because the boolean itself is a local field read, but that field's value
  only ever comes from previously-synced cloud org/team data; there's no
  separate "local-only" code path to point to beyond the `unwrap_or(true)`
  default.
- `is_orchestration_enabled` — classified "hybrid" because the toggle itself
  is agnostic to cloud vs. local, but in practice the agent driving
  orchestration decisions today is generally the cloud-backed main agent;
  worth re-checking as this fork's local-harness work progresses.
