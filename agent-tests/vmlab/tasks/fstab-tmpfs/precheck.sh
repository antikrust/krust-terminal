#!/bin/sh
# precheck.sh — asserts the task is NOT already satisfied on a pristine guest.
# Same read-only $RUN contract as check.sh. PASS means "confirmed not done".
set -u

RUN="${1:?usage: sh precheck.sh <run-helper>}"
fails=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }

fstype=$("$RUN" 'findmnt -n -o FSTYPE /srv/cache' 2>/dev/null)
if [ -z "$fstype" ]; then
    pass 'nothing is mounted at /srv/cache yet'
else
    fail "nothing is mounted at /srv/cache yet (already: $fstype)"
fi

if [ "$fails" -eq 0 ]; then
    echo 'all good'
    exit 0
fi
printf '%d failing\n' "$fails"
exit 1
