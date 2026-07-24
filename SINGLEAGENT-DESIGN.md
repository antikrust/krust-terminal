# Single-agent design

The counterpart to [MULTIAGENT-DESIGN.md](MULTIAGENT-DESIGN.md). That document
is about the **committee** — several models argued toward a verdict — and it
exists for one reason: there is no oracle for "is this code correct", so a
panel and a judge stand in for the checker you wish you had.

This document is about the case where you *have* the checker. One model
proposes, the machine grades, and there is no panel because there is nothing
for it to do. Two things live here:

1. **The single-agent runtime** — the local (Ollama) model with in-harness
   file and code tools, and the guardrails that keep a small model from
   quietly destroying what it was editing.
2. **The single-agent eval** — `agent-tests/vmlab/`, which measures that agent
   at sysadmin work inside a disposable Linux VM, graded on the state it leaves
   behind.

The one idea under both, and under the committee too, is the split: **the
model proposes, the machine checks.** A small model is good at proposing a
command or an edit and bad — about a coin flip — at judging whether it worked.
So every verdict here is a machine reading real state by value, never the
model's opinion of its own output.

## Why this is not a committee

A committee is what you build when you cannot grade the answer. Fan-out to
several decorrelated models, make them argue, and have a judge resolve the
disagreement — every part of that apparatus is a substitute for a missing
oracle.

The moment a real oracle exists, all of it dissolves. You do not need three
models and a judge to tell you whether `net.ipv4.ip_forward` is `1`; you run
`sysctl -n` and read it. Where the answer is checkable, one agent and a checker
*is the whole of it*. Running the eval across several models is then a
**benchmark** — independent graded rows, "which model does this best" — not a
panel converging on one answer. Looping one model until the check passes is
**best-of-N against the grader**. Neither is a committee, and calling either
one a committee (as an earlier version of this code did) mistakes the absence
of a judge for a clever property of one.

## The single-agent runtime

The local model runs through `ollama_stream_exec.rs` with three built-in tools
executed **in-harness**, not over MCP — the harness decides what runs, the
model never gets a vote on whether its work is checked:

| tool | what it does |
| --- | --- |
| `read_file` | read a file, bounded |
| `write_file` | create or overwrite a file with full contents |
| `run_code` | run a snippet and return output |

MCP tools are withheld from local models by default (`is_exposed_to_local_models`
passes only the built-in, Ollama-safe web-search server unless the user opts a
server in per-card) — weak models misuse general-purpose or stateful tools.

### The write_file guard: a rewrite must keep what it replaced working

`write_file` overwrites a whole file on the model's say-so, and overwriting a
working file is where a small model quietly loses what it did not understand —
told precisely which case to fix, a 7B fixes that case and breaks five others.
So an overwrite of an existing Python file now runs its functions before and
after (`probe_source`) and **puts the old file back if behaviour went
backwards** (`lost_ground`): a call that answered and now raises, one that
returned something and now returns nothing, a function that has stopped
existing.

It is deliberately narrow — it catches *losing* behaviour, not *changing* it.
An edit is usually asked to change something; a guard that blocked that would
be useless. Restructure freely, rename locals, change what the code does on
purpose — as long as what worked still works. This is the same
did-it-lose-ground rule the committee's rewrite rounds use, applied to the one
stage on the single-agent path that had been pure trust.

## The single-agent eval: `agent-tests/vmlab/`

A model is asked to do a real sysadmin job — create a user, stand up a service,
persist a kernel param — and is graded on the machine it leaves behind.

### The sandbox

Every command a model plans runs inside a **KVM guest with its own kernel**
(Ubuntu 22.04 server), booted from a throwaway qcow2 overlay over a read-only
base image. `vmlab.sh` exposes `up` / `run '<cmd>'` / `reset` / `down`; reset
is deleting the overlay and making a new one, so there is no path by which one
task's damage reaches the next — the disk a task wrote to no longer exists.

