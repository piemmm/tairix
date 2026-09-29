# `tairix-syntax` — colouring and checking a document

`lib/syntax` is what a text editor needs to colour and check a document
without parsing it itself (`plans/TEXTEDIT.md`). Its input is untrusted, so
every consumer runs it inside the parser sandbox
([`tairix-sandbox`](./sandbox.md)'s `textsyntax` service); the editor holds
answers, never a parser.

## Formats

`Format` is the closed set: plain text, HTML, XML (SVG included), CSS,
JavaScript, JSON, YAML, TOML, Markdown, Rust, C, Java, Python, shell, and the
TAIRiX settings stores — application settings, the program library, the
system and network configuration, the service overrides, the users and
groups databases, and font family manifests. `Format::ALL` and `index` are
the one wire order — `Format::COUNT` is the compiler's count of the variants
and a const assertion holds `ALL` to declaration order, so neither can drift
from the enum — `label` what a chooser shows, `is_store` whether a format is
validated, and `line_comment` the marker an editor's toggle inserts.

## Lexing

`lex_line(format, state, line, spans) -> LineState` reads one line — its
first `MAX_LEX_LINE` bytes — appends non-overlapping `Span`s classified by
`tairix_theme::SyntaxRole`, and answers the state the next line starts in.
It is total: any bytes and any state word yield well-formed spans and never
panic, which the fuzz harness `tests/fuzz_syntax.rs` holds for every format.
A line longer than the bound is coloured up to it and plain past it. An
editor keeps line states at sparse checkpoints, so an edit re-lexes from the
edited line forward and only as far as the view needs.

Every lexer costs time linear in the line: a scan either consumes what it
reads or is remembered, so an opener that finds no closer — a `[`, a
backtick run, a `<`, an unclosed `\u{` escape — never makes the next one
rescan the line. A Markdown code span closes only at a backtick run of its
own length, and a run longer than `MAX_CODE_TICKS` (32) is text, which keeps
that memory a fixed size.

The settings stores are coloured through their own crates' line grammar —
`tairix_util::conf::setting_line` and `comment_at`, `lib/appconf`'s
`line_shape`, `lib/users`' `UsersDb::line` and `GroupsDb::line` — so a
store's colours can never disagree with what its parser believes; a key with
no value is coloured as the error its parser reports.

## Detection

- `store_for_name` names the stores whose file name is fixed, each from the
  store's own path constant, so a store that moves cannot leave detection
  behind. An editor knows a document by its name only, so a file that merely
  shares a store's name reads as that store until the user picks otherwise.
- `format_for_head` reads a document's opening bytes: a users or groups
  database header, an interpreter line, a markup prolog or doctype.

An editor asks the name first (these and `lib/browse`'s extension registry),
then the head, and a format the user chose always wins.

## Validation

`validate(format, text) -> Vec<Diagnostic>` runs the **real** parser of a
store — `lib/sysconfig`, `lib/netconfig`, `lib/enrolment`, `lib/users`,
`lib/fontface`, `lib/appconf`, `lib/proglib` — and reports what the system
would make of the document: the first refusal of a strict store, with the
line its parser named, and every line a tolerant store ignores, as a
`Warning` beside that line. No grammar is restated here: each store exposes
its own line shape and located refusal. `MAX_DIAGNOSTICS` is the most it can
answer — one per line of the longest application settings document — and
`MAX_STORE_LEN` the longest document any store reads, each derived from the
stores' own bounds.

The crate is `no_std` + `alloc` and `forbid(unsafe_code)`. Stability tier:
experimental (`lib/syntax/README.md`).
