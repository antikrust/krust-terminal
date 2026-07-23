# The two-ended fixture

Everything else in `committee-tests/` measures the committee against code.
This measures it against a **move** — files going from one machine to
another, which is a different problem, because you cannot run it a hundred
times and throw the results away.

```bash
./committee-tests/ops/setup.sh [remote-host]     # default: sentience
./committee-tests/ops/verify.sh [remote-host]
```

`setup.sh` builds `~/committee-ops/source` here and an empty
`~/committee-ops/dest` at both ends, wiping whatever was there. `verify.sh`
compares the two by manifest and says whether the move arrived.

## Why a scratch directory and not a jail

The interesting failures in moving files between machines are real ssh, real
permissions, real network. Isolating those away leaves a test of nothing.
What makes this safe is not isolation but that **the target is a disposable
directory nothing else owns**, rebuilt by one command — exactly why the code
fixtures could be run ninety times without anyone caring what they did.

The `rm -rf` is built from variables, so both ends refuse to touch anything
that isn't literally `$HOME/committee-ops`.

## What's planted

The source tree is chosen to break naive copies, and every item on this list
has ruined a real migration:

| | why |
| --- | --- |
| `two words.txt` | spaces, for anything that splits on them |
| `ünïcode.txt` | non-ASCII names |
| `.hidden` | dotfiles, which `*` misses |
| `source/empty/` | empty directories, the classic silent casualty |
| `link-to-plain` | a symlink — `scp` copies what it points at |
| `nested/run.sh` | mode 755, for anything that loses the executable bit |
| `nested/blob.bin` | 2 MB, so a truncated transfer shows as a checksum and not as nothing |
| `dest/plain.txt` | already there with **different** content, so skipping it and clobbering it are both wrong |

## What "arrived" means

`manifest.py` prints one sorted line per entry — kind, mode, size, checksum,
relative path — and `verify.sh` diffs the two. No model, no exit code, no
transfer tool gets a say in whether the files are there.

Symlinks record their target and **not** their mode: those bits belong to
the operating system rather than the file, and macOS and Linux disagree
about the same link, so including them failed every cross-platform move for
a difference nothing caused and nothing could fix.

It is read-only at both ends, which is the point — it is safe to run at any
moment, including before deciding whether anything may be deleted. **A
migration that deletes its source before this passes is a data loss with
good intentions.**

Demonstrated catches, all on real transfers:

- `rsync -a` — arrives, 11 entries, byte for byte.
- `scp -r` — silently resolves the symlink into a copy of its target.
- a transfer truncated part-way — same path, same mode, wrong size and
  checksum.
