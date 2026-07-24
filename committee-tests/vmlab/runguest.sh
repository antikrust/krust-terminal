#!/bin/sh
# The read-only-check helper a task grader is handed. A `check.sh` calls it as
#   "$RUN" 'some command'
# and it runs that one command inside the vmlab guest, echoing its output and
# passing through its exit code — so a grader checks the guest's real end
# state without knowing anything about qemu or ssh.
exec "${VMLAB:-/mnt/terra/vmlab/vmlab.sh}" run "$1"
