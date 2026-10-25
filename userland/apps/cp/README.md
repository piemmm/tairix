# `tairix-cp` — copy files and directories

Stage 6 deliverable (`AGENTS.md` §3 `userland/apps/`). `cp` copies each
of its source operands to a destination. With a single source and a
destination that is not a directory, the source is copied to that exact
path. When the destination is an existing directory — and always when
there is more than one source — each source is copied *into* it under its
own base name. A directory source is copied only with `-r`, which
reproduces the whole subtree; naming a directory without `-r` is an
error. This is the POSIX model.

## Links, instead of bytes

`-l` gives the destination a **second name** for the source's own node
rather than a copy of its bytes, so the two names cannot diverge on a later
write; `-s` makes a **symbolic link** naming the source. Both are
alternatives to copying, not modifiers of it, so they are one last-wins
state (`Contents`) that cannot silently combine.

`-P`/`--no-dereference` reproduces a symbolic-link source **as a link
storing the same target, verbatim** — so a relative or dangling link
survives the copy — instead of following it. `--preserve=links` gives two
sources naming one node two *names* at the destination rather than two
copies, keyed on the identity the `fs_stat`/`fs_readdir` record carries
(`plans/SYMLINKS.md` S7): only a node whose name count exceeds one is
remembered, so the map holds the hard links a copy actually meets rather
than one entry per node on the volume, and it grows on demand from no fixed
ceiling. `-d` is exactly that pair, as in the GNU tool, spelled as the pair
so the two halves cannot drift.

Because a link or a second name is *created*, and a create never replaces a
name, an occupied destination needs `-f`, which puts the link in the name's
place in one step (`tairix_rt::replace_with_link`: made under a staging name,
then renamed over it), so a link that cannot be made leaves the name; without
`-f` the kernel's `AlreadyExists` reaches the caller rather than a silent
overwrite. (The byte copy needs neither: its own create truncates.) A second
name the destination already is counts as made, and a symbolic link put on
the node it would name is the same file, as GNU 9.4 judges each.

`-a`/`--archive` and the `--preserve` set other than `--preserve=links` are
**refused**, not narrowed: `-a` is `-dR --preserve=all`, and
`--preserve=all` includes a node's timestamps, which no call on this system
can set, so honouring it would report a preservation that did not happen.
`-dR` is the rest of `-a` and is available. This is the posture `ln` takes
for `-r` and `readlink` for `-f`: refused for a stated reason, never
stubbed.

## Modes

A new file is born with its source's permission bits, the set-ID and
sticky bits dropped, less the umask, as GNU's `cp` makes one, so a private
file's copy is never more readable than the file; a destination that
already exists keeps its own mode. A new directory is made with its
source's permission and sticky bits and its owner's read, write and search,
opened to its owner outright when the umask withheld any, filled through its
descriptor, and then given — through that descriptor — its source's
permission and sticky bits less the umask, GNU's result, so a `0500` source
copies to `0500` and a umask of `0277` still fills it. `--no-preserve=mode`
makes each new node in the default mode (`tairix_abi::fs::FS_NEW_FILE_MODE`,
`FS_NEW_DIR_MODE`) less the umask instead.

The crate is `no_std` (with `alloc`), has no `unsafe`, and no
`unwrap`/`expect`/`panic!` in production paths (`AGENTS.md` §2.9). It
links only `lib/*` crates — the copy itself is the shared engine
(`tairix-fscopy`) — plus `tairix-rt` for the freestanding `Run` binary, so it
never links a kernel or driver crate (`AGENTS.md` §17.4). The package is both
the copy library and the `cp` command app's `Run` binary
(`src/run.rs`), registered as the self-contained store bundle
`/System/Commands/cp.app` with its thirteen-locale `Help/` tree (plans/APPS.md
§12.1 Stage B).

## Usage

