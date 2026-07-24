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

## Safety invariants

- Model commands run **only** inside the guest, via `vmlab.sh run` (ssh).
- The base image is never mounted writable; tasks write to overlays.
- Precondition checks are read-only.
- The host side (`vmlab.sh`) only ever manages *its own* qemu process, by
  pidfile — it kills nothing it did not start.
