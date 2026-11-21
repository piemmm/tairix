# tairix-teletext

The Teletext character generator the BBC Micro's SAA5050 is and RISC OS draws
mode 7 with:

- **Glyphs** (`GLYPHS`): the 96 alphanumerics and the 64 contiguous and 64
  separated mosaics, each a 6 × 10 dot cell rounded to 12 × 20 half-dots, the
  diagonals between alternate interlaced lines filled in.
- **Attributes** (`Attributes`): what a row's control codes set as it is
  drawn — colours, mosaics, separation, flashing, double height, concealment
  and held mosaics — each code taking effect on its own cell or the next, as
  the chip has it.
- **Double height**: which rows are a pair's lower half, and what shows in one.

The glyph shapes are first-party designs in the proportions of the chip's
English character set. OpenMicro's SAA5050 draws from it a line at a time
across its pixel slots, and TAIRiX Bridge's VDU drivers draw mode 7's cells
from it a pixel per half-dot.

`no_std`, with no `unsafe`.

## Stability

Tier: `experimental`.
