# `tairix-mv` — move (rename) files and directories

Stage 6 deliverable (`AGENTS.md` §3 `userland/apps/`). `mv` relocates
each of its source operands to a destination. With a single source and a
destination that is not a directory, the source is moved to that exact
path. When the destination is an existing directory — and always when
there is more than one source — each source is moved *into* it under its
own base name. Unlike `cp`, a directory needs no flag: a directory is
moved like any other operand. This is the POSIX model.

The crate is `no_std` (with `alloc`), has no `unsafe`, and no
`unwrap`/`expect`/`panic!` in production paths (`AGENTS.md` §2.9). It
links only `lib/*` crates — a move across volumes copies through the shared
engine (`tairix-fscopy`) — plus `tairix-rt` for the freestanding `Run`
binary, so it never links a kernel or driver crate (`AGENTS.md` §17.4). The package is both
the move library and the `mv` command app's `Run` binary
(`src/run.rs`), registered as the self-contained store bundle
`/System/Commands/mv.app` with its thirteen-locale `Help/` tree (plans/APPS.md
§12.1 Stage B). The production seam maps the kernel's dedicated
`Errno::CrossVolume` rename refusal (the `EXDEV` equivalent) onto the
copy-then-remove fallback.

## Usage

```
mv [-finvT] [-t dir] [--] source... dest

  -f, --force                never prompt before overwriting
  -i, --interactive          ask before overwriting an existing destination
  -n, --no-clobber           never overwrite an existing destination
  -v, --verbose              report each move (renamed 'src' -> 'dst')
  -t dir, --target-directory=dir
                             move every source into dir
  -T, --no-target-directory  treat dest as a normal file (one source)
  -h, -?, --help             show this command's own short help
```

At least one source and a destination are required. Short options may be
combined (e.g. `-fn`), and a long option shortened to any unambiguous
prefix (`--verb`). `--` ends option parsing: every later argument is a
path. With more than one source the destination must be a directory.

## A move machine, not a data source

`run` renames each source onto its destination through the directories
that hold them, opened from the working directory through the shared tree
seam (`tairix_fscopy::tree`): a source is looked at without following a
link, and the rename moves only the node it looked at. Beside that seam
the run reaches the outside world through:

- `Staging` — the fresh name a move across volumes builds its copy under.
- `Prompt` — ask the `-i` overwrite question, fail-closed on an
  unanswerable prompt (never treated as consent).
- `Output` — write the usage banner and the `-v` reports (`mv` is
  otherwise silent on success).

On a running system these are the runtime's seams
(`tairix_rt::copy::WorkingDirectory`); in tests they are the shared
in-memory tree, whose attacker hook renames, replaces or relinks a name
between two calls, so every routing, fallback and race is testable
without a kernel.

## Cross-device relocation

A rename cannot be atomic across filesystems, and the kernel refuses one
with `Errno::CrossVolume`. The engine then keeps the rename's promise by
other means, entirely through nodes it holds open:

1. The source is opened without following a link.
2. The copy is made beside the destination under a fresh staging name,
   owner-only (`0600` a file, `0700` a directory), and kept open.
3. It is filled through its descriptors — bytes with their holes,
   extended attributes, then owner and mode — walking a source directory
   through the shared walk, and every copied entry is recorded with what it
   was when it was copied.
4. It is put on the medium, then
5. renamed onto the destination only while the staging name still holds it.
6. Only the copied entries are removed, each only while unchanged (a
   directory only while it is the one copied, and empty): an entry added or
   changed meanwhile is kept and reported.
7. A failure before the rename discards the copy through the node it was
   built in, and the staging name only while it still names that node.

## No-clobber and force

`-n` never overwrites: a source whose destination already exists is
kept and reported, and the rename itself refuses to replace, so a
destination that appears late is kept too. `-f` only cancels an earlier
`-i` or `-n`; it never removes a destination. Without either flag an
existing destination is overwritten, the default POSIX behaviour.

## Fail closed

An option `getopt` refuses is an `MvError::Option`, and fewer than two
operands or more than one source under `-T` an `MvError::Usage`; either
moves nothing and is reported as `mv: reason` and `Try 'mv --help' for
more information.`, with exit 1.
An operand that cannot be inspected surfaces the underlying `Errno` as
`MvError::Stat`; a rename that fails for a non-boundary reason is
`MvError::Rename` (`Stale` for a source or staged copy swapped since it
was looked at). During a cross-device relocation an unreadable source
is `MvError::Read`, an uncreatable destination is `MvError::Create`, a
failed write is `MvError::Write`, a copied entry that cannot be removed
is `MvError::Remove`, and one that changed meanwhile is
`MvError::Changed`. Each is reported and the next operand is still moved,
the run exiting 1; an operand's own move is all or nothing. There is no
panic.

## Tests

`cargo test -p tairix-mv` drives the parser and the move engine against
the shared in-memory tree and a recording output: the command grammar, a
file renamed to a new path, a directory renamed, a file moved into a
directory under its base name, several files into a directory, the
several-sources-to-a-non-directory `Usage` refusal, the missing-source
fail-closed path, a failed source reported while the next is still moved, `-n` keeping
and reporting an existing destination, a declined `-i` failing the run,
the default overwrite, the failed-rename path, a source swapped after its
look left where it is, the cross-device file / large-file / directory
relocations and their read/write/remove fail-closed paths, and the races
of a move across volumes: a staged copy moved aside behind a planted link
never reaching what the link names, a staged file swapped for a link
neither written through nor put in place, what is added or appended
during the copy kept and reported, a directory kept for an entry that
would not go not called changed, a sweep swapping each name for a link
before every call, a directory moved under a umask withholding its owner's
write, and each node's mode, owner and group carried as far as the mover
may.

See [`docs/src/userland/utilities.md`](../../../docs/src/userland/utilities.md)
for the full subsystem documentation.
