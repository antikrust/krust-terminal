# Local harness lifecycle: how a CLI agent run flows through Warp

This fork routes every terminal agent prompt to a **local CLI harness**
(`claude` or `codex`) instead of Warp's cloud "Oz" backend. This document is
the map of that pipeline for anyone — human or AI — editing it. The flows
below reuse Warp's own machinery deliberately; each section ends with the
traps that machinery hides. **Read the traps before changing anything.**

The companion history/plan doc is [agent-flow-map.md](agent-flow-map.md);
this file describes the code as it works today.

---

## Pipeline overview

```
Enter in agent input
  → input.rs submit: Event::RunLocalClaudeQuery { prompt, harness, model_id, reasoning_level }
  → TerminalView::run_local_claude_query (terminal/view.rs)
      spawns the CLI            → claude_stream_exec.rs / codex_stream_exec.rs
      parses stdout JSONL       → claude_stream.rs / codex_stream.rs → TurnEvent
      drain (main thread)       → turn_events_to_conversation.rs → api::Message
      injects into conversation → LocalHarnessStream (ai/blocklist/local_harness_stream.rs)
  → BlocklistAIHistoryModel → conversation/exchange/task → AI blocks render
```

`TurnEvent` (crates/ai/src/agent/turn_event.rs) is the provider-agnostic wire
format. Parsers translate each CLI's native stream into it; **everything
downstream depends only on `TurnEvent`**, never on a CLI's raw format. Add a
new harness by writing a parser + exec module, not by touching the drain.

## Spawn layer (terminal/view.rs `run_local_claude_query`)

- Args built by pure functions so they're unit-testable:
  `stream_json_args` (claude), `codex_exec_args` (codex).
- Model + reasoning come from the picker via the event, else from
  `persisted_model_for(harness)` (ai/harness_availability.rs), stored
  per-harness in `CloudAgentSettings.last_selected_harness_model`.
  Reasoning maps to `MAX_THINKING_TOKENS` (claude env) or
  `-c model_reasoning_effort` (codex flag).
- Conversation ↔ CLI-session bindings persist in
  `local_harness_threads.rs` (state_dir/local_harness_threads.json) so
  follow-ups resume across app restarts. **`lookup(conversation_id)` is also
  the "is this a local conversation?" predicate** used to gate cloud paths.
- A continued thread keeps its own harness even if the picker changed;
  model/reasoning are re-read from that harness's persisted selection
  (codex 400s on claude model ids and vice versa).

## Message mapping (turn_events_to_conversation.rs)

| TurnEvent            | api::Message                                | Renders as                  |
|----------------------|---------------------------------------------|-----------------------------|
| `Text`               | `AgentOutput`                               | markdown prose              |
| `Thinking`           | `AgentReasoning`                            | collapsible "Thinking" block|
| `ToolCallStarted`    | `ToolCall` → `CallMcpTool`                  | native tool card, collapsed |
| `ToolCallResult`     | `ToolCallResult` → `CallMcpToolResult`      | card's result detail        |
| `TurnCompleted` (err)| `AgentOutput` error block                   | error text                  |
| `SessionInit`/`TurnCompleted` (ok) | nothing — bookkeeping only    | —                           |

Why `CallMcpTool`: it is Warp's **generic "named tool + JSON args"
representation** — exactly what a harness tool call is. The card
(`RequestedCommandView`, type `McpTool`) is collapsed by default with a
working chevron, and its detail renders as the interactive JSON tree behind
`FeatureFlag::McpJsonTreeView` — enabled for this build in
`app/src/bin/oss.rs`.

### Decided direction: shell tools are shell, not MCP (2026-07-18)

The generic MCP card is wrong for **shell** tools (claude `Bash`, codex
`Shell`): the user is not "using MCP", they're using a harness that runs
commands, and a JSON tree is a poor way to read command output. The agreed
design — respect Warp's vocabulary, keep the MCP machinery intact for real
MCP servers, and put the fork-specific choice in **our mapping layer**:

- Shell-like tool calls map to Warp's native shell representation:
  `ToolCall` → `RunShellCommand { command }` and `ToolCallResult` →
  `RunShellCommandResult { command, command_finished: ShellCommandFinished
  { output, exit_code, command_id, … } }` (internal:
  `AIAgentActionType::RequestCommandOutput` /
  `RequestCommandOutputResult::Completed`). Every other tool keeps
  `CallMcpTool`.
- Facts to know before touching this (learned by reading, not guessing):
  - The command card (`RequestedActionViewType::Command`) shows the
    **command only**. In the cloud flow the *output* lives in the terminal
    block that executed it — not in the card. For view-only transcripts,
    `sync_command_from_result_for_viewer` (requested_command.rs) syncs just
    the command text from a finished result.
  - Warp's own path for showing conversation command blocks without having
    executed them locally: `conversation.to_serialized_blocklist_items()` →
    `prepare_conversation_block_restoration` (conversation_restoration.rs)
    → `BlockList::insert_restored_block` — this is how restored/CLI-agent
    conversations rebuild real terminal blocks (real block rendering, txtFX
    included) from persisted exchange data. Investigate feeding harness
    command results through that shape before inventing any custom output
    view; a "slightly custom" fork addition is acceptable only if that path
    can't carry live-streamed results.
  - `ShellCommandFinished.command_id` is a `BlockId`; for harness runs
    there is no local block unless we create one — a fresh id is fine for
    display-only, but nothing may assume the block exists.

