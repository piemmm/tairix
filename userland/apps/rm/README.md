# `tairix-rm` — remove files and directories

Stage 6 deliverable (`AGENTS.md` §3 `userland/apps/`). `rm` removes each
of its operands in order. A non-directory operand (a regular file, a
symbolic link — removed, never followed — a device node) is unlinked. A
directory operand is removed only with `-r`, which removes its contents
depth-first and then the directory itself; naming a directory without
`-r` is an error. With `-f` an operand that does not exist is skipped
rather than reported. This is the POSIX model. Without `-f`, when the
standard input is a terminal, a file the caller may not write — asked of
the kernel through `fs_access`, never of a link — is asked about as GNU
asks (`rm: remove write-protected regular file 'x'?`, `regular empty file`
for an empty one) and removed only on a reply beginning `y` or `Y`.

The crate is `no_std` (with `alloc`), has no `unsafe`, and no
`unwrap`/`expect`/`panic!` in production paths (`AGENTS.md` §2.9). It
depends only on `lib/*` crates — `tairix-abi`, the shared `lib/fscopy` tree
seam, `lib/path`, `lib/util` and the shared `lib/help` engine (plus
`tairix-rt` for the freestanding `Run` binary) — so it never links a kernel
or driver crate (`AGENTS.md` §17.4). The package is both the removal library
and the `rm` command app's `Run` binary (`src/run.rs`), registered as the
self-contained store bundle `/System/Commands/rm.app` with its
thirteen-locale `Help/` tree (plans/APPS.md §12.1 Stage B).

## Usage

```
rm [-dfiIrRv] [--] file...

  -r, -R, --recursive  remove directories and their contents
  -f, --force          ignore operands that do not exist; never prompt
  -d, --dir            remove empty directories
  -i, --interactive    prompt before every removal
  -I                   prompt once before removing many or recursively
  -v, --verbose        report each removal
  --preserve-root      refuse to remove '/' (the default)
  --no-preserve-root   allow removing '/'
  -h, -?, --help       show this command's own short help
```

At least one file operand is required unless `-f` is given (an empty
`rm -f` removes nothing and succeeds). Short options may be combined
(e.g. `-rf`), and a long option shortened to any unambiguous prefix
(`--rec`) but `--no-preserve-root`, which GNU accepts only in full. `--`
ends option parsing: every later argument is a path.

## A removal machine, not a data source

`run` describes each operand, walks each directory `-r` must remove, and
unlinks every reachable object. The operations that reach the outside
world are injected seams:

- `Roots` — the shared tree seam (`tairix_fscopy::tree`): each operand
  described and removed by name in the directory holding it, a final link
  read as itself; each tree walked through the directories the run holds
  open, each entry reached by name from its own directory and never
  through a link; whether a file is write-protected (its mode, ACL or
  capability gate refusing the caller a write) asked of the directory
  holding it.
- `Prompt` — ask the `-i`/`-I` confirmation questions and the
  write-protected one, fail-closed on an unanswerable prompt (never
  treated as consent), and say whether the replies come from a terminal.
- `Output` — write the usage banner and the `-v` reports (`rm` is
  otherwise silent on success).

On a running system the tree seam is `tairix_rt::copy::WorkingDirectory`
and the rest are console-backed; in tests the tree is `tairix_fscopy`'s
in-memory fixture, so every parsing, recursion, race and force decision is
testable without a kernel.

## Recursion order

`rm -r` removes a directory's contents depth-first — files and nested
directories before the directory that holds them — so a parent is never
unlinked while it still has children. Each directory's entries are read
whole before any is removed, so the walk is not steered by what it
removes. A link met in the tree is removed as itself; a subdirectory
swapped for a link mid-walk is never descended and what it names is never
touched, since the walk descends only through a directory it opens itself,
never through a link. A directory goes only while its name still holds the
node the walk emptied: one renamed into its place meanwhile is kept and
the refusal reported. A directory one of whose entries stays — declined,
or failed — is kept without asking or reporting, as GNU keeps every
ancestor of what it could not remove.

## Fail closed

An option `getopt` refuses (`RmError::Option`) or no operand without `-f`
(`RmError::Usage`) is reported as `rm: reason` and `Try 'rm --help' for
more information.`, and an abbreviated `--no-preserve-root`
(`RmError::AbbreviatedNoPreserveRoot`) alone, as GNU reports them; each
removes nothing and exits 1. A directory named without `-r` is a
`RmError::IsDirectory`. Under `-r` or `-d` an operand whose last
component is `.` or `..` is refused before anything is looked up, as POSIX
requires (`RmError::Dot`, GNU's `refusing to remove '.' or '..' directory:
skipping 'x'`). An operand is described without following a final link,
so a link is removed as itself; one that cannot be inspected surfaces the
underlying `Errno` as `RmError::Stat` — except that `-f` makes a
`NotFound` a silent no-op, while still surfacing any other errno (e.g.
`PermissionDenied`). A directory that cannot be enumerated is
`RmError::Read` unless it is empty, when it is removed as GNU removes it;
a failed unlink is `RmError::Remove`; a recursive removal of the view
root, spelled `/` or otherwise, is `RmError::PreserveRoot`. Each is
reported and the rest are still removed; a directory left holding
something is kept without asking or reporting, the run exits 1, and there
is no panic.

## Tests

`cargo test -p tairix-rm` drives the parser and the removal engine
against the shared in-memory tree and a recording output: the command
grammar (every option, clustered short flags, `-`/`--`, the no-operand and
usage-error paths), a single file, several files in order, the
directory-without-`-r` refusal, recursive depth-first removal of a nested
tree (asserting contents are unlinked before their directory), an empty
directory, the missing-operand fail-closed path and the `-f` skip, the
`-f`-does-not-mask-permission guarantee, a failed operand reported while
the next is still removed, a failed entry reported while its siblings go
and its ancestors stay, the unreadable-directory (empty and not) and
failed-unlink paths, links removed as themselves, `.` and `..` refused
before any lookup, a subdirectory swapped for a link before every seam
call of the walk leaving the link's target untouched, a directory renamed
into an emptied one's name kept and reported, GNU's kind-naming `-i`
questions and its silence over a declined entry's ancestors, the
write-protected question at a terminal (and its absence under `-f`,
without a terminal, and for a link or a directory, its wording under
`-i`, and a protection that cannot be read keeping the file), the root
refused however spelled, and `--no-preserve-root` naming the root's
entries with one slash.

See [`docs/src/userland/utilities.md`](../../../docs/src/userland/utilities.md)
for the full subsystem documentation.
