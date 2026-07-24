#!/bin/sh
# check.sh — HELD-OUT grader. The model never sees this file.
#
# Usage:  sh check.sh "<run-helper>"
#   "$RUN" '<cmd>' runs a READ-ONLY command in the guest, echoes its stdout,
#   and exits with the guest command's status. Nothing here modifies the guest.
#
# Grades END STATE by value on three independent axes, none of which reads a
# unit file or matches an ExecStart line, so a wrapper script or any unit name
# grades the same as an inline command:
#
#   1. EFFECT   — /var/log/heartbeat.log has a real date line in it. This is
#                 the method-agnostic proof the whole chain fires and appends
#                 to the right file.  ** TIMING NOTE **: it needs the timer to
#                 have fired at least once, i.e. the guest must be checked at
#                 least ~70s after the timer was started. Run check.sh a minute
#                 after the model finishes (or have the harness wait); firing
#                 is the only non-instant fact in this whole library.
#   2. CADENCE  — some active timer's next elapse is <= ~2 min away, i.e. it
#                 runs every minute and not every day. A daily/hourly timer's
#                 next run is hours out and fails this.
#   3. ON BOOT  — that minute-cadence timer is enabled, so it comes back after
#                 a reboot. `systemctl start` without `enable` fails this.
#
# The timer is discovered by scanning, not by an assumed name.
set -u

RUN="${1:?usage: sh check.sh <run-helper>}"
fails=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }

# --- 1. effect: the log has a date line -------------------------------------
log=$("$RUN" 'cat /var/log/heartbeat.log' 2>/dev/null)
if [ -n "$log" ] && printf '%s\n' "$log" | grep -Eq '[0-9][0-9]:[0-9][0-9]:[0-9][0-9]'; then
    pass 'a date line has been appended to /var/log/heartbeat.log'
else
    fail 'a date line has been appended to /var/log/heartbeat.log (has the timer fired yet? see TIMING NOTE)'
fi

# --- 2 & 3. cadence and reboot-safety, by scanning the loaded timers --------
now_epoch=$("$RUN" 'date +%s' 2>/dev/null)
now_up=$("$RUN" 'cat /proc/uptime' 2>/dev/null | awk '{print $1}' | cut -d. -f1)
case "$now_epoch" in ''|*[!0-9]*) now_epoch=0 ;; esac
case "$now_up" in ''|*[!0-9]*) now_up=0 ;; esac

# First field ending in .timer on each row — robust to a leading status glyph.
names=$("$RUN" 'systemctl list-units --type=timer --all --no-legend --no-pager' 2>/dev/null \
        | awk '{for(i=1;i<=NF;i++) if($i ~ /\.timer$/){print $i; break}}' \
        | tr '\n' ' ')

soon=0
enabled_soon=0
if [ -n "${names# }" ]; then
    records=$("$RUN" "systemctl show $names -p Id -p NextElapseUSecRealtime -p NextElapseUSecMonotonic -p UnitFileState -p ActiveState --no-pager" 2>/dev/null)
    result=$(printf '%s\n' "$records" | awk -v now="$now_epoch" -v up="$now_up" '
      function ev() {
        if (as == "active") {
          if ((er+0) > 0) {
            d = (er+0)/1000000.0 - now
            if (d >= -5 && d <= 130) { soon=1; if (ufs=="enabled") es=1 }
          }
          if ((em+0) > 0) {
            d = (em+0)/1000000.0 - up
            if (d >= -5 && d <= 130) { soon=1; if (ufs=="enabled") es=1 }
          }
        }
        er=0; em=0; as=""; ufs=""
      }
      BEGIN { er=0; em=0; as=""; ufs=""; soon=0; es=0 }
      /^NextElapseUSecRealtime=/  { er=substr($0, index($0,"=")+1) }
      /^NextElapseUSecMonotonic=/ { em=substr($0, index($0,"=")+1) }
      /^UnitFileState=/           { ufs=substr($0, index($0,"=")+1) }
      /^ActiveState=/             { as=substr($0, index($0,"=")+1) }
      /^$/ { ev() }
      END  { ev(); print soon "," es }
    ')
    soon=$(printf '%s' "$result" | cut -d, -f1)
    enabled_soon=$(printf '%s' "$result" | cut -d, -f2)
fi

if [ "${soon:-0}" = "1" ]; then
    pass 'a timer is scheduled to fire within ~2 minutes (minute cadence)'
else
    fail 'a timer is scheduled to fire within ~2 minutes (minute cadence)'
fi

if [ "${enabled_soon:-0}" = "1" ]; then
    pass 'that minute-cadence timer is enabled (survives reboot)'
else
    fail 'that minute-cadence timer is enabled (survives reboot)'
fi

if [ "$fails" -eq 0 ]; then
    echo 'all good'
    exit 0
fi
printf '%d failing\n' "$fails"
exit 1
