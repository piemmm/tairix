## NAME

head — output the first part of files

## SYNOPSIS

`head [option...] [file...]`

## DESCRIPTION

Prints the first 10 lines of each `file` to standard output. With more
than one `file`, each part is preceded by a `==> file <==` header. With
no `file`, or when `file` is `-`, standard input is read.

`-n` and `-c` change how much is printed: a plain count prints the
first `num` lines or bytes; a count written with a leading `-` prints
everything **except** the last `num` lines or bytes. A count may carry
a multiplier suffix: `b` (512), `kB` (1000), `K` (1024), `MB`, `M`,
`GB`, `G`, and so on for `T`, `P`, `E`, `Z`, `Y`, `R`, `Q` (a lone
letter multiplies by powers of 1024; with `B` by powers of 1000; with
`iB` by powers of 1024).

The historical first-argument form `head -num` (with optional trailing
`b`/`k`/`m` multipliers and `l`/`q`/`v`/`z` letters) is accepted, as in
the GNU tool.

A file that cannot be read is reported on standard error and the run
continues with the next file.

On a terminal, each name the output shows is quoted where a shell would
need it, and any control or other invisible character in it is written
as an escape, so a name cannot command or rearrange the screen; GNU
shows it unchanged. A pipe or a file receives the name byte for byte.

## OPTIONS

- `-c, --bytes <num>` — print the first `num` bytes of each file; with
  a leading `-`, all but the last `num` bytes.
- `-n, --lines <num>` — print the first `num` lines of each file; with
  a leading `-`, all but the last `num` lines.
- `-q, --quiet, --silent` — never print the `==> file <==` headers.
- `-v, --verbose` — always print the `==> file <==` headers.
- `-z, --zero-terminated` — lines are NUL-delimited instead of
  newline-delimited.
- `-h, -?` — show this command's own short help.

## EXAMPLES

- `head log.txt` — print the first 10 lines of `log.txt`.
- `head -n 3 a b` — print the first 3 lines of `a` and of `b`, each
  under its header.
- `head -c 1K image` — print the first 1024 bytes of `image`.
- `head -n -1 notes` — print `notes` without its last line.

## EXIT STATUS

- `0` — every file was printed (or the short help was written).
- `1` — a file could not be read, or the output could not be delivered.
  A command line that was not understood exits `1` too.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

- `cat`
- `wc`
- `man`
