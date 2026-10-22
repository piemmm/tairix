## NAME

mv — move (rename) files and directories

## SYNOPSIS

`mv [-finvT] [-t dir] [--] source... dest`

## DESCRIPTION

Moves each source operand to a destination. With a single source and
a destination that does not name a directory, the source is renamed
to that exact path. When the destination names an existing directory
— and always when there is more than one source — each source is
moved *into* that directory under its own base name.

A move within one volume is an atomic rename that preserves the node's
identity. A move across volumes keeps the same promise: the copy is
built beside the destination, written to the disk and renamed into place
in one step, and only then is the source removed — so the destination
holds what it held until the move is whole, and neither a failure nor a
crash loses anything. Either way, a directory replaces only an empty
directory, nothing else replaces a directory, and no directory moves
into its own subtree.

An existing destination is overwritten by default, skipped under
`-n`, and asked about on the standard error stream under `-i` (a
declined question skips that move without error; an unreadable reply
is never treated as consent). The first failure stops the run before
any later operand. `--` ends option parsing: every later argument is
a path.

The copy carries each file's holes, extended attributes — a RISC OS
filetype, a Mac type and creator — permission bits and owner wherever
the destination keeps them, and moves a symbolic link as a link storing
the same target, never what it names. A file the mover may not give back
to its owner becomes the mover's, without its set-ID bits. Timestamps
are not carried: no call on this system sets them. Attributes the
destination cannot keep are reported in one warning when the run ends,
and in an `omission` record on the standard information stream, before
the sources are gone for good.

## OPTIONS

- `-f, --force` — never prompt before overwriting. The later of `-f`,
  `-i`, and `-n` wins.
- `-i, --interactive` — ask before overwriting an existing
  destination; only a reply beginning `y`/`Y` consents.
- `-n, --no-clobber` — never overwrite an existing destination.
- `-v, --verbose` — report each move as `renamed 'source' -> 'dest'`,
  and a move across volumes as `copied 'source' -> 'dest'` then
  `removed 'source'`.
- `-t dir, --target-directory=dir` — move every source into `dir`,
  which must be an existing directory. The value follows attached
  (`-tdir`, `--target-directory=dir`) or as the next argument.
- `-T, --no-target-directory` — treat the destination as a normal
  file; exactly one source is permitted. Cannot be combined with
  `-t`.
- `-h, -?, --help` — show this command's own short help.

## EXAMPLES

- `mv draft.txt final.txt` — rename one file.
- `mv -v a.txt b.txt Archive` — move both files into `Archive`,
  reporting each move.
- `mv -n new.cfg current.cfg` — install a file only if the
  destination does not already exist.

## EXIT STATUS

- `0` — every move succeeded (a `-n` skip and a declined `-i`
  question are not failures).
- `1` — a filesystem, prompt, or output failure; the reason is
  printed on standard error.
- `2` — the command line was not understood.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

- `cp`
- `ls`
- `rm`
