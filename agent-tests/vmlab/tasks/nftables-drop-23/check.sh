#!/bin/sh
# check.sh — HELD-OUT grader. The model never sees this file.
#
# Usage:  sh check.sh "<run-helper>"
#   "$RUN" '<cmd>' runs a READ-ONLY command in the guest, echoes its stdout,
#   and exits with the guest command's status. Nothing here modifies the guest.
#
# Grades END STATE by value: it dumps the live ruleset and confirms an input
# base chain carries a rule that blocks tcp dport 23. All parsing is host-side,
# so the guest command is just `nft list ruleset` with no quoting to trip over.
#
# Trap this catches — the argument/direction slips a small model makes:
#   * dropped the wrong port (22/ssh instead of 23)   -> no dport-23 block.
#   * dropped it outbound (hook output, not input)    -> not in an input chain.
#   * accepted instead of dropped                     -> verdict is accept.
# drop and reject both count: either one blocks inbound 23, which is the
# outcome the prompt asks for. An input chain with a `policy drop` and no
# explicit rule is NOT accepted — the prompt asks for a rule.
#
# Assumes the nft binary is available in the guest; "using nftables" implies
# installing the nftables package if the base image lacks it.
set -u

RUN="${1:?usage: sh check.sh <run-helper>}"
fails=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }

ruleset=$("$RUN" 'nft list ruleset' 2>/dev/null)

# Walk the ruleset tracking whether we are inside a base chain hooked to input,
# and look there for a rule naming port 23 with a drop/reject verdict.
#   - isinput turns on at the `type filter hook input ...` line
#   - it turns off only at a line that is nothing but a closing brace, so an
#     inline set like `dport { 22, 23 }` does not reset it mid-rule
#   - 23 is matched as a standalone number so 230, 2323, 123 do not match
if printf '%s\n' "$ruleset" | awk '
      /hook[ \t]+input/            { isinput=1 }
      /^[ \t]*}[ \t]*$/            { isinput=0 }
      isinput && /dport/ && /(^|[^0-9])23([^0-9]|$)/ && /(drop|reject)/ { found=1 }
      END { exit(found ? 0 : 1) }
   '; then
    pass 'the live ruleset drops inbound tcp dport 23 in an input chain'
else
    fail 'the live ruleset drops inbound tcp dport 23 in an input chain'
fi

if [ "$fails" -eq 0 ]; then
    echo 'all good'
    exit 0
fi
printf '%d failing\n' "$fails"
exit 1
