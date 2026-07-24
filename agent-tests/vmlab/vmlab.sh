#!/bin/sh
# vmlab — a disposable KVM Linux sandbox for testing model-planned sysadmin
# commands safely.
#
#   vmlab.sh setup      # one-time: ssh key + cloud-init seed (needs base.qcow2)
#   vmlab.sh up         # boot a pristine guest, wait until ssh answers
#   vmlab.sh run 'cmd'  # run a command inside the guest, return its output
#   vmlab.sh reset      # roll the guest back to pristine
#   vmlab.sh down       # shut the guest down, remove the overlay
#   vmlab.sh status
#
# Why a real VM and not a container: the whole point is to run sysadmin
# commands a small model planned, and small models get argument order wrong —
# `rm -rf` the wrong side, `chown -R` the wrong tree. A VM has its own kernel,
# so a command that would wreck a host only wrecks a disposable guest. Every
# model command reaches the guest through `run`, which is ssh into it; nothing
# a model writes ever executes on this host.
#
# Reset is total because the guest boots from a qcow2 overlay on a read-only
# base: rolling back is deleting the overlay and making a new one, so a wrong
# command in one task cannot leak into the next. (A faster live-snapshot reset
# is a follow-up; this one is a ~30s reboot and is bulletproof.)
set -eu

LAB="${VMLAB_DIR:-/mnt/terra/vmlab}"
PORT="${VMLAB_PORT:-2222}"
KEY="$LAB/id_lab"
BASE="$LAB/base.qcow2"
OVERLAY="$LAB/run.qcow2"
SEED="$LAB/seed.iso"
PIDFILE="$LAB/qemu.pid"

ssh_guest() {
  ssh -p "$PORT" -i "$KEY" \
    -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=4 -o LogLevel=ERROR lab@127.0.0.1 "$@"
}

