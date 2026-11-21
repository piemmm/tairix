# `tairix-teletext`

The one Teletext character generator: OpenMicro's SAA5050 and TAIRiX Bridge's
mode 7 both draw from it (`plans/RISCOS-EMULATOR.md` RO21).

## Glyphs

`GLYPHS` holds every cell as twenty lines of twelve half-dots, the leftmost in
bit 11: the 96 alphanumerics from `&20`, then the 64 contiguous mosaics and the
64 separated ones. `alphanumeric` and `mosaic` give a code's cell. A glyph's
dot is two half-dots across and two lines down; where two dots touch only
diagonally, character rounding fills the half-dots either side of the join on
the lines between. A mosaic is six blocks, two across and three down, banded
three, four and three dot rows; a separated block loses its left column and
its bottom row.

## Attributes

`Attributes::LINE_START` is a row's state at its start, white alphanumerics on
black. `Attributes::character` takes the row's next byte and answers what it
shows — a cell, or none for a control code with nothing held — the attributes
it is shown with, and whether it is the double-height code; the attributes then
move on. `is_double_height` tells that of a byte alone, so a row can be found to
be a double-height pair's upper half without being decoded.

A row after one that held the double-height code is that pair's lower half,
unless it was a lower half itself (`lower_after`). In a lower half only
double-height characters show (`Attributes::shows_in`), each its glyph's
bottom half (`double_first_line`).

The codes that take effect at their own cell (steady, normal height,
conceal, contiguous, separated, black and new background, hold) act before the
cell is shown; the colour, flash, double-height and release codes act after.
