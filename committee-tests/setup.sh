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

# A repository whose committed state is correct and whose working tree is
# not — which is exactly what a review is for.
plant() {
    name="$1"
    repo="$TARGET/$name"
    rm -rf "$repo"
    mkdir -p "$repo"
    cd "$repo"
    git init --quiet
    git config user.email "committee@localhost"
    git config user.name "Committee Tests"
    cp "$FIXTURES/$name/clean.py" "$name.py"
    git add "$name.py"
    git commit --quiet -m "$name: the version everyone agreed on"
    cp "$FIXTURES/$name/buggy.py" "$name.py"
    echo "  $repo — 3 planted defects, uncommitted"
}

echo "Building committee tests in $TARGET"

plant stock
plant payday

# The creation test needs nothing but a clean place to write into.
repo="$TARGET/arcade"
rm -rf "$repo"
mkdir -p "$repo"
cd "$repo"
git init --quiet
git config user.email "committee@localhost"
git config user.name "Committee Tests"
printf '# arcade\n\nSomewhere for the panel to write a program.\n' > README.md
git add README.md
git commit --quiet -m "arcade: an empty stage"
echo "  $repo — empty, for the creation test"

# The edit test: one badly written file, committed, so `git diff` can show
# what the committee changed about it afterwards.
repo="$TARGET/notes"
rm -rf "$repo"
mkdir -p "$repo"
cd "$repo"
git init --quiet
git config user.email "committee@localhost"
git config user.name "Committee Tests"
cp "$FIXTURES/notes/release-notes.md" release-notes.md
git add release-notes.md
git commit --quiet -m "notes: the draft nobody wants to sign off"
# The panel can't read files, so the prompt has to carry the file. Written
# out ready to paste rather than left as an exercise.
{
    printf 'Rewrite this release note. Keep every fact, cut the hype, make it something a user would actually read. Output the complete new file.\n\n'
    cat release-notes.md
} > PROMPT.txt
echo "  $repo — one file to rewrite (paste PROMPT.txt)"

echo
echo "Ready. See committee-tests/README.md for what to type in each."
