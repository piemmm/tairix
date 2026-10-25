## NAME

cp — copy files and directories

## SYNOPSIS

`cp [-dfHilLnPrRsvT] [--sparse=WHEN] [--preserve=LIST] [--no-preserve=LIST] [-t dir] [--] source... dest`

## DESCRIPTION

Copies each source operand to a destination. With a single source and
a destination that does not name a directory, the source is copied to
that exact path. When the destination names an existing directory —
and always when there is more than one source — each source is copied
*into* that directory under its own base name.

A directory source is copied only with `-r`, which reproduces the
whole subtree; without `-r` a directory operand is refused. An
existing destination file is overwritten by default, skipped under
`-n`, and asked about on the standard error stream under `-i` (a
declined question skips that copy and fails the run, as GNU's does; an
unreadable reply is never treated as consent).
A destination that is the source's own file — the same name, another
name for it, or a link to it — is refused as the same file and left as
it was, save that `-l` takes it as already linked. A link `-f` puts in
an existing name's place takes that place in one step, made before
anything is replaced, so one that cannot be made leaves the name.

A file that cannot be read, made or written is reported and the rest
are still copied, the rest of its directory too. `--` ends option
parsing: every later argument is a path.

A new file is made with its source's permission bits, without the
set-ID and sticky bits, less the umask, so a private file's copy stays
private; a destination that already exists keeps its own mode. A new
directory ends with its source's permission and sticky bits less the
umask; while its entries are copied in it is open to its owner, so a
read-only source directory, or a umask that withholds the owner's write,
still copies whole. A symbolic link met inside a destination tree is
never written through.

GNU's `-a`/`--archive` and the `--preserve` set other than
`--preserve=links,xattr` are **refused**, not narrowed. `-a` is
`-dR --preserve=all`, and `--preserve=all` includes a node's timestamps,
which no call on this system can set — so honouring `-a` would report a
preservation that did not happen. Use `-dR` for the rest of it.

Each copy carries the source's extended attributes — a RISC OS filetype,
a Mac type and creator — wherever the destination keeps them, and keeps a
sparse file's holes as holes. Attributes a destination cannot keep are
reported in one warning when the run ends, and in an `omission` record on
the standard information stream; `--preserve=xattr` makes such a copy fail
instead.

A directory is never copied into itself: a destination inside the source
— however it is spelled, through a link, a `..` or another name for the
volume — is refused before anything is written, and a directory moved
into the source while the copy runs stops the copy where the walk meets
it.

## OPTIONS

- `-r, -R, --recursive` — copy directories and their contents.
- `-f, --force` — when a destination file cannot be created, remove
  it and retry the copy once; with `-l`, `-s`, or a link `-P`
  reproduces, put the link in an existing name's place.
- `-i, --interactive` — ask before overwriting an existing file; only
  a reply beginning `y`/`Y` consents, and a declined question skips
  that file and fails the run.
- `-n, --no-clobber` — never overwrite an existing file. The later of
  `-i` and `-n` wins.
- `-l, --link` — give the destination a second name for the source's own
  node instead of copying its bytes, so the two names cannot diverge on a
  later write. A directory source still needs `-r`.
- `-s, --symbolic-link` — make a symbolic link naming the source instead
  of copying it.
- `-P, --no-dereference` — reproduce every symbolic-link source as a
  link storing the same target, verbatim, rather than copying what it
  names (so a relative or dangling link survives the copy).
- `-L, --dereference` — follow every symbolic-link source and copy what
  it names.
- `-H` — follow a symbolic link named as an operand, but reproduce those
  met inside a tree. Without `-H`, `-L` or `-P`, a recursive copy
  reproduces every link, its operands' included, and any other copy
  follows its operands' links, as GNU's does; `-l` links what a followed
  link names.
- `--preserve=links` — two sources naming one node get two names at the
  destination rather than two copies, so the copy does not silently
  double the storage.
- `--preserve=xattr` — every extended attribute must be carried: a copy
  whose destination cannot keep one fails, naming it.
  `--preserve=links,xattr` asks for both.
- `--no-preserve=xattr` — copy no extended attributes. `ownership` and
  `timestamps` are accepted too, and are already not preserved.
- `--no-preserve=mode` — make each new file and directory in the mode
  a new one takes when nothing names one, less the umask, rather than
  its source's. `--no-preserve=all` does this too.
- `--sparse=WHEN` — how a sparse file is copied: `auto`, the default,
  keeps its holes; `always` also leaves unwritten every block of zeros it
  stores; `never` writes every byte.
- `-d` — `-P` and `--preserve=links` together, as in the GNU tool.
- `-v, --verbose` — report each copy as `'source' -> 'dest'`.
- `-t dir, --target-directory=dir` — copy every source into `dir`,
  which must be an existing directory. The value follows attached
  (`-tdir`, `--target-directory=dir`) or as the next argument.
- `-T, --no-target-directory` — treat the destination as a normal
  file; exactly one source is permitted. Cannot be combined with
  `-t`.
- `-h, -?, --help` — show this command's own short help.

## EXAMPLES

- `cp notes.txt backup.txt` — copy one file to a new name.
- `cp -r Projects Archive` — reproduce the `Projects` tree inside
  `Archive` (or as `Archive` if it does not exist).
- `cp -v -t Backup a.txt b.txt` — copy both files into `Backup`,
  reporting each copy.

## EXIT STATUS

- `0` — every copy succeeded (a `-n` skip is not a failure).
- `1` — a file could not be copied, an `-i` question was declined, or a
  prompt or output failed; each failure's reason is printed on standard
  error. A command line that was not understood exits `1` too.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

- `ls`
- `mv`
- `rm`
