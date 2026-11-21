# tairix-vdu

The Acorn VDU vocabulary the BBC Micro's MOS and RISC OS share:

- **Control codes** (`code`): every code 0–31 and 127, with the parameter bytes
  each gathers, which both define alike; and what VDU 23 does by its first
  parameter.
- **Plot codes** (`plot`): VDU 25's shapes and the bits that say how a point
  is given and which colour plots.
- **Numbered modes** (`mode`): RISC OS 5's modes 0–53, each with the mode
  variables `OS_ReadModeVariable` reads for it; the variables' numbers, the
  `ModeFlags` bits and RISC OS 5's default mode.
- **Palettes** (`palette`): the two- and four-colour modes' default colours,
  and the 256-colour modes' default palette and the arrangement in which a
  pixel's top four bits override the entry its low four select, as RISC OS 5
  programs it.
- **The system character set** (`font`): an 8×8 glyph for every character from
  32 to 255 in RISC OS's arrangement, the BBC's arrangement of 32 to 127, and
  every other character RISC OS's alphabets hold — Latin's other letters,
  Greek, Cyrillic and Hebrew — drawn for TAIRiX.

The mode variables are RISC OS 5's own constants. The glyphs are first-party
designs: OpenMicro's for 32 to 126, and new ones in the same style for the rest.
No RISC OS font is used.

OpenMicro's MOS builds its character set, its control-code table, its plot
codes and its default palettes from here; TAIRiX Bridge's International draws
each alphabet from it, its BASIC takes its plot codes and its VDU drivers take
their modes, codes, palettes and system font from it; and `lib/image` reads a
sprite's numbered mode and its 256-colour palettes through it.

`no_std`, with no `unsafe`.

## Stability

Tier: `experimental`.
