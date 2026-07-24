#!/bin/sh
# Deploy the vmlab harness + tasks to the KVM host and run the ops committee
# across every task, for the named models. Run from the repo side (the Mac);
# it ships the scripts and reports the results back.
#
#   MODELS="qwen2.5-coder:7b,mistral-nemo:12b" ./run-ops.sh [remote-host]
#
# Images (base.qcow2, the overlay, keys) live in /mnt/terra/vmlab and are
# never touched — only the harness under .../harness is synced, so a --delete
# here can't wipe the 700MB base image.
set -eu

REMOTE="${1:-sentience}"
MODELS="${MODELS:-qwen2.5-coder:7b}"
HERE="$(cd "$(dirname "$0")" && pwd)"
COMMIT="$(git -C "$HERE" rev-parse --short HEAD 2>/dev/null || echo unknown)"
DEST=/mnt/terra/vmlab/harness

rsync -a --delete --exclude='tasks/_demo' "$HERE"/ "$REMOTE:$DEST/"

ssh -o BatchMode=yes "$REMOTE" "
  set -eu
  cd $DEST
  tasks=\$(find tasks -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
  [ -n \"\$tasks\" ] || { echo 'no tasks/ to run — merge the fixtures first'; exit 1; }
  # Regenerate the cloud-init seed so any guest-config change here takes effect
  # on the next boot; idempotent, keeps the existing ssh key.
  ./vmlab.sh setup >/dev/null
  VMLAB=$DEST/vmlab.sh OPS_COMMIT=$COMMIT MODELS='$MODELS' \
    python3 ops_committee.py \$tasks
"