This is why a VM and not a scratch directory. A small model gets argument order
wrong — one insisted `needs_reorder(7, 0)` should be `True`, the arguments read
backwards — and the same slip in `chown -R user /` or `rm -rf a b` is not a
wrong answer, it is a wiped machine. Proven at the limit: a clean guest hit with
`rm -rf /etc /usr /var /bin /sbin` ate itself, and one `reset` restored it
pristine, because reset kills qemu and drops the overlay from the *host* and
needs nothing from the destroyed guest.

### The graders

Each task under `tasks/<name>/` is a directory of:

| file | who sees it | what it is |
| --- | --- | --- |
| `PROMPT.txt` | the model | one realistic job, phrased as a user would ask |
| `check.sh` | **nobody** | the held-out grader — reads live end state by value |
| `precheck.sh` | nobody | asserts a pristine guest hasn't already satisfied it |

Nothing in a `PROMPT.txt` may hint it is a test or name the grader — the same
rule as the code fixtures, for the same reason: a prompt that says "the check
wants mode 750" measures whether the model can read a rubric, not whether it
can do the job. Every assertion reads real system state — `stat`, `sysctl -n`,
`ss`, `getent`, `findmnt`, `nft list ruleset`, `systemctl show` — and compares
a **value**. None of them greps the model's shell history or matches the
commands it chose.

The eight tasks each plant a plausible-but-wrong command a small model reaches
for, chosen to leave a **detectably** wrong end state rather than an error —
the same failure families the code work surfaced:

- **Recursion** (`srv-app-perms`): `chmod 750` without `-R` — the top directory
  is right, the nested entries are not; the grader stats all of them.
- **Argument order** (`hosts-entry`): `/etc/hosts` takes the address first; the
  reversed line resolves to nothing — caught by asking the resolver, not the file.
- **Direction / port** (`nftables-drop-23`): dropping 22, or dropping 23
  outbound, both leave inbound telnet open.
- **Runtime vs. persistent** (`ip-forward-persist`, `fstab-tmpfs`,
  `heartbeat-timer`): the single commonest sysadmin miss — it works now and
  evaporates on reboot; each grades the two facts separately.
- **Group omission / done-as-root** (`deploy-user`): `useradd` without
  `-G www-data`, or a key set up under sudo so `authorized_keys` is one sshd
  silently refuses.

### The runner

`ops_bench.py` drives it: per task, per model — reset, ask for a plan (shell
only), stage it as a file and run it in the guest, wait out any `settle` delay,
then run the held-out `check.sh`. It prints one greppable line per run:

```
OPSBENCH task=<name> model=<tag> passed=<0|1> seconds=<n> commit=<sha>
```

Commit-stamped for the same reason the code bench's `BENCH` line is: a score is
a fact about the pipeline at one revision, so a table of them can be told when
it is stale (see `committee-tests/stale.sh`). Two harness traps shaken out
getting there, both caught by grading end state rather than trusting the plan:
piping a plan into `bash -s` lets an interactive `adduser`/`apt` eat the
following commands as its answers (stage as a file, stdin from `/dev/null`); and
`sudo` scrubs `DEBIAN_FRONTEND`, so the non-interactive env must be set *inside*
the script or Ubuntu's needrestart wedges apt.

## Safety invariants

- Model commands run **only** inside the guest, via `vmlab.sh run` (ssh).
- The base image is never mounted writable; tasks write to overlays.
- Precondition and grader checks are read-only — nothing a grader runs changes
  the guest.
- The host side manages only *its own* qemu process, by pidfile — it kills
  nothing it did not start.

## Where things live

| | |
| --- | --- |
| `committee-tests/` | the committee's code fixtures + `score.py` + `stale.sh` |
| `committee-tests/ops/` | the file-move manifest grader (single-agent; a candidate to move here) |
| `agent-tests/vmlab/` | this — the single-agent sysadmin eval |
| [MULTIAGENT-DESIGN.md](MULTIAGENT-DESIGN.md) | the committee |
| this file | the single agent |
