#!/bin/sh
# Deploy the vmlab harness + tasks to the KVM host and run the sysadmin eval
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
# Recommended sweep set (pass via MODELS): qwen2.5-coder:7b, mistral-nemo:12b,
# qwen2.5-coder:14b, deepseek-coder-v2:16b, phi4:14b. Leave out
# qwen2.5-coder:32b — the 19GB model OOMs Ollama on this box (measured 0/8, all
# HTTP 500 / RemoteDisconnected); run it on its own if the box ever has the RAM.
MODELS="${MODELS:-qwen2.5-coder:7b}"
HERE="$(cd "$(dirname "$0")" && pwd)"
COMMIT="$(git -C "$HERE" rev-parse --short HEAD 2>/dev/null || echo unknown)"
DEST=/mnt/terra/vmlab/harness

# Mirror each run into the Mac's Moloch so the sweep is watchable in the app,
# not just this log. Moloch binds to localhost, so a reverse tunnel gives the
# PC-side eval a local port that forwards back to it — no LAN exposure. With no
# token found the mirror stays off and the eval runs exactly as before.
MOLOCH_PORT="${MOLOCH_PORT:-10102}"
MOLOCH_TOKEN="${MOLOCH_TOKEN:-$(cat "$HOME/.moloch/api-token" 2>/dev/null || true)}"
TUNNEL=""
MOLOCH_ENVS=""
if [ -n "$MOLOCH_TOKEN" ]; then
  TUNNEL="-R ${MOLOCH_PORT}:127.0.0.1:${MOLOCH_PORT}"
  MOLOCH_ENVS="MOLOCH_URL=http://127.0.0.1:${MOLOCH_PORT} MOLOCH_TOKEN='${MOLOCH_TOKEN}'"
fi

# Optional model preamble — the "with preamble" arm of the A/B. Forwarded to
# ops_bench.py when OPS_PREAMBLE is set. It rides inside the double-quoted ssh
# command below (so it is not word-split locally) and is single-quoted for the
# remote shell — keep the preamble text free of single quotes. Empty -> unaided.
PREAMBLE_ENV=""
if [ -n "${OPS_PREAMBLE:-}" ]; then
  PREAMBLE_ENV="OPS_PREAMBLE='${OPS_PREAMBLE}'"
fi

rsync -a --delete --exclude='tasks/_demo' "$HERE"/ "$REMOTE:$DEST/"

# shellcheck disable=SC2086  # $TUNNEL must word-split into ssh's args
ssh -o BatchMode=yes $TUNNEL "$REMOTE" "
  set -eu
  cd $DEST
  tasks=\$(find tasks -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
  [ -n \"\$tasks\" ] || { echo 'no tasks/ to run — merge the fixtures first'; exit 1; }
  # Regenerate the cloud-init seed so any guest-config change here takes effect
  # on the next boot; idempotent, keeps the existing ssh key.
  ./vmlab.sh setup >/dev/null
  VMLAB=$DEST/vmlab.sh OPS_COMMIT=$COMMIT MODELS='$MODELS' $MOLOCH_ENVS $PREAMBLE_ENV \
    python3 -u ops_bench.py \$tasks
"
