#!/bin/sh
# precheck.sh — asserts the task is NOT already satisfied on a pristine guest.
# Same read-only $RUN contract as check.sh. PASS means "confirmed not done".
# A pristine Debian guest has an empty ruleset, so nothing blocks 23 yet.
set -u

RUN="${1:?usage: sh precheck.sh <run-helper>}"
fails=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }

ruleset=$("$RUN" 'nft list ruleset' 2>/dev/null)
if printf '%s\n' "$ruleset" | awk '
      /hook[ \t]+input/            { isinput=1 }
      /^[ \t]*}[ \t]*$/            { isinput=0 }
      isinput && /dport/ && /(^|[^0-9])23([^0-9]|$)/ && /(drop|reject)/ { found=1 }
      END { exit(found ? 0 : 1) }
   '; then
    fail 'nothing blocks inbound tcp 23 yet (already present)'
else
    pass 'nothing blocks inbound tcp 23 yet'
fi

if [ "$fails" -eq 0 ]; then
    echo 'all good'
    exit 0
fi
printf '%d failing\n' "$fails"
exit 1
