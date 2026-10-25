# tairix-glob

Shared filename-glob matcher for TAIRiX (`lib/glob`).

The one definition of shell wildcard patterns: the shell's filename
generation, `ls --ignore`/`--hide` and the tree file manager's filters and
searches compile their patterns here rather than each embedding a matcher.

## Glob, not regex

This crate implements shell **pattern matching notation** (POSIX XCU 2.13
and bash's `extglob`), deliberately not a regular-expression engine.

## API

- `Pattern::new(pattern)` / `Pattern::with_options(pattern, Options)` —
  compile pattern text, `\` escaping the next character; a malformed
  pattern is a `GlobError`.
- `Pattern::from_marked(&[Mark], Options)` — compile text whose literal
  characters the caller has marked (the shell's quoting), with no escaping
  round trip.
- `Pattern::matches(candidate)` / `matches_with(candidate, &mut Scratch)` —
  whether the whole candidate matches, or `OutOfMemory` when the heap
  refuses the working memory the match grows; `Scratch` lets a caller
  testing many candidates allocate once.
- `Pattern::components()` — one `Component` per piece between the `/`s
  outside groups in pathname mode, each reporting `literal()` (the exact
  name when it holds no wildcard) and `is_double_star()` (spelled exactly
  `**`) for a caller walking a tree.
- `Pattern::heap_bytes()` — the heap a compiled pattern holds, for a caller
  bounding a cache of them.
- `Options` — flags: `EXTENDED` (extglob operators), `CASE_INSENSITIVE` and
  `PERIOD` (a leading `.` must be matched by a `.`) — glibc's `FNM_EXTMATCH`,
  `FNM_CASEFOLD` and `FNM_PERIOD` — and `PATHNAME`, which matches component
  by component as POSIX's pathname expansion and bash's filename generation
  do: the pattern splits at each `/` outside a group, a `/` inside one
  belonging to its alternative, and no bracket expression spans a `/`.
- `may_hold_wildcards(&[Mark], extended)` — whether text holds a character
  that can begin a wildcard, the cheap test before compiling.
- `GlobError` — why a pattern was rejected; `OutOfMemory` — a match the
  heap could not hold, whose answer is unknown.

## Syntax

- `*` any run of characters, `?` exactly one.
- `[...]` one character: members, code-point ranges, `!`/`^` negation, the
  POSIX classes (`[:alpha:]` …) plus bash's `[:word:]` and `[:ascii:]`,
  equivalence classes `[=c=]`, collating symbols `[.c.]` and POSIX character
  names with bash's aliases (`[.space.]`, `[.minus.]`). `\` escapes inside
  an expression too. A `[` that opens no complete expression matches
  itself, as POSIX specifies.
- With `EXTENDED`: `?(…)`, `*(…)`, `+(…)`, `@(…)` and `!(…)` over
  `|`-separated alternatives.
- Case-insensitive matching folds literals and ranges, a range's order
  checked once folded; classes test the character as written, as bash's
  `nocaseglob` does.

## Bounds and fail-closed behaviour

A pattern is untrusted input: `MAX_PATTERN_LEN`, `MAX_TOKENS`,
`MAX_CLASS_ITEMS`, `MAX_GROUPS` and `MAX_GROUP_DEPTH` are fixed security
bounds, and a malformed pattern (a reversed range, an unknown class or
collating element, an unclosed group, a dangling escape) is a typed
`GlobError`, never matched as something else.

Matching never backtracks: it tracks the set of positions each step can
reach. A pattern without groups costs time linear in tokens times length: a
bracket expression tests a character by one bit lookup, or a binary search
over its ranges, and a check per class it holds, whatever its size. A
group's reach from each start is memoised, bounding an extended pattern by
the cube of the candidate's length, so a component holding a group matches
candidates of at most `MAX_EXTENDED_LEN` characters — the longest name a
filesystem stores.

`no_std` + `alloc`, `#![forbid(unsafe_code)]`.

## Stability

Tier: `experimental`.
