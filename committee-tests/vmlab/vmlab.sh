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
    ssh_guest true 2>/dev/null && { echo " — ready"; return 0; }
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
  run)    shift; cmd_run "$@" ;;
  *) echo "usage: vmlab.sh {setup|up|run 'cmd'|reset|down|status}" >&2; exit 1 ;;
esac