running() { [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; }

cmd_setup() {
  [ -f "$BASE" ] || { echo "no base image at $BASE (still downloading?)" >&2; exit 1; }
  mkdir -p "$LAB"
  [ -f "$KEY" ] || ssh-keygen -t ed25519 -N '' -f "$KEY" -C vmlab >/dev/null
  pub=$(cat "$KEY.pub")
  printf 'instance-id: vmlab\nlocal-hostname: vmlab\n' > "$LAB/meta-data"
  # A fresh overlay has an empty /var/lib/cloud, so cloud-init runs in full on
  # every `up` and configures the `lab` user from scratch — the constant
  # instance-id is fine precisely because the state that keys off it is gone.
  # apt is made non-interactive at the guest level, not by environment. A
  # model writes `sudo apt-get install ...`, and that inner sudo scrubs any
  # DEBIAN_FRONTEND the harness set, so Ubuntu's needrestart/debconf then hang
  # on a tty that isn't there. Setting the debconf frontend in the guest's own
  # database and needrestart to automatic makes every apt run quiet regardless
  # of how the plan invokes it — $nrconf is escaped so the shell heredoc leaves
  # it for needrestart.
  cat > "$LAB/user-data" <<EOF
#cloud-config
users:
  - name: lab
    sudo: ALL=(ALL) NOPASSWD:ALL
    shell: /bin/bash
    lock_passwd: false
    ssh_authorized_keys:
      - $pub
ssh_pwauth: false
package_update: false
package_upgrade: false
write_files:
  - path: /etc/needrestart/conf.d/90-vmlab.conf
    content: "\$nrconf{restart} = 'a';\n\$nrconf{kernelhints} = -1;\n"
  - path: /etc/apt/apt.conf.d/90-vmlab
    content: |
      Dpkg::Use-Pty "0";
      APT::Get::Assume-Yes "true";
      Acquire::ForceIPv4 "true";
runcmd:
  - [ sh, -c, "echo 'debconf debconf/frontend select Noninteractive' | debconf-set-selections" ]
  - [ sh, -c, "echo DEBIAN_FRONTEND=noninteractive >> /etc/environment" ]
  # Stop the guest running apt on its own. Ubuntu's apt-daily/unattended-upgrades
  # fire on boot and hold /var/lib/dpkg/lock; a model's `apt-get install` then
  # waits the whole lock timeout — one nginx task sat for 420s and hit the cap.
  # A disposable guest has no need to auto-update, so silence it and free apt.
  - [ sh, -c, "systemctl disable --now apt-daily.timer apt-daily-upgrade.timer apt-daily.service apt-daily-upgrade.service unattended-upgrades.service 2>/dev/null || true" ]
  - [ sh, -c, "systemctl mask apt-daily.timer apt-daily-upgrade.timer unattended-upgrades.service 2>/dev/null || true" ]
EOF
  genisoimage -quiet -output "$SEED" -volid cidata -joliet -rock \
    "$LAB/user-data" "$LAB/meta-data"
  echo "setup ok — key and seed in $LAB"
}

cmd_up() {
  running && { echo "already up"; return 0; }
  [ -f "$SEED" ] || cmd_setup
  qemu-img create -q -f qcow2 -b "$BASE" -F qcow2 "$OVERLAY" >/dev/null
  # User-mode networking: the guest gets NAT egress (apt works) but cannot see
  # the LAN — it has no route to 192.168.x, only the qemu gateway at 10.0.2.2.
  # Just the one host port is forwarded in, to reach ssh.
  qemu-system-x86_64 \
    -enable-kvm -cpu host -smp 2 -m 2048 \
    -drive file="$OVERLAY",if=virtio,format=qcow2 \
    -cdrom "$SEED" \
    -netdev user,id=n0,hostfwd=tcp:127.0.0.1:"$PORT"-:22 \
    -device virtio-net-pci,netdev=n0 \
    -display none -serial file:"$LAB/console.log" \
    -pidfile "$PIDFILE" -daemonize
  printf 'booting'
  i=0
  while [ "$i" -lt 60 ]; do
    if ssh_guest true 2>/dev/null; then
      # ssh answers well before cloud-init has finished, and until it does the
      # guest is still running its own apt (unattended-upgrades) and hasn't
      # applied our config — so a plan's `apt-get` collides with boot-time apt
      # and waits out the whole lock timeout. One nginx task sat for 420s on
      # exactly this. Wait for cloud-init to actually complete before calling
      # the guest ready.
      printf ' settling'
      ssh_guest "cloud-init status --wait >/dev/null 2>&1 || true" 2>/dev/null
      echo " — ready"
      return 0
    fi
    printf '.'; sleep 3; i=$((i + 1))
  done
  echo " — TIMED OUT after 3 min (see $LAB/console.log)" >&2
  exit 1
}

cmd_down() {
  if running; then
    kill "$(cat "$PIDFILE")" 2>/dev/null || true
    i=0
    while running && [ "$i" -lt 10 ]; do sleep 1; i=$((i + 1)); done
  fi
  rm -f "$OVERLAY" "$PIDFILE"
  echo "down"
}

cmd_reset() { cmd_down >/dev/null; cmd_up; }

cmd_run() {
  running || { echo "guest is down — run 'vmlab.sh up' first" >&2; exit 1; }
  ssh_guest "$@"
}

cmd_prime() {
  # Fetching apt indices over qemu's user-mode NAT is slow — the jammy
  # `universe` Packages file alone is 14 MB at ~50 KB/s — so a plan that runs
  # `apt-get update` before installing anything blew past the execution cap.
  # Prime the base ONCE: boot it, cache the indices and the .debs the tasks
  # need, shut down cleanly, and commit that state into the backing image. A
  # task's overlay then inherits fresh indices (so its `apt-get update` mostly
  # Hits) and pre-downloaded packages (so `apt-get install` needs no network).
  cmd_up >/dev/null
  echo "priming apt cache into the base image (one-off, slow)…"
  ssh_guest "sudo apt-get update && sudo apt-get install -y --download-only nginx nftables" \
    >/dev/null 2>&1 || { echo "prime: apt failed" >&2; }
  ssh_guest "sudo systemctl poweroff" 2>/dev/null || true
  i=0
  while running && [ "$i" -lt 45 ]; do sleep 2; i=$((i + 1)); done
  running && { kill "$(cat "$PIDFILE")" 2>/dev/null; sleep 2; }
  qemu-img commit "$OVERLAY" >/dev/null && echo "base primed"
  rm -f "$OVERLAY" "$PIDFILE"
}

cmd_selftest() {
  # The sandbox's own grader: prove that a change made in the guest does not
  # survive a reset. If this ever fails, nothing built on top of vmlab is
  # safe, because a task's damage would leak into the next.
  cmd_up >/dev/null 2>&1 || true
  running || { echo "selftest: guest would not boot" >&2; exit 1; }
  ssh_guest 'sudo useradd -m _vmlab_probe 2>/dev/null; echo probe | sudo tee /etc/_vmlab_probe >/dev/null' >/dev/null
  cmd_reset >/dev/null 2>&1
  u=$(ssh_guest 'id _vmlab_probe >/dev/null 2>&1 && echo survived || echo gone')
  f=$(ssh_guest 'test -e /etc/_vmlab_probe && echo survived || echo gone')
  if [ "$u" = gone ] && [ "$f" = gone ]; then
    echo "selftest OK — a planted user and file did not survive reset"
  else
    echo "selftest FAIL — reset did not revert (user=$u file=$f)" >&2
    exit 1
  fi
}

cmd_status() {
  if running; then
    echo "up (pid $(cat "$PIDFILE"), port $PORT)"
    ssh_guest 'echo "  guest: $(. /etc/os-release; echo "$PRETTY_NAME"), up $(uptime -p)"' 2>/dev/null \
      || echo "  (ssh not answering yet)"
  else
    echo "down"
  fi
}

case "${1:-}" in
  setup)  cmd_setup ;;
  up)     cmd_up ;;
  down)   cmd_down ;;
  reset)  cmd_reset ;;
  status) cmd_status ;;
  prime)  cmd_prime ;;
  selftest) cmd_selftest ;;
  run)    shift; cmd_run "$@" ;;
  *) echo "usage: vmlab.sh {setup|up|run 'cmd'|reset|down|status}" >&2; exit 1 ;;
esac
