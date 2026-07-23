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
COMMITTEE_HOST=http://192.168.1.150:11434 COMMITTEE_FIXTURE=stock cargo test -p warp --lib -- --ignored --nocapture a_real_committee
```

## By mode

| Mode | Fixture | Found | Invented | Doubled | Time |
| --- | --- | --- | --- | --- | --- |
| Review | stock | **3 / 3** | 0 | 2 | 17 min |
| Review (cross-file) | orders | **2 / 2** | 0 | 1 | 15 min |
| Edit | durations | **pass** — 12/12 held-out cases | — | — | 7½ min |
| Creation | arcade | see below | — | — | 8 min |
| Review | payday | see below | — | — | 11 min |

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
