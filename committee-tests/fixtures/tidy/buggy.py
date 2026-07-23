"""Reading a simple config file."""


def _is_noise(line):
    """True for a stripped line that carries no setting."""
    return not line or line.startswith("#")


def parse_config(text):
    """Every `key = value` line, as a dict. Blank lines and # comments are
    skipped, and whitespace around a key or value is not part of it."""
    settings = {}
    for raw in text.splitlines():
        line = raw.strip()
        if _is_noise(line):
            continue
        if "=" not in line:
            raise ValueError(f"not a setting: {line!r}")
        key, _, value = line.partition("=")
        settings[key.strip()] = value.strip()
    return settings


def merge(base, override):
    """`base` with `override`'s settings applied over it. Neither is changed."""
    return {**base, **override}
