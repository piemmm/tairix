# tairix-bbcbasic

BBC BASIC's tokens, and the tokenising and listing of its programs, in its two
dialects: BASIC II, the BBC Micro's and the Electron's, and BASIC V and VI,
RISC OS's.

- **Keywords** (`keyword`, `basic2`, `basic5`): the 113 keywords both dialects
  spell and number alike, defined once; each dialect's table in the order its
  tokeniser tries them, with its flags; BASIC V's two-byte tokens behind the
  prefixes `&C6`, `&C7` and `&C8`; and each dialect's spelling of every token.
- **Tokenising** (`tokenise`): a typed line, a statement or an expression made
  tokens as each dialect's tokeniser makes them, with BASIC V's warnings.
- **Listing** (`list`): a line's tokens as `LIST` shows them under `LISTO`.
- **Line references** (`lineref`): a line number as a program's text holds one.
- **Program images** (`program`): the lines an image holds, an image of lines,
  and a typed line's text as each dialect stores it.

BASIC II's rules are its ROM's, which OpenMicro's clean-room BASIC keeps; its
ROM builder emits its keyword table and its token values from here. BASIC V's
are RISC OS 5's. Every allocation is fallible, and a listed line is bounded by
its own length, as each dialect bounds its indentation.

`no_std`, with no `unsafe`.

## Stability

Tier: `experimental`.
