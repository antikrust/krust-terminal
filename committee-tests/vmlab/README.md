# vmlab — a disposable Linux sandbox for model-planned sysadmin

The code fixtures in `committee-tests/` measure a committee against code. This
measures it against **sysadmin work** — create a user, stand up a service,
set a mount — where the thing that goes wrong is not a wrong value but a
wrong *command*, run against a real system.

That is exactly why it needs a VM and not a scratch directory. A small model
gets argument order wrong — I watched one insist `needs_reorder(7, 0)` should
be `True`, the arguments read backwards. The same slip in `rm -rf a b` or
`chown -R user /` is not a wrong answer, it is a wiped machine. So every
command a model plans runs inside a **KVM guest with its own kernel**, booted
from a throwaway overlay, and nothing a model writes ever touches the host.

## The pieces

- **`vmlab.sh`** — the sandbox lifecycle. Runs on the KVM host (the PC).
  ```sh
  vmlab.sh setup      # once: ssh key + cloud-init seed
  vmlab.sh up         # boot a pristine guest
  vmlab.sh run 'cmd'  # run a command inside the guest
  vmlab.sh reset      # roll back to pristine
  vmlab.sh down
  ```
- **`tasks/`** — sysadmin jobs, each a `PROMPT.txt` and a held-out `check.sh`
  that grades the guest's end state by value, never by the commands used. The
  committee never sees `check.sh`, same discipline as the code fixtures'
  graders.

## The guest

Ubuntu 22.04 server cloud image — mainstream `systemd` + `apt`, so a model's
sysadmin knowledge applies directly and the sandbox's own quirks don't show
up as model errors. (Debian genericcloud was the first choice for the same
reasons; its mirror was down, and Ubuntu is the same family with a reliable
CDN.) User `lab`, passwordless sudo, reached over ssh on a forwarded port.

Networking is qemu user-mode: the guest has NAT egress so `apt` works, but no
route to the LAN — it cannot see `192.168.x`, only the qemu gateway. A
sysadmin task that "moves something to another server" is pointed at the
`committee-tests/ops` fixture's `~/committee-ops`, not at anything real.

## Why reset is trustworthy

The guest boots from a qcow2 overlay whose backing file is the read-only base
image. Rolling back is deleting the overlay and making a new one — there is
no path by which one task's damage reaches the next, because the disk a task
wrote to no longer exists. A live-snapshot reset (seconds instead of a
reboot) is a follow-up; the reboot version is slower and impossible to get
subtly wrong.

## The committee here needs no judge

The code committee has to make several models argue toward a verdict because
there is no oracle for "is this code correct". Here there is: the running
system. So the ops committee is just **fan-out and grade** — each model
proposes a plan, each plan runs in its own fresh guest, and `check.sh` reads
the real end state. No model ever judges another's work, because the machine
already knows the answer. `ops_committee.py` with `MODELS=a,b,c` does exactly
this and reports each independently.

## What's proven

Validated on the real guest, on the PC:

- **Containment, at the limit.** A clean guest (176 `/etc` entries, a planted
  marker) was hit with `rm -rf /etc /usr /var /bin /sbin`. It ate itself —
  afterwards it couldn't run `ls`. `reset` restored it to a pristine
  176-entry `/etc` with the marker gone, because reset kills qemu and drops
  the overlay from the *host* and needs nothing from the destroyed guest.
  This is the whole safety case in one test: the worst a mis-ordered model
  command can do is wreck a disposable guest, and the wreck costs one reset.
- **User management.** "create `deploy` in the sudo group with a bash shell" —
  passes, graded by `id`/`getent` on the guest.
- **Package + service + port.** "install nginx, serve 200 on :8080, enable at
  boot" — passes in ~46s, graded by `systemctl is-enabled` and a real `curl`.
- **Fan-out.** `qwen2.5-coder:7b` (6s) and `mistral-nemo:12b` (37s) both graded
  independently on the same task.

Getting there shook out two harness traps worth knowing: piping a plan into
`bash -s` lets an interactive `adduser`/`apt` eat the following commands as
its answers (stage the plan as a file, run with stdin from `/dev/null`); and
`sudo` scrubs `DEBIAN_FRONTEND`, so Ubuntu's needrestart hangs apt unless the
env is set *inside* the script. Both were caught by grading the end state
rather than trusting the plan.

## Safety invariants

- Model commands run **only** inside the guest, via `vmlab.sh run` (ssh).
- The base image is never mounted writable; tasks write to overlays.
- Precondition checks are read-only.
- The host side (`vmlab.sh`) only ever manages *its own* qemu process, by
  pidfile — it kills nothing it did not start.
