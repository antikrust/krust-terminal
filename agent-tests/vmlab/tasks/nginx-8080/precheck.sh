#!/bin/sh
# precheck.sh — asserts the task is NOT already satisfied on a pristine guest.
# Same read-only $RUN contract as check.sh. PASS means "confirmed not done".
set -u

RUN="${1:?usage: sh precheck.sh <run-helper>}"
fails=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }

if "$RUN" 'command -v nginx' >/dev/null 2>&1; then
    fail 'nginx is not installed yet (already present)'
else
    pass 'nginx is not installed yet'
fi

listen=$("$RUN" 'ss -Hltn' 2>/dev/null)
case "$listen" in
    *:8080\ *|*:8080)   fail 'nothing is listening on 8080 yet (already bound)' ;;
    *)                  pass 'nothing is listening on 8080 yet' ;;
esac

if [ "$fails" -eq 0 ]; then
    echo 'all good'
    exit 0
fi
printf '%d failing\n' "$fails"
exit 1
