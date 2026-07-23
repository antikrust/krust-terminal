# Multi-agent committee design (Ollama)

Written: 2026-07-21. Current as of 2026-07-22 (v1 shipped: review + panel, the
verification layer, casting, the stage knobs).

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
       Stage 2a Suite gate  the project's own tests, before anyone reviews
       Stage 2b Behavior    call the changed functions, record what they do
       Stage 2c Contracts   the unchanged files the diff imports, for their promises
       Stage 2d Oracle      one model says what those calls SHOULD have returned,
                            never shown what they did; the harness compares
       Stage 3  Fan-out     each reviewer model runs once, sequentially
                            (twice — cold then warm — with second opinion on)
       Stage 4  Consensus   merge findings mechanically; verify file locations
       Stage 2e Propose     a model writes one realistic call per changed function,
                            because a generic battery cannot build a basket
       Stage 5  Proof       the raiser writes a repro; the harness runs it,
                            reads the comparison it printed, and ignores the
                            exit code it chose
       Stage 6  Judge       one model rules on survivors, narrates the argument
       Stage 7  Verdict     Definite / Might be / Rejected + leaderboard update
       Stage 8  Audit       a model that sat the run out checks the verdict cold
       Stage 9  Transcript  the whole run written to disk, kept only if asked
  -> TurnEvents (SessionInit / Text / ToolCall* / Thinking / TurnCompleted)
  -> LocalHarnessStream -> conversation blocks
