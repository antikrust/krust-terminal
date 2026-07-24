#!/bin/sh
# precheck.sh — asserts the task is NOT already satisfied on a pristine guest.
# Same read-only $RUN contract as check.sh. PASS means "confirmed not done".
# Debian 12 ships net.ipv4.ip_forward = 0.
set -u

RUN="${1:?usage: sh precheck.sh <run-helper>}"
fails=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }

runtime=$("$RUN" 'sysctl -n net.ipv4.ip_forward' 2>/dev/null)
if [ "$runtime" = "0" ]; then
    pass 'net.ipv4.ip_forward starts at 0'
else
    fail "net.ipv4.ip_forward starts at 0 (is: ${runtime:-unreadable})"
fi

if [ "$fails" -eq 0 ]; then
    echo 'all good'
    exit 0
fi
printf '%d failing\n' "$fails"
exit 1
