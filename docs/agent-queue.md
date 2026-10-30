# Agent queue, Auto models, and the chooser

Several agents at once, spread over the free models you have: on this Mac (Ollama and BYOR keys),
on other machines' Ollama, and, through Moloch, on machines running acolyte. One chooser decides
which model takes which task, and the same algorithm runs in the app and in Moloch.

**Status:** written, not yet compiled. Nothing here has been through a build; `task_router.rs` and
the pure parts of Moloch's code were checked with standalone `rustc`. Build, then test by hand.

## The pieces

| | Where | What it does |
|---|---|---|
| Chooser | `app/src/ai/task_router.rs`, Moloch `chooser.rs` | `decide(task)` rates difficulty (an explicit `difficulty:` line wins, else signals in the text); `strength(model)` and `speed(model)` rate a model from its name, price and context; `choose(policy, difficulty, candidates)` is the one entry point. Golden cases: `docs/chooser-golden.json`, the same file in Moloch; both must give the same answers. |
| Auto models | `api_providers.rs` (`auto_models`, `resolve_auto_model`), `harness_availability.rs`, `terminal/view.rs` | "Auto (cost-efficient / responsive / genius)" in the model picker. Each turn resolves to one failover row and the conversation shows the model that ran. Never below medium difficulty (a short follow-up is not easy work). |
| Queue | `app/src/ai/agent_queue.rs` | Headless tasks, one running per account, each to the model the chooser picks; a failed task retries a tier up on another model (3 attempts). Saved between runs. Transcripts live in memory. |
| Panels | `settings_view/ai_page.rs` (Agent queue, under the failover list), `workspace/view/conversation_list/view.rs` (QUEUE block in the left panel) | Add tasks, load a plan, start/stop; click a task in the left panel for a view-only tab (`WorkspaceAction::OpenQueueTaskTranscript`, built in memory, not saved with conversations). |
| Orchestrator | `app/src/ai/queue_tools.rs` | `plan_tasks` and `collect_results` tools for the API and Ollama tool loops: a model splits a goal into independent tasks, the queue runs them, the model collects the answers. The local counterpart of Warp's `run_agents`. |
| Plans | reverse-lab `tools/plan.py` | `notes/plan.json`: lanes (mechanical, exceptions, gui, serial); load it in the queue panel. |

## One pool, two rules

The failover list is the pool of models: which you have, on or off, free-only, health. Agents and
failover use it differently.

- **Failover** (a request failed, keep the conversation going) sends the same request to a model
  about as capable as the one that failed, or more, nearest first; weaker ones come after. List
  order only breaks ties, and a local agent app (Claude Code, Codex) stays after the API rows.
  `failover_order` in `task_router.rs`, applied in `failover_routes`.
- **Agents** (the queue, Auto) pick per task by difficulty with the chooser; each task starts
  fresh, so there is no conversation to protect.
- **Tiers.** Each row has a "tier" button: auto (the guess from the model's name, price and
  context), easy, medium or hard. A tier the user sets wins over the guess (`byor_model_tiers`,
  `route_strength`); the name still orders models within a tier.

## Capabilities

A task can say what a model must be able to do to take it: `needs` (`vision`, `image_out`, `embed`)
on a queue task, on a plan lane or task, or in `plan_tasks`. The queue, the Auto entries and
failover only consider models that have those capabilities (`app/src/ai/model_caps.rs`); the
chooser itself is unchanged and sees the filtered candidates. A model's capabilities come from, in
order: the vision button on its Agents-page row (stored as `byor_model_caps`), what the provider
reported (Ollama's `/api/show`, asked once per model and kept in `model_caps.json`; OpenRouter's
model list), a guess from the name for a hosted model, and for Claude Code and Codex a fixed yes
for vision. A model nothing is known about has none, so an image task never goes to it; text
tasks are unaffected. A task nothing on offer can take waits with the reason on its row.

Images go with a turn two ways: staged in the input (the turn then needs vision, for Auto and
failover too), or as `images` on a queue task, plan task or `plan_tasks` task (paths, relative to
the working directory). `harness/turn_images.rs` holds them in the history beside the text and
converts them when a request is sent: Ollama's `images` list, or OpenAI-style content parts. Claude
Code and Codex get file paths in the prompt. Not done: a turn that failover hands to Claude Code or
Codex loses its images, and the runbook and committee modes ignore them.

## Local and Moloch

The task format is the same in both: title, prompt, difficulty (a `difficulty:` line in the prompt),
workspaces. With no Moloch the app's queue runs tasks on the failover rows. With Moloch, a run is
sent as `/agent/run` with `model_id: "auto"` (or `auto/<policy>`); Moloch's assignment picks among
the models its connected workers advertise (`resolve_auto_model` in Moloch `lib.rs`) and the run
carries the model it was given. Runs show in the left panel as cloud runs do.

## Workspaces

A run can carry files: `config.workspaces = [{id: <sha256>, name}]`. Bundles are `.tar.gz`, stored by
hash in Moloch (`PUT/GET /api/v1/workspaces/{sha256}`, quota with least-recently-used eviction,
`MOLOCH_WORKSPACE_DIR`, `MOLOCH_WORKSPACE_QUOTA_GB`), unpacked by acolyte into the guest at
`/work/<name>` and `/work/.orig/<name>`; the run's changes come back as a patch, stored the same
way, named in the transcript. `scripts/moloch-run.py` in the Moloch repo builds bundles (git's file
list, no secrets, size cap), uploads, submits, waits and applies the patch. Repositories (a remote
and a commit) are a separate task.

## Not done

- Building and testing any of the Rust above.
- The app sending runs to Moloch with workspaces from its own UI (the script does it).
- `/mnt/terra` on sentience as the workspace store and its quota; the guest image needing the
  toolchains.

## Pictures: `generate_image` and `imagine`

`app/src/ai/image_gen.rs` makes pictures for two front doors:

- **`generate_image`**, a tool next to `plan_tasks`: the Ollama and API tool loops get it in
  process, Claude Code and Codex over the queue MCP server (socket op `image`). It answers with
  JSON: the saved `path`, the size, and a `markdown` image to put in the reply.
- **`imagine "a chrome push button"`**, a command in every Krust shell (the bundle's
  `Contents/Resources/bin`, already on the shells' PATH, so nothing to install). Hidden worker mode
  `image-cli` of the `krust` binary: it sends the request to the running app over the queue socket
  and prints the path, or `--json`, and draws the picture in the block when the terminal draws Kitty
  graphics (`TERM_PROGRAM=KrustTerminal`; `--show` / `--no-show` override). Flags: `--out`, `--size`,
  `--seed`, `--ref`, `--transparent`, `--backend command|pool`, `--cols`.

Backends, tried in turn (`image_gen::plan`): the **local command** (setting `image_command`, else
`KRUST_IMAGE_COMMAND`, else `~/github/image-lab/bin/imagine-engine`: any program taking
`imagine.py`'s flags and printing one JSON line with the saved `path`) and then every **pool model with
the `image_out` capability** (a custom endpoint's or provider's `/images/generations`, or an Ollama
`/api/generate` on a build that still has image generation). Ollama removed its image generation in
0.32.6 (still gone in 0.40.13), so its `x/flux2-klein` and `x/z-image-turbo` need 0.32.5 or earlier.
Ollama lists these models with the capability `image`, which maps to `image_out`.
