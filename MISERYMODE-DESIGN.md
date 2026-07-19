# Misery mode — technical design

An escalation of swear mode in which the terminal annotates your commands with
contempt. It reads as though something is paying attention to what you're
doing; it is in fact a weighted template grammar over static word lists plus
live context. No model, no network, no latency.

Written 2026-07-19. Describes the implementation as it currently stands in
`app/src/terminal/misery/`.

---

## 1. Status

**Complete and wired.** 56 tests passing plus one opt-in inspection tool, zero
warnings. Lines render in the terminal on command submit and completion.

| File | LOC | Role |
|---|---:|---|
| `mod.rs` | 216 | `MiseryEngine`, `MiseryContext`, session state, anti-repetition |
| `classify.rs` | 260 | Command → `ParsedCommand` + `CommandClass` |
| `templates.rs` | 402 | The template bank (54 templates), `Phase`, `Cond` |
| `grammar.rs` | 194 | Slot resolution and expansion |
| `vocab.rs` | 215 | 14 word lists, 153 entries |
| `session.rs` | 137 | Per-terminal state, context assembly, cwd listing |
| `view.rs` | 63 | `MiseryLineView` — renders one line |
| `*_tests.rs` | 730 | 56 tests + a manual sampler |

Settings: `AISettings.misery_mode` (`agents.warp_agent.other.misery_mode`),
default off, never synced to cloud.

---

## 2. Pipeline

```
  raw command string
        │
        ▼
  classify::parse ─────────► ParsedCommand { name, subcommand, args,
        │                                    flags, class, is_compound }
        ▼
  deserve::probability ────► f32, clamped to 0.04..=0.75
        │                     (skip and stay silent on a failed roll)
        ▼
  templates::candidates ───► filter by Phase + CommandClass + Cond
        │
        ▼
  grammar::expand ─────────► fill {SLOT}s, or fail and retry
        │
        ▼
  anti-repetition check ───► reject if said in the last 40 lines
        │
        ▼
  "42 filenames. you were looking for one."
```

Everything is synchronous and allocation-light. The expensive-looking step —
`candidates` — is a linear scan of 54 static entries.

---

## 3. Two voices

Driven by `Phase`, chosen by the caller:

| | `Phase::Submit` | `Phase::Complete` |
|---|---|---|
| **When** | command submitted, before output | block finished |
| **Tense** | present | past |
| **Stance** | weary anticipation | verdict |
| **Knows** | command text only | + exit code, duration, output volume |
| **Example** | `bugger me. listing files. the pinnacle of computing.` | `42 filenames. you were looking for one.` |

Submit fires at 0.55× the completion rate (`deserve.rs`), so most commands
produce at most one line and the two-line case stays a treat rather than the
norm.

---

## 4. Classification

`classify::parse` is deliberately **not** a shell parser. It:

- takes the first segment of any pipeline or `&&` list — that's the intent
  being judged, so `ls | wc -l` classifies as `Listing`;
- skips wrappers and env assignments (`sudo`, `env`, `time`, `nohup`,
  `RUST_LOG=debug …`) to find the real command;
- strips directory prefixes and `.exe`;
- splits flags from positionals, treating `-5` and a bare `-` as operands;
- extracts a subcommand only for commands that actually have them, so
  `cat foo.txt` doesn't acquire a `foo.txt` subcommand.

A wrong classification costs a slightly off-topic insult. That is an acceptable
failure mode, and the reason this doesn't reach for a real parser.

### `CommandClass` and baseline contempt

14 classes, each with a starting probability:

| Class | Baseline | Class | Baseline |
|---|---:|---|---:|
| `Clear` | 0.50 | `Search` | 0.20 |
| `Listing` | 0.45 | `Vcs` | 0.18 |
| `Navigation` | 0.42 | `Editor` | 0.15 |
| `Inspection` | 0.30 | `Network` | 0.15 |
| `Shell` | 0.28 | `Unknown` | 0.15 |
| `Process` | 0.25 | `PackageMgmt` | 0.12 |
| `Destructive` | 0.25 | `Build` | 0.07 |

`Destructive` sits mid-table on purpose: `rm -rf` earns *attention*, not
contempt. `Build` is lowest because it's the one thing that looks like work.

> **Design note — these baselines were originally 2× higher and it was a bug.**
> With `Listing` at 0.9, `ls` saturated the 0.75 clamp on its own, so
> repetition, failure and pipeline modifiers all became no-ops. Three tests
> caught it. Any future edit here must leave headroom for the modifiers.

