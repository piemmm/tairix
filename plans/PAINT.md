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
| PT10 | The window: a vertical tool box, the tool-controls bar of the tool in use with its values in `NumberField`s, the palette strip and the status band, around the colour dock | done |
| PT11 | File types: TIFF, BMP and GIF encoders and lossless reading of what they hold; New chooses a format; the Save As sheet holds the format and that format's options alone | done |
| PT12 | The engine: `lib/raster` row coverage, exact and centre-sampled (D476); soft selection masks with rectangle, ellipse, lasso, polygon and magic-wand selection, combining modes and feathering; the dab brush engine (size, hardness, opacity, flow, spacing, antialias) with an antialiased airbrush; fills, gradients, clone, text, shapes, crop, hand and zoom | done |
| PT13 | Adjustments and filters, previewed live | done |
| PT14 | Layers, and OpenRaster as the format that keeps them | done |

## What it is

`Paint.app` (`name = "Paint"`, `title = "Paint"`, `id = os.tairix.paint`,
`kind = "application"`, `library = "Graphics"`): pixel editing and painting in
truecolour or a palette, in layers. It opens every raster format TAIRiX
decodes — SVG has no pixels to edit — and writes PNG, JPEG, GIF, BMP, TIFF,
sprite areas and OpenRaster; a picture read from any other format saves as a
new file. It claims `image/png`, `image/jpeg`, `image/gif`, `image/bmp`,
`image/tiff`, `image/x-riscos-sprite` and `image/openraster` — the formats it
writes back — beside the viewer's claims; which of the two a double-click opens
follows the store's order.

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

## Files

- **Reading** (`tairix_image::open_native`, through the sandbox's
  `imageedit`): a palette picture as its indices and palette — a PNG, a GIF's
  first frame with its transparent entry clear, an indexed BMP, a TIFF palette
  page — a TIFF as its pages (`EditKind::Pages`), a sprite area as its sprites,
  an OpenRaster file as its layers (`EditKind::Layers`, at most
  `MOST_ORA_LAYERS`), each placed where its stack puts it on the canvas
  (`load::Rows`, built `within` the layer's area so what it does not cover
  stays one shared tile), the topmost painted on; every picture with its
  `Density`, and how the file was written (`Written`) seeding the document's
  `SaveSettings`. A pasted file of layers is pasted as they show together.
- **Kinds of document**: a sprite area where it was read or made as one or
  holds a sprite's name; a TIFF's pages where it was read or made as a TIFF and
  holds none; several pictures of any other origin are a sprite area. The menu
  is Sprites or Pages to match; a page has no name.
- **New picture** names the format first (`Origin::New(SaveFormat)`), offering
  the depths it admits (`SaveFormat::admits`) and a clear background where it
  holds one.
- **Save As** puts up a sheet before the picker
  (`DocumentView::ask_how`, answered by `Request::SaveWhere`): the formats
  the document can be written as, the chosen one's settings alone, and what
  it cannot keep (`save::Loss`) in the sheet's message. What each format
  loses is found on the worker (`Compute::Survey`, `save::survey`), since a
  loss reads every pixel; `ask_how` hands the host that request as an outcome
  and the sheet goes up when it lands. The picker holds the name to that
  format's endings. A plain Save with nowhere to write opens the sheet too; a
  sheet put up for a save before closing holds a quit.