```

Every stage is one model loaded at a time. Concurrency is a config knob, not an
architectural assumption.

Stages 2a, 2b and 5 are the machine's own turns, and each can be switched off
(Settings → Ollama). Stage 5 is what separates *Definite* from *Might be*: with the
proof round off, nothing is executed and no finding can be better than an opinion.

Stage 2d is the newest and, per measured runs, the most valuable, because of how
little it asks. A prover has to pick an input, decide what correct would be, call
the code, compare, and encode the answer as an exit code — five steps, and small
models fail the middle one relentlessly, writing the observed value into
`expected` and refuting a real defect with a comparison between a value and
itself. The oracle asks only *what should this have returned?*, withholds what it
did return so there is nothing to copy, and leaves the comparing to the harness.
What comes out is a located defect with no model holding the casting vote:

```text
needs_reorder(3, 3) returned False, but its docstring says it should return True
```

After the verdict the conversation stays live, and that is where most of the value
is: **any question is answered from the run** by a model holding it, `tests` turns
the findings into a regression file, `recheck <n> [why]` re-proves one finding with
the user's objection in hand, `round <kind>` runs another panel round, and `save`
keeps whichever artefact you mean.

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
  judge's critique — and the verdict block offers **in-chat continuation**: typing
  `round notes|rewrite|peers|converge` as the next message runs one more round on
  the saved answers (per-conversation, in-memory) and re-judges, so iteration is
  decided where the results are read, not only planned in settings beforehand.

Stage details (Review):

1. **Assemble.** Ask the live server what it can load (`/api/tags`) — the server
   is the source of truth, since it may run with its own `OLLAMA_MODELS` — with a
   disk read as fallback; drop the committee's own pseudo-entry; probe `/api/show`
   caps per model. Assign roles (below). Narrate the bench as a Text block.
2. **Evidence.** `git diff HEAD` in the turn's working directory, surfaced as a
   read-only Shell tool card (same rendering path as agent shell calls), truncated
   to a byte budget that fits an 8K-token context.
2a. **Suite gate.** If the repo declares a suite (`Cargo.toml`, `go.mod`,
   `pytest.ini`/`pyproject.toml`, a real `package.json` test script), run it under a
   timeout before anyone reviews. Green or red is evidence handed to every later
   stage; a suite that *can't* run (no toolchain, timeout) is `Unavailable` and
   contributes nothing — a missing `cargo` must never read as a claim about the
   user's code.
2b. **Behavior report.** Reviewers cannot call tools, so the harness exercises the
   changed code *for* them: every top-level function the diff adds is called against
   a deliberately broad generic battery (empty collections, zero, negatives, small
   lists, strings, `None`) and every return value and exception is recorded. Broad on
   purpose — nothing here knows how the code is really called. The report goes into
   the reviewers' prompt as established fact, so a model that has already seen
   `split_pot(0, []) !! ZeroDivisionError` and `top_scores([3,1,2], 2) -> [1, 2]`
   argues from observation instead of guessing. Python only for now (import-and-call
   is trivial there); other languages need a compile step and wait for v2.
3. **Fan-out.** Each reviewer gets the diff, the task, and the machine evidence from
   2a/2b — but never another reviewer's output, so it cannot anchor. The prompt
   demands strict JSON findings
   (`file`, `line`, `severity`, `title`, `detail`), says style nits don't count, and
   says an empty array is a good answer — without that clause small models invent
   problems to look useful. Parsing tolerates fences and surrounding prose (same
   recovery philosophy as the qwen tool-call recovery); unparseable output is kept
   as commentary for the judge but excluded from consensus.
3a. **Sanitation** (mechanical, before anyone reads a report). Small models come off
   the rails in three shapes and all three are detectable without a model: a
   repetition loop (one model reported the same finding 100 times) collapses to one
   finding, line numbers past the diff's own hunk extent are cleared — the finding
   survives, the invented citation doesn't — and a report longer than the findings
   cap is truncated. What was dropped is announced (`went derp — got stuck repeating
   "…" 100 times. Kept 1 of 100.`) rather than silently swallowed: a reviewer that
   loops is exactly what the user needs to see. The leaderboard counts only what
   survived.
3b. **Second opinion** (optional). The same reviewer re-reads the same diff at
   temperature 0.8, and the two reports are folded: what it raised both times is
   marked, what it raised once is kept anyway. A model asked once can report a
   defect the sampler suggested; asked again down a different path, it mostly
   repeats what is actually in the code. The union is kept rather than the
   intersection, because trading a hallucination for a miss is not an improvement —
   corroboration is a *ranking* signal, and the proof round remains the arbiter.
   Self-corroboration ranks below two models agreeing, which is decorrelated
   evidence rather than one model twice.
4. **Consensus.** Pure-mechanical merge: same file + (similar titles or nearby
   lines) ⇒ one finding with an agreement count. Each finding's file is checked
   against the working tree and the diff — `location verified` is machine truth,
   not model opinion. Findings two reviewers raised independently outrank
   singletons.
5. **Proof round.** The machine gets the deciding vote where it can: for each
   finding (up to a budget), the reviewer who raised it writes a minimal python3
   script that runs in the working tree, prints one observation line, and exits 0
   if the defect is real / 1 if the code is actually fine. The harness executes it
   under a hard timeout (a wrapper maps timeouts to exit 124 so they can't read as
   refutations). Execution beats opinion in everything downstream.
6. **Judge.** Preferably a thinking-capable model that was NOT a reviewer. With the
   proof outcomes in hand it rules confirm / reject / unsure per finding, adds its
   own `additions` for defects in functions no finding covered (backstop reviewer;
   mechanically deduped against the committee, never self-credited), names the most
   valuable reviewer, and narrates the argument. Reasoning models stream their
   deliberation as Thinking events — visible theatre.
7. **Verdict, tiered by certainty.** Findings render in three groups: **Definite**
   (execution-demonstrated, or judge-confirmed when unproven), **Might be**
   (unsure/unproven), **Rejected** (execution-refuted or judge-rejected) — a
   demonstrated finding is definite even if the judge waffles, a refuted one is out
   even if the judge liked it. Only definite findings feed the leaderboard's
   confirmed count. `TurnCompleted` carries summed token usage across every stage.

8. **Audit.** A model that took no part — largest idle model, thinking models
   first — reads the diff and the committee's conclusions cold and answers three
   questions: which rulings are wrong, what did everyone miss, what did they get
   right. Everything before this stage was written by models arguing with each
   other, and a committee that has spent six stages agreeing with itself is exactly
   where a fresh reader earns its keep. With every model seated it falls back to the
   judge marking its own homework, and says so. One call; on by default.
9. **Transcript.** The run writes itself down as it happens — narration, tool
   commands, their output — and lands in `state_dir()/committee_runs/`, newest
   twenty kept. Nothing is written to the user's repository unless they ask:
   `save transcript` is a rename, ignoring it is free, and the sweep handles the
   rest. It exists so the run can be re-read, handed to another model, or attached
   to a bug report — a review that only ever existed in the scrollback is a review
   you cannot act on twice.

Error policy: a reviewer that fails (service died, model evicted) is noted and
skipped; the run continues with whoever is left. If every reviewer fails, the turn
fails. A failed judge degrades to consensus-only output — findings still shown,
rulings marked unavailable.

Follow-up turns in a committee conversation are independent runs (fresh committee,
same conversation). Cross-turn memory is future work.

## The cast

Every seated model is introduced by a name — Penelope Crustacean, Furgeront Bastille,
Ron Caramel — drawn deterministically from its tag, so the same model is the same
person every run and the name becomes shorthand the way a tag never does. Whoever
holds the gavel is addressed as Judge, on the surname.

This is **display only**, and the distinction is load-bearing. Prompts, leaderboard
keys, casting, settings and the `roles` map all use the tag, because the machinery
must never have to work out which model "Furgeront Bastille" was. Two places bridge
the gap: the bench block prints name *and* tag together (the tag is what costs RAM,
so it stays visible where it matters), and model-written prose — the judge's
argument, its per-finding reasons, the audit — is rewritten through the same cast
before rendering, longest tag first so `qwen2.5-coder:14b` can't eat
`qwen2.5-coder:14b-instruct`. A model the judge names that never sat keeps its tag.

`Committee is introduced by: name / model tag` on the Ollama page for anyone who
wants serial numbers back.

## Model roles

Auto-picked by default ("the app chooses the best models for the job"): heuristics
seed the choice, the leaderboard refines it over time, and the user can overrule
either. Each installed model's row carries a casting link — `auto` → *always
reviewer* → *always judge* → *never use* — and casting beats every heuristic below.
A forced judge doesn't consume a reviewer seat; casting everything off leaves no
committee, and the run says so instead of half-running.

Above casting sits the model's own switch (also on the Installed row): a model
turned **off** is offered nowhere — not in the model menu, not to the committee —
while its weights stay on disk. That is the answer to a model too heavy for the
machine that the user isn't ready to delete; `Remove` is still there for when they
are.

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
{
  "reviewers": 2,
  "max_loaded": 1,
  "panel_plan": ["notes"],
  "roles": { "qwen2.5-coder:32b": "benched" },
  "suite_gate": true,
  "behavior_report": true,
  "proof_round": true,
  "proof_timeout_secs": 15,
  "findings_cap": 12,
  "second_opinion": false
}
```

