# PAINT — the desktop image editor (`Paint.app`)

Binding under `AGENTS.md`. What the image editor is, what it reads and writes,
the RISC OS sprite semantics it keeps, and the seams that keep decoding out of
its address space and everything that grows with the picture off its loop.

## Ledger

| Id | Item | Status |
|---|---|---|
| PT1 | `lib/image` encoders: PNG (palette kept), baseline JPEG, RISC OS sprite areas at every depth, over one `PictureSource` | done |
| PT2 | The Wimp desktop palette for a sprite with no palette of its own, in `lib/image` and so in the viewer as in the editor | done |
| PT3 | `lib/sandbox::imageedit`: the sandboxed edit decode — every entry at its own depth, a sprite it cannot read kept byte for byte | done |
| PT4 | The engine: the tiled canvas, the history, strokes and shapes, fill, selection, transforms and quantisation, the viewport, the view | done |
| PT5 | The shared document host (`tairix_window::docapp`) both editors run in, with the shared title, save question, rectangle and surface helpers | done |
| PT6 | The `Run` binary: the decode worker and the painter's own work over the host | done |
| PT7 | The bundle: manifest, icon, Help in every required locale, docs, registration | done |
| PT8 | Zoom by the wheel and by pinch, anchored on the pointer, continuous between the ladder's rungs (`plans/POINTING.md` PO4, PO7) | done |
| PT9 | Colour: one colour-model home (HSV, HSL, hex notation) every copy in the tree moves onto; a `lib/controls` colour picker; Paint's colour dock; Terminal's scheme editor moved onto the picker | done |
| PT10 | The window: a vertical tool box, the tool-controls bar of the tool in use with its values in `NumberField`s, the palette strip and the status band, around the colour dock | planned |
| PT11 | File types: TIFF, BMP and GIF encoders and lossless reading of what they hold; New chooses a format; the Save As sheet holds the format and that format's options alone | planned |
| PT12 | The engine: `lib/raster` row coverage, exact and centre-sampled (D476); soft selection masks with rectangle, ellipse, lasso, polygon and magic-wand selection, combining modes and feathering; the dab brush engine (size, hardness, opacity, flow, spacing, antialias) with an antialiased airbrush; fills, gradients, clone, text, shapes, crop, hand and zoom | planned |
| PT13 | Adjustments and filters, previewed live | planned |
| PT14 | Layers, and OpenRaster as the format that keeps them | planned |

## What it is

`Paint.app` (`name = "Paint"`, `title = "Paint"`, `id = os.tairix.paint`,
`kind = "application"`, `library = "Graphics"`): pixel editing and painting in
truecolour or a palette. It opens every raster format TAIRiX decodes — SVG
has no pixels to edit — and writes PNG, JPEG and sprite areas; a picture read
from any other format saves as a new file. It claims `image/png`, `image/jpeg`
and `image/x-riscos-sprite` — the formats it writes back — beside the viewer's
claims; which of the two a double-click opens follows the store's order.

## Sprites

A sprite keeps its name, mode, palette and mask. Depths are edited as stored:
1, 2, 4 and 8-bit indexed and 32-bit colour. The mode's eigen factors give the
pixel's aspect, so a mode 12 sprite shows its tall pixels.

- **No palette of its own** shows the desktop's colours, never a PC palette:
  sixteen colours are Wimp colours 0–15, two are Wimp 0 and 7, four are 0, 2, 4
  and 7, and 256 are the RISC OS tint arrangement
  (`tairix_image::desktop_palette`). It is written back without a palette
  while its colours are still those (`save::restated`), and with its own full
  palette once they are not.
- **Kept**: a sprite the editor cannot read — a CMYK, JPEG or YCbCr sprite, a
  Teletext or extension mode, one larger than an editor opens, or one whose
  bytes are damaged (`KeptReason`) — is carried byte for byte and written back
  unchanged; it cannot be painted on. One whose length is not a whole number
  of words is refused on save rather than written, since every sprite after it
  would start off a word boundary.
- **Masks**: a translucent PNG palette written as a sprite moves its
  transparency into the mask, and the mode is moved to an alpha mask when a
  picture needs one.

## Writing back

A file is written back only in a format Paint writes and only where the read
kept everything the file held: a 16-bit PNG read as 8 bits, or a PNG or JPEG
holding anything beside its picture (`tairix_image::Unkept`), opens read-only.
A document of several entries is refused as a PNG or JPEG before the file is
touched. A save chooses its format on the queue's worker from the frozen
document and the file's name, so a chained save is written as what it was
when it was asked for; the loop checks the same decision first to refuse
without touching the file.

## The engine

- **Canvas**: 64-pixel tiles behind `Arc`, copied on write and allocated
  fallibly; a blank canvas shares one tile.
