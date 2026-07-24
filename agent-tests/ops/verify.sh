#!/bin/sh
# Did the move actually move it? Compares the source tree here against the
# destination on the other machine, by manifest.
#
#   ./agent-tests/ops/verify.sh [remote-host]
#
# Read-only at both ends, so it is safe to run at any point, including
# before deciding whether anything may be deleted — which is the point. A
# migration that deletes its source before this passes is a data loss with
# good intentions.
set -eu

REMOTE="${1:-sentience}"
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$HOME/agent-ops"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

python3 "$HERE/manifest.py" "$ROOT/source" > "$WORK/source.txt"

# The manifest script goes over with the request, so the remote needs
# nothing installed and cannot drift out of step with this copy of it.
ssh -o BatchMode=yes "$REMOTE" 'cat > /tmp/ops-manifest.py' < "$HERE/manifest.py"
ssh -o BatchMode=yes "$REMOTE" 'python3 /tmp/ops-manifest.py "$HOME/agent-ops/dest"' \
    > "$WORK/dest.txt" || {
        echo "Could not read the destination on $REMOTE — has anything arrived?" >&2
        exit 1
    }

if diff -u "$WORK/source.txt" "$WORK/dest.txt" > "$WORK/diff.txt"; then
    printf 'ARRIVED  %s files, byte for byte, modes and symlinks intact\n' \
        "$(grep -c . "$WORK/source.txt")"
    exit 0
fi

echo "DIFFERS — the destination is not the source:"
echo
# `-` is in the source and missing or wrong at the destination; `+` is at
# the destination and not in the source.
sed -n '3,$p' "$WORK/diff.txt" | head -40
echo
# `^---`/`^+++` are diff's own headers, not entries; counting them makes a
# perfect transfer look like it lost one file and gained one.
printf 'missing or wrong: %s\n' "$(grep -c '^-[^-]' "$WORK/diff.txt" || true)"
printf 'unexpected:       %s\n' "$(grep -c '^+[^+]' "$WORK/diff.txt" || true)"
exit 1