Plus one sibling setting, `ollama_disabled_models` — a JSON array of tags the user
switched off. It lives outside the committee config because it governs the whole
Ollama surface (the model menu reads it too), not just the bench.

Every field is optional and every absent field reads as the full pipeline, so a
config written before a knob existed keeps behaving the way it did. Values are
clamped, not rejected: a hand-edited `proof_timeout_secs` off the UI's ladder is
honoured (1–600s), a zero is not, and junk falls back to defaults.

UI: a **Committee** section at the bottom of the Ollama settings page — description,
cycling links in the page's existing link idiom, the "takes minutes, that's the
point" expectation, and the current standings when the leaderboard has data. The
committee only functions with 2+ installed models; the section says so rather than
hiding.

| Link | Cycles | Governs |
| --- | --- | --- |
| `Reviewers per run` | 1–4 | Fan-out width |
| `Models kept loaded` | 1–4 | `keep_alive` (1 = unload after every stage) |
| `Machine evidence` | suite + probe → probe only → suite only → none | Stages 2a/2b |
| `Proof round` | off → 5s → 15s → 30s → 60s → off | Stage 5 and its per-script limit |
| `Findings kept per reviewer` | 6 → 12 → 24 → 48 | Stage 3a's cap |
| `Second opinion` | off → on | Stage 3b (doubles the fan-out) |
| `Audit pass` | on → off | Stage 8 (one call, by a model with no stake) |
| `Round 2` / `Round 3` | Off → notes → rewrite → peers → converge → Off | Panel plan |
| `Committee is introduced by` | name → model tag | The cast (display only) |

The round plan also has a **menu in the input bar**, beside the model picker and
shown only while the committee is the selected model
(`agent_input_footer/committee_rounds_selector.rs`). It offers whole plans —
"Judge notes", "Notes → rewrite", "Peers → converge" — rather than per-slot
controls, because "notes then rewrite" is the decision a user actually makes, and
composing it out of two dropdowns is the settings page's job. It writes the same
`panel_plan` the settings page writes, so the two can never disagree.

Two design notes on those. The evidence stages are one control rather than two
switches because they answer one question — *how much does the machine check before
the models speak* — and the first click drops the test suite, the stage that costs
the minutes on a real repo. The proof round folds its on/off into the same ladder as
its timeout, since "how long may a proof run" is meaningless when none do. Turning
either off is announced in the run itself: with no proof round, no finding can reach
**Definite**, and the transcript says so instead of quietly demoting everything to
*might be*.

