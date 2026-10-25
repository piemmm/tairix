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
built beside the destination, readable only by its owner until it is
whole, written to the disk and renamed into place in one step, and only
then is what was copied removed — so the destination holds what it held
until the move is whole, and neither a failure nor a crash loses
anything. A file added to the source, or one changed, while it was
being copied is kept where it was and reported, with the directory
holding it; on a volume that records no change count, or keeps coarse
times (FAT's two seconds), a rewrite of the same length made within that
grain may go unseen. Either way, a directory replaces only an empty directory,
nothing else replaces a directory, and no directory moves into its own
subtree.

An existing destination is overwritten by default, kept under `-n`
(which says `not replacing` and fails the run, as GNU's does), and
asked about on the standard error stream under `-i` (a declined
question skips that move and fails the run; an unreadable reply is
never treated as consent). An operand that cannot be moved is
reported and the next is still moved. `--` ends option parsing: every
later argument is a path.

The copy carries each file's holes, extended attributes — a RISC OS
filetype, a Mac type and creator — permission bits and owner wherever
the destination keeps them, and moves a symbolic link as a link storing
the same target, never what it names. A file given back to another owner
arrives without its set-ID bits, which the system clears as an owner
changes; one the mover may not give back becomes the mover's, in its own
group where the mover may keep it, and without the set-ID bits of an owner
or a group it did not keep. Timestamps
are not carried: no call on this system sets them. Attributes the
destination cannot keep are reported in one warning when the run ends,
and in an `omission` record on the standard information stream, before
the sources are gone for good.

## OPTIONS

- `-f, --force` — never prompt before overwriting. The later of `-f`,
  `-i`, and `-n` wins.
- `-i, --interactive` — ask before overwriting an existing
  destination; only a reply beginning `y`/`Y` consents.
- `-n, --no-clobber` — never overwrite an existing destination; each
  one kept is reported as `not replacing 'dest'`.
- `-v, --verbose` — report each move as `renamed 'source' -> 'dest'`,
  and a move across volumes entry by entry, as GNU does: `created
  directory 'dest'` for each directory and `copied 'source' -> 'dest'`
  for everything else as it is copied, then `removed 'source'` or
  `removed directory 'source'` for each as the source goes.
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
  destination does not already exist (saying `not replacing` if it
  does).

## EXIT STATUS

- `0` — every move succeeded.
- `1` — an operand could not be moved, `-n` kept a destination, an
  `-i` question was declined, something in a source changed while it
  was moved and was kept, or a prompt or output failed; each failure's
  reason is printed on standard error. A command line that was not
  understood exits `1` too.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

- `cp`
- `ls`
- `rm`
