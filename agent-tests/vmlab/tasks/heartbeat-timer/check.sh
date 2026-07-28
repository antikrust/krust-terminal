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
# Read each timer's next fire as an absolute wall-clock date and convert it with
# `date -d`. systemd renders an absolute NEXT even for monotonic
# OnBootSec/OnUnitActiveSec timers, so this sidesteps `systemctl show`'s
# NextElapseUSec* fields entirely — which on Ubuntu 22.04's systemd come back
# HUMAN-FORMATTED ("NextElapseUSecMonotonic=2min 514.166ms"). The old
# raw-microsecond math (`value/1000000`) misread that as the integer 2 and so
# failed every correct minute-timer; and because "enabled" was only credited
# inside that same branch, reboot-safety failed with it. That was a grader bug,
# not a model error.
now_epoch=$("$RUN" 'date +%s' 2>/dev/null)
case "$now_epoch" in ''|*[!0-9]*) now_epoch=0 ;; esac

# First field ending in .timer on each row — robust to a leading status glyph.
names=$("$RUN" 'systemctl list-units --type=timer --all --no-legend --no-pager' 2>/dev/null \
        | awk '{for(i=1;i<=NF;i++) if($i ~ /\.timer$/){print $i; break}}')

soon=0
enabled_soon=0
for t in $names; do
    # This timer's next elapse: the leading "YYYY-MM-DD HH:MM:SS TZ" on its
    # list-timers row, or empty when it has none scheduled (shown as "n/a").
    next=$("$RUN" "systemctl list-timers $t --all --no-legend --no-pager" 2>/dev/null \
           | awk '$2 ~ /^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]$/ { print $2, $3, $4; exit }')
    [ -n "$next" ] || continue
    next_epoch=$(date -d "$next" +%s 2>/dev/null)
    case "$next_epoch" in ''|*[!0-9]*) continue ;; esac
    d=$((next_epoch - now_epoch))
    # Fires within ~2 minutes -> minute cadence, not hourly/daily.
    if [ "$d" -ge -5 ] && [ "$d" -le 130 ]; then
        soon=1
        # ON BOOT: the timer that actually ticks must itself be enabled, so it
        # returns after a reboot — judged on that timer, not on any enabled one.
        if [ "$("$RUN" "systemctl is-enabled $t" 2>/dev/null)" = enabled ]; then
            enabled_soon=1
        fi
    fi
done

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
