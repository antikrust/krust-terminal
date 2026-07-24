"""Everything about a directory tree that a move is supposed to preserve.

    python3 manifest.py <root>

One sorted line per entry — kind, mode, size, path, and a checksum for
regular files — so two ends can be compared with `diff` and nothing has to
trust a model, a transfer tool, or an exit code about whether the files
arrived.

Sorted and relative on purpose: the whole point is that a manifest taken on
one machine is comparable byte-for-byte with one taken on another, where the
absolute paths and the directory ordering will both differ.
"""

import hashlib
import os
import stat
import sys


def digest(path):
    """The file's SHA-256, read in chunks — a fixture may hold a big file
    precisely because big files are where transfers get truncated."""
    sha = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            sha.update(block)
    return sha.hexdigest()


def entries(root):
    for current, dirnames, filenames in os.walk(root, followlinks=False):
        # Deterministic order, so the manifest of an unchanged tree is
        # always the same text.
        dirnames.sort()
        filenames.sort()
        for name in dirnames + filenames:
            full = os.path.join(current, name)
            relative = os.path.relpath(full, root)
            info = os.lstat(full)
            mode = stat.S_IMODE(info.st_mode)
            if stat.S_ISLNK(info.st_mode):
                # The target, not the resolved content: a move that turns a
                # symlink into a copy of what it pointed at has changed the
                # tree, even though every file still reads the same.
                #
                # No mode. A symlink's permission bits are the operating
                # system's business, not the file's — macOS says 0755 and
                # Linux says 0777 for the very same link — so including them
                # fails every cross-platform move for a difference nothing
                # did and nothing can fix.
                yield f"link -> {os.readlink(full)} {relative}"
            elif stat.S_ISDIR(info.st_mode):
                # Empty directories are the classic silent casualty, so they
                # get a line of their own.
                yield f"dir  {mode:04o} {relative}"
            else:
                yield f"file {mode:04o} {info.st_size:>10} {digest(full)} {relative}"


def main():
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    root = sys.argv[1]
    if not os.path.isdir(root):
        raise SystemExit(f"not a directory: {root}")
    for line in sorted(entries(root)):
        print(line)


if __name__ == "__main__":
    main()
