# Local harness lifecycle: how a CLI agent run flows through Krust

This fork routes every terminal agent prompt to a **local CLI harness**
(`claude`, `codex`, or direct local `ollama`) instead of Krust's cloud "Oz" backend. This document is
the map of that pipeline for anyone — human or AI — editing it. The flows
below reuse Krust's own machinery deliberately; each section ends with the
traps that machinery hides. **Read the traps before changing anything.**

The companion history/plan doc is [agent-flow-map.md](agent-flow-map.md);
this file describes the code as it works today.

---

## Pipeline overview

```
Enter in agent input
  → input.rs submit: Event::RunLocalClaudeQuery { prompt, harness, model_id, reasoning_level }
  → TerminalView::run_local_claude_query (terminal/view.rs)
      spawns the CLI/API client → claude_stream_exec.rs / codex_stream_exec.rs / ollama_stream_exec.rs
      parses provider output    → provider parser → TurnEvent
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
  `stream_json_args` (claude), `codex_exec_args` (codex), and
  `ollama_curl_args` plus a JSON request body (ollama).
- Model + reasoning come from the picker via the event, else from
  `persisted_model_for(harness)` (ai/harness_availability.rs), stored
  per-harness in `CloudAgentSettings.last_selected_harness_model`.
  Reasoning maps to `MAX_THINKING_TOKENS` (claude env) or
  `-c model_reasoning_effort` (codex flag).
- Ollama is a separate picker provider and uses the native local generate API.
  Code Llama and the original DeepSeek Coder reject Codex's tool schema, so
  the adapter uses a constrained structured response (`shell` or `text`)
  instead. Shell requests execute in the terminal working directory and emit
  the same `ToolCallStarted` / `ToolCallResult` events as the CLI harnesses, so
  Krust renders its normal command cards and multiline output. The final
  `context` token array is retained per local session so follow-ups remain
  conversational.
  Requests deliberately do not override Ollama's context, generated-token,
  CPU-thread, or prompt-batch settings. Both installed models advertise a
  native 16K context window, which the Ollama service is configured to use.
  The runner still unloads after five idle seconds so its allocation is not
  retained after a turn. Model text is decoded from the streamed structured
  JSON and appended to one stable message in small chunks, so chunk boundaries
  never become line endings.
  If the local API is not running, the adapter starts `ollama serve` directly
  as a headless background process and waits for `/api/version` before sending
  the turn. It never opens `Ollama.app`, so Ollama has no separate window,
  Dock activation, or visible startup step in the integrated flow.
- Cloud API models (`LocalHarnessKind::Api`, `api_stream_exec.rs`) are Z.ai,
  Groq, and Gemini over the OpenAI chat-completions API, run with the user's
  own key (`ai/api_providers.rs`; keys in the keychain under
  `ApiProviderKeys`, set on Settings → AI → Bring your own key). Model ids are
  `api/<provider>/<model>`; like Ollama's they ride the Codex picker row and
  are split out by prefix, and a provider's models are listed only once its
  key is set. The runner is the Ollama tool loop's twin — same built-in tools
  and exposed MCP servers, same `TurnEvent`s — over curl with SSE parsing.
  The key reaches curl through its environment (`--variable` +
  `--expand-header`, curl 8.3+), never its argv. Nothing goes through Krust's
  cloud.
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
| `Text`               | `AgentOutput`                               | complete markdown prose     |
| `TextDelta`          | appended to one stable `AgentOutput`        | live markdown prose         |
| `Thinking`           | `AgentReasoning`                            | collapsible "Thinking" block|
| `ToolCallStarted`    | `ToolCall` → `CallMcpTool`                   | inline read-only tool card  |
| `ToolCallResult`     | `ToolCallResult` → `CallMcpToolResult`       | result inside the same card |
| `TurnCompleted` (err)| `AgentOutput` error block                   | error text                  |
| `SessionInit`/`TurnCompleted` (ok) | nothing — bookkeeping only    | —                           |

Shell correlation: `turn_event_to_messages` takes a `shell_commands`
registry (`call_id` → command) to pair the shell call and result while keeping
both in the generic inline-card representation. It must never translate a
local transcript into `RunShellCommand`, because that message type means
"create/restore a terminal block" elsewhere in Krust.

Why `CallMcpTool`: it is Krust's **generic "named tool + JSON args"
representation** — exactly what a harness tool call is. The card
(`RequestedCommandView`, type `McpTool`) is collapsed by default with a
working chevron, and its detail renders as the interactive JSON tree behind
`FeatureFlag::McpJsonTreeView` — enabled for this build in
`app/src/bin/oss.rs`.

### Shell tools are inline transcripts (implemented 2026-07-18)

Claude `Bash`, Codex `Shell`, and Ollama `Shell` events describe commands the
harness already ran. They therefore use Krust's generic named-tool card
(`CallMcpTool` / `CallMcpToolResult`) and a view-only action result. This is a
representation choice, not an MCP transport claim. Using
`RunShellCommand`/`RequestCommandOutput` here makes the command card expand
into a real terminal block below the AI block—the duplicate layout this path
must avoid. Command and multiline output stay together in the collapsible
grey card for live and restored conversations.

- **Output display — inside the card, never as a separate block.** The live
  drain registers a `CallMCPToolResult`; restore reads the matching persisted
  `CallMcpToolResult` message. Neither shape feeds the command-block extractor.
- **The exchange still splits at tool results**
  (`LocalHarnessStream::continue_after_tool_results`, driven by the drain's
  `local_harness_pending_tool_calls` phase tracking): that is the cloud
  turn shape and exactly the exchange structure restore derives from the
  persisted messages, so live and restored conversations agree. The
  continuation's inputs are the phase's `ActionResult`s, not `UserQuery`s
  — no spurious query bubbles.
- The command-permission autonomy speedbump ("always ask…") is gated off
  for view-only action models (block.rs) — transcripts never execute, so
  execution-permission banners are noise; this also fixes shared-session
  viewers.
- Shell failures remain errors on the same inline card; no synthetic terminal
  exit block is created.

**Traps:**
- Do **not** patch shared renderers (`requested_command.rs`, markdown, etc.)
  to change how fork content displays. Presentation changes go through Krust's
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
(`should_render_krusting` in block/status_bar.rs keys off
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

## Running commands ("Use agent")

Upstream's "Use agent" on a command the user is running tags the cloud
agent in so it can type into the command. A local harness runs in its own
process and can't, so for a local terminal
`TerminalView::explain_running_command_with_local_agent`
(terminal/view/use_agent_footer/mod.rs) shows a toast saying so instead,
and, when the conversation on screen is a local one that can take another
turn, sends its agent a follow-up carrying the command and its screen.
Shared-session viewers still tag in: their prompts go to the sharer.

**Traps:**
- A **fresh** local run can't show while a command is running in the
  fullscreen agent view. `LocalHarnessStream::begin` always starts a new
  conversation and the drain then switches the agent view to it, but
  `AgentViewController::try_enter_agent_view` refuses that while a command
  runs: the user gets "Cannot enter agent mode while a command is
  running." and the reply streams into a conversation nothing shows. Only
  follow-ups (`begin_follow_up`, appending to the conversation on screen)
  work then. Never `begin_follow_up` an empty conversation instead: its
  root task is still optimistic, and appending wedges it (see the
  AddMessagesToTask trap above).
- Runbook mode treats any word of a prompt that names an existing file as
  the runbook to run, so don't route prompts carrying terminal output into
  it.

## Known rendering trap (blocklist paint)

In `block_list_element.rs::paint`, one running `grid_origin` is carried
through the item loop; the Block arm advances it **as a side effect of
`draw_block(&mut grid_origin, …)`**. Never shadow `grid_origin` inside that
arm — a shadowed rebind made every item after a terminal block paint on top
of it (immune to scroll/resize). Per-block visual offsets must be applied to
the shared origin and reverted at the end of the arm.

## Golden rules for editing these paths

1. **Use Krust's representation, don't invent one.** Tool calls are
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
5. **Run the mapper tests** (`cargo test -p krust turn_events_to_conversation`)
   after touching message shapes — a bad message wedges conversations (see
   the AddMessagesToTask trap).
