## NAME

rm — remove files and directories

## SYNOPSIS

`rm [-dfiIrRv] [--] file...`

## DESCRIPTION

Removes each file operand, in order. A non-directory operand is
unlinked — a symbolic link is removed itself, never what it names; a
directory operand is removed only with `-r` (which removes its
contents depth-first and then the directory itself) or, when empty,
with `-d`. With either, an operand whose last component is `.` or `..`
is refused, as POSIX requires.

With `-f` an operand that does not exist is skipped silently and no
question is ever asked. `-i` asks on the standard error stream before
every removal, naming what it would remove
(`remove regular empty file 'a'?`, `remove symbolic link 'l'?`), and
before descending into a directory that holds anything; `-I` asks once
up front before removing more than three operands or before a
recursive removal. A declined
question keeps the object (or skips the whole run, for `-I`) without
error; an unreadable reply is never treated as consent. The later of
`-f`, `-i`, and `-I` wins.

Without `-f`, when the standard input is a terminal, a file the caller
may not write is asked about before it is removed
(`remove write-protected regular file 'x'?`) and removed only on a reply
beginning `y` or `Y`; a symbolic link is never asked about. Under `-i`
the question names the protection too.

A path that cannot be inspected, read, or removed is reported and the
rest are still removed; a directory left holding something is kept
without a word. A recursive removal of `/`, however it is spelled, is
refused under `--preserve-root`, the default. `--` ends option
parsing: every later argument is a path.

## OPTIONS

- `-r, -R, --recursive` — remove directories and their contents.
- `-f, --force` — ignore operands that do not exist; never prompt.
- `-d, --dir` — remove empty directories.
- `-i, --interactive` — prompt before every removal; only a reply
  beginning `y`/`Y` consents.
- `-I` — prompt once before removing more than three operands, or
  before a recursive removal.
- `-v, --verbose` — report each removal as `removed 'file'`.
- `--preserve-root` — refuse to remove `/` recursively (the default).
- `--no-preserve-root` — allow removing `/`. It is accepted only
  spelled in full.
- `-h, -?, --help` — show this command's own short help.

## EXAMPLES

- `rm notes.txt` — remove one file.
- `rm -r Scratch` — remove the `Scratch` tree and everything in it.
- `rm -I a b c d` — ask once, then remove all four files on a `y`.

## EXIT STATUS

- `0` — every removal succeeded (a declined question and a `-f` skip
  are not failures).
- `1` — a path could not be removed, or a prompt or output failed; each
  reason is printed on standard error. A command line that was not
  understood exits `1` too.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

- `cp`
- `ls`
- `mv`