Per-model controls (the on/off switch, agent/chat mode, casting, Open terminal,
Remove) live on the Installed rows, next to the model they act on, rather than in
this section. A switched-off row hides the rest of its controls: none of them apply
to a model that isn't offered anywhere, and showing them would imply otherwise.

Three chat commands cover the moments a settings page can't reach, since a harness
turn cannot pause to ask:

| Typed after a run | Does |
| --- | --- |
| *anything else* | Answered **from the last review** by a model holding the diff, the findings and the proofs — a follow-up question is a question about the run, and putting it to a fresh panel asks three models to speculate about something they never saw |
| `tests` | Writes one python test file pinning the findings, runs it, and reports which fail. A failure is the good outcome: it pins a defect that is still there and turns green when it's fixed |
| `recheck <n> [why]`, `recheck all [why]` | Re-proves one finding or every finding, handing the prover the user's objection **and** the script that failed last time — the steering wheel for "that repro was nonsense" |
| `round notes\|rewrite\|peers\|converge` | Runs one more panel round on the saved answers and re-judges |
| `save [name]` | Writes the winning answer's first code block under the working directory |
| `save transcript`, `save tests <name>` | Moves the run's write-up, or the generated test file, out of the scratch directory and into the working one |
| `panel <question>` | Forces a fresh committee instead of an answer about the last review |

And one typed *before*: a prompt starting `rounds: notes rewrite — <question>` plans
that run's panel rounds inline, overriding the settings plan for that run only.

Gating mirrors the rest of Ollama: the global "Use local models" switch governs the
committee too (the id is `ollama/…`, so the existing enforcement in
`run_local_claude_query` already applies).

## Wiring

| Piece | Where |
| --- | --- |
| Committee runner, roles, consensus, prompts, leaderboard | `app/src/ai/agent_sdk/driver/harness/ollama_committee.rs` (new) |
| Verification layer: proof scripts, suite gate, behavior report | `harness/ollama_committee_proof.rs` (new) |
| Reused plumbing (chat command, accumulator, service start, caps probe, shell) | `ollama_stream_exec.rs`, loosened to `pub(crate)` where needed |
| Picker entry (`ollama/committee` when ≥ 2 models), switched-off filter | `ai/harness_availability.rs` |
| Dispatch (committee id → committee runner; switched-off models benched) | `terminal/view.rs` pump match |
| Config settings | `settings/ai.rs` (`ollama_committee_config`, `ollama_disabled_models`) |
| On/off + casting storage helpers | `ai/ollama/service.rs` (`is_model_disabled`, `with_model_disabled`) |
| Settings UI section + per-model row controls | `settings_view/ollama_page.rs` |
| Unit tests (roles, parsing, consensus, sanitation, second opinion, config) | `harness/ollama_committee_tests.rs` |
| Verification-layer tests (exit contract, suite detection, probes) | `harness/ollama_committee_proof_tests.rs` |

## Why the verification layer is not an MCP server

MCP is a transport for *a model choosing to call a tool mid-conversation*. Every
property that makes it right for that makes it wrong here:

- **The model must not get a vote.** The proof round's whole value is that the
  harness checks a claim whether or not the model wants it checked. A tool the model
  invokes is a tool the model can skip.
- **These local models are deliberately kept away from tools.**
  `is_exposed_to_local_models` already hides third-party MCP servers from them
  because weak models misuse general-purpose tooling. Routing verification through
  the same door reopens that.
- **It buys nothing.** A subprocess, a JSON-RPC transport, and lifecycle/reconnect
  handling, in exchange for calls that are three in-process `await`s today.

The argument *for* MCP would be exposing this verification to **other** agents
(claude/codex harnesses, external tools) — a real future case, and the reason the
layer lives in its own module with a narrow surface rather than tangled into the
runner.

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

## What a verdict has to survive

A finding reaches **Definite** only if execution demonstrated it AND a second
source saw it — another reviewer that found it independently, or an oracle
mismatch about the same function. Everything else that is still plausible
goes to **Might be** with the reason it could not be settled, and there are
four of those reasons, all of them real runs:

| Conflict | Why neither side wins |
| --- | --- |
| The interpreter demonstrated it, the judge rejected it | One is wrong and the run cannot say which |
| A proof refuted what several reviewers each found alone | One script that tested the wrong input should not delete a consensus |
| One reviewer, and only its own script agreeing | `actual` is computed, `expected` is asserted; that is one source |
| An oracle that changed its answer between temperatures | A slip and a reading look the same from outside |

