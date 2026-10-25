## NAME

chmod — change file mode bits

## SYNOPSIS

`chmod [-cfRv] [--] MODE file...`

## DESCRIPTION

Changes the permission bits of each file operand to `MODE`, in order.
`MODE` is either an absolute octal value (`644`, `0755`, …) that
replaces the permission bits outright, or a comma-separated list of
symbolic clauses `[ugoa]*([-+=]([rwxXst]*|[ugo]))+` or `[-+=][0-7]+`
(`g+w`, `o-rx`, `a=rx`, `u+s`, `g=u`, `=644`) that transform the
file's current bits. The symbolic `X` grants execute only to a
directory or to a file that already carries an execute bit, `g=u`
gives the group the owner's bits, `s` sets the owner's and the group's
set-ID bits, and `t` sets the sticky bit. A clause that names no one
(`+x`, `=r`) changes every field except the bits the umask holds.

On a directory, the set-user-ID and set-group-ID bits stay as they are
unless the mode names them: with `s` (`g-s`), with octal after an
operator (`=755`), or with an octal value of five digits or more
(`00755`). A directory's set-group-ID bit is what gives its new
entries its group, so `chmod 755` keeps it.

Only a file's owner may change its mode; the kernel refuses anyone
else, and holding a capability grants no override. A symbolic link
named as an operand changes the file it names. With `-R` a directory
operand is changed and then its contents are changed recursively; a
symbolic link met inside the tree is left alone, and so is what it
names. A file that cannot be reached or changed is reported and the
rest are still changed. `--` ends option parsing: every later argument
is an operand. To set a mode beginning with `-`, write it without the
dash (`a-w`) or end options first (`chmod -- -w file`).

## OPTIONS

- `-R, --recursive` — change files and directories recursively.
- `-c, --changes` — report only files whose mode actually changed.
- `-v, --verbose` — report every file processed, including one whose
  change was refused or that could not be reached.
- `-f, --silent, --quiet` — suppress most error messages; the run
  still fails and the exit status reports it.
- `--preserve-root` — with `-R`, refuse to change `/`, however it is
  reached.
- `--no-preserve-root` — treat `/` like any other directory (the
  default).
- `-h, -?, --help` — show this command's own short help.

## EXAMPLES

- `chmod 644 notes.txt` — owner read/write, everyone else read-only.
- `chmod g+w shared.txt` — add group write to the current bits.
- `chmod -R a=rx Docs` — make the `Docs` tree world-readable and
  traversable.
- `chmod +x build.sh` — make a script executable by everyone the umask
  allows.

## EXIT STATUS

- `0` — every mode change succeeded.
- `1` — a file could not be reached or changed, or output failed; each
  reason is printed on standard error (suppressed under `-f`). A command
  line that was not understood, or whose mode operand parsed as neither
  octal nor symbolic, exits `1` too.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

- `ls`
- `mkdir`
- `rm`
