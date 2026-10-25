## NAME

chown — change file owner and group

## SYNOPSIS

`chown [OPTION]... [OWNER][:[GROUP]] file...`

`chown [OPTION]... --reference=RFILE file...`

## DESCRIPTION

Changes the owning user and group of each file operand, in order.
`OWNER` changes the user and keeps the group, `OWNER:GROUP` changes
both, and `:GROUP` changes only the group. `OWNER:` also gives the file
the owner's login group. An empty operand, or `:` alone, changes
neither. The older `OWNER.GROUP` spelling works with a warning, unless
the whole operand names an account. With `--reference`, each file takes
the owner and group of what `RFILE` names.

`OWNER` and `GROUP` are account and group names, or numbers: a name is
tried first, and a number is taken only when nothing has that name. A
leading `+` (`+1000`) always means a number.

Giving a file to another account, or to a group you are not a member
of, needs the `CAP_FS_CHOWN` capability; without it you may change the
group of a file you own to one of your own groups. A change clears a
file's set-user-ID bit, and its set-group-ID bit when its group may run
it; a directory keeps both.

A symbolic link named as an operand changes the file it names, or with
`-h` the link itself. With `-R` a directory operand and everything in
it are changed, each directory after its contents. A symbolic link met
inside the tree is changed itself, and what it names is never touched;
with `--dereference` the link is left alone too. With `-R` a symbolic
link named as an operand is changed itself and not followed:
`--dereference` changes what it names instead, and `-H` walks a
directory it names. A file that cannot be reached or changed is
reported and the rest are still changed. A report names each owner and
group by name, or by number when no account or group has it. `--` ends
option parsing: every later argument is an operand.

## OPTIONS

- `-c, --changes` — report only files whose ownership actually changed.
- `-f, --silent, --quiet` — suppress most error messages; the run still
  fails and the exit status reports it.
- `-v, --verbose` — report every file processed, including one whose
  change was refused or that could not be reached.
- `--dereference` — change what a symbolic link names rather than the
  link; without `-R` this is the default.
- `-h, --no-dereference` — change a symbolic link itself rather than
  what it names.
- `--preserve-root` — with `-R`, refuse to change `/`, however it is
  reached.
- `--no-preserve-root` — treat `/` like any other directory (the
  default).
- `--from=CURRENT_OWNER:CURRENT_GROUP` — change a file only if its
  current owner and group are these; leave either out to accept any.
- `--reference=RFILE` — give each file the owner and group of `RFILE`
  instead of naming them.
- `-R, --recursive` — change files and directories recursively.
- `-H` — with `-R`, walk the directory a symbolic link named as an
  operand leads to.
- `-L` — with `-R`, follow every symbolic link to a directory; refused,
  because a recursive change never follows a link inside the tree.
- `-P` — with `-R`, follow no symbolic link (the default). The last of
  `-H`, `-L` and `-P` given wins.
- `-?, --help` — show this command's own short help.
- `--version` — print the tool's name and version.

## EXAMPLES

- `chown alice notes.txt` — give `notes.txt` to `alice`, keeping its
  group.
- `chown alice:staff notes.txt` — give it to `alice` and the group
  `staff`.
- `chown :staff shared.txt` — change only the group.
- `chown -R alice Projects` — give the `Projects` tree to `alice`.
- `chown -R --from=bob alice Shared` — give `alice` only what `bob`
  owns in `Shared`.
- `chown --reference=template.txt notes.txt` — give `notes.txt` the
  owner and group of `template.txt`.

## EXIT STATUS

- `0` — every change succeeded.
- `1` — a file could not be reached or changed, or output failed; each
  reason is printed on standard error (suppressed under `-f`). A command
  line that was not understood, or an owner, group or reference file
  that could not be read, exits `1` too and changes nothing.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

- `chmod`
- `ls`
- `stat`
