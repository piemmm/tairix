# `tairix-chmod` — change file mode bits

Stage 6 deliverable (`AGENTS.md` §3 `userland/apps/`). `chmod` applies a
mode to each of its file operands. The mode is either an absolute octal
value (`644`, `0755`, …) that replaces the permission bits outright, or a
comma-separated list of symbolic clauses (`g+w`, `o-rx`, `a=rx`, `u+s`,
`g=u`, `=644`) that transform the file's current bits. With `-R` a
directory operand is changed and then its contents are changed
recursively. This is the POSIX model.

The crate is `no_std` (with `alloc`), has no `unsafe`, and no
`unwrap`/`expect`/`panic!` in production paths. It depends only on
`lib/*` crates, so it never links a kernel or driver crate.

## Usage

```
chmod [-cfRv] [--] MODE file...

  -R, --recursive       change files and directories recursively
  -c, --changes         report only files whose mode actually changed
  -v, --verbose         report every file processed
  -f, --silent, --quiet suppress most error messages
  -h, --help            show the usage banner
```

A mode and at least one file are required. A long option may be
shortened to any unambiguous prefix (`--rec`). `--` ends option parsing:
every later argument is an operand. POSIX `chmod` spells recursive `-R`;
a bare `-r` is not an option. To set a mode that begins with `-` (for
example, "remove write for all"), write it without the dash (`a-w`) or
end option parsing first (`chmod -- -w file`).

## The mode grammar

The grammar is the shared `tairix_util::mode`, the one `mkdir -m` and the
shell's `umask` read too.

- **Octal**: octal digits, at most `7777`, set the low twelve permission
  bits (the `rwx` triples plus the setuid/setgid/sticky bits) outright.
- **Symbolic**: comma-separated clauses, each
  `[ugoa]*([-+=]([rwxXst]*|[ugo]))+` or `[-+=][0-7]+`. `u`/`g`/`o` select
  the owner/group/other field and `a` selects all. `+` turns the bits on,
  `-` off, and `=` sets the selected fields to exactly those bits.
  Permissions are `r`, `w`, `x`, `X` (execute only for a directory or a
  file that already carries an execute bit), `s` (setuid for the owner,
  setgid for the group), `t` (sticky, for the other field), or one field's
  bits copied (`g=u`); octal after an operator is taken as written. A
  clause may chain several operator sections that share its who (e.g.
  `u+x-w`). A clause naming no one changes every field but leaves the bits
  the process's umask holds, as GNU's does.
- **Directories**: the setuid and setgid bits of a directory stay as they
  are unless the mode names them — `s`, octal after an operator, or an
  octal value of five digits or more (`00755`) — so `chmod 755` keeps a
  directory's group inheritance.

## A mode-changing machine, not a data source

`run` opens each operand through its final link, reads its kind and
current mode, computes the new mode and sets it through that same
descriptor, and walks each directory `-R` must descend. The operations
that reach the outside world are injected seams:

- `Roots` — the shared tree seam (`tairix_fscopy::tree`): each operand
  opened through its final link, a directory held as one so the walk
  descends the very node changed; each tree walked through the directories
  the run holds open, each entry opened by name from its own directory,
  never through a link, and read and changed through that one descriptor.
- `Output` — write the usage banner and the `-c`/`-v` change reports
  (`chmod` is otherwise silent on success).

The process's umask, which a clause naming no field spares, is handed to
`run` as a value. On a running system the tree seam is
`tairix_rt::copy::WorkingDirectory`; in tests it is `tairix_fscopy`'s
in-memory tree, so every parsing, mode-algebra, recursion and race
decision is testable without a kernel.

## Fail closed

An option `getopt` refuses (`ChmodError::Option`), a missing operand
(`ChmodError::Usage`), and a mode operand that is neither octal nor
symbolic (`ChmodError::BadMode`, `invalid mode: 'zzz'`) change nothing: each is reported as
`chmod: reason` and `Try 'chmod --help' for more information.`, exit 1,
as GNU's is. An operand that cannot be opened or read surfaces the
underlying `Errno` as `ChmodError::Stat`; a mode that cannot be applied
is `ChmodError::Apply`; a directory that cannot be opened or listed during
a recursive descent is `ChmodError::Read`; a link operand naming nothing
is `ChmodError::Dangling`. Each is reported in GNU's words and the rest
are still changed, the rest of its directory too; under `-v` a refused
change is followed by GNU's `failed to change mode of …` line and an
unreachable operand by `… could not be accessed`. A link operand changes
what it names; a link met during a recursive descent — listed as one, or
swapped in since the listing — is left alone with what it names (`neither
symbolic link … nor referent has been changed` under `-v`, nothing under
`-c`). The run exits 1 when anything failed, and there is no panic.

## Tests

`cargo test -p tairix-chmod` drives the parser and the engine against the
shared in-memory tree and a recording output (the mode algebra is
`tairix-util`'s own suite): the command grammar (octal and symbolic
modes, the recursive flag, the `-r`-is-not-recursive and unknown-option
refusals, `--`, too-few-operands and bad-mode paths), a clause naming no
one sparing the umask, a directory keeping its set-ID bits under an octal
mode, an octal change, a symbolic change, several files, a
non-recursive directory change leaving its contents alone, a recursive
change touching each directory before its contents, per-node `X`
resolution under recursion, a link in the tree left alone (reported under
`-v`, silent under `-c`), a link operand changing what it names, a
dangling operand, an entry swapped for a link before every seam call of
the walk never carrying the change to what the link names, and the
missing-operand / stat / apply / read-during-recursion fail-closed paths.

See [`docs/src/userland/utilities.md`](../../../docs/src/userland/utilities.md)
for the full subsystem documentation.
