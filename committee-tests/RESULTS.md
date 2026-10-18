# What the committee actually does

Measured overnight on 2026-07-23, against the fixtures in this directory,
with every model running on one machine across the network (a GTX 1070 box
whose Ollama could not initialise CUDA that night, so everything below is
CPU inference and the times are accordingly awful).

Every number here is scored by `score.py`, not by the verdict's own
headline. That distinction is not pedantry — see *Two green results that
weren't*, below.

## The bench

**Reviewers** `qwen2.5-coder:7b` + `mistral:latest` + `mistral-nemo:12b`,
**judge** `phi4:14b`. That is deliberately the smallest decorrelated set the
machine had: one small coder model, two small general ones. The judge is
held constant so that reviewer size is the only thing varying.

```bash
COMMITTEE_HOST=http://192.168.1.150:11434 COMMITTEE_FIXTURE=stock cargo test -p krust --lib -- --ignored --nocapture a_real_committee
```

## By mode

Measured at `1bdc2e5`. Run `./committee-tests/stale.sh` to see whether the
pipeline has moved since — a score is a fact about the commit that produced
it, and this one changed by five of eight in a single night.


| Mode | Fixture | Found | Invented | Doubled | Time |
| --- | --- | --- | --- | --- | --- |
| Edit | durations | **12 / 12** held-out cases | — | — | 7½ min |
| Review | stock | **3 / 3** (twice) | 0 | 2 | 17 min |
| Review (cross-file) | orders | **2 / 2** | 0 | 1 | 15 min |
| Creation | arcade | **7 / 8** held-out cases | — | — | 4½ min |
| Review | payday | **3 / 3** | 0 | 2 | 19 min |

The one `arcade` failure is worth quoting, because it is what a small-model
first draft looks like when it is nearly right:

```text
FAIL  wrap('supercalifragilistic', 5) -> ['', 'supercalifragilistic'],
                                  wanted ['supercalifragilistic']
```

A word too long to break is flushed after an empty accumulator, so the
result carries a phantom empty first line. Everything else — collapsed
whitespace, exact-width fits, multi-line breaking, empty input — is right.

`payday`'s third defect took three attempts and is the best illustration
here of how a defect gets lost. It is `if not players: return {}` having
been deleted, so an empty roster now divides by zero.

The oracle cannot see it: it is not a docstring violation, and asked what an
empty roster should split into the model answered one thing cold and another
warm and was correctly discarded — the docstring says nothing about nobody.
There is no new line to look at, only a missing old one, and the reviewer
rubric said "never report a defect in a `-` line", which had been written to
stop reviewers describing deleted code as still present. Fixing that got
reviewers raising it for the first time — and the judge rejected it, on the
grounds that a `ZeroDivisionError` is "correctly handled by Python's runtime
error mechanism". Only after the judge was told that a crash is not a way of
handling something did the run reach 3/3. One prompt built the blind spot,
the next stage down ate the fix for it, and neither was visible except by
running the whole thing and reading the reasons.

**Editing is the mode to trust.** It is the fastest, the most reliable, and
the only one whose success is a single objective question. The task is
bounded, the file travels in the prompt, and the grader the committee never
sees says yes or no. If you want one thing from this, it is this one.

**Reviewing works, and did not a day earlier.** stock went 1/3 → 0/3 → 2/3 →
3/3 across the night; the 0/3 was a change of mine that made things worse and
had to be backed out conceptually (see *the prover*, below). Nothing has been
invented in the last five runs.

**Cross-file review is the most improved and the most fragile.** `orders`
plants two defects neither of which is visible from the file it lives in —
what makes them wrong is `pricing.py`, which does not change and so is not in
`git diff HEAD`. Until the unchanged imports were fed in as evidence, that
test was not merely failed but *unpassable*: the information was not in the
room. It now passes 2/2, and the defect that had been missed was caught by a
prover that built its own basket out of the contract it had just been shown.

**Creation is the weakest mode and the hardest to measure.** The first
version of this test asked for a terminal fireworks animation and marked it
by whether the output parsed. It reported a pass on a script with a
guaranteed `NameError` and two curses methods that do not exist. A program
that has to be *watched* cannot be marked by a machine, so the creation test
now asks for `wrap(text, width)` and is graded by a held-out file like the
edit test.

## The rewrite rounds

Measured at `1bdc2e5`, except the five-row table below, which is the record
of a pipeline being repaired and is dated by its own rows.


`COMMITTEE_ROUNDS=notes` runs a round where the judge writes each panelist
notes and everyone revises. Measured against the same graders:

| arcade (creation) | Score |
| --- | --- |
| No round | **7 / 8** |
| Round, vague notes | 6 / 8 |
| Round, notes quoting the failing call | **3 / 8** |
| ↳ plus a lose-ground guard on battery calls | 5 / 8 |
| ↳ plus task-shaped calls to compare on | 7 / 8, guard never fired |

