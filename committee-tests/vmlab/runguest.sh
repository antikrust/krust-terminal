#!/bin/sh
# The read-only-check helper a task grader is handed. A `check.sh` calls it as
#   "$RUN" 'some command'
# and it runs that one command inside the vmlab guest, echoing its output and
# passing through its exit code — so a grader checks the guest's real end
# state without knowing anything about qemu or ssh.
# Read-only checks run as root. A grader has to see the whole system to judge
# it — the owner and mode of files inside another user's 0700 home, the live
# nftables ruleset (root-only), a service's unit file — and the guest login is
# the unprivileged `lab`, which gets "Permission denied" and reports the file
# as missing. That masked real results in three tasks as a uniform failure.
# It is safe because these commands only ever read; sudo is passwordless in
# the disposable guest.
exec "${VMLAB:-/mnt/terra/vmlab/vmlab.sh}" run "sudo -n $1"
