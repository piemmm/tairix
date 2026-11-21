# tairix-riscos-vdu

TAIRiX Bridge's VDU drivers (`plans/RISCOS-EMULATOR.md` §8, RO21): RISC OS 5's
screen modes and screen memory, the VDU stream, `OS_Plot`, colours and
palettes, Teletext, the pointer, and the ScreenModes module, over the guest
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
  asked, the cursor and the pointer to draw over them.

Every rule is held to genuine RISC OS 5.30 by the screen corpus in
`tests/riscos5/`, which `cargo xtask riscos --reference` records, but one:
a segment can differ from it by a pixel or a few at its chord's end, a known
difference (`plans/RISCOS-EMULATOR.md` RO21.2).

`no_std`, with no `unsafe`.

## Stability

Tier: `experimental`.
