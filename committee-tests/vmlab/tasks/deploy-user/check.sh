#!/bin/sh
# check.sh — HELD-OUT grader. The model never sees this file.
#
# Usage:  sh check.sh "<run-helper>"
#   "$RUN" '<cmd>' runs a READ-ONLY command in the guest, echoes its stdout,
#   and exits with the guest command's status. Nothing here modifies the guest.
#
# Grades END STATE by value: any command sequence that reaches the correct
# state passes. The commands the model used are never inspected.
#
# The whole point of the traps here is that they leave a DETECTABLY wrong end
# state — a user with no www-data group, a home that was never made, an
# authorized_keys sshd will silently ignore because root owns it or the
# directory is group-writable.
set -u

RUN="${1:?usage: sh check.sh <run-helper>}"
fails=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }

# The distinctive body of the ed25519 key the prompt handed out. Matched as a
# substring so a trailing comment difference doesn't matter.
KEYBODY='AAAAIH2wJ3qk9r7Z1nQm4Xc0Vb8yT5sL6pA2dF3gH1jK9lQ'

# --- the account exists -----------------------------------------------------
pw=$("$RUN" 'getent passwd deploy' 2>/dev/null)
if [ -n "$pw" ]; then
    pass 'user deploy exists'
else
    fail 'user deploy exists'
fi

# --- membership of www-data -------------------------------------------------
# The classic trap: useradd deploy without -G www-data. The account is there,
# the group is not, and only a by-value check of the group list notices.
groups=$("$RUN" 'id -nG deploy' 2>/dev/null)
case " $groups " in
    *" www-data "*) pass 'deploy is in the www-data group' ;;
    *)              fail "deploy is in the www-data group (groups: ${groups:-none})" ;;
esac

# Everything below needs the home directory. Pull it from passwd rather than
# assuming /home/deploy, so a deliberately relocated home still grades.
home=$(printf '%s' "$pw" | cut -d: -f6)

if [ -z "$home" ]; then
    fail 'deploy has a home directory (no passwd entry to read it from)'
else
    kind=$("$RUN" "stat -c %F $home" 2>/dev/null)
    if [ "$kind" = "directory" ]; then
        pass "home directory $home exists"
    else
        # useradd without -m: passwd names a home that was never created.
        fail "home directory $home exists (stat: ${kind:-missing})"
    fi

    # --- home not writable by group/other (sshd StrictModes) ----------------
    hmode=$("$RUN" "stat -c %A $home" 2>/dev/null)
    hgw=$(printf '%s' "$hmode" | cut -c6)
    how=$(printf '%s' "$hmode" | cut -c9)
    if [ "$hgw" = "-" ] && [ "$how" = "-" ]; then
        pass "home directory is not writable by group or others ($hmode)"
    else
        fail "home directory is not writable by group or others ($hmode)"
    fi

    ssh_dir="$home/.ssh"
    ak="$ssh_dir/authorized_keys"

    # --- authorized_keys holds the key --------------------------------------
    akcontent=$("$RUN" "cat $ak" 2>/dev/null)
    case "$akcontent" in
        *"$KEYBODY"*) pass 'authorized_keys contains the deploy key' ;;
        *)            fail 'authorized_keys contains the deploy key' ;;
    esac

    # --- owned by deploy, not root ------------------------------------------
    # Doing the whole job under sudo and forgetting to chown leaves .ssh and
    # the key file owned by root: deploy cannot manage its own keys, and it is
    # the wrong end state even though sshd might still read it.
    sdir_owner=$("$RUN" "stat -c %U $ssh_dir" 2>/dev/null)
    if [ "$sdir_owner" = "deploy" ]; then
        pass '.ssh directory is owned by deploy'
    else
        fail ".ssh directory is owned by deploy (owner: ${sdir_owner:-missing})"
    fi

    ak_owner=$("$RUN" "stat -c %U $ak" 2>/dev/null)
    if [ "$ak_owner" = "deploy" ]; then
        pass 'authorized_keys is owned by deploy'
    else
        fail "authorized_keys is owned by deploy (owner: ${ak_owner:-missing})"
    fi

    # --- .ssh and the key not writable by group/other (sshd StrictModes) ----
    smode=$("$RUN" "stat -c %A $ssh_dir" 2>/dev/null)
    sgw=$(printf '%s' "$smode" | cut -c6)
    sow=$(printf '%s' "$smode" | cut -c9)
    if [ "$sgw" = "-" ] && [ "$sow" = "-" ]; then
        pass ".ssh directory is not writable by group or others ($smode)"
    else
        fail ".ssh directory is not writable by group or others ($smode)"
    fi

    amode=$("$RUN" "stat -c %A $ak" 2>/dev/null)
    agw=$(printf '%s' "$amode" | cut -c6)
    aow=$(printf '%s' "$amode" | cut -c9)
    if [ "$agw" = "-" ] && [ "$aow" = "-" ]; then
        pass "authorized_keys is not writable by group or others ($amode)"
    else
        fail "authorized_keys is not writable by group or others ($amode)"
    fi
fi

if [ "$fails" -eq 0 ]; then
    echo 'all good'
    exit 0
fi
printf '%d failing\n' "$fails"
exit 1
