"""Turning short duration strings into a number of seconds."""

UNITS = {"h": 3600, "m": 60, "s": 1}


def parse_duration(text):
    """The number of seconds a duration string describes.

    Understands any run of number-then-unit pairs, largest first:
    "90s" is 90, "5m" is 300, "1h30m" is 5400, "2h5m30s" is 7530.

    Raises ValueError on anything that isn't that shape, including the
    empty string, a bare number with no unit, and an unknown unit.
    """
    total = 0
    number = ""
    for character in text:
        if character.isdigit():
            number += character
        elif character in UNITS:
            if not number:
                raise ValueError(f"not a duration: {text!r}")
            total = int(number) * UNITS[character]
            number = ""
        else:
            raise ValueError(f"not a duration: {text!r}")
    if number or not text:
        raise ValueError(f"not a duration: {text!r}")
    return total
