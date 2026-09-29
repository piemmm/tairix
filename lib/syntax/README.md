# tairix-syntax

Stability tier: **experimental**.

What a text editor needs to colour and check a document without parsing it
itself: the closed `Format` set, detection from a document's name and its
head, one lexer per language family, and validation of the TAIRiX settings
stores.

- **Lexers** are total and bounded: `lex_line(format, state, line, spans)`
  reads one line of bytes (the first `MAX_LEX_LINE` of them), appends
  non-overlapping `Span`s classified by `tairix_theme::SyntaxRole`, and
  returns the state the next line starts in. Any byte sequence, any state
  word, never panics.
- **The settings stores** are coloured and validated through their own
  crates (`lib/appconf`, `lib/proglib`, `lib/sysconfig`, `lib/netconfig`,
  `lib/enrolment`, `lib/users`, `lib/fontface`), so no grammar is restated
  here. Validation
  reports what the system would do with the document: the first refusal of a
  strict store with its line, and every line a tolerant one ignores.
- Every consumer runs this crate inside the parser sandbox
  (`lib/sandbox::textsyntax`): its input is an untrusted document.

The fuzz harness `tests/fuzz_syntax.rs` holds the totality and span
invariants for every format.
