# tairix-riscos-basic

TAIRiX Bridge's BBC BASIC V and VI: RISC OS 5's BASIC and BASIC64 modules.
The design and its staging are `plans/RISCOS-EMULATOR.md` §7; the
description is `docs/src/desktop/tairix-bridge.md`.

Its notices are [the bridge's](../../README.md).

Stability: `experimental`.

- `five` — BASIC V's five-byte real: its arithmetic and conversions to
  BASIC V's bits, its functions and every bound at which one refuses.
- `six` — BASIC VI's double, every operation the FPA's under the program's
  own floating-point status; numbers read through a packed decimal load and
  written from a packed decimal store.
- `text` — numbers as text: the decimal syntax `VAL` and a program's literals
  share, the layouts `@%` gives `PRINT` and `STR$`, and `@%` given a string.
- The interpreter — a resumable machine whose program, variables, heap and
  stacks live in guest memory, so it never recurses on the host and can stop
  at any statement: the language, the machine interface, the prompt and its
  commands, and the assembler, which encodes through `tairix-riscos-arm`'s
  tables.
- `corpus` — the recorded corpora's formats, shared by the replays and the
  reference harness that records them.

BASIC V's transcendentals, and its reading of a number at a tie, are
rounded correctly where BASIC V's own are a place or two off; the rest of
both dialects is held to genuine RISC OS. BASIC's messages are the BBC BASIC
Reference Manual's text where it gives one and TAIRiX Bridge's own words
otherwise, as are `HELP`'s topics.

It holds no `unsafe` code. Its corpora, recorded by
`cargo xtask riscos --reference` from genuine BASIC V and BASIC VI, are in
`tests/riscos5/`: the numbers' answers (`numbers.txt`, replayed by
`tests/riscos5.rs`), the programs and what they printed (`programs.txt`,
`printed.txt`, `tests/riscos5_language.rs`), the assembler's lines and what
each laid down (`assembler.txt`, `assembled.txt`,
`tests/riscos5_assembler.rs`), and what is typed at the prompt
(`sessions.txt`, `typed.txt`, `tests/riscos5_sessions.rs`).
`tests/hostile.rs` holds programs that overwrite BASIC's own workspace and
stack. `tests/fuzz_numbers.rs` and `tests/fuzz_basic.rs` are its fuzz
harnesses.
