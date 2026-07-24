#!/bin/sh
# check.sh — HELD-OUT grader. The model never sees this file.
#
# Usage:  sh check.sh "<run-helper>"
#   "$RUN" '<cmd>' runs a READ-ONLY command in the guest, echoes its stdout,
#   and exits with the guest command's status. Nothing here modifies the guest.
#
# Grades END STATE by value: every entry in the tree is stat'd and checked for
# owner, group and mode. It never looks at which commands ran.
#
# Traps this is built to catch, each leaving a detectably wrong end state:
#   * chmod 750 /srv/app  (no -R)  -> data/ and config.yml keep their default
#     modes; only the nested entries reveal it.
#   * chown -R app app /srv/app    -> the space instead of a colon leaves the
#     GROUP as root; a wrong-word "app" file argument is silently skipped.
#   * chown/chmod against /srv or the wrong path -> the tree is not app:app 750.
#   * group app never created      -> %G resolves to a number, not "app".
set -u

RUN="${1:?usage: sh check.sh <run-helper>}"
fails=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }

# Every entry the prompt asks for, with the kind it must be. One stat call each
# returns owner:group:mode:kind, colon-separated (kind is the last field and is
# the only one that may contain a space, so a plain cut is unambiguous).
check_entry() {
    path="$1"; wantkind="$2"
    info=$("$RUN" "stat -c %U:%G:%a:%F $path" 2>/dev/null)
    if [ -z "$info" ]; then
        fail "$path exists"
        return
    fi
    u=$(printf '%s' "$info" | cut -d: -f1)
    g=$(printf '%s' "$info" | cut -d: -f2)
    m=$(printf '%s' "$info" | cut -d: -f3)
    k=$(printf '%s' "$info" | cut -d: -f4)
    # GNU stat calls a zero-byte file "regular empty file", not "regular file".
    # A `touch`ed config.yml is a perfectly good regular file; the distinction
    # is stat's, not the task's. (Only the live guest showed this — the mock
    # the fixture was tested against returned "regular file".)
    [ "$k" = "regular empty file" ] && k="regular file"

    if [ "$k" = "$wantkind" ]; then
        pass "$path is a $wantkind"
    else
        fail "$path is a $wantkind (is: ${k:-missing})"
    fi
    if [ "$u" = "app" ] && [ "$g" = "app" ]; then
        pass "$path is owned by app:app"
    else
        fail "$path is owned by app:app (is: ${u:-?}:${g:-?})"
    fi
    if [ "$m" = "750" ]; then
        pass "$path is mode 750"
    else
        fail "$path is mode 750 (is: ${m:-?})"
    fi
}

check_entry /srv/app                 directory
check_entry /srv/app/data            directory
check_entry /srv/app/data/config.yml 'regular file'

if [ "$fails" -eq 0 ]; then
    echo 'all good'
    exit 0
fi
printf '%d failing\n' "$fails"
exit 1
