#!/bin/bash
# Shared repo-path resolution for the update-command-spec-version scripts.
#
#   CMD_SIGS_DIR - the command-signatures repo, inferred from this script's own
#                  location (these scripts live inside that repo), so it always
#                  points at the checkout the skill ships from.
#   DERP_DIR     - the derp repo, taken from the $DERP_DIR environment variable
#                  when set; otherwise a sibling of the command-signatures repo
#                  (same parent directory, named "derp").

# Directory this file lives in (resolved through symlinks).
_COMMON_SH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# The command-signatures repo root that contains these scripts.
CMD_SIGS_DIR="$(git -C "${_COMMON_SH_DIR}" rev-parse --show-toplevel)"

# The derp repo: prefer $DERP_DIR, else a sibling of command-signatures.
if [ -n "${DERP_DIR:-}" ]; then
    DERP_DIR="${DERP_DIR}"
else
    DERP_DIR="$(dirname "${CMD_SIGS_DIR}")/derp"
fi

export CMD_SIGS_DIR DERP_DIR