```
cp [-dfilnPrRsvT] [-t dir] [--] source... dest

  -r, -R, --recursive        copy directories and their contents
  -f, --force                remove an unwritable destination and retry
  -i, --interactive          ask before overwriting an existing file
  -n, --no-clobber           never overwrite an existing file
  -l, --link                 a second name for the source's node
  -s, --symbolic-link        a symbolic link naming the source
  -P, --no-dereference       reproduce a link source as a link
  --preserve=links           keep two sources' shared node shared
  --no-preserve=mode         make each new file in the default mode
  -d                         -P and --preserve=links together
  -v, --verbose              report each copy ('src' -> 'dst')
  -t dir, --target-directory=dir
                             copy every source into dir
  -T, --no-target-directory  treat dest as a normal file (one source)
  -h, -?, --help             show this command's own short help
```

At least one source and a destination are required. Short options may be
combined (e.g. `-rf`), and a long option shortened to any unambiguous
prefix (`--rec`). `--` ends option parsing: every later argument is a
path. With more than one source the destination must be a directory.

## A copy machine, not a data source

`run` looks at each source, streams a regular file's bytes from source to
destination, and walks each directory `-r` must reproduce through the
shared tree seam (`tairix_fscopy::tree`). The operations that reach the
outside world are injected seams:

- `FileSystem` — the shared seam's `Roots` (operands opened from the
  working directory, and the directories and files held from them), plus
  an operand read or written through its final link, a resolved spelling,
  and a link put in a name's place in one step.
- `Prompt` — ask the `-i` overwrite question, fail-closed on an
  unanswerable prompt (never treated as consent).
- `Output` — write the usage banner and the `-v` reports (`cp` is otherwise silent on
  success).

On a running system these are syscall- and console-backed; in tests they
are the shared in-memory tree, so every parsing, recursion, mode and force
decision is testable without a kernel.

## Streaming and recursion

A regular file is streamed in fixed-size chunks, so an arbitrarily large
file copies with a bounded buffer. A directory is reproduced by making its
destination (or merging into an existing one), then copying each entry —
files and nested directories alike — by name from the source directory
held open into the destination directory held open. Each directory's
entries are read whole before the copy descends, a directory is entered
only through `Dir::open_dir`, never through a link, and a link met inside
the destination tree is never written through.

## Force

`-f` covers a destination that cannot be created (for example, an
existing read-only file): the destination is removed and the create is
retried exactly once. Without `-f`, a create failure surfaces as a
`CpError::Create`, reported while the rest are still copied.

## Fail closed

An option `getopt` refuses is a `CpError::Option`, and fewer than two
operands or more than one source under `-T` a `CpError::Usage`; either
copies nothing and is reported as `cp: reason` and `Try 'cp --help' for
more information.`, with exit 1.
A directory source named without `-r` is a `CpError::IsDirectory`; a
directory source whose destination already exists as a non-directory is a
`CpError::Overwrite`. An operand that cannot be inspected surfaces the
underlying `Errno` as `CpError::Stat`; an unreadable source is
`CpError::Read`; an uncreatable destination is `CpError::Create`; a failed
write is `CpError::Write`. Each is reported and the rest are still
copied, the rest of its directory too, and the run exits 1; an unusable
destination, a refused prompt or a dead terminal ends the run. There is no
panic.

## Tests

`cargo test -p tairix-cp` drives the parser and the copy engine against
an in-memory tree and a recording output: the command grammar (every
option, clustered short flags, `-`/`--`, the too-few-operands and
unknown-option paths), a single file to a new path, a file copied across
the streaming-chunk boundary, an empty file, a file copied into a
directory under its base name, several files into a directory, the
several-sources-to-a-non-directory `Usage` refusal, the
directory-without-`-r` refusal, recursive reproduction of a nested tree,
a recursive merge into an existing directory, the recursive-onto-a-file
refusal, the missing-source fail-closed path, a failure stopping before a
later source, the unreadable-source / uncreatable-destination /
failed-write paths, the `-f` remove-and-retry recovery, and the
trailing-slash base-name join.

See [`docs/src/userland/utilities.md`](../../../docs/src/userland/utilities.md)
for the full subsystem documentation.