- **Layers in a file**: OpenRaster (`tairix_image::encode_ora`, over borrowed
  `OraLayerSource`s, so no layer is copied whole) keeps them, each written
  whole on the canvas, with their composite and a thumbnail no larger than 256
  on a side; every other format is written the layers laid together
  (`Picture::flattened`, sharing the one layer's tiles where it shows alone),
  stating `Loss::Layers`. A palette picture saved as OpenRaster states
  `Loss::Palette`, as its layers read back as colour.
- **Writing back**: only in a format Paint writes and only where the read kept
  everything the file held (`tairix_image::Unkept`: narrowed, held beside,
  restated); such a file opens read-only. A document of several entries is
  refused as a one-picture format and a kept sprite as anything but a sprite
  area, before the file is touched. A save chooses its format and finds its
  losses on the queue's worker from the frozen document and the file's name,
  before the file is touched, so a chained save is written as what it was
  when it was asked for and a refusal leaves the file whole; the loop checks
  the same decision first. A colour picture saved as a GIF is reduced to 256
  colours there, 255 where it needs a clear entry.

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
- **Layers** (`document::Layer`, `Picture`): one or more, the bottom first,
  alike in size and kind, at most `MOST_LAYERS`; a palette picture holds one,
  shown wholly. Every paint lays the visible layers together source-over
  (`compose::compose_run`, the one routine the paint, the status band, the
  eyedropper and a flatten share), the layer painted on carrying the strokes,
  shapes, text, gradient, preview and floating selection being drawn on it.
  A merge lays its layers together onto nothing, which keeps the look because
  source-over is associative. Adding, removing, moving and reshowing a layer
  are steps of their own (`Step::Layer*`) holding only what left the picture;
  a merge or flatten is a picture step. A tile step names its layer, and
  undoing one paints on that layer again (`Damage::Layers`).
- **Strokes and shapes**: coverage is composited from the state before the
  stroke (`stroke::Build`: the most of the opacity a stroke builds to, each
  dab adding its flow), so a stroke never darkens where it crosses itself;
  dabs (`brush::Tip`: size, hardness, opacity, flow, spacing) are laid along
  the path with the spacing carried between segments, the airbrush on a timer
  while held still; the clone tool reads from the stroke's starting picture
  at the Alt-clicked offset. Shapes are integer fixed-point with antialiased
  edges from `lib/raster` row coverage, rectangles with rounded corners.
- **Selections** (`mask::Mask`): a rectangle held as its bounds, or an alpha
  mask over them; rectangle, ellipse, lasso, polygon and wand recipes made and
  combined (replace, add, subtract, intersect) and feathered on the worker
  (`Compute::Select`); every stroke, fill, gradient, text and adjustment is
  held to the selection as much of each pixel as it chooses.
- **Fill, gradients, adjustments, transforms, quantisation**: a flood fill
  finds its region and fills it on the worker, as a gradient is laid; the
  adjustments and filters (`filter::Filter`, Invert among them) run there on
  the layer painted on, previewed one job at a time with the latest settings
  asked again once it lands, and applying adopts the preview's tiles where they
  are current; on a palette picture an adjustment maps the palette. Scale
  (nearest or smooth), canvas size, crop, turns, flips, mask and depth
  conversion — to the desktop palette or an optimised median-cut one, with
  Floyd–Steinberg diffusion — run there on every layer, a grown canvas filled
  beneath them and clear over the bottom; a picture of layers is flattened
  before it becomes a palette picture. The picture takes no edits until a job
  is back, and its answer lands on the layer it was asked of.
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
  left unrecorded, a marquee marking nothing, a dragged selection back where
  the drag began.
- **Floating selection**: a lift is a `Floating` view over the layer's tiles
  and the lifted mask, writing nothing; putting it down or clearing it runs on the
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

## The window

- **Bands** (`layout::Layout`): the tool-controls bar across the top, the view
  strip at its end (zoom out and in, fit, actual size, and the pixel grid,
  marked while it shows); the tool box down the left — the shared `Toolbar`
  laid out `ScrollOrientation::Vertical`, scrolling when the window is short;
  the colour dock down the right; the palette strip beneath the canvas and its
  bars; the status band along the bottom.
- **The bar** (`tool_controls::ToolControls`): the tool's name, then its
  `tool::Setting`s — numbers in `NumberField`s, choices in `ComboBox`es,
  switches in `Checkbox`es, smoothing held off on a palette picture —
  wrapping onto as many rows as it needs; the band is sized to the most rows
  any tool takes, so a tool change never moves the canvas. A value applies as
  it is typed and writes nothing; an open list owns the pointer and the
  keyboard until it closes. Its placement is resolved with the layout
  (`tool_controls::Placement`).
- **The menu**: one tree a secondary press opens, every command a row
  (`view_tests::the_window_menu_holds_every_command` pins that none is left
  out for want of room in the menu protocol's bounds); the Layers submenu
  adds, duplicates, deletes, steps between, goes to, raises, lowers, merges,
  flattens, shows and names layers (`Action::*Layer*`, `view_layers.rs`), the
  name and opacity through the layer form (`dialog::Form::layer`), which keeps
  the exact opacity until its slider moves.
- **The palette strip**: the whole palette in as few rows as hold every well
  at least 14 logical pixels across, a well up to 24; the grid's rows follow
  the layout (`SwatchGrid::set_columns`).
- **The floor**, declared once as a window opens and so held for any document:
  the view strip beside the widest setting, the bar's band at the most rows
  any tool wraps to there, and round a canvas the tool box showing a tool, the
  dock at its width, and the strip of a full palette and its clear ink
  (`view::MOST_WELLS`).
- **The keyboard**: Tab walks the picture, the bar's settings, the palette and
  the dock, and back to the picture; Shift+Tab the other way, passing a part
  with nothing that can act. Escape returns it to the picture. A field claims
  every key but Tab and its owner's chords (`tairix_controls::owner_chord`),
  and a chord settles the field before the window acts. A press elsewhere, or
  the menu, settles and releases the bar and the dock; the palette takes the
  keyboard only from Tab and gives it up to any press.
- **The pointer**: every part of the chrome sees every event, so a hover
  leaves and a press held on one part ends wherever it is let go.

## The loop

Paint runs in `tairix_window::docapp`'s host, which owns its windows, saves,
choosers and the queue saves are written on. Its own: a decode worker holding
the sandbox, released after each document so each document and each paste is
read by a fresh worker; and, on the queue beside saves, clipboard encodes,
fills, gradients, selections, adjustments and their previews, transforms of
every layer, merges and flattens, the Save As survey, and a floating
selection's putting down or clearing (`view::Compute`, each answer landing as
its `Lands` says). A window's own work is held to its share of the queue
(`JOBS_PER_WINDOW`), and a pass of the loop adopts only what had landed when it
began.

## Invariants

- No filesystem capability; nothing of a document or a pasted picture is
  parsed in the painter's address space.
- While a worker has the picture nothing changes the document, the entry
  showing or the layer painted on, and its answer is written only over the
  entry, layer and generation it was asked of.
- What a window shows is what a save writes: the layers are laid together by
  the one routine the paint and a flatten share.
- Save As offers only names Paint can write: the picker holds the name to the
  endings of the format the sheet chose before any file is made.
- A paint reads nothing and costs what the window shows, not what the picture
  holds.
- Every allocation that grows with the picture is fallible; a refusal is
  stated in the window, never an abort.
- A kept sprite is written back exactly as it was read.
- A colour edited in the dock writes nothing until it settles, and lands as
  one step.
- A tool's setting writes nothing: it is the window's own, applied as it
  changes.
- Closing a window or quitting with unsaved changes asks first.
