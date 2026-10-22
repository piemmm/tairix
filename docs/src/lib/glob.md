# `tairix-glob` — the shared glob matcher

`lib/glob` is the one definition of shell wildcard patterns. The shell's
filename generation, `ls --ignore`/`--hide` and the tree file manager's tag,
filter and search prompts compile their patterns here; none embeds a matcher
of its own.

## Syntax

Shell pattern matching notation (POSIX XBD 2.13) and bash's `extglob`:

- `*` matches any run of characters, `?` exactly one.
- `[...]` matches one character: members, code-point ranges, `!`/`^`
  negation, the POSIX classes (`[:alnum:]` `[:alpha:]` `[:blank:]`
  `[:cntrl:]` `[:digit:]` `[:graph:]` `[:lower:]` `[:print:]` `[:punct:]`
  `[:space:]` `[:upper:]` `[:xdigit:]`) with bash's `[:word:]` and
  `[:ascii:]`, equivalence classes `[=c=]`, and collating symbols `[.c.]` or
  the portable character set's names (`[.space.]`, `[.hyphen.]`). A `]`
  first, or a `-` first or last, is a member; `\` escapes inside an
  expression too. A `[` that opens no complete expression is an ordinary
  character, as POSIX specifies — `[abc` matches itself.
- Classes are Unicode-aware; `digit` and `xdigit` stay ASCII, as POSIX
  requires in every locale, and `punct` is what remains of `graph` once
  letters and digits are removed.
- With `Options::EXTENDED`: `?(…)`, `*(…)`, `+(…)`, `@(…)` and `!(…)` over
  `|`-separated alternatives, nested to `MAX_GROUP_DEPTH`.

`Options` also carries `CASE_INSENSITIVE` (literals and ranges fold through a
one-to-one lower-case mapping, while classes test the character as written,
as bash's `nocaseglob` does), `PERIOD` (a leading `.` is matched only by a
literal `.` — never by `*`, `?`, a bracket expression or `!(…)`), and
`PATHNAME` (pattern and candidate split at every `/` and matched component by
component). They are glibc's `FNM_EXTMATCH`, `FNM_CASEFOLD`, `FNM_PERIOD` and
`FNM_PATHNAME`; `ls` matches with `PERIOD`, as GNU's `fnmatch` call does.

## Compiling and matching

`Pattern::with_options` compiles pattern text, `\` escaping the next
character; `Pattern::from_marked` compiles characters the caller has already
marked literal — the shell's quoting — so a word reaches the matcher without
an escaping round trip. Compiling is the only fallible step: a reversed
range, an unknown class or collating element, an unclosed group or a
dangling escape is a typed `GlobError`, never matched as something else.

`Pattern::components()` exposes each pathname component's `literal()` text
(when it holds no wildcard) and whether it is spelled `**`, which is what a
tree walker needs; `may_hold_wildcards` is the cheap test for whether text
can be a pattern at all. A `Scratch` lets a caller testing many candidates
against one pattern allocate once.

## Bounded matching

A pattern is untrusted input, so `MAX_PATTERN_LEN`, `MAX_TOKENS`,
`MAX_CLASS_ITEMS`, `MAX_GROUPS` and `MAX_GROUP_DEPTH` are fixed security
bounds. Matching never backtracks: the engine tracks, as bit sets, the
positions of the candidate each node can reach from those the previous one
reached. A pattern without groups costs time linear in its nodes times the
candidate's length. Each group's reach from a start position, and what its
alternatives reach in one match, are memoised, and a repetition's rows are
built from the candidate's end downwards — a match only moves forward, so
each row is a union of rows already known — which bounds an extended pattern
by the cube of the candidate's length. A component holding a group therefore
matches candidates of at most `MAX_EXTENDED_LEN` characters, the longest name
a filesystem stores (`FS_NAME_MAX`).

## Tests

Unit tests cover the syntax, every option, the bounds and the fail-closed
errors, and check the engine against a matcher written straight from the
definitions over thousands of random extended patterns. `tests/fuzz_glob.rs`
compiles and matches hostile text under every option combination.
