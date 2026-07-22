# Multi-agent committee design (Ollama)

Written: 2026-07-21

This is the design for 0term's local multi-model "committee" mode: one prompt fans out
across several installed Ollama models, machines referee everything they can, an LLM
judge rules on the rest, and a narrated verdict says what won out. It is deliberately
slow, verbose, and theatrical — the stages stream into the conversation as they happen.

If the change touches local Agent Mode message creation or lifecycle, read
[docs/local-harness-lifecycle.md](docs/local-harness-lifecycle.md) first. The committee
is a normal local-harness runner: it emits the same `TurnEvent` stream as every other
runner and must not bypass that protocol to render its stages.

## TL;DR

```text
User prompt (model picker: "Committee")
  -> run_local_claude_query            (terminal/view.rs, LocalHarnessKind::Ollama)
  -> run_ollama_committee              (harness/ollama_committee.rs)
       Stage 0  Assemble    enumerate installed models, probe caps, assign roles
       Stage 1  Evidence    git diff HEAD (Review mode) — or none (Panel mode)
       Stage 2  Fan-out     each reviewer model runs once, sequentially
       Stage 3  Consensus   merge findings mechanically; verify file locations
       Stage 4  Judge       one model rules on survivors, narrates the argument
       Stage 5  Verdict     markdown verdict block + leaderboard update
  -> TurnEvents (SessionInit / Text / ToolCall* / Thinking / TurnCompleted)
  -> LocalHarnessStream -> conversation blocks
```

Every stage is one model loaded at a time. Concurrency is a config knob, not an
architectural assumption.

## Why a committee works on small hardware

Three asymmetries make multiple weak models worth more than one:

1. **Decorrelated blind spots.** Models from different families hallucinate different
   fake APIs and miss different bugs. A model cannot see its own hallucinations; a
   different family often can. Family diversity beats model count.
2. **Verification asymmetry.** Checking a diff against a claim is easier than writing
   the diff. A 7B critic adds real signal on a 32B model's output.
3. **Machine refereeing.** The compiler, the tests, and the diff itself are committee
   members that never hallucinate. The LLM judge only rules on what machines can't
   verify — the less it has to decide, the better the committee performs.

What deliberately does NOT exist here: approval voting ("do you approve?" — local
models rubber-stamp), same-model debate (self-preference), and running the combination
methods against each other (noise measuring noise).

## Hardware model: sequential-first

The base case is a machine that can hold **one** model in memory. The pipeline is a
sequence of single-model stages, so it runs correctly everywhere; bigger machines only
go faster, never differently.

The **"models kept loaded"** setting (default 1) maps to Ollama's per-request
`keep_alive`:

| Setting | keep_alive per stage call | Effect |
| --- | --- | --- |
| 1 | `0` | Each model unloads as soon as its stage ends — max headroom, swap cost per stage |
| 2+ | `5m` | Models stay resident between stages; Ollama's own `OLLAMA_MAX_LOADED_MODELS` governs how many actually coexist |

v1 never issues parallel requests. When a later version does, only *independent*
stages (two reviewers reading the same diff) may overlap, and only at setting ≥ 2.

Expectation to set in the UI: at setting 1, every stage pays a model swap (seconds to
tens of seconds) plus a full inference pass. A committee run takes minutes. That is
the point.

## v1: the committee runner

A pseudo-model in the picker: id `ollama/committee`, listed by
`harness_availability::append_available_ollama_models` whenever **two or more** real
Ollama models are installed (one model has no one to argue with). The id flows through
the existing Ollama selection plumbing untouched; `terminal/view.rs` dispatches it to
`run_ollama_committee` instead of `run_ollama_stream`.

Two modes, auto-selected per turn (`committee_mode`):

- **Review** — the working tree has a diff (`git diff HEAD`) and the prompt is empty
  or review-shaped ("review", "diff", "check my changes", …). Reviewers hunt for
  concrete defects in the diff; consensus and judge follow. A review-shaped prompt
  with a clean tree answers immediately: nothing to review.
