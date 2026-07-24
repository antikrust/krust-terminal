# MOLOCH — the broker between the app's AI calls and a machine that has AI

> *"Moloch whose mind is pure machinery!"* — the hollow idol that appears
> all-powerful and has nothing inside, fed by offerings carried from
> elsewhere. This service is exactly that: it answers the app as though it
> were the AI, and contains none. It replaces Derp's cloud agent (Oz).

> **The server now lives in its own private repo: `/Volumes/Instruments/moloch`**
> (proprietary; it also holds **acolyte**, the conduit that runs beside the
> models on the AI machine). It was moved out of this workspace because this
> fork is AGPL-3.0-only and a crate here would inherit AGPL. Nothing was lost
> in the move — Moloch never depended on the app.
>
> **What stays in this repo** is the client half: [`app/src/moloch.rs`](../app/src/moloch.rs)
> (config that points the app at a Moloch), the `AuthState` credential-order
> fix, and this document. Sections below describing the server's internals
> and slots refer to code in that other repo.

Moloch is the fork's replacement for the cloud backend the app already talks
to for AI. The app is built to send AI work to a server ("the cloud"); Derp's
servers are gone in this fork. Moloch stands in for them — **but it owns no
AI**. It is a *broker*:

```
  the app (any local machine)          MOLOCH              an executor (has AI)
  ───────────────────────────      ──────────────      ────────────────────────
  a manager-settings AI function ─▶  queue the job  ◀── poll ──  acolyte beside
  or a cloud agent run               (SQLite)                    Ollama models,
                                          │                      a local harness,
                       answer  ◀──────────┘  ◀── result ──       or the built-in
                                                                 executor
```

The app's AI call sites hit the server; the server queues the work; a machine
that *actually has AI* claims it, does it, and posts the result back; the
server answers the original caller **as though it were the external AI**. No
AI runs on the request path — the server is a relay. "Rigging up some AI on
the server itself" is a later, optional step (the built-in executor, below).

This is why wiring the client is not about new endpoints: these are the
**existing pieces the app already sends to the cloud**, now pointed at a
server we control. Terminal chat still runs through local harnesses
(agent-flow-map.md); this backs the *ambient / cloud-agent* surfaces and the
manager-settings AI functions.

The service is `account_server`'s sibling and holds the same invariants
([SERVER-DESIGN.md](../SERVER-DESIGN.md)): standalone crate, one binary, one
SQLite file in WAL mode behind a mutex, bearer-token auth (only a digest at
rest), pinned self-signed TLS, config in env / `.env`.

## Where the API came from

The endpoint surface is the **Derp/Oz Agent API** as published at
`https://therob.lol/derp-terminal/help/api/` (the rebranded derp.baby docs —
see [[derp-terminal-website]]). Base URL `https://app.derp.baby/api/v1`, bearer
auth. The machine-readable `openapi.json` is the ground truth; the endpoint
list below mirrors it. This replaced my first draft, which reverse-engineered
the shapes from the client's `ServerApi` Rust code — the docs are
authoritative where the two disagree.

## The two executor models

**External worker (the default, the point of the design).** A machine with AI
long-polls `POST /api/v1/worker/poll`, advertising the job `kinds` it takes
and the `models` it can serve. It claims a job, does the work, and reports
back — streaming an agent run's transcript/events via the run endpoints, or
returning a function's answer via `POST /api/v1/worker/jobs/{id}/complete`.
This is how "the local machine deals with the task then sends it back": your
laptop's `claude`/`codex` harness, or **the Linux box serving its Ollama
models** (it has the memory to run several — each is advertised in `models`
and shows up in `GET /agent/models`).

**Built-in executor (optional, off by default).** `MOLOCH_BUILTIN_EXECUTOR=1`
runs an in-process executor that claims the same jobs and runs a local CLI
harness *on the server box itself* — the "rig up some crappy AI on the server"
path. It uses the exact headless invocation the fork's local Claude harness
uses (`claude --print --output-format stream-json …`). Needs `claude`
authenticated for the service user. Default off: the server is a pure broker
and jobs wait for an external worker.

Either way, findings land in the same tables and flow back to local machines
identically.

## The API slot inventory

All slots under `/api/v1` require `Authorization: Bearer <token>` except
`/api/v1/health` and the capability-URL blob upload. Paths mirror the Oz API
docs; where a slot is fork-specific (the relay's worker side, the
notifications inbox) it is marked.

### Agent runs — the Oz lifecycle

