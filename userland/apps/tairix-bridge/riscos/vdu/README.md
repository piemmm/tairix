# tairix-riscos-vdu

TAIRiX Bridge's VDU drivers and sprites (`plans/RISCOS-EMULATOR.md` §8, RO21
and RO22): RISC OS 5's screen modes and screen memory, the VDU stream,
`OS_Plot`, colours and palettes, Teletext, the pointer, and the ScreenModes
module; ColourTrans, InverseTable and BlendTable; `OS_SpriteOp`, SpriteExtend
with its JPEG SWIs, and SpriteUtils; and the Hourglass (RO61); over the guest
memory the kernel gives them.

- **Modes**: the numbered modes from `lib/vdu`, selectors and mode strings,
  what the current monitor type shows and what it trades a mode it lacks for,
  and the services around a change.
- **Screen memory**: dynamic area 2, mapped twice, its banks, hardware
  scrolling, `OS_Word` 22, and the area's handler.
- **The stream**: every control code, VDU 23 and its operations, windows,
  cursors, VDU 4 and VDU 5 text, paged mode, the printer and the bell.
- **Graphics**: every `OS_Plot` code, the colour blocks zero page holds, ECF
  patterns, `HLineAddr`, `OS_ChangedBox`.
- **Palettes**: every depth's, behind `PaletteV`.
- **The display**: what a front end shows, the lines written since it last
  asked, the cursor and the pointer to draw over them, and whether the
  hourglass shows.
- **The hourglass**: each context's own, held with its screen, so it goes
  with the program that turned it on; its SWIs and frames as RISC OS 5's,
  drawn for TAIRiX.
- **Sprites**: system and user areas through `lib/image`'s structures, every
  `OS_SpriteOp` reason, output to a sprite or its mask, the kernel's and
  SpriteExtend's plots with translation tables, alpha and colour mapping,
  pointer shapes, screen dumps, and JPEGs through `lib/image`'s decoder.

Every rule is held to genuine RISC OS 5.30 by the screen corpus in
`tests/riscos5/`, which `cargo xtask riscos --reference` records, but the
known differences `plans/RISCOS-EMULATOR.md` §8 states: a segment's chord end
(RO21.2), exact ties in transformed plots that step unevenly, blends and
JPEGs on a palettised screen, and JPEG pixels (RO22).

`no_std`, with no `unsafe`.

## Stability

Tier: `experimental`.
