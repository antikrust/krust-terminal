# Committee tests

Six things to run the local committee against, kept here rather than in
`/tmp` so a reboot doesn't take them with it.

```bash
./committee-tests/setup.sh
```

Builds the repositories in `~/committee-tests` (pass a path to put them
elsewhere). Re-run it whenever you want to reset — it rebuilds every repo from
scratch, so a run that leaves a tree in a mess costs one command to undo.

They have to be their own repositories: review mode runs `git diff HEAD` in
the session's working directory, and a folder inside this fork would hand the
committee the fork's own diff.

**Nothing a fixture says may hint that it is a fixture.** An early version of
`stock.py` explained in its module docstring that it held three planted
defects — which went straight into the diff, and every reviewer in every run
duly reported exactly three findings. A test that tells the models the answer
measures nothing.

---

## 1. The diff test — `~/committee-tests/stock`

**What to do:** open a committee tab in that directory and type

```
review my changes
```

**What's planted.** Three defects, one per function, deliberately of three
different kinds — a committee that finds all three has genuinely decorrelated
blind spots rather than one shared instinct. All three are *wrong values of
the right type*, which is the class the reviewer prompt hunts, and all three
can be demonstrated in two lines:

| Function | Defect | Proof |
| --- | --- | --- |
| `cheapest` | `if not best` treats a price of **0** as "no price yet" | `cheapest([0, 5])` → `5`, should be `0` |
| `needs_reorder` | `<` where the docstring says "fallen **to** the threshold" | `needs_reorder(3, 3)` → `False`, should be `True` |
| `restock` | `log=[]` is one list shared by every call that omits it | call it twice; the second returns both deliveries |

**What good looks like.** All three in **Definite**, each with its own proof
script and an observation line. Two is a decent run. One means the fan-out
isn't decorrelating — try a different bench.

The two that keep getting missed are `cheapest` and `needs_reorder`, and they
are missed the same way both times: a prover reaches for the *empty list* and
for *unequal* arguments, neither of which touches the defect. The behaviour
report exists to put `cheapest([0, 5])` and `needs_reorder(3, 3)` in front of
them before they guess.

**Then try, in the same conversation:**

- `tests` — writes a test file pinning what it found and runs it. The
  failures are the point: each one is a defect that's still there.
- `recheck 2 <why you disagree>` — re-proves finding 2 with your objection
  and its own failed script in front of the prover.
- Any question at all — "why is the empty list case fine?" — is answered from
  the run rather than by a fresh panel.
- `save transcript` — moves the whole run into the repo as a markdown file.

## 2. The cross-file test — `~/committee-tests/orders`

**What to do:** same as test 1 — `review my changes`.

**What's planted.** Two defects in two files, and **neither file is wrong on
its own reading**. What makes them wrong is `pricing.py`, which doesn't
change and so isn't in the diff at all:

| File | Change | Why it's wrong |
| --- | --- | --- |
| `invoice.py` | `£{total:.2f}` | `line_total` returns **pence**; `render_line("Tea", 250, 5)` prints `£1250.00`, should be `£12.50` |
| `cart.py` | `apply_discount(total, percent_off)` | `apply_discount` wants a **fraction**; `checkout([("Tea", 250, 5)], 20)` returns `-23750`, should be `1000` |

**What good looks like.** This is the hard one, and a committee that only ever
sees `git diff HEAD` *cannot* pass it — the units contract lives in a file it
was never shown. Treat a run that finds either defect as evidence the
reviewers are being fed enough context, and a run that finds neither as a
question about what they're being fed rather than about the models.

## 3. The creation test — `~/committee-tests/arcade`

**What to do:** open a committee tab there, set **Rounds** in the input bar
(try *Notes → rewrite*), and type

```
write a python3 file called wrap.py containing a function wrap(text, width) that breaks text into a list of lines of at most width characters. Break only at spaces and never split a word; a word longer than width gets a line to itself. Collapse runs of whitespace, and return an empty list for empty text. Standard library only.
```

Then when the verdict lands:

```
save wrap.py
```

```bash
python3 test_wrap.py
```