| Slot | Notes |
|---|---|
| `POST /api/v1/agent/run` · `POST /api/v1/agent/runs` | Both spawn forms. Creates a run (state `QUEUED`) and enqueues an `agent_run` job. `at_capacity: true` when no executor is available to take it (honest: with the built-in executor off and no worker connected, it says so). Returns `{task_id, run_id, at_capacity}` |
| `GET /api/v1/agent/runs?limit&cursor&state&q&sort_by&sort_order&created_after&created_before&…` | Paginated list, `AmbientAgentTask` wire shape. All the Oz filter params are accepted; unknown ones ignored |
| `GET /api/v1/agent/runs/{runId}` | Full run record. UPPERCASE states, every non-defaulted client field present, plus fork extras (`harness`, `harness_session_id`, `executed_by`, `result_summary`) |
| `GET /api/v1/agent/runs/{runId}/transcript` | Raw stream-json NDJSON — the findings local machines already render. A worker-uploaded transcript blob wins over recorded lines |
| `GET /api/v1/agent/runs/{runId}/timeline` | Chronological lifecycle events (from `run_events`) |
| `POST /api/v1/agent/runs/{runId}/followups` | Queues a follow-up + re-enqueues the run so an executor resumes the session |
| `POST /api/v1/agent/runs/{runId}/cancel` · `POST /api/v1/agent/tasks/{taskId}/cancel` | Cancels: signals a live built-in run, sets `cancel_requested` for an external worker to observe, cancels queued jobs, settles queued runs |
| `GET /api/v1/agent/artifacts/{artifactUid}` | Reported artifact payload, verbatim |

### The AI-manager functions — on the app's own paths (WORKING)

Served on the exact paths + JSON shapes the client's `ServerApi` already
POSTs to its server root, so they light up with **zero client changes** once
the toggles on the AI page are on:

| Slot | Feature | Client call site |
|---|---|---|
| `POST /ai/generate_input_suggestions` | **Next command** ghost text (`is_intelligent_autosuggestions_enabled`) | `ServerApi::generate_ai_input_suggestions` → `next_command_model.rs` |
| `POST /ai/generate_am_query_suggestions` | **Prompt suggestion** banner after a command (`is_prompt_suggestions_enabled`, legacy path) | `ServerApi::generate_am_query_suggestions` |
| `POST /ai/predict_am_queries` | **NL autosuggestions** while typing an agent query (`is_natural_language_autosuggestions_enabled` + `FeatureFlag::PredictAMQueries`) | `ServerApi::predict_am_queries` |

Execution order per request: the **gateway** when configured (direct provider
call — ghost text is latency-sensitive), else the **relay** (an acolyte
answers), else 503. The client's own rule that `most_likely_action` must
extend the typed prefix is enforced server-side. The gateway
(`MOLOCH_GATEWAY=ollama|anthropic|openai` + `MOLOCH_GATEWAY_MODEL`) is
Moloch's own provider line-out with keys held on the server — see the moloch
repo's `.env.example`. Not yet reconstructed: `/ai/relevant_files` (needs a
codebase index) and the passive-suggestions multi-agent flavor.

### The relay — AI functions and the worker side (fork-specific)

| Slot | Notes |
|---|---|
| `POST /api/v1/ai/function` | **The manager-settings AI functions call here.** `{kind, input, prompt?}` → enqueues → **blocks** until an executor delivers a result → `{ok, output, error}`. `503 "no executor available"` if none answers within `MOLOCH_FUNCTION_TIMEOUT_SECS`. `kind` is one of `relay::FUNCTION_KINDS` (prompt/code suggestions, next_command, git_ops, …) |
| `POST /api/v1/worker/poll` | An executor claims the oldest matching job. `{worker, kinds, models}` → `{job: {id, kind, payload}, run}` (the full run record is included for `agent_run` jobs). Records the heartbeat that makes the worker "connected" |
| `POST /api/v1/worker/jobs/{jobId}/complete` | Executor delivers a result: `{ok, output, error}`. Unblocks a waiting function request; settles an agent run |
| `POST /api/v1/agent/runs/{runId}/transcript-append` | An external worker streams `{lines, event?}` as the harness produces output, so watchers see findings live. Returns `{cancel_requested}` so the worker knows to stop |
| `GET /api/v1/agent/connected-self-hosted-workers` | Workers heartbeating within 30s, with their advertised kinds + models |
| `GET /api/v1/agent/notifications?since&limit` | Human-readable findings inbox: progress notes (`notify-user`) + one `finished` entry per settled run |

### Resources

| Slot | Notes |
|---|---|
| `GET /api/v1/agent/models` | Models available for runs: the connected workers' advertised models (the Linux box's Ollama models), plus the built-in harness's Claude aliases when that executor is on. First is default |
| `GET /api/v1/agent/environments` | Empty — execution is local machines / this box, no cloud environments |

