## NAME

cp — copy files and directories

## SYNOPSIS

`cp [-dfilnPrRsvT] [--sparse=WHEN] [--preserve=LIST] [--no-preserve=LIST] [-t dir] [--] source... dest`

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
declined question skips that copy without error; an unreadable reply
is never treated as consent).
A destination that is the source's own file — the same name, another
name for it, or a link to it — is refused as the same file and left as
it was.

The first failure stops the run before any later operand. `--` ends
option parsing: every later argument is a path.

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

## OPTIONS

- `-r, -R, --recursive` — copy directories and their contents.
- `-f, --force` — when a destination file cannot be created, remove
  it and retry the copy once.
- `-i, --interactive` — ask before overwriting an existing file; only
  a reply beginning `y`/`Y` consents.
- `-n, --no-clobber` — never overwrite an existing file. The later of
  `-i` and `-n` wins.
- `-l, --link` — give the destination a second name for the source's own
  node instead of copying its bytes, so the two names cannot diverge on a
  later write. A directory source still needs `-r`.
- `-s, --symbolic-link` — make a symbolic link naming the source instead
  of copying it.
- `-P, --no-dereference` — reproduce a symbolic-link source as a link
  storing the same target, verbatim, rather than copying what it names
  (so a relative or dangling link survives the copy). Without it a link
  source is followed.
- `--preserve=links` — two sources naming one node get two names at the
  destination rather than two copies, so the copy does not silently
  double the storage.
- `--preserve=xattr` — every extended attribute must be carried: a copy
  whose destination cannot keep one fails, naming it.
  `--preserve=links,xattr` asks for both.
- `--no-preserve=xattr` — copy no extended attributes. `mode`, `ownership`
  and `timestamps` are accepted too, and are already not preserved.
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

- `0` — every copy succeeded (a `-n` skip and a declined `-i`
  question are not failures).
- `1` — a filesystem, prompt, or output failure; the reason is
  printed on standard error.
- `2` — the command line was not understood.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

- `ls`
- `mv`
- `rm`