**What it exercises.** Panel mode, the rounds menu, the judge picking a
winner, the winner's final revision, and `save` writing a runnable file. If
it fails cases, `round notes` and let them fix it — that loop is the whole
point of the rounds.

**Why this and not something prettier.** This asked for a terminal fireworks
animation for about a day, and the mark was whether the result parsed —
which passed a script that referenced a local belonging to another function
and called two curses methods that do not exist. It could not have run for a
second. A program that has to be *watched* cannot be marked by a machine, so
the creation test asks for something with an answer, and `test_wrap.py` —
committed alongside, never shown to the committee — says whether it got it.

By all means still ask it for fireworks. Just don't let a machine tell you
that one worked.

## 4. The edit test — `~/committee-tests/durations`

**What to do:** open a committee tab there and paste the contents of
`PROMPT.txt` (the instruction and the file together, written out by
`setup.sh`). Then:

```
save durations.py
```

```bash
python3 test_durations.py
```

**What makes this one useful:** it grades itself. `test_durations.py` is
committed alongside, the committee never sees it, and it prints a PASS or
FAIL line per case. The planted bug is `total =` where it should be `total
+=`, so `"1h30m"` comes back as `1800` instead of `5400` — three of the
twelve cases fail before the committee touches it, and none should fail
after. It doesn't matter *how* they fix it.

`git diff` afterwards shows exactly what changed, which is worth reading even
on a pass: a model that rewrites the whole function to get there has told you
something about how much it can be trusted with a small edit.

## 5. The prose edit test — `~/committee-tests/notes`

**What to do:** paste `PROMPT.txt`, then `save release-notes.md`, then
`git diff`.

**The honest caveat.** The committee has no "edit this file" mode: panel
models get the prompt and nothing else, so the file has to travel *in* the
prompt — that's what `PROMPT.txt` is for. `save` does write the winner back
over the original, and `git diff` then shows exactly what the committee
changed.

**What good looks like.** Shorter, every fact kept, no invented ones, and the
"known issues" section still saying the two things it said. A panel that
quietly drops a known issue has failed the test even if the prose is nicer.

## 6. `~/committee-tests/payday`

The original demo, rebuilt here so it survives too: float cents in
`split_pot`, a division by zero on an empty roster, and `top_scores`
returning the *lowest* scores. Same instructions as test 1.

---

## Running one without the app

`a_real_committee` runs the whole pipeline against one of these fixtures on
whatever models you name, and scores the verdict against what was planted:

```bash
COMMITTEE_HOST=http://192.168.1.150:11434 COMMITTEE_FIXTURE=stock COMMITTEE_REVIEWERS=qwen2.5-coder:7b,deepseek-coder-v2:16b,qwen2.5-coder:14b COMMITTEE_JUDGE=phi4:14b cargo test -p bork --lib -- --ignored --nocapture a_real_committee
```

Everything named runs on `COMMITTEE_HOST` and every model on this machine is
benched, so the run can't quietly seat a laptop model and measure the laptop.
`COMMITTEE_OUT=/path/to/file.md` keeps the transcript.

A word on model choice: a 32B on a box with no working GPU offload is about a
token a second, and one review stage takes twenty minutes. Check
`curl -s $HOST/api/ps` — if `size_vram` is 0 it's on the CPU, and a 7B/14B
bench will tell you more per hour than the big model will.

## Scoring a run

The verdict's own counts are not the score. Read a run like this:

- **Found** — a real defect in **Definite** whose proof script actually
  exercised the defective input. A proof that exits 0 without comparing
  anything has demonstrated nothing, however green it looks.
- **Missed** — a real defect not in Definite, *including* one that was raised
  and then refuted by a proof that tested the wrong thing.
- **Invented** — anything in Definite that isn't a real defect. An
  `AttributeError` from passing an `int` where a list goes is the behaviour
  probe's doing, not the code's.
- **Doubled** — the same defect counted twice under two line numbers. It
  inflates Definite and costs the run nothing, which is why it's worth
  writing down separately.

A run that finds two, misses one and invents two is worse than a run that
finds two and invents none, and the verdict header prints both as "Definite".
