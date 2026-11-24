# tairix-riscos-font

TAIRiX Bridge's Font Manager and ROM fonts (`plans/RISCOS-EMULATOR.md` §8,
RO25): RISC OS 5.30's Font Manager 3.80 over the guest memory and the screen
the kernel and the VDU drivers give it, and `ROMFonts`, the fonts and
encodings ResourceFS holds.

- **Fonts.** RISC OS's own families — Trinity, Homerton and Corpus — are
  TAIRiX faces asked of the font service, by the kind `lib/drawing` maps each
  to. The desktop font, `Desktop`, is the TAIRiX desktop's interface face in
  the family the session is given; the Font Manager names it in the Wimp's
  `Wimp$Font` variables. A guest's Acorn fonts on `Font$Path` — metrics,
  outlines, bitmaps and encodings — are read as hostile and refused whole on
  any fault, and the catalogue of them is bounded in the path's elements, the
  entries read, a tree's depth and an identifier's length.
- **Calls.** Every `Font_` SWI and `*` command: handles, identifiers and the
  catalogue, measuring, painting anti-aliased through the colour tables,
  `MakeBitmap`, the cache, and `Font_SwitchOutputToBuffer`, which writes
  text as Draw path objects.
- **The cache** is the session's, bounded by its memory policy; the area the
  guest sees is its size.

Every rule is held to genuine RISC OS 5.30 by the screen corpus in
`tests/riscos5/`, painted from the first-party fonts `build` makes, and by the
cases recorded in the crate's own tests, but the known differences
`docs/src/desktop/tairix-bridge.md` states.

`no_std`, with no `unsafe`.

## Stability

Tier: `experimental`.
