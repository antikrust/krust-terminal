#!/bin/sh
# Builds the two-ended fixture a move can be tested against: a source tree
# here and an empty destination on another machine, both disposable.
#
#   ./agent-tests/ops/setup.sh [remote-host]
#
# The remote defaults to `sentience`. Both ends are wiped and rebuilt, so a
# run that mangles either of them costs one command to undo — which is the
# only reason it is safe to point a local model at this at all.
#
# Deliberately NOT a jail. The interesting failures in moving files between
# machines are real ssh, real permissions and real network; what makes this
# safe is not isolation but that the target is a scratch directory nothing
# else owns.
set -eu

REMOTE="${1:-sentience}"
ROOT="$HOME/agent-ops"

# A `rm -rf` built from variables is worth one guard. This only ever removes
# a directory called agent-ops directly under a home directory.
case "$ROOT" in
    "$HOME"/agent-ops) ;;
    *) echo "refusing to touch $ROOT" >&2; exit 1 ;;
esac

echo "Local end: $ROOT"
rm -rf "$ROOT"
mkdir -p "$ROOT/source/nested/deeper" "$ROOT/source/empty" "$ROOT/dest"

# The contents are chosen to break naive copies, because those are the bugs
# worth catching and every one of them has bitten a real migration.
printf 'the ordinary case\n' > "$ROOT/source/plain.txt"
printf 'a file with spaces in its name\n' > "$ROOT/source/two words.txt"
printf 'unicode: caffè, naïve, 日本語\n' > "$ROOT/source/ünïcode.txt"
printf '#!/bin/sh\necho executable\n' > "$ROOT/source/nested/run.sh"
chmod 755 "$ROOT/source/nested/run.sh"
printf 'buried\n' > "$ROOT/source/nested/deeper/buried.txt"
printf 'a dotfile, which globs miss\n' > "$ROOT/source/.hidden"
ln -s plain.txt "$ROOT/source/link-to-plain"
# Big enough that a truncated transfer shows up as a checksum mismatch
# rather than as nothing at all.
dd if=/dev/urandom of="$ROOT/source/nested/blob.bin" bs=1024 count=2048 2>/dev/null

# One file already at the destination, with different content: a move that
# skips existing paths, or clobbers without checking, is wrong in opposite
# directions and both need catching.
printf 'stale content that must not survive a correct move\n' > "$ROOT/dest/plain.txt"

echo "Remote end: $REMOTE:~/agent-ops"
ssh -o BatchMode=yes "$REMOTE" '
    set -eu
    root="$HOME/agent-ops"
    case "$root" in
        "$HOME"/agent-ops) ;;
        *) echo "refusing to touch $root" >&2; exit 1 ;;
    esac
    rm -rf "$root"
    mkdir -p "$root/dest"
    printf "stale content that must not survive a correct move\n" > "$root/dest/plain.txt"
'

echo
echo "Source:      $ROOT/source"
echo "Local dest:  $ROOT/dest"
echo "Remote dest: $REMOTE:~/agent-ops/dest"
echo
echo "Check a move with:  ./agent-tests/ops/verify.sh [remote-host]"