- **Panel** — any other prompt. Every reviewer answers the question independently;
  the judge picks the winner and writes a synthesis. This is the "fight it out"
  mode: the verdict names which model's answer won and why. A round plan (set up
  front on the Ollama page — the turn can't pause mid-run to ask) may insert up to
  two intermediate rounds before the final verdict, each of one kind: **Judge
  notes** (per-panelist right/wrong notes; everyone revises their own answer),
  **Judge's rewrite** (the judge rewrites the best answer itself; everyone
  responds to that concrete version), **Peer critique** (panelists critique each
  other round-robin, no judge), or **Converge** (the judge picks the current
  leader and everyone else builds on that answer — deliberate style collapse,
  use late). After the final verdict the winner revises once more with the
  judge's critique.

Stage details (Review):

1. **Assemble.** Ask the live server what it can load (`/api/tags`) — the server
   is the source of truth, since it may run with its own `OLLAMA_MODELS` — with a
   disk read as fallback; drop the committee's own pseudo-entry; probe `/api/show`
   caps per model. Assign roles (below). Narrate the bench as a Text block.
2. **Evidence.** `git diff HEAD` in the turn's working directory, surfaced as a
   read-only Shell tool card (same rendering path as agent shell calls), truncated
   to a byte budget that fits an 8K-token context.
3. **Fan-out.** Each reviewer gets ONLY the diff and the task — never another
   reviewer's output, so it cannot anchor. The prompt demands strict JSON findings
   (`file`, `line`, `severity`, `title`, `detail`), says style nits don't count, and
   says an empty array is a good answer — without that clause small models invent
   problems to look useful. Parsing tolerates fences and surrounding prose (same
   recovery philosophy as the qwen tool-call recovery); unparseable output is kept
   as commentary for the judge but excluded from consensus.
4. **Consensus.** Pure-mechanical merge: same file + (similar titles or nearby
   lines) ⇒ one finding with an agreement count. Each finding's file is checked
   against the working tree and the diff — `location verified` is machine truth,
   not model opinion. Findings two reviewers raised independently outrank
   singletons.
5. **Judge.** Preferably a thinking-capable model that was NOT a reviewer. It rules
   confirm / reject / unsure per finding with a reason, names the most valuable
   reviewer, and narrates where the committee agreed, disagreed, and what won out.
   Reasoning models stream their deliberation as Thinking events — visible theatre.
6. **Verdict.** One markdown block: findings table (severity, location, raised-by,
   agreement, ruling), the judge's narrative, and the leaderboard standings after
   this run. `TurnCompleted` carries summed token usage across every stage.

Error policy: a reviewer that fails (service died, model evicted) is noted and
skipped; the run continues with whoever is left. If every reviewer fails, the turn
fails. A failed judge degrades to consensus-only output — findings still shown,
rulings marked unavailable.

Follow-up turns in a committee conversation are independent runs (fresh committee,
same conversation). Cross-turn memory is future work.

## Model roles

Auto-picked, no manual casting in v1 ("the app chooses the best models for the job").
Heuristics seed the choice; the leaderboard refines it over time.

