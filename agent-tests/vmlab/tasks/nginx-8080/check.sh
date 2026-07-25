#!/bin/sh
# check.sh — HELD-OUT grader. The model never sees this file.
#
# Usage:  sh check.sh "<run-helper>"
#   "$RUN" '<cmd>' runs a READ-ONLY command in the guest, echoes its stdout,
#   and exits with the guest command's status. Nothing here modifies the guest.
#
# Grades END STATE by value: it asks the running system what is listening on
# 8080 and what that socket actually serves. It never reads nginx config or
# checks which commands ran — a config that is correct but never reloaded is a
# fail, because nothing is listening.
#
# Traps this catches:
#   * left on port 80 (ignored "not the default 80") -> nothing on 8080.
#   * served the stock nginx welcome page            -> body lacks the marker.
#   * edited config but never reloaded/started nginx -> nothing on 8080.
#
# Assumes the guest can fetch its own loopback: curl or wget. Both are in
# mainstream Debian userland; nginx pulls neither, so if the base image has
# neither, install one in the image or relax this to nginx's own access log.
set -u

RUN="${1:?usage: sh check.sh <run-helper>}"
fails=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }

# --- nginx is installed -----------------------------------------------------
# `which`, not `command -v`: the run-helper wraps every probe in `sudo -n`, and
# sudo execs a binary — it cannot run the `command` shell builtin, so
# `command -v nginx` fails even when nginx is installed and serving. `which` is
# a real binary and sudo's secure_path includes /usr/sbin, where nginx lives.
if "$RUN" 'which nginx' >/dev/null 2>&1; then
    pass 'nginx is installed'
else
    fail 'nginx is installed'
fi

# --- something is listening on TCP 8080 -------------------------------------
# ss is from iproute2 (always present). -H drops the header, -l listening,
# -t tcp, -n numeric so :8080 is literal and not a service name.
listen=$("$RUN" 'ss -Hltn' 2>/dev/null)
case "$listen" in
    *:8080\ *|*:8080)   pass 'a service is listening on TCP 8080' ;;
    *)                  fail 'a service is listening on TCP 8080' ;;
esac

# --- the socket serves the expected page ------------------------------------
# Loopback only: deterministic, no external network. Try curl, then wget.
body=$("$RUN" 'curl -s http://localhost:8080/ 2>/dev/null || wget -qO- http://localhost:8080/ 2>/dev/null' 2>/dev/null)
case "$body" in
    *"deploybox up"*)   pass 'GET :8080/ returns a page containing "deploybox up"' ;;
    *)                  fail 'GET :8080/ returns a page containing "deploybox up"' ;;
esac

if [ "$fails" -eq 0 ]; then
    echo 'all good'
    exit 0
fi
printf '%d failing\n' "$fails"
exit 1
