"""The grader. The committee never sees this file — that is the point.

    python3 test_durations.py
"""

import durations

WORKS = [
    ("90s", 90),
    ("5m", 300),
    ("1h", 3600),
    ("1h30m", 5400),
    ("2h5m30s", 7530),
    ("45m15s", 2715),
]

RAISES = ["", "fish", "5x", "12", "h", "1h5"]

failures = 0

for text, expected in WORKS:
    try:
        actual = durations.parse_duration(text)
    except Exception as error:
        actual = f"{type(error).__name__}: {error}"
    if actual == expected:
        print(f"PASS  parse_duration({text!r}) -> {actual!r}")
    else:
        print(f"FAIL  parse_duration({text!r}) -> {actual!r}, wanted {expected!r}")
        failures += 1

for text in RAISES:
    try:
        actual = durations.parse_duration(text)
    except ValueError:
        print(f"PASS  parse_duration({text!r}) raises ValueError")
        continue
    except Exception as error:
        print(f"FAIL  parse_duration({text!r}) raised {type(error).__name__}, wanted ValueError")
    else:
        print(f"FAIL  parse_duration({text!r}) -> {actual!r}, wanted ValueError")
    failures += 1

print()
print("all good" if not failures else f"{failures} failing")
raise SystemExit(1 if failures else 0)