**Traps:**
- Do **not** patch shared renderers (`requested_command.rs`, markdown, etc.)
  to change how fork content displays. Presentation changes go through Warp's
  mechanisms: feature flags (enabled per-channel in `bin/oss.rs`), message
  types, or the proto. If the display is wrong, you are probably emitting the
  wrong representation.
- `CallMcpTool.args` must be a JSON **object** (prost `Struct`); the
  api→internal conversion errors on missing args. Non-object inputs are sent
  argless.
- `tool_call_id` (= `TurnEvent.call_id`) is the **action id**. The result
  message and the live action-result registration must use the same id or the
  card never resolves.

## Stream lifecycle (local_harness_stream.rs)

`begin` / `begin_follow_up` replay the handshake the Oz server stream would
perform — **all four steps are load-bearing**, skipping one silently drops
replies:

1. `update_conversation_for_new_request_input` — registers the exchange
   (follow-ups also `mark_conversation_in_progress`; nothing else flips it
   back for a local run).
2. `initialize_output_for_response_stream` — gives the exchange a live output
   (else `OutputNeverInitialized`).
3. Client-minted `CreateTask` — upgrades the optimistic root task (else
   `TaskNotInitialized`). Follow-ups must **not** re-send it.
4. `mark_active_conversation_id` — active-pointer parity.

Plus: both paths echo the prompt as a `UserQuery` task message
(`user_query_message`) — restoration rebuilds the query bubbles from task
messages (`user_inputs_from_messages`), so without the echo, restored
conversations show answers with no questions.

**Trap:** the upstream `AddMessagesToTask` handler removes the task from the
store before `add_messages` and does not restore it on failure — one
malformed injected message wedges the conversation with "Task not found"
forever. Validate new message shapes with the unit tests
(`turn_events_to_conversation_tests.rs`) before shipping.

## Action lifecycle (native tool cards)

Local conversations are **read-only transcripts** — the CLI already executed
every tool. Three pieces make the cards behave:

1. `run_local_claude_query` puts the per-terminal
   `BlocklistAIActionModel` in **view-only mode** (`set_view_only(true)`),
   the same mode shared-session viewers use. This suppresses accept/reject
   affordances on the cards.
2. **Live runs** register each result explicitly: the drain calls
   `tool_call_action_result` → `apply_finished_action_result` (guarded by
   `get_action_result(id).is_none()`). Without this the card shows no output
   until an app restart.
3. **Restore** needs no extra work: `restore_action_results_from_exchanges`
   (load_ai_conversation.rs) rebuilds results from the persisted
   `ToolCallResult` messages, which convert to `AIAgentInput::ActionResult`
   exchange inputs.

## The conversation status machine (the shimmer)

`ConversationStatus` drives the "Sending…/Thinking…" status bar
(`should_render_warping` in block/status_bar.rs keys off
`conversation.status().is_in_progress()`). The local flow owns it end to end:

- **Start:** `begin`/`begin_follow_up` mark InProgress.
- **During:** "Sending…" until the exchange output has messages, then
  "Thinking…" (`status_bar.rs`).
- **End:** the drain completes the exchange on the **`TurnCompleted` event**,
  not process EOF — the CLI lingers seconds after its result (plugin/MCP
  shutdown), EOF is only a fallback.
- `LocalHarnessStream::complete` then **stamps `ConversationStatus::Success`
  itself**. This is not optional: upstream's `mark_request_completed` only
  flips Success when the output has **no actions**; with actions it stays
  InProgress expecting the cloud action flow (execute → follow-up request) to
  finish the turn. Tool cards put actions in every local output.

**The two cloud gates** — local conversations must never reach cloud request
paths (they 401 under this fork's local auth):

- `controller.rs` `FinishedAction` subscription: returns early for local
  conversations (`local_harness_threads::lookup(...).is_some()`), next to the
  shared-session viewer gate. This is what stops the post-turn cloud
  follow-up. It must stay an **early return** — falling through to the
  status-update branch would stamp Success mid-run whenever a tool result
  lands.
- `history_model.rs` `mark_response_stream_completed_successfully`: skips
  server metadata fetch for conversations with an empty server token.

If you add a code path that reacts to finished actions/exchanges and might
send a request, gate it the same way: `local_harness_threads::lookup`.

## Known rendering trap (blocklist paint)

In `block_list_element.rs::paint`, one running `grid_origin` is carried
through the item loop; the Block arm advances it **as a side effect of
`draw_block(&mut grid_origin, …)`**. Never shadow `grid_origin` inside that
arm — a shadowed rebind made every item after a terminal block paint on top
of it (immune to scroll/resize). Per-block visual offsets must be applied to
the shared origin and reverted at the end of the arm.

## Golden rules for editing these paths

1. **Use Warp's representation, don't invent one.** Tool calls are
   `CallMcpTool` messages, presentation is feature flags/channel config,
   status is `ConversationStatus` — if you're patching a shared component to
   special-case fork content, stop and find the representation upstream
   already renders correctly.
2. **Local conversations never touch cloud request paths.** Gate with
   `local_harness_threads::lookup`. The 401 banner is the symptom.
3. **The stream owns the lifecycle.** Status flips happen in
   `LocalHarnessStream` (begin → InProgress, complete → Success), nowhere
   else.
4. **Provider differences stop at the parsers.** New CLI behavior belongs in
   `*_stream.rs`/`*_stream_exec.rs`, translated into `TurnEvent`.
5. **Run the mapper tests** (`cargo test -p warp turn_events_to_conversation`)
   after touching message shapes — a bad message wedges conversations (see
   the AddMessagesToTask trap).
