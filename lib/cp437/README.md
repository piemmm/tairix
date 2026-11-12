# tairix-cp437

IBM code page 437, the IBM PC's character set, and Unicode. A byte reads two
ways: as **text**, where the bytes below `80h` are ASCII and its controls, as
zip spells a member's name; and as the **glyph** a PC's screen draws, where
`01h`–`1Fh` and `7Fh` are the faces, arrows and card suits the character
generator holds. The upper half is the same either way.

- `char_of(byte)` — the character `byte` spells as text.
- `glyph_of(byte)` — the character `byte` draws on the screen; `00h` draws
  nothing and reads as a space.
- `byte_of(ch)` — the byte that spells `ch` as text, if one does.
- `glyph_byte_of(ch)` — the byte that draws `ch`, if one does; none for a
  control.

The upper half is Unicode's published mapping
(`https://www.unicode.org/Public/MAPPINGS/VENDORS/MICSFT/PC/CP437.TXT`); the
control range's glyphs are those IBM's character set draws. The reverse table
is sorted at compile time from the forward one, so neither direction is
transcribed twice, and is searched in logarithmic time. `no_std`,
allocation-free, with no `unsafe`.

## Stability

Tier: `stable`. The code page is fixed.
