## NAME

elsh — the TAIRiX command shell

## SYNOPSIS

`elsh [-h | -?]`

## DESCRIPTION

Runs an interactive command shell — a read-eval-print loop over the
inherited standard streams. A typed command word is resolved first
against the shell's builtins, then the system command store
(`/System/Commands`), the system application store
(`/System/Applications`), the user's own command store
(`<home>/Commands`) and application store (`<home>/Applications`),
then the directories of the `PATH` variable; those four stores are a
fixed prefix the user cannot reorder or override, so `PATH` can never
shadow a system command. An unresolved word exits `127`; a resolved
but non-executable bundle exits `126`.

The builtins:

- `cd <path>`, `pwd` — change and print the working directory.
- `echo ...` — print its operands.
- `export NAME=value`, `unset NAME` — edit the exported environment.
- `jobs`, `fg`, `bg` — job control.
- `ulimit` — read and impose resource limits.
- `elevate <user> <program> [argument ...]` — run one command, with the
  arguments given, re-authenticated through the console's login
  supervisor.
- `shopt [-pqsu] [-o] [name ...]` — show or set the filename-matching
  options.
- `help` — list the builtins.
- `exit [code]` — end the session.

A word holding an unquoted `*`, `?` or `[...]` is a filename pattern,
replaced by the sorted names it matches, as in bash: `cp *.tmp /tmp`. A
leading `.` must be matched by a `.`, and a pattern matching nothing is
left as written. Braces expand first: `a{b,c}` gives `ab ac`, and
`{1..5}` counts. Quoting makes any of these characters literal, and a
variable's value is never read as a pattern. `shopt -s` turns on bash's
options: `nullglob` (no match gives no word), `failglob` (no match is an
error), `dotglob`, `nocaseglob`, `globstar` (`**` matches any depth) and
`extglob` (`?(…)`, `*(…)`, `+(…)`, `@(…)`, `!(…)`); `shopt -o -s noglob`
turns filename matching off.

On a terminal the shell runs an interactive line editor with the
familiar bash/zsh keys: Up/Down (or `Ctrl-P`/`Ctrl-N`) walk the
session's command history, `Ctrl-R` searches it incrementally,
Left/Right/Home/End and `Ctrl-A`/`Ctrl-E` move the cursor,
`Ctrl-K`/`Ctrl-U`/`Ctrl-W`/`Ctrl-Y` kill and yank, `Ctrl-T`
transposes, `Ctrl-L` repaints on a cleared screen, `Ctrl-C` discards
the line under edit, and `Ctrl-D` on an empty line ends the session.
Tab completes the word under the cursor: command names (builtins and
installed command apps), file paths, and — for a redirection target or
a reference-shaped word — resource references such as `sys:random`.
A namespace completes its registered selectors a segment at a time
(`state:` → `net/` → `wan/` → `link`); where the registry knows only the
shape — an interface name, an interrupt line — Tab offers the machine's own
names, or nothing at all when this session may not list them. `info:`/`state:`/`stats:` appear
as arguments and after `<`, which reads the value
(`cat < info:mem/physical`), but never after `>`: such a resource is never
written by redirection.
Piped or scripted input bypasses the editor and behaves identically
with or without a terminal.

The shell takes no operands: script execution is not yet part of its
grammar.

## OPTIONS

- `-h, -?` — show this command's own short help and exit.

## EXIT STATUS

- The `exit` builtin's code, or `0` when the input stream ends (or the
  short help was shown).
- `2` — the invocation was not understood.

## ENVIRONMENT

- `PATH` — the directories searched after the fixed store prefix.
- `GLOBIGNORE` — `:`-separated patterns whose matches a filename pattern
  drops.
- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`), exported to every launched command.

## SEE ALSO

- `man`