- **Reviewers** (count = "reviewers" setting, default 2, capped by what's installed):
  coder-family models first (name contains a code marker: `coder`, `codellama`,
  `codegemma`, `codestral`, `devstral`, `starcoder`), larger parameter counts first
  (parsed from the tag via `capability::ParameterSize`), distinct families before
  second helpings of one family.
- **Judge**: thinking-capable model (`/api/show` advertises `thinking` — deepseek-r1,
  qwen3, …) that isn't a reviewer; else the largest non-reviewer; else the largest
  model doubles as judge (degraded, and the verdict says so).
- **Family** = leading alphabetic run of the base tag (`qwen2.5-coder:7b` → `qwen`,
  `deepseek-r1:8b` → `deepseek`). Used for diversity in reviewer picks and for
  preferring a judge from outside the reviewers' families.

## Leaderboard

`state_dir()/ollama_committee_leaderboard.json` (same atomic write-then-rename and
non-fatal-failure policy as `local_harness_threads.json`). Per model tag:

```json
{ "qwen2.5-coder:7b": { "runs": 12, "findings_raised": 31, "findings_confirmed": 19, "panel_wins": 3 } }
```

Written after every run from machine-checkable outcomes: findings raised per reviewer,
judge confirmations, panel winners. Read at assembly time to break ties in reviewer
ranking (higher confirm rate first, once a model has enough runs to judge) — so "the
app chooses the best models" starts as heuristics and *learns from its own verdicts*
which models actually win on this machine. Shown in the verdict block and on the
settings page.

## Settings and UI

One JSON setting (`agents.warp_agent.other.ollama_committee_config`, never synced),
following the `ollama_model_mode_overrides` string-JSON pattern:

```json
{ "reviewers": 2, "max_loaded": 1 }
```

UI: a **Committee** section at the bottom of the Ollama settings page — description,
two cycling links in the page's existing link idiom (`Reviewers: 2`,
`Models kept loaded: 1`), the "takes minutes, that's the point" expectation, and the
current standings when the leaderboard has data. The committee only functions with
2+ installed models; the section says so rather than hiding.

Gating mirrors the rest of Ollama: the global "Use local models" switch governs the
committee too (the id is `ollama/…`, so the existing enforcement in
`run_local_claude_query` already applies).

## Wiring

| Piece | Where |
| --- | --- |
| Committee runner, roles, consensus, prompts, leaderboard | `app/src/ai/agent_sdk/driver/harness/ollama_committee.rs` (new) |
| Reused plumbing (chat command, accumulator, service start, caps probe, shell) | `ollama_stream_exec.rs`, loosened to `pub(crate)` where needed |
| Picker entry (`ollama/committee` when ≥ 2 models) | `ai/harness_availability.rs` |
| Dispatch (committee id → committee runner) | `terminal/view.rs` pump match |
| Config setting | `settings/ai.rs` (`ollama_committee_config`) |
| Settings UI section | `settings_view/ollama_page.rs` (new widget) |
| Unit tests (roles, parsing, consensus, mode heuristic, config) | `harness/ollama_committee_tests.rs` |

## Roadmap

- **v2 — Tournament generation.** For "write me X": fan the task to 2–3 models, run
  the project's check/tests against every candidate, judge only the survivors, and
  narrate the bracket. Adds the execution gate (per-candidate scratch worktrees) and
  reuses assembly, judging, verdict, and leaderboard as-is.
- **v3 — Plan-doc decomposition.** Planner model writes a plan doc with file-disjoint
  chunks and explicit interface signatures at the boundaries; chunks are doled out to
  different models (still sequential at setting 1); the merged result must compile
  before judging. Coarse chunks only (2–4). This stage is last because integration is
  where multi-agent systems classically fail: two 7B models happily write two halves
  that disagree about the seam.
- **Cross-turn committee memory.** Follow-ups that interrogate a past verdict
  ("explain finding 2") without re-running the committee.

## Failure modes this design already answers

| Failure | Answer |
| --- | --- |
| Small models rubber-stamp | Findings lists with "empty is fine", never approval questions |
| Nitpick spam | Consensus demotion of singletons; no-style-nits rubric; mechanical location check |
| Judge is the weakest link | Machines pre-verify everything checkable; judge rules only on survivors; thinking model preferred |
| Reviewer anchoring | Reviewers never see each other's output, only the evidence |
| One model on disk | Committee entry not offered below 2 models |
| Model swap thrash | keep_alive follows the max-loaded setting; review is latency-tolerant by design |
| Mid-run model failure | Skip and note; degrade to consensus-only if the judge dies; fail only when no reviewer survives |
