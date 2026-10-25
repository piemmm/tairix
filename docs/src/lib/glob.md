# `tairix-glob` — the shared glob matcher

`lib/glob` is the one definition of shell wildcard patterns. The shell's
filename generation, `ls --ignore`/`--hide` and the tree file manager's tag,
filter and search prompts compile their patterns here; none embeds a matcher
of its own.

## Syntax

Shell pattern matching notation (POSIX XCU 2.13) and bash's `extglob`:

- `*` matches any run of characters, `?` exactly one.
- `[...]` matches one character: members, code-point ranges, `!`/`^`
  negation, the POSIX classes (`[:alnum:]` `[:alpha:]` `[:blank:]`
  `[:cntrl:]` `[:digit:]` `[:graph:]` `[:lower:]` `[:print:]` `[:punct:]`
  `[:space:]` `[:upper:]` `[:xdigit:]`) with bash's `[:word:]` and
  `[:ascii:]`, equivalence classes `[=c=]`, and collating symbols `[.c.]` or
  the portable character set's names and bash's aliases (`[.space.]`,
  `[.hyphen.]`, `[.minus.]`). A `]`
  first, or a `-` first or last, is a member; `\` escapes inside an
  expression too. A `[` that opens no complete expression is an ordinary
  character, as POSIX specifies — `[abc` matches itself.
- Classes are Unicode-aware; `digit` and `xdigit` stay ASCII, as POSIX
  requires in every locale, and `punct` is what remains of `graph` once
  letters and digits are removed.
- With `Options::EXTENDED`: `?(…)`, `*(…)`, `+(…)`, `@(…)` and `!(…)` over
  `|`-separated alternatives, nested to `MAX_GROUP_DEPTH`.

`Options` also carries `CASE_INSENSITIVE` (literals and ranges fold through a
one-to-one lower-case mapping, a range's order checked on its folded
endpoints so `[a-Z]` is `[a-z]`, while classes test the character as
written, as bash's `nocaseglob` does), `PERIOD` (a leading `.` is matched only by a
literal `.` — never by `*`, `?`, a bracket expression or `!(…)`), and
`PATHNAME` (the pattern split at every `/` outside a group, the candidate at
every `/`, and the two matched component by component). The first three are
glibc's `FNM_EXTMATCH`, `FNM_CASEFOLD` and `FNM_PERIOD`; `PATHNAME` reads a
`/` as POSIX's pathname expansion (XCU 2.13.3) and bash's filename generation
do, not as `FNM_PATHNAME`: a `/` inside a group belongs to its alternative,
which then matches no single component, and a bracket expression never spans
a `/`, its `[` standing for itself even inside a group, where glibc reads
`[a/b]` as a bracket that cannot match `/`. `ls` matches with `PERIOD`, as
GNU's `fnmatch` call does.

## Compiling and matching

`Pattern::with_options` compiles pattern text, `\` escaping the next
character; `Pattern::from_marked` compiles characters the caller has already
marked literal — the shell's quoting — so a word reaches the matcher without
an escaping round trip. Compiling refuses a reversed range, an unknown class
or collating element, an unclosed group or a dangling escape as a typed
`GlobError`, never matched as something else. Matching grows its working
memory fallibly: a heap that refuses it is `OutOfMemory`, an unknown answer
the caller fails closed on, never an abort.

`Pattern::components()` exposes each pathname component's `literal()` text
(when it holds no wildcard) and whether it is spelled `**`, which is what a
tree walker needs; `may_hold_wildcards` is the cheap test for whether text
can be a pattern at all. A `Scratch` lets a caller testing many candidates
against one pattern allocate once, and `Pattern::heap_bytes` reports what a
compiled pattern holds, so a caller keeping patterns can charge them.

## Bounded matching

A pattern is untrusted input, so `MAX_PATTERN_LEN`, `MAX_TOKENS`,
`MAX_CLASS_ITEMS`, `MAX_GROUPS` and `MAX_GROUP_DEPTH` are fixed security
bounds. Compiling costs time linear in the pattern's length: a component's
bracket expressions share one table of where each `[:…:]`, `[=…=]` and
`[.….]` element can close, and of the positions a scan is known to read on
from to the end unclosed, so a run of unclosed `[` never re-reads what
follows it. Matching never backtracks: the engine tracks, as bit sets, the
positions of the candidate each node can reach from those the previous one
reached. A pattern without groups costs time linear in its nodes times the
candidate's length: a bracket expression keeps its ASCII members as a bit set
and the rest as sorted ranges that never overlap, so it tests a character by
one bit lookup, or a binary search over at most `MAX_CLASS_ITEMS` ranges, and
a check per class it holds. Each group's reach from a start position, and what its
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
compiles and matches hostile text under every option combination in turn,
and under a heap refusing the match's working memory.
