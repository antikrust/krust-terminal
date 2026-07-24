# The sysadmin task fixtures

The fixtures in `committee-tests/` measure a model against code. These measure
it against a **machine** — a fresh guest that a small model is asked to
configure, and that is then graded on the state it left behind.

The guest is disposable and isolated: a KVM sandbox that `vmlab.sh` boots
clean, hands to the model, and rolls back with one command. That rollback is
the whole reason these tasks can be destructive without anyone caring —
`chown -R` the wrong tree, truncate the wrong file, lock yourself out with a
firewall rule, and it costs one `vmlab reset` to undo. The interesting
failures in sysadmin work are real users, real permissions, real services;
isolating those away would leave a test of nothing.

## The contract these are written against

`vmlab.sh` (built separately — not here) exposes two verbs:

- `vmlab run '<cmd>'` — runs a shell command **inside the guest** as `lab`
  (NOPASSWD sudo), returning its stdout and exit code.
- `vmlab reset` — rolls the guest back to pristine.

Each task is a directory holding:

| file | who sees it | what it is |
| --- | --- | --- |
| `PROMPT.txt` | the model | one realistic job, phrased as a user would ask it |
| `check.sh` | **nobody** | the held-out grader — the model never sees it |
| `precheck.sh` | nobody | asserts a pristine guest hasn't already satisfied it |

`check.sh` and `precheck.sh` are invoked `sh check.sh "<run-helper>"`, where
`$1` is a command called as `"$RUN" '<cmd>'` that runs a **read-only** probe in
the guest and echoes its stdout, its exit code mirroring the guest command's.
Each prints `PASS <desc>` / `FAIL <desc>` per assertion, a final `all good` or
`N failing`, and exits 0 iff everything passed. Nothing a grader runs ever
changes the guest.

**Nothing in a `PROMPT.txt` may hint that it is a test or name the grader.**
The same rule as the code fixtures, for the same reason: a prompt that says
"the check wants mode 750" measures whether the model can read a rubric, not
whether it can do the job.

## What's here

Eight jobs across the domains a first-day sysadmin actually gets handed.
Every one is graded by the **end state**, by value — many command sequences
reach the same correct machine, and all of them pass.

| Task | Domain | Graded end state | Trap it plants |
| --- | --- | --- | --- |
| `deploy-user` | users, groups, SSH keys | `deploy` exists, is in `www-data`, home not group/other-writable, `~/.ssh/authorized_keys` holds the key, owned by deploy, modes sshd will accept | `useradd` that omits `-G www-data`; no `-m` so the home is never made; the whole job done as root so `.ssh` is root-owned or group-writable and sshd ignores the key |
| `srv-app-perms` | ownership, recursion | every entry in `/srv/app` (a dir, a subdir, a file) is `app:app` mode `750` | `chmod 750` **without `-R`** — the nested entries keep their default modes; `chown app app` (space, not colon) leaves the group as root |
| `nginx-8080` | packages, services, ports | nginx installed, **something listening on 8080**, `GET :8080/` returns a body containing `deploybox up` | left on the default port 80; served the stock welcome page; edited config but never reloaded, so nothing is bound |
| `ip-forward-persist` | kernel params, persistence | `net.ipv4.ip_forward` is `1` **now** and a sysctl config file sets it for boot | `sysctl -w` / `echo 1 >` only — right now, gone after reboot; or edit a `.conf` and never apply it — persisted but still 0 |
| `heartbeat-timer` | systemd timers | a date line has been appended to `/var/log/heartbeat.log`; a timer fires ~every minute and is **enabled** | `systemctl start` without `enable` — runs now, dead after reboot; a daily/hourly cadence instead of minutely |
| `nftables-drop-23` | firewall | the live ruleset drops inbound `tcp dport 23` in an **input** chain | dropped port 22 (ssh) by fat-finger; dropped it **outbound**; used `accept` where `drop` was meant |
| `hosts-entry` | name resolution | `db.internal` resolves to `10.0.0.5` on the box | `/etc/hosts` is `ADDRESS name` — writing `db.internal 10.0.0.5` (name first) makes it not resolve at all |
| `fstab-tmpfs` | mounts, persistence | a 32M tmpfs is mounted at `/srv/cache` **now** and `/etc/fstab` carries the entry for boot | `mount -t tmpfs` only — mounted now, gone after reboot; edit fstab but never mount it — in the table, not mounted |

