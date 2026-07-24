#!/bin/sh
# check.sh — HELD-OUT grader. The model never sees this file.
#
# Usage:  sh check.sh "<run-helper>"
#   "$RUN" '<cmd>' runs a READ-ONLY command in the guest, echoes its stdout,
#   and exits with the guest command's status. Nothing here modifies the guest.
#
# Grades END STATE by value: it asks the name-service layer to resolve the name
# and checks the address that comes back. It never greps /etc/hosts, so any
# correct mechanism grades the same — but a wrongly written file still fails,
# because it won't resolve.
#
# Trap this catches — the argument-ORDER slip:
#   /etc/hosts is "ADDRESS name", so a line written "db.internal 10.0.0.5" (name
#   first) makes getent read db.internal as the address; the lookup returns
#   nothing and this fails. A typo'd address returns the wrong value and fails.
set -u

RUN="${1:?usage: sh check.sh <run-helper>}"
fails=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }

# getent hosts prints "ADDRESS  canonical [aliases...]"; the first field is the
# resolved address. A correct /etc/hosts entry resolves locally with no DNS.
out=$("$RUN" 'getent hosts db.internal' 2>/dev/null)
addr=$(printf '%s\n' "$out" | awk 'NR==1{print $1}')

if [ "$addr" = "10.0.0.5" ]; then
    pass 'db.internal resolves to 10.0.0.5'
else
    fail "db.internal resolves to 10.0.0.5 (got: ${out:-no result})"
fi

if [ "$fails" -eq 0 ]; then
    echo 'all good'
    exit 0
fi
printf '%d failing\n' "$fails"
exit 1
