"""Reading a simple config file."""


def parse_config(text):
    """Every `key = value` line, as a dict. Blank lines and # comments are
    skipped, and whitespace around a key or value is not part of it."""
    settings = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if "=" not in line:
            raise ValueError(f"not a setting: {line!r}")
        key, value = line.split("=", 1)
        settings[key.strip()] = value.strip()
    return settings


def merge(base, override):
    """`base` with `override`'s settings applied over it. Neither is changed."""
    merged = dict(base)
    merged.update(override)
    return merged
