#!/bin/sh
# check.sh — HELD-OUT grader. The model never sees this file.
#
# Usage:  sh check.sh "<run-helper>"
#   "$RUN" '<cmd>' runs a READ-ONLY command in the guest, echoes its stdout,
#   and exits with the guest command's status. Nothing here modifies the guest.
#
# Grades END STATE by value on two independent axes, because "persistently" is
# two facts, not one:
#   * runtime  — the live sysctl reads 1 right now.
#   * on boot  — some sysctl config file under the standard load paths sets it
#     to 1, so `sysctl --system` at boot re-applies it.
#
# Two-sided trap:
#   * sysctl -w / echo 1 >/proc/sys/... only  -> runtime 1, no config: reboots
#     back to 0. The config axis fails.
#   * edited a .conf file but never applied it -> config 1, runtime still 0.
#     The runtime axis fails.
# Only a solution that does both passes.
set -u

RUN="${1:?usage: sh check.sh <run-helper>}"
fails=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }

# --- runtime value ----------------------------------------------------------
runtime=$("$RUN" 'sysctl -n net.ipv4.ip_forward' 2>/dev/null)
if [ "$runtime" = "1" ]; then
    pass 'net.ipv4.ip_forward is 1 at runtime'
else
    fail "net.ipv4.ip_forward is 1 at runtime (is: ${runtime:-unreadable})"
fi

# --- persisted in a sysctl config file --------------------------------------
# Gather every occurrence across the directories sysctl --system reads at boot.
# -r recursive, -h no filename, -s ignore missing paths. Directories that do
# not exist are simply skipped.
conf=$("$RUN" 'grep -rhs net.ipv4.ip_forward /etc/sysctl.conf /etc/sysctl.d /run/sysctl.d /usr/lib/sysctl.d /lib/sysctl.d' 2>/dev/null)
# Strip comments and whitespace host-side, then anchor-match the setting.
active=$(printf '%s\n' "$conf" | sed 's/#.*//' | tr -d ' \t')
if printf '%s\n' "$active" | grep -q '^net\.ipv4\.ip_forward=1$'; then
    pass 'a sysctl config file sets net.ipv4.ip_forward=1 for boot'
else
    fail 'a sysctl config file sets net.ipv4.ip_forward=1 for boot'
fi

if [ "$fails" -eq 0 ]; then
    echo 'all good'
    exit 0
fi
printf '%d failing\n' "$fails"
exit 1