- **History**: each step swaps what left the document, so a step is its own
  inverse and holds only what the document no longer does. Room is reserved
  before a change, so a change is always recordable; a refusal sheds redo and
  then the oldest steps before it refuses the change. Memory pressure bounds
  it — everything at normal pressure, twice the document's size at mild, once
  at moderate, nothing at severe — and the band last told holds as steps are
  recorded, each step charged again for what it alone holds before a trim.
- **Strokes and shapes**: coverage is composited from the state before the
  stroke, so a stroke never darkens where it crosses itself; shapes are
  integer fixed-point with antialiased edges.
- **Fill, transforms, quantisation**: a flood fill finds its region and fills
  it on the worker; scale (nearest or smooth), canvas size, crop, turns,
  flips, invert, mask and depth conversion — to the desktop palette or an
  optimised median-cut one, with Floyd–Steinberg diffusion — run there too,
  the picture taking no edits until they are back.
- **Viewport**: a continuous zoom from 1/16 to 64 in 4096ths of actual size
  (`viewport::Zoom`), every rung of the ladder exactly, anchored on the
  pointer and centred when the picture is smaller than the window; Ctrl and
  the wheel step it a rung per detent's worth of turn through the shared
  `wheel_steps` carry, from between rungs to the nearer one in the turn's
  direction; a pinch zooms by the spread and pans with the fingers' centre,
  both from where it began; a paint reads only the rows in view,
  maps its columns once, and, zoomed out, reads only the pixels it samples.
- **Gestures**: a drag belongs to the button that began it; anything that
  takes the pointer or changes the picture under it finishes the drag first,
  and Escape turns it down: a stroke taken back off the picture rather than
  left unrecorded, a marquee marking nothing, a dragged layer back where the
  drag began.
- **Selection**: a lift is a `Floating` view over the picture's tiles and the
  lifted rectangle, writing nothing; putting it down or clearing it runs on the
  queue (`Compute::PutDown`, `Compute::Clear`) and lands as one step, then what
  waited on it carries on (`Then`: rest, the action, a paste floated, a save
  before closing).
- **Names**: one search finds a sprite name free (`document::free_name`): the
  name itself, else the least number free after it, read in one pass over the
  names held.

## Colour

- **One home**: `lib/colour` is the tree's sRGB colour — `Rgb` and `Rgba`, the
  transfer, `Hsv` and `Hsl` in fixed point exact enough that every 8-bit colour
  comes back through each, and hex notation (`parse_hex`, `Hex`). The theme,
  `lib/raster`, an SVG asset's `#rgb` and `hsl()`, the wallpaper and terminal
  settings and Paint convert and spell colours through it alone. The copies
  this item did not absorb are `plans/OPEN-DEFECTS.md` D568–D576.
- **The picker**: `lib/controls::ColourPicker` — a saturation and value plane,
  hue and alpha strips, the earlier colour, a hex field and seven
  `NumberField`s. It answers `Edited` per sample and `Settled` once an
  interaction ends, so what is durable is done at the settle; a field claims
  every key but Tab and the owner's chords, so typing reaches no shortcut.
- **The dock** on the window's right edge holds the two wells and the picker,
  editing the well chosen by a press or by *Edit primary/secondary colour*. A
  colour picture's ink takes the colour live and records nothing; a colour
  with no alpha is the clear ink (`Ink::of_colour`). A palette picture's ink
  names an entry: the session reserves the history's room and copies the
  palette once, each sample sets the entry in place with no allocation, and
  the settle lands the whole interaction as one step — none where the palette
  came back as it was. Anything else that takes input settles the dock first;
  a drag whose ink becomes another under it — a conversion landing — ends
  there. The picker is withheld from a palette edit while a worker has the
  picture and from the clear ink, which is the mask rather than a colour; a
  sprite's entries offer no alpha.
- **The eyedropper** is the tool that takes a colour from the picture, so
  "picker" names one thing.

## The loop

Paint runs in `tairix_window::docapp`'s host, which owns its windows, saves,
choosers and the queue saves are written on. Its own: a decode worker holding
the sandbox, released after each document so each document and each paste is
read by a fresh worker; and, on the queue beside saves, clipboard encodes,
fills, transforms, and a floating layer's putting down or clearing. A window's
own work is held to its share of the queue (`JOBS_PER_WINDOW`), and a pass of
the loop adopts only what had landed when it began.

## Invariants

- No filesystem capability; nothing of a document or a pasted picture is
  parsed in the painter's address space.
- While a worker has the picture nothing changes the document or the entry
  showing, and its answer is written only over the entry and generation it was
  asked of.
- Save As offers only names Paint can write: the picker holds the name to the
  document's save endings before any file is made.
- A paint reads nothing and costs what the window shows, not what the picture
  holds.
- Every allocation that grows with the picture is fallible; a refusal is
  stated in the window, never an abort.
- A kept sprite is written back exactly as it was read.
- A colour edited in the dock writes nothing until it settles, and lands as
  one step.
- Closing a window or quitting with unsaved changes asks first.
