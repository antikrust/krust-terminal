# Committee tests

Three things to run the local committee against, kept here rather than in
`/tmp` so a reboot doesn't take them with it.

```bash
./committee-tests/setup.sh
```

Builds four git repositories in `~/committee-tests` (pass a path to put them
elsewhere). Re-run it whenever you want to reset — it rebuilds every repo from
scratch, so a run that leaves a tree in a mess costs one command to undo.

They have to be their own repositories: review mode runs `git diff HEAD` in
the session's working directory, and a folder inside this fork would hand the
committee the fork's own diff.

---

## 1. The diff test — `~/committee-tests/stock`

**What to do:** open a committee tab in that directory and type

```
review my changes
```

**What's planted.** Three defects, one per function, deliberately of three
different kinds — a committee that finds all three has genuinely decorrelated
blind spots rather than one shared instinct. All three are *wrong values of
the right type*, which is the class the reviewer prompt now hunts, and all
three can be demonstrated in two lines:

| Function | Defect | Proof |
| --- | --- | --- |
| `cheapest` | `if not best` treats a price of **0** as "no price yet" | `cheapest([0, 5])` → `5`, should be `0` |
| `needs_reorder` | `<` where the docstring says "fallen **to** the threshold" | `needs_reorder(3, 3)` → `False`, should be `True` |
| `restock` | `log=[]` is one list shared by every call that omits it | call it twice; the second returns both deliveries |

**What good looks like.** All three in **Definite**, each with its own proof
script and an observation line. Two is a decent run. One means the fan-out
isn't decorrelating — try a different bench.

The behaviour report should catch `restock` before any model speaks: the
probe calls it six times, so the log visibly grows down the report.

**Then try, in the same conversation:**

- `tests` — writes a test file pinning what it found and runs it. The
  failures are the point: each one is a defect that's still there.
- `recheck 2 <why you disagree>` — re-proves finding 2 with your objection
  and its own failed script in front of the prover.
- Any question at all — "why is the empty list case fine?" — is answered from
  the run rather than by a fresh panel.
- `save transcript` — moves the whole run into the repo as a markdown file.

## 2. The creation test — `~/committee-tests/arcade`

**What to do:** open a committee tab there, set **Rounds** in the input bar
(try *Notes → rewrite*), and type

```
write a python3 script that animates fireworks in the terminal using only the standard library — no pip installs, runs for 30 seconds, ctrl-c quits cleanly
```

Then when the verdict lands:

```
save fireworks.py
```

```bash
python3 fireworks.py
```

**What it exercises.** Panel mode, the rounds menu, the judge picking a
winner, the winner's final revision, and `save` writing a runnable file. If
the script crashes, `round notes` and let them fix it — that loop is the
whole point of the rounds.

## 3. The edit test — `~/committee-tests/notes`

**What to do:** open a committee tab there and paste the contents of
`PROMPT.txt` (the instruction and the file together, written out by
`setup.sh`). Then:

```
save release-notes.md
```

```bash
git diff
```

**The honest caveat.** The committee has no "edit this file" mode: panel
models get the prompt and nothing else, so the file has to travel *in* the
prompt — that's what `PROMPT.txt` is for. `save` does write the winner back
over the original, and `git diff` then shows exactly what the committee
changed, which is the interesting part.

**What good looks like.** Shorter, every fact kept, no invented ones, and the
"known issues" section still saying the two things it said. A panel that
quietly drops a known issue has failed the test even if the prose is nicer.

## 4. `~/committee-tests/payday`

The original demo, rebuilt here so it survives too: float cents in
`split_pot`, a division by zero on an empty roster, and `top_scores`
returning the *lowest* scores. Same instructions as test 1.