### Scheduled agents (cron-triggered runs)

Full CRUD per the Oz schedules API: `POST/GET /api/v1/agent/schedules`,
`GET/PUT/DELETE /api/v1/agent/schedules/{id}`,
`POST …/{id}/pause`, `POST …/{id}/resume`. Rows are stored; **the cron trigger
loop that actually spawns runs from due schedules is the one piece not yet
built** (a tick that reads due, non-paused schedules and calls the spawn path).

### harness-support (worker → server, wire parity with Oz)

The full `HarnessSupportClient` surface so an external worker phones home
exactly as the app's `derp agent run` binary would to the cloud. Run
identified by the `X-Derp-Cloud-Agent-ID` header. Slots:
`external-conversation`, `resolve-prompt`, `transcript` (POST target / GET
download), `block-snapshot`, `upload-snapshot`, `report-artifact`,
`notify-user`, `finish-task`, `report-shutdown`; plus
`PUT /api/v1/blob-upload/{id}` backing every `UploadTarget` (capability URL,
not presigned GCS — the unguessable id is the credential).

### Handoff + conversations

`POST /api/v1/agent/handoff/upload-snapshot`,
`POST /api/v1/agent/conversations/{id}/fork`, `POST …/rename`.

### Empty collections / declared stubs

Registries this server doesn't keep answer with empty collections (`GET
/api/v1/agent` skills, `/api/v1/agent/identities`, `/api/v1/memory_stores`).
The interactive protobuf-SSE loop (`/ai/multi-agent`) and the legacy GraphQL
surface (`/graphql/v2`) return a self-describing `501` — terminal chat stays
local, so they are declared, not built. `GET /agent/runs/{id}/conversation`
and `GET /agent/conversations/{id}` (normalized-message view) and the agent
identity CRUD mutations are declared slots too.

## Configuration — env and `.env`

Every setting is an env var; all can also live in a `.env` file loaded at
startup (`MOLOCH_ENV` to relocate it; the real environment always wins).
**The server address is the thing that changes between machines, so it belongs
in `.env`, not code** — copy `.env.example` and edit. Full list in
`.env.example` and `main.rs`; the load-bearing ones:

| Variable | Default | Meaning |
|---|---|---|
| `MOLOCH_BIND` / `MOLOCH_PORT` | `0.0.0.0` / `10102` | listen address (non-web port, next to account-server's 10101) |
| `MOLOCH_PUBLIC_URL` | unset | absolute base for minted upload URLs; set to the address workers/app dial |
| `MOLOCH_TOKEN` | minted into `api-token` (0600) | bearer token; only its path is logged |
| `MOLOCH_TLS` / `MOLOCH_TLS_DIR` | on / state dir | pinned self-signed TLS; `0` only behind localhost/VPN |
| `MOLOCH_BUILTIN_EXECUTOR` | `0` (off) | run the optional in-process "AI on the server" executor |
| `MOLOCH_HARNESS_PROGRAM` | `claude` | built-in executor's harness (only when enabled) |
| `MOLOCH_MAX_CONCURRENT_RUNS` | `2` | built-in executor worker loops |
| `MOLOCH_TURN_TIMEOUT_SECS` / `MOLOCH_FUNCTION_TIMEOUT_SECS` | `3600` / `60` | per-turn cap / how long a function waits for an executor |

Build/deploy like account-server: `cargo build --release -p moloch (in the moloch repo)`
→ one binary, bundled SQLite. If the built-in executor is on, use a **fixed
service user, not `DynamicUser`** — the harness needs a stable `$HOME` for its
persisted `claude` credentials. Otherwise mirror account-server's systemd
hardening.

## Pointing the app at Moloch — DONE

`app/src/moloch.rs` is the client side. Because Moloch deliberately speaks the
protocol the app already speaks, two settings do the whole job — **no new call
sites**, just the existing cloud ones landing somewhere we control:

| Setting | Effect |
|---|---|
| `MOLOCH_URL` | becomes `ChannelState::server_root_url()`, so every `/api/v1/agent/*` request the app already makes goes to Moloch |
| `MOLOCH_TOKEN` | becomes the API-key credential, so those requests carry `Authorization: Bearer <token>` — what Moloch checks |

Resolution order (first hit wins): **environment**, then
**`<state_dir>/moloch.env`** (same `KEY=VALUE` dialect as the server's `.env`).
The address changes between machines, so it lives in a file — repointing the
app never needs a rebuild. With nothing configured the app keeps its built-in
URL and normal auth, so this is inert until you opt in.

```bash
# <state_dir>/moloch.env
MOLOCH_URL=https://192.168.1.50:10102
MOLOCH_TOKEN=<contents of the server's api-token file>
```

**Two things had to change beyond config**, both found by actually running the
app against a live Moloch:

1. **The app bootstraps over GraphQL, not REST.** The first authenticated
   thing it does is a `GetUser` query; a failure there aborts login before any
   `/api/v1` call is attempted (symptom: `Authentication failed: … Failed to
   fetch user response data: 501`). So `POST /graphql/v2` answers `GetUser`
   (plus the settings/telemetry writes that follow) with a synthetic local
   identity. It is not a GraphQL implementation — it dispatches on the
   operation name and returns a canned document; unknown operations come back
   as a GraphQL `errors` document naming the operation, so gaps are legible in
   the app's log. `__typename: "UserOutput"` is load-bearing (the client's
   `UserResult` is an inline-fragment union and picks the variant by it), and
   the LLM choice lists are deliberately empty because this fork's model
   picker is served locally by `default_harnesses()`.
2. **An API key now beats the synthetic test user** in
   `AuthState::initialize` (`crates/derp_server_auth`). The fork pins
   `should_use_test_user()` to `true` so the app works with no account, but
   those credentials are `SessionCookie` → `AuthToken::NoAuth` → **no
   `Authorization` header at all**, so a configured token could never reach
   the server and every request 401'd (`missing bearer token`). The two
   branches are reordered so a real credential wins; the stand-in *user* is
   still set, so every user-dependent gate in the fork behaves exactly as
   before — only the wire credential changes.

Two further wiring details worth knowing:

- The override is applied in `run()` **before** upstream's
  `--server-root-url` handling, so an explicit CLI flag still wins for a
  one-off, and it is **not** gated on `allows_server_url_overrides()` (that
  gate exists to stop shipped Derp builds being redirected off Derp's
  servers — in this fork that redirect is the point).
- The app's API-key path prefixes tokens with `wk-`
  (`derp_server_auth::API_KEY_PREFIX`), so Moloch accepts the token **with or
  without** that prefix. The plain token from `api-token` works from the app,
  curl, and worker scripts alike.

### Verified end to end

With `MOLOCH_URL`/`MOLOCH_TOKEN` set and a live server, the real `derp`
binary:

- logs `server_root_url: "http://127.0.0.1:18104"` — pointed at Moloch;
- completes its auth handshake against Moloch's GraphQL bootstrap;
- runs `derp agent skills` and `agent list` successfully against
  `/api/v1/agent` and `/api/v1/agent/identities`;
- reads back a run that an external worker claimed, streamed a transcript
  for, and completed (`state: SUCCEEDED, executed_by: worker`).

### Still to do on the client

1. **TLS pin** — with `MOLOCH_TLS` on, the app must pin the server's
   self-signed fingerprint the way `account_client` already does. Until then
   use `MOLOCH_TLS=0` behind localhost/VPN, or a fingerprint the OS trusts.
2. **Manager-settings AI functions** — repoint each function's call site
   (today: `ServerApiProvider` → cloud GraphQL / multi-agent, audited in
   [ai-settings-functions.md](ai-settings-functions.md)) at
   `POST /api/v1/ai/function` with the function's `kind`. The transport is
   live and tested; this is per-function client plumbing.
3. **A worker adapter** — the piece that makes a local machine *be* the AI:
   poll `/api/v1/worker/poll`, run the existing local harness
   (`claude`/`codex`/Ollama), stream back via `transcript-append` +
   `worker/jobs/{id}/complete`. The fork's local-harness runner is liftable
   almost as-is. Until one runs, set `MOLOCH_BUILTIN_EXECUTOR=1` so the box
   serves itself.

## Working on it

- Tests are round-trips against the real router + temp-file SQLite, no mocking
  (`src/lib_tests.rs`): the broker→worker relay, the blocking AI-function
  round trip, the built-in executor's full spawn→subprocess→findings pipeline
  (stand-in `sh` harness), cancel, follow-ups, schedules. `cargo test -p
  moloch`.
- Migrations are embedded diesel migrations; add new ones, never edit shipped
  ones.
- Wire shapes mirror the client types by hand (`AmbientAgentTask`,
  `AgentRunEvent`, the harness-support bodies) — the SERVER-DESIGN.md "mirror,
  don't link crates" rule.
- Not yet built: the schedule cron-trigger tick; the normalized-conversation
  view; agent-identity CRUD; `/ai/multi-agent` (deliberate — chat is local).
