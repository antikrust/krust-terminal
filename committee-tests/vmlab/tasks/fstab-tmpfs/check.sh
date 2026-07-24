#!/bin/sh
# check.sh — HELD-OUT grader. The model never sees this file.
#
# Usage:  sh check.sh "<run-helper>"
#   "$RUN" '<cmd>' runs a READ-ONLY command in the guest, echoes its stdout,
#   and exits with the guest command's status. Nothing here modifies the guest.
#
# Grades END STATE by value on two independent axes, because "mounted now AND
# on every boot" is two facts:
#   * MOUNTED  — findmnt reports a tmpfs actually mounted at /srv/cache now.
#   * ON BOOT  — /etc/fstab carries an uncommented tmpfs entry for /srv/cache,
#                so `mount -a` at boot restores it.
#
# Two-sided trap:
#   * `mount -t tmpfs ...` only          -> mounted now, absent from fstab:
#     gone after a reboot. The fstab axis fails.
#   * edited fstab but never mounted it  -> in fstab, not mounted now. The
#     mounted axis fails.
#
# The size axis assumes the kernel reports a 32M tmpfs as size=32768k in the
# live mount options (32*1024). If your guest's util-linux formats it
# differently, widen the SIZE case below.
set -u

RUN="${1:?usage: sh check.sh <run-helper>}"
fails=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }

# --- mounted now, as tmpfs --------------------------------------------------
fstype=$("$RUN" 'findmnt -n -o FSTYPE /srv/cache' 2>/dev/null)
if [ "$fstype" = "tmpfs" ]; then
    pass '/srv/cache is a tmpfs mounted right now'
else
    fail "/srv/cache is a tmpfs mounted right now (fstype: ${fstype:-not mounted})"
fi

# --- the 32M size -----------------------------------------------------------
opts=$("$RUN" 'findmnt -n -o OPTIONS /srv/cache' 2>/dev/null)
case ",$opts," in
    *size=32768k*|*size=32M*|*size=32m*|*size=33554432*)
        pass 'the tmpfs is sized 32M' ;;
    *)
        fail "the tmpfs is sized 32M (options: ${opts:-none})" ;;
esac

# --- persisted in /etc/fstab ------------------------------------------------
# A non-comment line whose mountpoint (field 2) is /srv/cache and whose type
# (field 3) is tmpfs. Parsed host-side.
fstab=$("$RUN" 'cat /etc/fstab' 2>/dev/null)
if printf '%s\n' "$fstab" | awk '
      /^[ \t]*#/ { next }
      $2 == "/srv/cache" && $3 == "tmpfs" { found=1 }
      END { exit(found ? 0 : 1) }
   '; then
    pass '/etc/fstab has a tmpfs entry for /srv/cache (restored on boot)'
else
    fail '/etc/fstab has a tmpfs entry for /srv/cache (restored on boot)'
fi

if [ "$fails" -eq 0 ]; then
    echo 'all good'
    exit 0
fi
printf '%d failing\n' "$fails"
exit 1