### Hooking the real parser

`crates/warp_completer/src/parsers/hir` already produces
`Command::{Classified(ShellCommand), Unclassified(ExternalCommand)}` with a
proper `command_name_span()`. `classify::parse` accepts a raw string so the
engine stays unit-testable without a live shell, but a caller holding a real
HIR parse can pass the extracted name for better accuracy on exotic input.

---

## 5. Adaptive frequency (`deserve.rs`)

The single most important design decision: **misery mode is only funny if it
knows when to shut up.** A flat rate mocks a hard command and ignores your
fifth `ls` in a row.

Starting from the class baseline:

| Signal | Effect |
|---|---|
| Repeated command (1 / 2 / 3+ times recently) | +0.10 / +0.22 / +0.32 |
| Bare command, no args or flags | +0.08 |
| Compound (pipe, `&&`, redirect) | −0.18 |
| Non-zero exit | +0.30 |
| Slow (>5s) **and** trivial | +0.25 |
| Slow (>5s) and not trivial | −0.20 |
| Each recent line already emitted (per-phase window 3) | −0.16 |
| `Phase::Submit` | ×0.8 |

Clamped to `0.04..=0.75`.

The two bounds are both deliberate. The ceiling means even a fifth consecutive
`ls` is never a certainty — unpredictability is most of the comedy. The floor
is never zero, because the rare unearned jab during a long build is the funniest
one.

Consequences worth stating: a pipeline is treated as evidence of thought and
gets spared; a slow trivial command is its own punchline while a slow build is
left alone; and the backoff window means misery arrives in waves rather than a
constant drizzle.

Measured over 12,800 simulated commands: **submit 27%, complete 32%**.

> **Design note — the backoff window is tracked per phase.** It was originally
> shared, which meant the two voices starved each other: a completion line
> suppressed the very next submit roll, and since completions are the likelier
> of the two, submit lines landed around 18% and never registered as a second
> voice at all. Widening the shared window fixed submit but dragged completions
> down with it. Splitting the window per phase lets each pace itself.

---

## 6. Template bank (`templates.rs`)

54 templates — 32 submit, 27 complete (counts overlap where a template names
both phases in its class list). Each declares:

```rust
Template {
    text:    "{LINES} lines. you'll read four of them.",
    phase:   Complete,
    classes: &[],            // empty = universal
    cond:    Cond::HasOutput,
    weight:  7,
}
```

**`classes`** — topical targeting. Empty means universal, and universal lines
carry lower weights (4–9) than topical ones (9–12) so the specific jab wins
when both apply.

**`cond`** — outcome precondition. Five variants: `Any`, `Success`, `Failure`,
`HasOutput` (≥3 lines), `NoOutput`.

> **Design note — `Cond` exists because slot-checking was not enough.** Slot
> resolution catches missing *data*. It does not catch wrong *tone*. Printing
> real samples exposed three cases that all type-checked and all read as bugs:
>
> | Output | Problem |
> |---|---|
> | `0 lines. you'll read four of them.` | volume remark on no output |
> | `exit 0. I'd explain, but you wouldn't read that either.` | failure voice on success |
> | `done in 3.4 seconds. I spent it…` on a failed `git push` | success voice on failure |
>
> `Cond` fixed all three, and four tests now lock them down. `Cond::Success`
> and `Cond::Failure` both check `succeeded == Some(_)`, which also stops
> outcome language leaking into the submit phase where no outcome exists.

---

## 7. The grammar (`grammar.rs`)

Templates contain `{SLOT}` placeholders. Expansion walks the string once,
resolving each against the context and an RNG.

**A slot that cannot be resolved fails the entire expansion**, and the caller
picks a different template. This is what prevents `I was alphabetising
against` in an empty directory. It also means a `{DURATION}` template can sit
in the bank unguarded — the submit phase filters it out automatically.

### Slots

| Slot | Source |
|---|---|
| `{EXCLAIM}` | `EXCLAIM_CLEAN` or `EXCLAIM_SWEAR` per swear mode |
| `{NOUN_USER}` | `NOUN_USER_CLEAN` or `NOUN_USER_SWEAR` per swear mode |
| `{AGAIN}` `{TALENT}` `{ADJECTIVE}` `{NOUN_TASK}` `{COMPLAINT}` `{NEVER_EVEN}` `{GERUND}` `{PREP}` | direct word lists |
| `{A_ADJECTIVE}` | adjective with its indefinite article agreed |
| `{VERDICT}` | `VERDICT_SUCCESS` or `VERDICT_FAILURE` per outcome |
| `{ACTIVITY}` | **synthesised** — see below |
| `{FILE}` `{CMD}` `{DIR}` | live context |
| `{DURATION}` `{LINES}` `{EXIT}` | completion facts |

