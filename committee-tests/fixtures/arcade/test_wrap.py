"""The grader for the creation test. The committee never sees this file.

    python3 test_wrap.py

A terminal animation cannot be marked by a machine — the first version of
this test asked for one, and the panel produced a script that referenced a
local from another function and called two curses methods that do not
exist. It passed, because the only thing checked was that it parsed.
Something with an answer can be marked; something that has to be watched
cannot.
"""

import wrap

CASES = [
    ("", 10, []),
    ("hello", 10, ["hello"]),
    ("hello world", 5, ["hello", "world"]),
    ("a b c", 3, ["a b", "c"]),
    # A word with nowhere to break gets a line of its own, over-long.
    ("supercalifragilistic", 5, ["supercalifragilistic"]),
    ("the quick brown fox", 10, ["the quick", "brown fox"]),
    # Runs of whitespace collapse, and leading/trailing space is not a word.
    ("  lots   of   space  ", 7, ["lots of", "space"]),
    ("one two three four", 9, ["one two", "three", "four"]),
]

failures = 0
for text, width, expected in CASES:
    try:
        actual = wrap.wrap(text, width)
    except Exception as error:
        actual = f"{type(error).__name__}: {error}"
    if actual == expected:
        print(f"PASS  wrap({text!r}, {width}) -> {actual!r}")
    else:
        print(f"FAIL  wrap({text!r}, {width}) -> {actual!r}, wanted {expected!r}")
        failures += 1

print()
print("all good" if not failures else f"{failures} failing")
raise SystemExit(1 if failures else 0)
