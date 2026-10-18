# Running the vmlab eval — a runbook

This is the operational how-to: how to run the sysadmin eval, watch a run
happen, read the leaderboard, and tweak it (add a model, add a task, fix a
stale guest). For *what it is and why*, see [README.md](README.md) and
[../../SINGLEAGENT-DESIGN.md](../../SINGLEAGENT-DESIGN.md).

## The shape of it (two machines)

- **The Mac** (this repo) drives the eval and runs **Moloch** — the app's AI
  backend, where runs land so you can watch them. The board lives here.
- **The PC** (`sentience`, a KVM host) runs **Ollama** (the models) and
  **vmlab** (the disposable Ubuntu guest), under `/mnt/terra/vmlab`.

The eval runs *on the PC* (next to the models and the guest) but streams each
run *back to the Mac's Moloch* over an **ssh reverse tunnel** — so nothing is
exposed on the LAN. `run-ops.sh` sets the tunnel up for you.

## Before you start

- Moloch up on the Mac: `moloch-ctl status` (start with `moloch-ctl on`). Its
  token is `~/.moloch/api-token`.
- The app pointed at Moloch: a `moloch.env` (holding `MOLOCH_URL` +
  `MOLOCH_TOKEN`) in the app's state dir, e.g.
  `~/Library/Application Support/krust.baby.KrustOss/moloch.env`. **The app reads
  this once at startup — restart the app after changing it.**
- The PC reachable as `sentience` over ssh (key auth), with Ollama running and
  vmlab installed at `/mnt/terra/vmlab`.

## Run the whole sweep

```bash
cd agent-tests/vmlab
MODELS="qwen2.5-coder:7b,mistral-nemo:12b,qwen2.5-coder:14b,deepseek-coder-v2:16b,phi4:14b" \
  ./run-ops.sh sentience
```

It rsyncs the harness to the PC, opens the tunnel, primes nothing (see below),
and runs **every task × every model**. Watch it live in the app's **Moloch
page** (Runs + Leaderboard), or read the greppable line per run:

```
OPSBENCH task=<name> model=<tag> passed=<0|1> seconds=<n> commit=<sha>
```

## Run one task, or a subset of models

`run-ops.sh` always runs all tasks. To run one, call `ops_bench.py` directly
over the tunnel (re-sync first if you changed the harness):

```bash
rsync -a --delete --exclude='tasks/_demo' ./ sentience:/mnt/terra/vmlab/harness/
TOK=$(cat ~/.moloch/api-token)
ssh -R 10102:127.0.0.1:10102 sentience \
  "cd /mnt/terra/vmlab/harness && VMLAB=/mnt/terra/vmlab/vmlab.sh \
   MODELS='phi4:14b,qwen3:14b' \
   MOLOCH_URL=http://127.0.0.1:10102 MOLOCH_TOKEN='$TOK' \
   python3 -u ops_bench.py tasks/nginx-8080"
```

## Watch ONE run happen, live

```bash
… python3 -u watch_run.py tasks/heartbeat-timer phi4:14b
```

`watch_run.py` streams the model **writing its plan** (and `💭` thinking, for
models that expose Ollama's think channel — reasoning models like `qwen3` do,
the `qwen2.5-coder`/`phi4` set does not), then the plan running, then each
grader check — into the run's transcript. Watch it in the **web panel**
(`http://127.0.0.1:10102/panel`, token from `~/.moloch/api-token`). These runs
are titled `watch:<task>` and **do not count** toward the board.

## The leaderboard

- API: `GET /api/v1/agent/leaderboard` (aggregates `vmlab:*` runs by task ×
  model into pass-rate + median seconds — no separate table, the runs *are* the
  store).
- In the app: Moloch page → **Leaderboard**. Only `vmlab:*` runs count.

## Tweak: add a model

```bash
ssh sentience 'ollama pull <model>'      # e.g. qwen3:14b, llama3.1:8b
# then add <model> to MODELS
```

Keep it within the box's RAM: **`qwen2.5-coder:32b` (19 GB) OOMs Ollama here**
(measured 0/8, all HTTP 500 / RemoteDisconnected) — it's deliberately left out.
A model that drops the connection now fails only *its own* run (per-run
isolation), never the whole sweep.

## Tweak: add a task

Create `tasks/<name>/`:

| file | rule |
| --- | --- |
| `PROMPT.txt` | the job, as a user would phrase it. **Never** mention the test or name the grader. |
| `check.sh` | the held-out grader. Grade **end state by value** (`stat`, `sysctl -n`, `ss`, `getent`, `nft list ruleset`) — never the commands used. |
| `precheck.sh` | asserts a pristine guest doesn't already satisfy the task. |
| `settle` | optional: seconds to wait before grading (e.g. a minutely timer must fire once → `75`). |

`check.sh`/`precheck.sh` are called `sh check.sh "$RUN"`; `"$RUN" '<cmd>'` runs
a read-only probe in the guest **as root** (via `sudo -n`). **Gotcha:** `$RUN`
execs a real binary, so don't use shell builtins — `which nginx`, not
`command -v nginx` (that exact bug made `nginx-8080` unwinnable).

## Keep the guest healthy

- `vmlab.sh reset` — roll back to pristine (the harness does this between runs).
- `vmlab.sh prime` — **re-download the apt cache into the base image.** Run this
  when apt goes stale: the primed index drifts as the Ubuntu mirror supersedes
  package versions, and a dep can start 404-ing (that's what silently made
  `nginx-8080` fail — nginx's `libxpm4` dep 404'd until a re-prime).

## Gotchas worth knowing (all learned the hard way)

- **apt over qemu NAT is slow** → prime the base, or a plan's `apt-get install`
  blows the 420 s execution cap.
- **Graders run as root** (`sudo -n`) so they can read root-only state — use
  binaries, not builtins.
- **Big models OOM**; per-run isolation keeps one failure local.
- **Finding, so far:** the general model (`phi4:14b`) beats every coder model at
  sysadmin. Reasoning models (`qwen3`) are the next thing to try.
