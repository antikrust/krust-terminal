#!/bin/sh
# Are the numbers in RESULTS.md still about this code?
#
#   ./committee-tests/stale.sh
#
# A score is a fact about the commit that produced it and nothing else. This
# pipeline moved thirty times in one night, twice in ways that changed a
# result by five of eight, so a table with no commit against it is a table
# nobody can tell is out of date — which is worse than no table, because it
# reads as current.
#
# Reports, per recorded commit, how many changes to the pipeline have landed
# since. Judgement stays with the reader: a docs commit does not invalidate
# anything and a prompt change might invalidate everything.
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
RESULTS="$HERE/RESULTS.md"

# What counts as "the pipeline": if none of this changed, no number here can
# have changed either.
PIPELINE="app/src/ai/agent_sdk/driver/harness/ollama_committee.rs
app/src/ai/agent_sdk/driver/harness/ollama_committee_proof.rs
app/src/ai/agent_sdk/driver/harness/ollama_committee_bench.rs
app/src/ai/agent_sdk/driver/harness/ollama_stream_exec.rs
committee-tests/fixtures
committee-tests/setup.sh"

cd "$REPO"

commits="$(grep -oiE 'measured at `[0-9a-f]{7,}`' "$RESULTS" 2>/dev/null \
    | grep -oE '[0-9a-f]{7,}' | sort -u || true)"

if [ -z "$commits" ]; then
    echo "No 'measured at <commit>' markers in RESULTS.md."
    echo "Add one per block of numbers; the bench prints commit=… on every run."
    exit 0
fi

echo "HEAD is $(git rev-parse --short HEAD)"
echo
stale=0
for commit in $commits; do
    if ! git cat-file -e "$commit^{commit}" 2>/dev/null; then
        echo "  $commit — not in this repository (rebased away?)"
        stale=$((stale + 1))
        continue
    fi
    # shellcheck disable=SC2086
    changes="$(git log --oneline "$commit..HEAD" -- $PIPELINE | wc -l | tr -d ' ')"
    when="$(git log -1 --format=%cr "$commit")"
    if [ "$changes" -eq 0 ]; then
        echo "  $commit ($when) — current: nothing in the pipeline has changed since"
    else
        echo "  $commit ($when) — $changes pipeline change(s) since:"
        # shellcheck disable=SC2086
        git log --format='      %h %s' "$commit..HEAD" -- $PIPELINE | head -6
        stale=$((stale + 1))
    fi
done

echo
if [ "$stale" -eq 0 ]; then
    echo "Every number in RESULTS.md was measured on the pipeline as it stands."
else
    echo "$stale block(s) predate a change to the pipeline. Re-measure before"
    echo "quoting them, or note in RESULTS.md why the change cannot matter."
fi