### `{ACTIVITY}` — the load-bearing trick

Combines a gerund with one or two of the user's **real filenames**:

```
cross-referencing `Cargo.toml` against `README.md`
summarising `ACCOUNT-DESIGN.md`
counting the semicolons in `build.rs` versus `src`
```

20 gerunds × 6 prepositions × the local file set. This is what makes the output
sound researched rather than sampled — and it cost 26 lines of vocabulary.

**Filenames only. File contents are never read.**

### Two details that matter

- **Filenames are drawn without replacement.** The pool is shuffled once per
  expansion and consumed by cursor, so two `{FILE}` slots in one line can never
  name the same file. Tested across 50 seeds.
- **Doubled sentence punctuation is collapsed on the way out.** Some slot
  values are whole sentences that already end in punctuation — `{VERDICT}.`
  with "there. happy?" yields "there. happy?.". Rather than forbid punctuation
  in the vocabulary (a mechanical constraint on the writing), `tidy_punctuation`
  fixes it after expansion.
- **Indefinite articles agree.** `{A_ADJECTIVE}` yields "an inert little
  exercise", not "a inert". Spelling is a poor proxy for pronunciation, so
  `indefinite_article` carries explicit lists for the two classes where it
  lies: "yoo"-initial vowels take *a* (a **u**seful tool), silent-h words take
  *an* (an **h**our). Prefix-matched on whole words so "unimaginative" and
  "inert" aren't caught by a naive `u`/`i` rule.

---

## 8. Session state (`mod.rs`)

`MiseryEngine` is stateful — one per session. The state is what makes
consecutive lines feel authored:

| Buffer | Size | Purpose |
|---|---:|---|
| `recent_lines` | 40 | Never repeat a line within the window |
| `recent_commands` | 6 | Repetition detection for `deserve` |
| `recent_spoke` | 4 | Backoff so misery comes in waves |

`pick_line` retries up to 24 times, because a draw can fail two ways — the
template didn't expand (missing slot) or the result was said recently. Both are
cheap.

### API

```rust
MiseryEngine::new()                         // entropy-seeded
MiseryEngine::with_seed(u64)                // reproducible

engine.note_command(&parsed);               // once per command, at submit
engine.generate(&ctx, phase) -> Option<String>        // rolls for deservedness
engine.generate_forced(&ctx, phase) -> Option<String> // always tries; for previews
```

`generate` returning `None` is the **common** case and is not an error.

---

## 9. Rendering

### Where the line goes

Lines are inserted into the block list as **rich content** — the same mechanism
agent blocks and zero-states use, via `TerminalView::insert_rich_content`. That
gets height, scrolling and selection handled for free.

**It is never written to the PTY.** This is the one hard constraint. Injecting
into the real output stream would corrupt piped output and break anything
parsing it — `ls | wc -l` would start counting insults. The line looks like it
is in the output; it is a sibling of the block, not part of it.

`MiseryLineView` renders dim italic text at a small left inset. Dim and italic
because the terminal is muttering, not announcing — and because this is the one
piece of text on screen that no program actually printed, so it should read as
clearly subordinate to real output.

**Vertical spacing is asymmetric, and it is load-bearing.** Both voices render
between blocks, so without it a submit line is visually indistinguishable from
a verdict on the block *above* it — the line appears above the command's prompt
row, right against the previous block's output. Each line now hugs the block it
refers to (submit: 10px above, 1px below; complete: the reverse) and lets
proximity do the disambiguating.

### The two hooks

Both live in `app/src/terminal/view.rs`:

| Phase | Event | Insertion |
|---|---|---|
| Submit | `ModelEvent::AfterBlockStarted` | `Append { insert_below_long_running_block: false }` |
| Complete | `ModelEvent::AfterBlockCompleted` | `Append { insert_below_long_running_block: true }` |

The insertion flag is what places each voice correctly. With `false`,
`append_rich_content` always takes its `insert_non_block_item_before_block`
branch, putting the line *above* the block that is about to run — so it reads
as a reaction to the command, not a comment on output that hasn't arrived. With
`true` at completion, the finished block is no longer active, so the line lands
after it.

`AfterBlockStarted` already returns early for in-band commands, so those are
excluded for free. Agent-driven commands are skipped explicitly at completion
(`was_part_of_agent_interaction`) — mocking those would be aimed at the wrong
target.

