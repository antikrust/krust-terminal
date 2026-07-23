"""Marks a committee transcript against what was actually planted.

    python3 committee-tests/score.py <fixture> <transcript.md>...

The bench prints its own score as a run finishes, and that score is too
generous: it matches a Definite finding to a planted defect by function name
alone. A live run produced

    ✅ Incorrect logic in cheapest function
       DEMONSTRATED by execution — expected None, got a

which is `cheapest('ab')` returning 'a' — a string passed where a list of
prices goes, which is the wrong-type case the reviewer prompt explicitly
excludes, and nothing to do with the planted defect that `cheapest([0, 5])`
returns 5. It scored as a hit. So a defect has to be recognised by something
that only that defect produces, and that is what MARKERS are.

Four numbers per run, because the verdict's own headline is one:

    found     a planted defect in Definite, recognised by its marker
    missed    a planted defect that isn't there — including one raised and
              then refuted by a proof that tested the wrong input
    invented  a Definite claim matching no planted defect
    doubled   the same planted defect counted twice
"""

import re
import sys

# (function, description, [any one of these substrings identifies it])
PLANTED = {
    "stock": [
        (
            "cheapest",
            "`if not best` treats a price of 0 as no price yet",
            ["[0, 5]", "[0,5]", "got 5", "return 0", "returns 0", "zero"],
        ),
        (
            "needs_reorder",
            "`<` where the docstring says fallen *to* the threshold",
            ["expected true", "3, 3", "0, 0", "equal", "equals"],
        ),
        (
            "restock",
            "`log=[]` is one list shared by every call",
            ["state between", "mutable default", "shared", "same list", "accumulat"],
        ),
    ],
    "payday": [
        (
            "split_pot",
            "`/` makes whole cents into floats",
            ["float", "33.33", "not an integer", "whole cent", "//"],
        ),
        (
            "split_pot",
            "an empty roster divides by zero",
            ["zerodivision", "division by zero", "empty"],
        ),
        (
            "top_scores",
            "sorted ascending, so the *lowest* scores come back",
            ["lowest", "ascending", "reverse", "smallest"],
        ),
    ],
    "orders": [
        (
            "render_line",
            "pence printed as pounds — £1250.00 for £12.50",
            ["1250", "pence", "pound", "/ 100", "100"],
        ),
        (
            "checkout",
            "a percentage handed to something wanting a fraction",
            ["fraction", "percent", "-2", "0.2", "/ 100"],
        ),
    ],
}


def definite_entries(transcript):
    """Each ✅ block of the verdict, as one lowercased string."""
    start = transcript.find("**Definite (")
    if start < 0:
        return []
    rest = transcript[start:]
    for marker in ("**Might be (", "**Rejected ("):
        cut = rest.find(marker)
        if cut > 0:
            rest = rest[:cut]
    entries, current = [], None
    for line in rest.splitlines():
        if line.startswith("✅"):
            if current is not None:
                entries.append(current)
            current = line
        elif current is not None:
            current += "\n" + line
    if current is not None:
        entries.append(current)
    return [entry.lower() for entry in entries]


def score(fixture, transcript):
    planted = PLANTED.get(fixture)
    if planted is None:
        raise SystemExit(f"no planted defects recorded for {fixture}")
    entries = definite_entries(transcript)
    claimed = [False] * len(entries)
    results = []
    for function, description, markers in planted:
        hit = None
        for index, entry in enumerate(entries):
            if claimed[index] or function not in entry:
                continue
            if any(marker.lower() in entry for marker in markers):
                hit = index
                break
        if hit is not None:
            claimed[hit] = True
        results.append((function, description, hit is not None))

    # A leftover Definite entry that describes a defect already counted is
    # the same defect twice, not a new one — and it doesn't have to name the
    # function to be doing that. "Mutable default argument for log parameter"
    # never says `restock`, and is unmistakably about it.
    already = [
        (function, markers)
        for (function, _, found), (_, _, markers) in zip(results, planted)
        if found
    ]
    doubled = invented = 0
    for index, entry in enumerate(entries):
        if claimed[index]:
            continue
        same = any(
            function in entry or any(marker.lower() in entry for marker in markers)
            for function, markers in already
        )
        if same:
            doubled += 1
        else:
            invented += 1
    return results, doubled, invented


def main():
    if len(sys.argv) < 3:
        raise SystemExit(__doc__)
    fixture = sys.argv[1]
    for path in sys.argv[2:]:
        with open(path, encoding="utf-8") as handle:
            transcript = handle.read()
        results, doubled, invented = score(fixture, transcript)
        print(f"--- {path}")
        for function, description, found in results:
            print(f"  {'FOUND ' if found else 'MISSED'}  {function} — {description}")
        found = sum(1 for (_, _, hit) in results if hit)
        print(
            f"  {found}/{len(results)} found, {invented} invented, {doubled} doubled"
        )


if __name__ == "__main__":
    main()
