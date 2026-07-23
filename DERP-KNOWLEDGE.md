# Knowledge settings page — what each control actually does

_Settings → **AI** umbrella → **Knowledge** subpage._

Same treatment as [DERP-AGENT.md](DERP-AGENT.md): this is about the **runtime
behavior** of each control, not the toggle plumbing.

## The short answer

The Knowledge page is small — **three toggles and one button**, all rendered by a
single widget (`AIFactWidget`). None of the toggles does AI work itself. Each one
just persists a bool whose **runtime effect is to set a boolean flag in the agent
request** sent to derp-server; the server then does the actual work (loading your
Rules, or your Library, into the agent's context). The **button** is local
navigation — it opens the Rules collection in the Derp Drive panel.

So: the toggles are **local settings** that gate **server-side context injection**.
The data they gate (Rules, Library items) lives in **Derp Drive**, not on this page.

## Where this lives in the code

| Thing | Location |
|---|---|
| Subpage enum | `AISubpage::Knowledge` — [app/src/settings_view/ai_page.rs](app/src/settings_view/ai_page.rs) |
| Widget list for the page | Knowledge arm — [ai_page.rs:2877](app/src/settings_view/ai_page.rs) → pushes `AIFactWidget` |
| The whole page | `AIFactWidget` — [ai_page.rs:6863](app/src/settings_view/ai_page.rs) |
| Page gate | renders only if `FeatureFlag::AIRules` — [ai_page.rs:6988](app/src/settings_view/ai_page.rs) |
| Agent request flags | `rules_enabled` / `derp_drive_context_enabled` — [app/src/ai/agent/api/impl.rs:74](app/src/ai/agent/api/impl.rs) |
| A "Rule" object | Derp Drive AI Fact — [app/src/drive/items/ai_fact.rs](app/src/drive/items/ai_fact.rs), [app/src/ai/facts/mod.rs](app/src/ai/facts/mod.rs) |

## Enablement

Every control is gated on the global AI switch: the getters are
`is_any_ai_enabled(app) && <flag>` ([app/src/settings/ai.rs:1971](app/src/settings/ai.rs)).
There is no separate "Active AI" gate here (that only governs the Derp Agent
page's passive features). All three settings sync to cloud
(`SyncToCloud::Globally(Yes)`) and default **on**.

## The core mechanism (how Rules & Library reach the model)

When Derp builds an agent request, it sets two booleans in the request `Settings`
([impl.rs:74](app/src/ai/agent/api/impl.rs)):

```rust
rules_enabled: params.is_memory_enabled,
derp_drive_context_enabled: params.derp_drive_context_enabled,
```

built from the settings getters ([app/src/ai/agent/api.rs:208](app/src/ai/agent/api.rs)):

```rust
let is_memory_enabled = ai_settings.is_memory_enabled(app);
let derp_drive_context_enabled = ai_settings.is_derp_drive_context_enabled(app);
```

The client does **not** inline your rule text or library contents into this
request — it sends the flags, and **derp-server injects the corresponding Derp
Drive data server-side**. (No fact/library content is attached client-side in the
agent API path.)

> **Fork note:** because application is server-side, these flags only have effect
> if the underlying Rules / Library objects are available to the server (i.e.
> synced). Worth keeping in mind given this fork's local-Drive direction — see
> `docs/local-drive.md`.

---

## Control-by-control

### Rules  (toggle)
- **Setting:** `memory_enabled` → `is_memory_enabled` (toml `agents.knowledge.rules_enabled`)
- **Render:** `render_rules_toggle` — [ai_page.rs:6872](app/src/settings_view/ai_page.rs); action `ToggleRules`.
- **What a "Rule" is:** a Derp Drive **AI Fact** object — `AIFact::Memory { content, name }`,
  `DriveObjectType::AIFact`, model type name **"Rule"**
  ([ai_fact.rs](app/src/drive/items/ai_fact.rs), [facts/mod.rs:19](app/src/ai/facts/mod.rs)).
  Rules capture your conventions/workflows so the agent follows them.
- **Runtime effect:** sets `rules_enabled: true` in the agent request. The server
  loads your saved Rules into the agent's context. Toggle itself = local settings write.
- **Verdict:** local toggle → **server-side** context injection; rule data lives in Derp Drive.

### Suggested Rules  (toggle)
- **Setting:** `rule_suggestions_enabled_internal` → `is_rule_suggestions_enabled`
  (toml `agents.derp_agent.active_ai.rule_suggestions_enabled`); only shown when
  `FeatureFlag::SuggestedRules` is enabled ([ai_page.rs:7021](app/src/settings_view/ai_page.rs)).
- **Render:** `render_rule_suggestions_toggle` — [ai_page.rs:6924](app/src/settings_view/ai_page.rs); action `ToggleRuleSuggestions`.
- **Runtime effect:** when on, the agent/server proposes new rules based on your
  interactions; the client surfaces them as a **suggestion chip in the block
  output** and, on click, a "save this rule" dialog
  ([app/src/ai/blocklist/block.rs:2576](app/src/ai/blocklist/block.rs) →
  `suggested_rule_modal::SuggestedRuleAndId`). Accepting saves a new Rule (Drive AI Fact).
  A "disable rule suggestions" affordance is wired alongside it.
- **Verdict:** local toggle → gates a **server-proposed** rule surfaced as a chip;
  saving writes a Derp Drive object.

### Manage rules  (button)
- **Action:** `OpenAIFactCollection` — [ai_page.rs:7013](app/src/settings_view/ai_page.rs).
- **Runtime effect:** emits an event ([settings_view/mod.rs:1926](app/src/settings_view/mod.rs))
  that opens the Rules/Knowledge collection in the **Derp Drive panel**, split right
  (`open_ai_fact_collection_pane`, [workspace/view.rs:14719](app/src/workspace/view.rs)),
  and fires a `KnowledgePaneOpened` telemetry event. It's where you view/edit/delete
  your saved Rules.
- **Verdict:** **local UI navigation.** No AI request; opens a Drive object list.

### Library as agent context  (toggle)
- **Setting:** `derp_drive_context_enabled` → `is_derp_drive_context_enabled`
  (toml `agents.knowledge.derp_drive_context_enabled`)
- **Render:** `render_derp_drive_context_toggle` — [ai_page.rs:6952](app/src/settings_view/ai_page.rs); action `ToggleDerpDriveContext`.
- **Runtime effect:** sets `derp_drive_context_enabled: true` in the agent request.
  The server then uses your Derp Drive **Library** — Workflows, Notebooks, and
  Environment Variables — as additional agent context.
- **Verdict:** local toggle → **server-side** context injection from your Library.

---

## Data that leaves the machine (privacy view)

| Control | On the agent request |
|---|---|
| Rules | flag `rules_enabled: true` (server pulls your Rules) |
| Suggested Rules | gates server-proposed rule chips; accepting one saves a Drive object |
| Manage rules | nothing — local navigation |
| Library as agent context | flag `derp_drive_context_enabled: true` (server pulls your Library) |

Unlike the Derp Agent page's passive features (which POST command/output/diff
context to `/ai/…` endpoints), the Knowledge toggles send no content of their own —
they flip flags on the main agent request, and the corresponding Derp Drive data is
applied server-side.
