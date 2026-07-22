#!/bin/sh
# Builds the committee's test repositories somewhere that survives a reboot.
#
# Each one is a real git repository, because the committee's review mode runs
# `git diff HEAD` in the working directory — a folder inside this repo would
# hand it the fork's own diff instead.
#
# Re-run it any time to reset every test to its starting state: it is
# destructive and idempotent by design, so a run that leaves a tree in a mess
# costs one command to undo.
#
#   ./committee-tests/setup.sh                 # builds in ~/committee-tests
#   ./committee-tests/setup.sh /some/where     # or wherever you say
set -eu

FIXTURES="$(cd "$(dirname "$0")/fixtures" && pwd)"
TARGET="${1:-$HOME/committee-tests}"

mkdir -p "$TARGET"

# Every repo starts the same way, and none of them may mention that they are
# a test: a fixture that says "three planted defects" in a docstring is a
# fixture that puts the answer in the diff.
fresh() {
    repo="$TARGET/$1"
    rm -rf "$repo"
    mkdir -p "$repo"
    cd "$repo"
    git init --quiet
    git config user.email "committee@localhost"
    git config user.name "Committee Tests"
}

# A repository whose committed state is correct and whose working tree is
# not — which is exactly what a review is for.
plant() {
    name="$1"
    fresh "$name"
    cp "$FIXTURES/$name/clean.py" "$name.py"
    git add "$name.py"
    git commit --quiet -m "$name: the version everyone agreed on"
    cp "$FIXTURES/$name/buggy.py" "$name.py"
    echo "  $TARGET/$name — 3 planted defects, uncommitted"
}

echo "Building committee tests in $TARGET"

plant stock
plant payday

# The cross-file test. Both defects are in the diff and neither is visible
# from it: what makes them wrong is pricing.py, which does not change.
fresh orders
cp "$FIXTURES/orders/pricing.py" pricing.py
cp "$FIXTURES/orders/invoice.clean.py" invoice.py
cp "$FIXTURES/orders/cart.clean.py" cart.py
git add pricing.py invoice.py cart.py
git commit --quiet -m "orders: pence in, pounds out"
cp "$FIXTURES/orders/invoice.buggy.py" invoice.py
cp "$FIXTURES/orders/cart.buggy.py" cart.py
echo "  $TARGET/orders — 2 defects across 2 files, uncommitted"

# The creation test needs nothing but a clean place to write into.
fresh arcade
printf '# arcade\n\nSomewhere for the panel to write a program.\n' > README.md
git add README.md
git commit --quiet -m "arcade: an empty stage"
echo "  $TARGET/arcade — empty, for the creation test"

# The edit test, graded by a file the committee never sees. The panel can't
# read files, so the prompt has to carry the one it is meant to fix.
fresh durations
cp "$FIXTURES/durations/durations.py" durations.py
cp "$FIXTURES/durations/test_durations.py" test_durations.py
git add durations.py test_durations.py
git commit --quiet -m "durations: parses some of what it claims to"
{
    printf 'This function has a bug: it does not add its components up. Fix it, keeping everything else about it the same, and output the complete file.\n\n```python\n'
    cat durations.py
    printf '```\n'
} > PROMPT.txt
echo "  $TARGET/durations — one bug to fix, graded by test_durations.py"

# The prose edit test: one badly written file, committed, so `git diff` can
# show what the committee changed about it afterwards.
fresh notes
cp "$FIXTURES/notes/release-notes.md" release-notes.md
git add release-notes.md
git commit --quiet -m "notes: the draft nobody wants to sign off"
{
    printf 'Rewrite this release note. Keep every fact, cut the hype, make it something a user would actually read. Output the complete new file.\n\n'
    cat release-notes.md
} > PROMPT.txt
echo "  $TARGET/notes — one file to rewrite (paste PROMPT.txt)"

echo
echo "Ready. See committee-tests/README.md for what to type in each."