## The traps, and why the sandbox exists

At least half of these plant a plausible-but-wrong command a small model
reaches for, chosen so it leaves a **detectably** wrong end state rather than
an error:

- **Path / recursion** (`srv-app-perms`). `chmod 750 /srv/app` without `-R`
  looks done — the top directory is right — and the grader stats the subdir
  and the file, which a non-recursive chmod left at `755` and `644`. Grading
  only the top entry would pass it.
- **Argument order** (`hosts-entry`). `/etc/hosts` takes the address first.
  The reversed line parses as "an address literally called `db.internal`",
  which resolves to nothing — caught because the grader asks the resolver, not
  the file.
- **Direction / port** (`nftables-drop-23`). Dropping `tcp dport 22`, or
  dropping 23 on the `output` hook, both leave inbound telnet open; the grader
  walks the ruleset for a 23-drop *in an input chain* and finds neither.
- **Runtime vs. persistent** (`ip-forward-persist`, `fstab-tmpfs`,
  `heartbeat-timer`). The single most common sysadmin miss: it works now and
  evaporates on reboot. Each of these three grades the two facts separately.
- **Group omission / done-as-root** (`deploy-user`). `useradd deploy` with no
  `-G` gives a real account with no group; doing the key setup under sudo and
  forgetting to `chown` leaves an `authorized_keys` sshd silently refuses.

## How they grade

Every assertion reads real system state and compares a **value**: `stat`,
`sysctl -n`, `ss`, `getent`, `findmnt`, `nft list ruleset`,
`systemctl show`. None of them greps the model's shell history or matches the
commands it chose. All pattern-matching is done host-side in the grader, so
the guest commands stay simple, quote-free reads — which also means the
graders are POSIX `sh` and were checked against simulated guest output for
both the passing state and each trap before they ever met a VM.

## What the guest is assumed to provide

Written blind — no VM existed yet — so these are the assumptions to validate
against the real image:

- **Stock tooling in `$PATH`:** `stat` (GNU: `%U %G %a %A %F`), `sysctl`,
  `ss`, `getent`, `findmnt`, `systemctl`, `id`, `grep -rhs`, `cut`, `sed`,
  `awk`. All base Debian 12.
- **`deploy-user`** assumes the stock `www-data` group (GID 33) already
  exists, which it does on a base Debian install. It derives the home path
  from `getent passwd` rather than hard-coding `/home/deploy`.
- **`nginx-8080`** assumes the guest can fetch its own loopback with **curl or
  wget** (the grader tries both). nginx pulls in neither; if the base image
  has neither, add one to the image. The page fetch is loopback-only, so it is
  deterministic and never touches the network.
- **`ip-forward-persist`** assumes Debian's default `net.ipv4.ip_forward = 0`
  (the precheck asserts it).
- **`nftables-drop-23`** assumes the `nft` binary is available; "using
  nftables" is taken to include installing the package if the base image
  lacks it. drop and reject both count — either blocks inbound 23.
- **`fstab-tmpfs`** assumes the kernel reports a 32M tmpfs as `size=32768k` in
  the live mount options (it also accepts `32M`/`32m`/`33554432`). If your
  `util-linux` formats it otherwise, widen the SIZE case in `check.sh`.
- **`heartbeat-timer`** has the **one timing assumption** in the library: its
  effect check wants the log to hold a real date line, which needs the timer
  to have fired at least once. Run `check.sh` at least ~70s after the model
  finishes (or have the harness wait a minute). The cadence and reboot checks
  are instant; only the append needs a tick to have happened. Cadence is found
  by scanning timers for one whose next elapse is within ~2 minutes, so no
  unit name is assumed and a daily timer is rejected.
- **The run-helper is a single token.** The graders call it exactly as the
  contract documents — `"$RUN" '<cmd>'` — so `$1` must be one executable that
  takes the guest command as its single argument, not a two-word string like
  `vmlab run`.