The shape is the same every time, and it is the single most useful thing
learned from measuring this: **when two kinds of evidence disagree, a bench
of small models is far better at saying "these disagree" than at deciding
which is right.** Reporting the ambiguity costs a line of reading. Resolving
it wrongly costs a real defect or invents a false one.

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
| Model degenerates into a loop | Repeated titles collapse mechanically, the report is capped, and the run names what it did |
| Invented line numbers | Cleared against the diff's own hunk extents — the finding survives, the citation doesn't |
| A broken repro read as a refutation | The proof shim separates crash (125) and no-verdict (126) from a real exit 1; neither refutes |
| Sampler artefacts mistaken for defects | Optional second opinion marks what one model says at both temperatures |
| A model too heavy for the machine | Cast it off the bench, or switch it off entirely — the weights stay, the model stops being offered |
| A bad proof the user can see is bad | `recheck <n> <why>` re-proves it with the objection and the failed script in the prompt |
| A proof script that crashes instead of deciding | One automatic retry with the interpreter's complaint quoted back; `assert` is banned in the prompt because an AssertionError is a crash |
| Six stages of models agreeing with each other | The closing audit is read by a model that sat the run out |
| A verdict that scrolls away | Every run is written down; `save transcript` keeps it |
| A proof whose exit code owes nothing to what it saw | `expected:`/`actual:` lines are checked against the exit code; a claim of "defect real" with nothing compared is inconclusive |
| A retry that regenerates the same broken script | The second attempt is sampled warm; at temperature 0 it was returning the identical script, traceback and all |
| A defect under an unchanged signature | Probe targets come from hunk headers and context lines, not only added `def`s |
| A defect only the caller can see | The unchanged files the diff imports go in as evidence, signatures and contracts only |
| The interpreter and the judge flatly disagreeing | Neither wins: it goes to "Might be" and says which two things conflict |
| A fixture that tells the models the answer | Found the hard way — see below |
| A prover that picks an input which cannot settle the question | It is told which call to reproduce, from the oracle's own mismatch |
| A prover that writes the observed value into `expected` | It is shown the calls with results stripped, and the contract separately |
| A rewrite round that breaks what it was fixing | The revision is run against the draft it replaces and dropped if it lost ground |
| A judge that calls a crash "correct error handling" | Told that a function raising where it used to return has lost behaviour |
| A clean diff reported as defective | Definite needs a second source; an uncorroborated demonstration is "Might be" |

## What the runs actually showed

Every entry above the line was designed in. The ones below were found by
running it, which is what `committee-tests/` and the `a_real_committee`
bench exist for.

**The fixture was the first bug.** `stock.py`'s module docstring explained
that it held three planted defects, one per function. That docstring is part
of the diff. Across two runs, all six reviewers reported exactly three
findings. Nothing measured until that came out.

**A green proof round is not evidence of anything by itself.** Both runs
certified defects on scripts that never touched them. One printed the result
of `cheapest([])` — not the defect — and called `exit(0)` under a comment
saying "defect is real if it returns a non-None value"; the comparison was in
the comment, and the interpreter doesn't read comments. Another wrote
`assert result == False` for `needs_reorder(5, 5)`, asserting the buggy
answer as the expected one, then reported the defect it had just certified as
correct. Both reached **Definite**.

**The behaviour report was probing the wrong function.** It only ran
functions whose `def` line was an added line — which in the stock diff meant
`restock` and nothing else, while the two defects that kept being missed were
in the two functions whose bodies changed under untouched signatures.

**And its noise was manufacturing findings.** Its second line was
`restock(1, 2, 3) !! AttributeError: 'int' object has no attribute 'append'`
— the probe passing an int where a list goes. A reviewer copied it out as a
finding, a prover reproduced it exactly, the judge confirmed it, and the run
reported a defect that was entirely the probe's own doing.

**The scoreboard flattered itself.** A verdict reading "Definite (5)" was, on
inspection, two real defects, one of them counted twice under two line
numbers, one invention from the paragraph above, and one finding the judge
had rejected in the same breath. Which is why `committee-tests/README.md`
scores runs as found / missed / invented / doubled rather than by the
verdict's own headline.
| A recognisable command that isn't understood | Asks what it needs — it never silently becomes a panel question |
