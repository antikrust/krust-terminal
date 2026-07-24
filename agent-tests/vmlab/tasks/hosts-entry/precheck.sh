#!/bin/sh
# precheck.sh — asserts the task is NOT already satisfied on a pristine guest.
# Same read-only $RUN contract as check.sh. PASS means "confirmed not done".
set -u

RUN="${1:?usage: sh precheck.sh <run-helper>}"
fails=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }

out=$("$RUN" 'getent hosts db.internal' 2>/dev/null)
if [ -z "$out" ]; then
    pass 'db.internal does not resolve yet'
else
    fail "db.internal does not resolve yet (already: $out)"
fi

if [ "$fails" -eq 0 ]; then
    echo 'all good'
    exit 0
fi
printf '%d failing\n' "$fails"
exit 1