`durations` (edit) stayed 12 / 12 through every one of those, and cost
6½ extra minutes to do it.

Measured properly afterwards — three paired runs, alternating so any drift
hit both arms — the current pipeline gives:

| Pair | No round | One round | Reverts |
| --- | --- | --- | --- |
| 1 | 7 / 8 | 7 / 8 | 0 |
| 2 | 7 / 8 | 7 / 8 | 0 |
| 3 | 7 / 8 | 7 / 8 | 0 |

Both arms are deterministic: the three no-round winners are byte-identical
to each other, and so are the three round winners — and the two groups
differ from each other. **The round reliably rewrites the answer into a
different program that scores exactly the same**, fails the same case it was
told to fix, and breaks nothing. It costs 284s → 632s to do it.

And the round *kind* makes no difference either. Judge notes, rival critique
(`peers`) and the judge's own rewrite (`exemplar`) were each run against the
same baseline:

| Round | Score | Failing case | Program |
| --- | --- | --- | --- |
| none | 7 / 8 | over-long word | A |
| notes | 7 / 8 | over-long word | B |
| peers | 7 / 8 | over-long word | C |
| exemplar | 7 / 8 | over-long word | D |

Nor does model size. A bench of `deepseek-coder-v2:16b` +
`qwen2.5-coder:14b` + `mistral-nemo:12b` scores 7 of 8 and fails the same
case. Three levers — round kind, round presence, model size — and one
invariant result.

**Four different programs, one score, the same failure.** Every form of
feedback moves the answer somewhere new and none of them moves it up. The
mechanism explores perfectly well; it has no gradient, so it wanders on a
level set. That is not "rounds are useless" — it is "rounds are
directionless", which is a different problem with a different fix.

So the earlier numbers were not noise, which is what a first reading of the
variance suggested. They were real measurements of the pipelines that
existed at the time, and they tell a coherent story: the rounds genuinely
were harmful, and the three fixes repaired them, 3/8 → 5/8 → 7/8, landing at
neutral. Nothing has ever made a result better.

The middle row is the one to look at. Told exactly which call was wrong, a
7B fixed exactly that call and broke five others doing it: **making the
feedback more precise made the outcome worse**, because a model this size
rewrites a function around the case it is shown rather than patching it.

**The recommendation is to leave rounds off.** They cost a model call per
panelist per round and the best outcome observed is the one you get without
them.

The cause was that panel rounds had no evidence at all. Review mode probes
the code, asks an oracle what each call should have returned, and executes a
proof per claim; a rewrite round was the judge reading code, forming an
opinion, and three small models revising on the strength of it, with nothing
ever run. Churn on a nearly-right answer breaks it more often than it mends
it — the same coin-flip as the 41% below, pointed at a draft that was
already almost correct.

Drafts are now run before the judge writes notes, and what they did goes in
as measurement rather than opinion. **Both numbers above predate that**, so
they are a baseline and not a verdict on rounds as such. Re-measuring them
is the obvious next thing to do.

## Two green results that weren't

Both were caught by reading what the run wrote rather than the number it
produced, and both had been reported before they were caught.

**The scorer counted the wrong thing.** It matched a Definite finding to a
planted defect by function name, so this —

```
✅ Incorrect logic in cheapest function
   DEMONSTRATED by execution — expected None, got a
```

— which is `cheapest('ab')` returning `'a'`, a string passed where a list of
prices goes, scored as having found the defect about `cheapest([0, 5])`
returning 5. A run reported as 3/3 was 2 found, 1 invented, 1 counted twice.

**And then it was too strict in the other direction.** A later run wrote
"restock() does not clear the log between calls", which is exactly the
planted defect, and scored as a miss because the marker list said "state
between" and not "between call". It nearly went out as a regression. Small
models name the same defect differently every run.

## The prover, and the code executor

Counted over 36 proof scripts.

| | |
| --- | --- |
| Scripts that stated the comparison they were asked for | 100% of those that ran |
| **Scripts whose exit code contradicted their own output** | **41%** |
| Scripts that crashed instead of deciding | 12%, all `assert`, which the prompt bans |
| Crashes misread as a refutation by the executor | **0** |
| Timeouts, failed imports | **0** |

The executor is not a source of error. Everything that goes wrong in the
proof round is the model writing the script, and it goes wrong in exactly
two places — both of which have now been taken away from it.

**Which input to use.** Across the stock runs, `cheapest` was proved with
the empty list 13 times and with `[0, 5]` — the actual defect — 11 times. A
coin flip, and every empty-list attempt refuted something real, because
`cheapest([])` returning `None` is correct and settles nothing. The prover
is now told which call to reproduce.

**Which way the exit code goes.** 41% is not a rounding error; it is the
single largest source of wrong verdicts found all night. Every one of those
would have been a wrong outcome under the original rule — three real defects
thrown out, four non-defects certified. A script that prints