### Context assembly

`MiserySession` (in `session.rs`) owns the engine, so anti-repetition memory
spans the terminal session rather than resetting per block. It maps block facts
into a `MiseryContext`:

| Field | Source |
|---|---|
| `command_name` | `UserBlockCompleted::command` |
| `exit_code` / `succeeded` | `serialized_block.exit_code.value()` |
| `duration` | `started_at.elapsed()` |
| `output_lines` | `num_output_lines`, saturated to `u32` |
| `working_directory` | `serialized_block.pwd`, or `active_block().pwd()` at submit |
| `nearby_files` | directory listing, cached per path |
| `swear_mode` | `AISettings::is_swear_mode_enabled()` |

The directory listing is cached by path — re-listing on every command would be
a syscall per block for a cosmetic feature, and slightly stale raw material for
insults is fine. Dotfiles are skipped: `.DS_Store` and `.git` aren't funny, and
naming someone's hidden files reads as prying rather than as a joke.

`note_command` runs on every submit regardless of whether a line is produced —
"not this again" on the third `ls` depends on having counted the first two.

## 10. Settings and gating

Two layers, both required:

1. **Runtime** — `AISettings::is_misery_mode_enabled()` returns
   `is_swear_mode_enabled() && *misery_mode`.
2. **UI** — the "Misery mode" toggle in the Agents settings page is rendered
   with `is_setting_toggleable = is_any_ai_enabled && *ai_settings.swear_mode`,
   so it greys out entirely while swear mode is off.

Misery is an escalation of swear mode, not an independent axis. A live toggle
that silently does nothing would be a worse experience than a greyed-out one
that explains itself by association.

Also wired: a `ToggleMiseryMode` action, the `Misery_Mode` binding flag, and
`misery misery mode insults contempt` in the settings-search terms.

---

## 11. Test coverage

45 tests. The interesting ones are the property-style checks, which is where
the real bugs surfaced:

**Invariants over 400+ generated lines:**
- no unfilled `{SLOT}` or stray brace, no double spaces, never empty
- no volume claim when output was empty
- no `exit 0` in a failure-voiced line
- no success voice (`done in`, `built. in`, `it's gone`, `congratulations`) on
  a failure
- no outcome language at all during submit

**Behavioural:**
- >60 distinct lines from 400 draws on a single command
- no repeat within the 40-line memory
- distinct command classes reach lines the other cannot
- survives an empty directory and an unparseable command

**Statistical** (40 seeds × 20 draws):
- `ls` is mocked more than `cargo build`
- failures outdraw successes

**Session-level** (`session_tests.rs`):
- directory listings are cached per path and skip dotfiles
- a missing directory, absent cwd, or absent start time are all non-fatal
- `u64::MAX` output lines does not overflow the `u32` context field

**Manual** — `misery::density` prints a simulated session at the real adaptive
rate. Not an assertion; run it after touching vocabulary. It is how the doubled
punctuation bug was found.

---

## 12. Extending it

**Adding vocabulary is always cheaper than adding templates.** One new gerund
multiplies against every template using `{ACTIVITY}`; one new template adds one
line shape. Current lists:

| List | Entries | List | Entries |
|---|---:|---|---:|
| `EXCLAIM_CLEAN` | 15 | `NOUN_USER_CLEAN` | 9 |
| `EXCLAIM_SWEAR` | 12 | `NOUN_USER_SWEAR` | 8 |
| `GERUND` | 20 | `NEVER_EVEN` | 10 |
| `ADJECTIVE` | 14 | `NOUN_TASK` | 10 |
| `TALENT` | 12 | `AGAIN` | 9 |
| `COMPLAINT` | 12 | `VERDICT_SUCCESS` | 8 |
| `PREP` | 6 | `VERDICT_FAILURE` | 8 |

Two rules when adding:

1. **Entries within a list must be interchangeable.** Every entry will
   eventually be paired with every partner. A line that only works with one
   specific neighbour will look broken.
2. **Tag outcome-specific templates with a `Cond`.** If it says "failed",
   "exit", or counts output, it needs one. The tests will catch you, but the
   samples will catch you faster — print them.

### Ideas not implemented

- **Time-of-day awareness** — different contempt at 3am.
- **Streak detection across classes** — noticing an `ls`/`cd`/`ls` loop as a
  pattern rather than three separate repeats.
- **Typo callbacks** — referencing a command that failed with 127 earlier.
- **Escalation over a session** — the voice getting progressively more
  personal the longer you work.
