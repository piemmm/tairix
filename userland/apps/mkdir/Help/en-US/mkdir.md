## NAME

mkdir — make directories

## SYNOPSIS

`mkdir [-pv] [-m MODE] [--] directory...`

## DESCRIPTION

Creates each directory operand, in order. Without `-p` every operand's
parent must already exist and the operand itself must not; an operand
that cannot be made is reported and the next is still made.

With `-p` every missing ancestor is created first, outermost first, and
an operand (or ancestor) that already exists as a directory is not an
error. An ancestor that exists as a file still fails: nothing is ever
silently replaced.

A new directory takes the mode `a=rwx` less the umask. With `-m` each
directory operand made gets exactly `MODE` instead, whatever the umask:
an octal value or symbolic clauses, as `chmod` reads them, starting
from `a=rwx`. A missing ancestor `-p` makes keeps its owner's write
and search however the umask reads, so the directory below it can
always be made. A directory made inside a set-group-ID directory keeps
the bit it inherits unless `MODE` names it. An operand that already
exists keeps its mode. `--` ends option parsing: every later argument
is a path.

## OPTIONS

- `-m, --mode=MODE` — give each directory operand made the mode
  `MODE`, octal or symbolic as in `chmod`, rather than `a=rwx` less the
  umask.
- `-p, --parents` — make missing parent directories; an operand that is
  already a directory is not an error.
- `-v, --verbose` — report each created directory as
  `mkdir: created directory 'dir'`.
- `-h, -?` — show this command's own short help (also `--help`).

## EXAMPLES

- `mkdir Notes` — create one directory in the current directory.
- `mkdir -p Projects/os/build` — create the whole chain, skipping the
  parts that already exist.
- `mkdir -pv Home:/tools/bin` — create under an alias root, reporting
  each new directory.
- `mkdir -m 700 Private` — create a directory only its owner may use.

## EXIT STATUS

- `0` — every directory was created (or, under `-p`, already existed).
- `1` — a filesystem or output failure, or a mode that could not be
  given; the reason is printed on standard error. A command line that
  was not understood, or a `MODE` that is no mode, exits `1` too.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

chmod, rmdir, rm, ls