```
expected: True
actual: False
```

and then exits "the code is fine" has still measured the defect. The two
values are computed; the exit code is the model's reasoning *about* them, so
the values decide and the exit code is noted and ignored.

That is the shape of everything that worked here: **take the arithmetic away
from the model and leave it the reading.** The oracle asks only what a call
*should* return, never having been shown what it did; the harness does the
comparing. The contract supplies the expectation, the report supplies the
input, execution supplies the answer.

## The funnel is the fragile part, not the fan-out

The design premise was that several small models with decorrelated blind
spots would notice more than one would. That half works. What kept going
wrong is everything *after* the noticing — five separate places where a
correctly-found defect died on its way to the verdict:

| Stage | How it lost one |
| --- | --- |
| Prover | refuted a real defect with an input that could not settle it |
| Judge | rejected a finding the interpreter had demonstrated |
| Judge | called a `ZeroDivisionError` "correctly handled by Python's runtime" |
| Oracle | right answer discarded by its own cold/warm double-check |
| Notes | shown the failing call, wrote "may produce incorrect results" |

Every one had to be found by running the thing and reading what it wrote,
and every one needed its own fix. Getting a small model to *notice* a defect
is the easy half; the expensive half is not losing it again over the next
four stages.

Two related lessons, both of which cost a run before they were understood.
**Evidence reaching a stage is not the same as a stage using it** — the
auditor contradicted observations it had never been given, and then the
notes pass ignored observations it had. And **a fix at one stage does not
carry to the result**; twice a change that demonstrably worked showed no
improvement at all, because the next stage down ate it.

## What it does to code that is fine

`tidy` is a refactor with nothing wrong with it — proved equivalent over
four thousand random inputs. Every other fixture here measures whether the
committee finds defects; this one measures whether it invents them, and it
invented one:

```text
✅ tidy.py:25 — Incorrect handling of line splitting in parse_config
   DEMONSTRATED — expected {'a': 1, 'b': 2}, got {'a': '1', 'b': '', 'c': '2'}
```

The prover made the expectation up. It wrote integer values against a
docstring that describes string ones, and its own observed dict has a third
key, so the expectation does not even match the input it used. The values
differ, so the rule that decides by comparing them certified it.

That is the residual hole in the change that fixed more than any other
tonight. **`actual` is computed; `expected` is asserted.** Against code with
a real defect a fabricated expectation still usually lands on the right
function, which is why it never showed up in five fixtures full of planted
bugs. Against clean code it manufactures a defect out of nothing, and no
planted-defect fixture could ever have revealed that.

**One invention per clean review is the number to weigh** before leaving
this running unattended. It is not nothing, and it is not the four or five
a run produced before the night's work either.

Fixing it means requiring corroboration — an oracle mismatch or a second
reviewer — before a single script's word promotes anything to Definite.
Measured across all four review fixtures afterwards, it fired twice, both
times on claims that were not planted defects, and demoted no genuine
finding at all:

| Fixture | Fired | Real findings lost |
| --- | --- | --- |
| tidy | once, on the invention above | — |
| stock | never | none |
| payday | never | none |
| orders | once, on an extra claim | none |

The worry that it would cost real detections was wrong, and wrong for a
reassuring reason: when a defect is real the corroboration is usually
already there. `tidy` now produces no **Definite** section at all, which is
the correct verdict for a diff with nothing wrong with it.

## These numbers move between runs

Worth stating plainly, because several conclusions drawn during the night
rested on single runs. `stock` has come in at 3/3 twice and 2/3 twice on
effectively the same pipeline. `payday` has been 2/3, 3/3 and 2/3, and *which*
defect it misses moves. A single run is a weak signal and a difference of one
between two runs is usually noise.

The stable results are the deterministic ones: the `tidy` invention, byte for
byte identical three times running, and anything measured by a held-out
grader. Where a number here comes from one run, treat it as a hint.

## What is still weak

- **Doubles.** Every run carries one or two: "restock() does not clear the
  log between calls" and "restock initializes log with an empty list instead
  of None" are one bug. They cost a proof slot and inflate the count, never a
  wrong conclusion. Merging them by title would collapse `split_pot`, which
  really does hold two separate defects, so the verdict says how many
  functions its findings are about instead.
- **The oracle is only as good as the model answering it.** On the small
  bench it got two of four calls wrong, including insisting
  `needs_reorder(7, 0)` should be True. Asking twice at different
  temperatures filters slips but not systematic misreadings, which is what
  that one was.
- **A prover can still refute a real defect** by choosing a valid but
  irrelevant input. Mitigated rather than fixed: a refutation of something
  several reviewers independently raised goes to "Might be" rather than
  "Rejected".
- **Nobody has measured the rounds.** `COMMITTEE_ROUNDS=notes` runs a rewrite
  round and nothing here has been run with one.
