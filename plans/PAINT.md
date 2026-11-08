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
| PT13 | Adjustments and effects, previewed live | done |
| PT14 | Layers, and OpenRaster as the format that keeps them | done |
| PT15 | `lib/colour`: device CMYK, CIE XYZ, L\*a\*b\* and LCh(ab) under D65, and a correlated colour temperature with its tint | done |
| PT16 | The tool box in two columns: the shared `Toolbar` seats its tools in lanes across its breadth | done |
| PT17 | Panes: the left and right docks of mini-titled panes — Tools, Colour, Layers, Adjustments, Effects — each collapsed, closed, and moved within and between the docks by its header | done |
| PT18 | The Adjustments and Effects panes: one `op_pane::OpPane` each — a permanent selector of its family's operations, the chosen one's settings, Preview, Reset and Apply — one preview across both, leaving an operation applying it where it was moved; every worker answer lands only on the state it was asked of | done |
| PT19 | Histograms of the layer painted on, held to the selection, worked out on the worker for the panes that show them | done |
| PT20 | Levels: input and output levels for the composite and each channel over its histogram, black, grey and white point pickers, and Auto | done |
| PT21 | Curves: a tone curve for the composite and each channel, edited by its points over the histogram | done |
| PT22 | White balance: temperature and tint on tracks of the correction's colour, a neutral-point picker, and Auto | done |
| PT23 | Hue and saturation by colour range over the hue spectra; colour balance by tone range; threshold over the histogram | done |
| PT24 | The colour selector: the shared picker's square, wheel and slider views, its fields in RGB, HSV, HSL, CMYK, Lab, LCh or grey; the Colour pane's wells, recent colours and one-shot picture pick | done |
| PT25 | The document host: an application's own icon-bar rows and its app windows | done |
| PT26 | The settings window: Paint's own window from its icon bar's *Settings…* row, its settings by category, held in its private app-data store, applied to every window as it is made and written as it settles | done |
| PT27 | The grid: spacing, offset, colour, opacity and style; the pixel grid's threshold; snapping to the grid | done |
| PT28 | Tool windows: a pane dragged out of its dock floats in a tool window — the window manager's mini-titled transient (`plans/APPWIN.md` AW7, `plans/COMPOSITOR-WORK.md` stage M), the document host's tool windows of a document window — and docks again when dragged over a dock | done |
| PT29 | Help in every locale, the docs page and the README for all of it | done |
| PT30 | The document host's secondary press on a close button: the window closes and its file is shown in the file manager (`plans/TEXTEDIT.md` TE17) | done |
| PT31 | The document host's further windows onto one document: a member window shares its primary's file, is never asked to save, has its engine's changes handed to the application as they land, and closes with its primary | done |
| PT32 | The sprite file window: a sprite area opens as a window of its sprites, the first window it shows, each its thumbnail over its name, where sprites are made, renamed, copied, deleted and reordered | done |
| PT33 | Sprite windows: a sprite opens in a window of its own, sixteen at most a choice, across sprite files, its edits its file's as they land | done |
| PT34 | The palette bar: one row of wells packed a pixel apart, scrolled sideways only when they do not all fit; the primary and secondary inks' wells show their RISC OS colour numbers | done |
| PT35 | CMYK, JPEG-data and YCbCr sprites drawn as well as kept: today the decoder keeps them byte-exact and the sprite window shows the picture glyph; their pixel layouts are to be confirmed against real samples before they are read | planned |
| PT36 | The palette's tool window: a pop-out at the strip's right end and View ▸ Panes ▸ *Float palette* float the palette as a near-square grid of wells; resized by its edges between one column and one row of them, the wells keep their size, reflow to its width and scroll down where they overflow; it docks only in its strip — by its close mark, a refusal, or a move over the window's foot — and is stored as `palette:strip` | done |
| PT37 | One column: each pane's body states a least and a natural height; a dock seats every band, then each least, then grows the bodies the most recently opened first, and a body given less scrolls within itself, never the column | done |
| PT38 | The Layers pane: rows top layer first with an eye, a thumbnail drawn on a worker by its layer's revision, the name and the opacity; picking a row is no step, and the eye, a rename in place, a reorder by drag, an opacity changed live over the composite beneath, New, Duplicate, Delete and Merge down are one each; floating, it is resizable; the layer form is gone | done |
| PT39 | The operations engine: eleven adjustments and seven effects (`op::Op`) worked out a block of tiles at a time across a `lib/parallel` runner, bit-identical however cut; `lib/raster`'s one box blur over sixteen-bit premultiplied pixels | done |
| PT40 | The preview worker: previews, histograms and the Layers pane's thumbnails and composite off the ordered queue, a job running and one waiting per window, tickets abandoning stale work between blocks, the tiles in view first, overlays Apply adopts or completes | done |

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
- **The sprite file window** (PT32): a sprite area — read as one, made as one,
  or holding a sprite's name — opens as a window of its sprites, as RISC OS
  Paint shows one: each sprite's thumbnail, at its pixel shape, over its name,
  in a grid that reflows to the window's width and scrolls down. It is the
  first window the file shows: the document host keeps a window opened for a
  document off the screen until the document lands, so no picture window
  stands in for it unless the read outlasts the host's one-second wait
  (`plans/TEXTEDIT.md`). A thumbnail is
  drawn on the queue's worker from the sprite as it stands and kept until the
  sprite or the side it is drawn at changes, asked for each round the window
  lacks one; the paint draws what has landed at the side laid out, the
  picture glyph meanwhile. A kept sprite is the glyph with its name, opening
  nothing. Each cell is the shared `IconTile` (two name lines, the file
  manager's large tile), sized by `TileLayout::width_for`/`height_for` so the
  thumbnail side is the tile's own `icon_side`; a chosen sprite wears the
  tile's selection mark. Its
  open size (`DocumentView::open_size`, in the theme and its faces at the
  reference scale) is 250 logical pixels across and as tall as its sprites
  need at that width, at most 250, never below the window's minimum, fitted by
  the one function its layout is (`grid_fit`), so the two cannot disagree and
  its scroll bar comes out of the 250 only when the rows overflow; what its
  band says goes at the next act. New picture as a sprite file, and the
  untitled window when `new.format` is `sprite`, open the file window with its
  sprite in a sprite window. Selection is the file
  manager's: a press, Ctrl to add, Shift for a run, the arrows, Home and End.
  The window's menu makes, renames, copies, deletes and reorders sprites, and
  saves the file; those are the file window's history, undone there. Opening
  a sprite — a double-click, Enter, or *Edit* — gives it its sprite window.
- **Sprite windows** (PT33): a sprite window is the whole editor over one
  sprite, a member of its file window's document (PT31), titled with the
  sprite's name and the file's. One choice opens at most `SPRITES_AT_ONCE`
  (16), stopping at the first the desktop refuses and saying once what it
  left; opening a sprite already open raises its window. Each has its own
  history. Its edits become the file's as they land, so the file window's
  thumbnail follows and the file is modified; Save there saves the file. A
  floating selection is the window's own until it is put down, and a save made
  from another window writes the sprite without it. Closing a sprite window
  never asks; closing the file window asks for every sprite, its sprite
  windows closing with it. Deleting a sprite closes its window; a rename, or
  the file saved, or saved under another name, retitles it, each round bringing
  the sprite windows of a file that moved into line. Entries are found by an identity the file's document gives each
  (`EntryId`), so a reorder, insertion or undo never sends an edit to the
  wrong sprite.
- **The palette bar** (PT34): a palette picture's wells — every entry of its
  own palette, or the desktop's colours where it has none, and the clear ink
  of its mask — stand in one row beneath the canvas, at least `LEAST_WELL`
  across; a row longer than the window scrolls sideways by the wheel or its
  bar, which is shown only then and repeats while held as the window's other
  bars do, and keeps the well of the ink last changed in view. Every event
  reaches the wells but a press beside the strip, so a well pressed and let go
  elsewhere is let go; the primary button chooses the primary ink and the
  middle the secondary, each by a press and release on one well, and a change
  of ink repaints only the wells a mark left or reached. The wells stand a
  pixel apart (`WellSpacing::Packed`), and the primary and secondary inks'
  wells show their RISC OS colour numbers in place of their marks — a colour
  picture's the Wimp colours 0–15, a palette picture's its own entries' —
  inside the well where the digits fit, else on a badge kept within the grid;
  the clear ink has no number and keeps its mark.
- **The floating palette** (PT36): a square control at the strip's right end,
  as tall as the strip's inside and sized as one more well
  (`layout::palette_row`), floats the palette in a tool window (PT28), and
  View ▸ Panes ▸ *Float palette* floats or docks it. Floating, the wells open
  in a grid as near square as the entries make, never fewer than
  `LEAST_COLUMNS` across where there are as many, each `MOST_WELL` across, the
  clear ink after the entries, every well showing. The tool window is
  resizable (`PaneMeasures::sizing`): from one column of wells, with the bar
  where one may show, to one row of them, never shorter than a well or the
  bar's least length, and never so narrow that a palette too short for its bar
  would need one; the range is restated as the well count changes. Resized,
  the wells keep their size and set out as many to a row as the width holds;
  where the rows overflow the height they scroll down by the palette's own bar
  at its right. The size is window state, kept in logical pixels
  (`Scale::logical_length`) so a change of scale keeps it, and forgotten when
  the palette docks or the panes are reset; it is never a setting. The layout
  points the palette's rectangles into the tool window's and says which way
  the wells scroll (`Layout::palette_axis`), so hit tests, keys — Up and Down a
  reflowed row — scrolling, revealing a well, settling, painting and damage
  are the strip's own code along either axis, and a resize repaints the tool
  window alone. The strip claims no band while the palette floats or is
  hidden. A conversion or a mask change lays the
  window out again and the host refits the tool window; an entry's edit
  repaints its cell.

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
  holds a sprite's name, shown in a sprite file window; a TIFF's pages where it
  was read or made as a TIFF and holds none, its Pages menu going to, adding,
  copying, deleting and reordering pages, which have no name; one picture
  otherwise.
- **New picture** names the format first (`Origin::New(SaveFormat)`), offering
  the depths it admits (`SaveFormat::admits`) and a clear background where it
  holds one.
- **Save As** puts up a sheet before the picker
  (`DocumentView::ask_how`, answered by `Request::SaveWhere`): the formats
  the document can be written as, the chosen one's settings alone, and what
  it cannot keep (`save::Loss`) in the sheet's message. What each format
  loses is found on the worker (`Compute::Survey`, `save::survey`), since a
  loss reads every pixel; `ask_how` hands the host that request as an outcome
  and the sheet goes up when it lands. The picker is offered that format alone
  (`save::save_formats`), opens on it, and types the file it makes as it, so
  the save writes the format the file says (`save::format_for` over the name
  and the type it records); the rename the save makes records that type, and
  a Save As that never lands leaves the document's own file and format as
  they were. A
  sprite file is named by its type alone: `Untitled`, never `Untitled.spr`. A
  plain Save with nowhere to write opens the sheet too; a
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
  A merge lays its layers together onto nothing, which keeps the look to
  within a level's rounding because source-over is associative; it merges
  only layers that show, a hidden one's pixels having nowhere to go. Adding, removing, moving and reshowing a layer
  are steps of their own (`Step::Layer*`) holding only what left the picture;
  a merge or flatten is a picture step. A tile step names its layer, and
  undoing one paints on that layer again (`Damage::Layers`). Each layer
  carries a revision (`Layer::revision`), raised to one its document has never
  held whenever its pixels change, and only then: what its thumbnail is kept
  by.
- **Strokes and shapes**: coverage is composited from the state before the
  stroke (`stroke::Coverage`: a line or shape keeps the most any pass
  covered, a brush's dabs build up to its opacity, held in 65535ths so the
  faintest flow still reaches it), so a stroke never darkens where it crosses
  itself;
  dabs (`brush::Tip`: size, hardness, opacity, flow, spacing) are laid along
  the path with the spacing carried between segments; the airbrush adds a dab
  per interval held, laid one report behind where the pointer was at that
  instant by the device times its reports carry, and at the point only once a
  silence longer than any report's transit shows it was held still
  (`brush::Spray`); the clone tool reads from the stroke's starting picture
  at the Alt-clicked offset. Shapes are integer fixed-point with antialiased
  edges from `lib/raster` row coverage, rectangles with rounded corners.
- **Selections** (`mask::Mask`): a rectangle held as its bounds, or an alpha
  mask over them; rectangle, ellipse, lasso, polygon and wand recipes made and
  combined (replace, add, subtract, intersect) and feathered on the worker
  (`Compute::Select`); every stroke, fill, gradient, text and adjustment is
  held to the selection as much of each pixel as it chooses.
- **Fill, gradients, operations, transforms, quantisation**: a flood fill
  finds its region and fills it on the worker, as a gradient is laid; the
  adjustments and effects (`op::Op`, "Adjustments and effects") are worked out
  on the layer painted on a block of tiles at a time, previewed on the preview
  worker, an Apply adopting a complete preview and completing an incomplete one
  on the worker; on a palette picture an adjustment maps the palette. Scale
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
- **The Colour pane** holds the two wells and the picker,
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
- **Models** (PT15): `lib/colour` adds `Cmyk` (device CMYK, uncalibrated —
  what a CMYK field means with no profile), `Xyz`, `Lab` and `Lch` under the
  D65 white, and `Illuminant`: the white of a correlated colour temperature
  and Duv along the Planckian locus (Krystek's rational fit in the uv plane,
  1000–15000 K, smooth across it), and the temperature and Duv a colour lies
  at.
- **The selector** (PT24): the shared picker has three views
  (`tairix_controls::PickerView`) — the square, the wheel (a hue ring round a
  saturation–value triangle, its pure corner turned to the hue) and sliders
  (one track per channel, each drawn as that channel sweeps with the rest held,
  and one for opacity) — and shows its fields in one model
  (`tairix_controls::ColourModel`): RGB, HSV, HSL, CMYK, Lab, LCh or grey. A
  model's values are held as typed, so editing one channel keeps the others as
  shown; the CIE values are held to tenths; a Lab or LCh colour outside sRGB is
  shown at its nearest and the swatch marked, `lib/colour` calling a colour
  clipped only where holding it to the gamut changed an 8-bit level. The view
  and model are the owner's to keep. The Colour pane (`view_colour.rs`) holds
  the two wells, Swap, Reset (black and white, or the entries nearest them) and
  Pick — a one-shot pick that takes the next press on the picture into the ink
  the pane edits, the tool in use carrying on, and Escape putting it down —
  the View and Fields choices, the picker, and the last 16 colours settled or
  picked, eight to a row, offered on a colour picture alone.

## The window

- **Bands** (`layout::Layout`): the tool-controls bar across the top, the view
  strip at its end (zoom out and in, fit, actual size, and the pixel grid,
  marked while it shows); a dock of panes down each side (`layout::DockLayout`,
  "Panes"); the palette strip beneath the canvas and its bars; the status band
  along the bottom. The Tools pane holds the tool box — the shared `Toolbar`
  laid out `ScrollOrientation::Vertical` in two lanes (`TOOL_BOX_LANES`), its
  tools two to a line in reading order, scrolling a line at a time when the
  pane is short.
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
  flattens and shows layers (`Action::*Layer*`, `view_layers.rs`); a layer is
  named and its opacity set in the Layers pane ("The Layers pane").
- **The palette strip**: the whole palette in one row of wells 14 to 24
  logical pixels across beside its pop-out, or in a floating palette's grid;
  the grid's columns follow the layout (`SwatchGrid::set_columns`), and the
  strip's height is the one `PaletteRow::band` the floor and the layout share.
- **The floor**, declared once as a window opens and so held for any document:
  the view strip beside the widest setting, the bar's band at the most rows
  any tool wraps to there, and round a canvas the Tools pane showing a tool,
  the Colour pane at its width, and the strip of a full palette and its clear
  ink (`view::MOST_WELLS`).
- **The keyboard**: Tab walks the picture, the bar's settings, the palette, the
  Colour pane's buttons and choices, its picker and its recent colours, the
  Layers pane's controls and rows, and the Adjustments and Effects panes'
  settings, and back to the picture; Shift+Tab the other way, passing a part
  with nothing that can act. Escape returns it to the picture. A field claims
  every key but Tab and its owner's chords (`tairix_controls::owner_chord`),
  and a chord settles the field before the window acts. A press elsewhere, or
  the menu, settles and releases the bar and the dock; the palette takes the
  keyboard only from Tab, docked or floating, and gives it up to any press.
- **The pointer**: every part of the chrome sees every event, so a hover
  leaves and a press held on one part ends wherever it is let go; while a pane
  is dragged by its band nothing else takes the pointer.

## Panes (PT17, PT37)

- **Docks** run down the window's left and right edges between the bar and the
  status band, each one column of panes stacked top down: by default the Tools
  pane on the left — the tool box, its tools in two columns — and on the right
  the Colour pane, which the Layers, Adjustments and Effects panes join as
  View ▸ Panes shows them or an adjustment or an effect opens.
- **Seating** (`layout::PaneMeasures::seat`): each body states a least and a
  natural height (`layout::BodyHeights`). A dock seats every band first, then
  gives each open body its least, then grows the bodies towards their natural
  heights, the pane opened most recently first (`Arrangement::opened`, never
  stored); a pane left no room shows its band alone. A dock is as wide as its
  widest pane, and an empty dock takes no width.
- **Scrolling**: a body given less than its natural height scrolls within it,
  and the column never does. The Tools and Layers panes scroll their own parts
  (`Fit::Own`). The Colour, Adjustments and Effects panes (`Fit::Scrolled`)
  are laid out whole and unscrolled from their body's top
  (`PaneSlot::content`) and scrolled by a bar down its right (`PaneSlot::bar`)
  through one mapping (`View::body_view`, `View::held_view`, a
  `tairix_controls::ScrollView`) that every paint, event, hit test and damage
  passes through; a part the keyboard enters is scrolled into view. The
  Adjustments and Effects panes state their heights from their settings
  (`Needs::operations`).
- **The palette** is a pane whose one dock is its strip beneath the canvas
  (`PaneKind::Palette`): shown, floated and hidden as any pane is, it never
  docks down a side, never rolls up, and has no band in its strip (PT36).
- **A pane** is a header and a body. The header is a mini title band — one
  caption line between gaps, far shallower than a window's title bar — naming
  the pane, with a disclosure that collapses it to the header and a close
  mark. It is window content, never furniture: it moves the pane, not the
  window.
- **Dragging a header** moves its pane within its dock, into the other, or —
  let go anywhere that is not a dock, or carried to the window's edge — into
  a tool window (PT28). While a drag is over a dock, or within reach of the
  window's edge where a dock is empty, the gap it would land in is marked
  from the sample that starts the drag on. Escape, or the window losing the
  keyboard, turns the drag down, and the release then floats nothing
  (`view_panes.rs`).
- **Closing** hides a pane, and View ▸ Panes shows it again; *Reset panes* puts
  every pane back where the settings say a new window starts. The arrangement
  is the window's; the one a new window opens with is a setting (PT26), stored
  as each pane once, `pane:side` down each dock in turn, then `palette:strip`
  where the palette lies in its strip, and then the rest, a floating pane
  marked `:floating` and a hidden one `:hidden` (`Arrangement::spell`), the
  palette's place always `strip`. A stored arrangement lacking the palette
  does not read, and a new window takes the shipped one instead: nothing has
  shipped, so the spelling evolves in place.

## Tool windows (PT28)

- A floating pane is a **tool window**: a transient of its document window that
  the window manager decorates with a mini title bar — what the pane's band
  names, the open adjustment's or effect's for the Adjustments and Effects
  panes, in the caption face,
  close alone — above its owner, off the icon bar and the window
  picker, hidden while its owner is minimised, closed with it, and given the
  keyboard only when pressed.
- Carried to the window's edge — as far as a held press's pointer is reported
  once it has left the window — a pane opens in a tool window under the
  pointer at the place along its band the press held its header, and the
  window manager takes the press on as its own move, so the pane leaves
  without the press being let go. Let go in the window away from a dock, it
  opens where it was dropped, its band where the header would have been. A
  pane that starts floating, from the settings, opens under the top band
  against the edge of its home side; the palette, popped out or floating from
  the settings, against the canvas's lower right corner.
- A floating pane is laid out at its own breadth and open height — the
  palette and the Layers pane at the size their tool windows were resized to,
  which each keeps as window state in logical pixels (`Scale::logical_length`)
  until it docks,
  the Layers pane between its controls over two rows and every row, and from
  its own breadth to twice it — in a rectangle of the
  window's drawing beside the window's own area
  (`Layout::floating`), so one drawing and one set of hit tests serve the
  window and its tool windows: the host translates, and an open list in a
  floating pane is held within that pane's own window (`Layout::pane_window`).
- While a tool window is moved, the window manager says where the pointer is
  over its owner's client area, and nothing about the screen; over a dock the
  landing place is marked — for the palette, a band `EDGE_REACH` deep along the
  window's foot between the docks — and let go there the pane docks and the
  tool window closes.
- Its close mark hides the pane, as a docked pane's does, but the palette's
  docks it in its strip, so it is never lost; a tool window the desktop
  refuses to open docks its pane where it last stood (`Arrangement::redock`).
  A secondary press on a tool window opens the window's menu over that tool
  window (`docapp::menu_over`). A key the pane does not take acts on the
  document as if typed in its window, and a question the window asks veils
  its floating panes with it.

## The Layers pane (PT38)

- **Rows** (`layers_pane::LayersPane`, `view_layers.rs`) run top layer first
  beneath the pane's controls — the opacity of the layer painted on, New,
  Duplicate, Delete and Merge down — each an eye, a thumbnail, the name and
  the opacity, the layer painted on marked. The rows scroll by a bar of their
  own; the controls stay whole.
- **Picking** a row, or Up, Down, Home and End on the rows, paints on its
  layer, and is no step; an opacity still changing lands, and the open
  operation is applied and a floating selection put down, first, as stepping
  to a layer does (`View::work_on`). A press the pane's controls take takes
  the keyboard from the rows, and a field claims every key it is given, so a
  key typed there never reaches the window's shortcuts.
- **One step each**: the eye, or Space on the rows, painting staying where it
  was; a rename in place — a click on the name of the layer painted on, or
  Enter on the rows; Enter, a press elsewhere, any action, a menu entry, a
  close or the window losing the keyboard settles it (`View::settle_rename`),
  Escape drops it, and an empty name keeps the old one; a rename is held by its
  layer's revision (`Layer::revision`), so it lands on that layer or, the layer
  gone, on none; a reorder by a row dragged past the slop, the landing gap
  marked; New, Duplicate, Delete and Merge down, the Layers menu's own
  commands. There is no layer form, and no blend mode.
- **Opacity**, dragged or typed, shows each sample live
  (`Document::set_opacity_live`: no step, no generation) and lands as one step
  when it settles (`Document::record_shown`), its room held before the first
  sample; any action, a menu entry, a press elsewhere, showing, renaming or
  picking a row, a close or the window losing the keyboard settles it first.
  The first sample asks the preview worker to lay the layers beneath together
  a tile at a time (`PreviewWork::Under`, `compose::laid_together`), given up
  between tiles as any preview is; while that composite is of the entry,
  layer and generation it was laid from, each paint lays only the layer and
  those above over it (`compose::lay_run`), exactly what laying every layer
  shows. Severe memory pressure lets the composite go, and each paint lays
  every layer until the opacity settles.
- **Thumbnails** (`View::wanted_preview`, `PreviewWork::LayerThumbnails`) are
  drawn on the preview worker, up to eight of the rows in view a job, and kept
  by the revision of the layer they show (`Layer::revision`):
  a stroke asks again for its own layer's alone, an undo of how the layers
  show, their order or their number for none, and nothing is asked again
  until the picture, the rows in view or the side move — but a job given up
  to the queue's work, which is asked again. A row shows the picture glyph
  until its thumbnail lands.
- **Withheld**: the controls for a palette picture's one layer and while a
  worker has the picture, when no rename begins either; a rename whose layer
  no longer holds its revision is dropped.
- **Floating**, the pane is resizable from its controls over two rows to every
  row, and from its breadth to twice it, and keeps the size it is given.

## Adjustments and effects (PT13, PT18–PT23, PT39, PT40)

- **Two panes, one type** (`op_pane::OpPane`): the Adjustments pane offers the
  eleven operations that map each colour on its own — brightness & contrast,
  levels, curves, hue & saturation, colour balance, white balance, desaturate,
  invert, posterise, threshold and the channel mixer — and the Effects pane the
  seven that make each pixel from its neighbours, which only a colour picture
  takes — Gaussian blur, unsharp mask, median, add noise, find edges, emboss
  and pixelate (`op::Family`, `op::Op`); the Adjust menu lists every one, each
  row opening its pane on it under a function of its own (`Op::every`,
  `Op::symbol`). Each is a permanent selector of its
  family's operations, the chosen one's settings declared into one
  `panel::Panel` — a list, a number with its slider or a swept track,
  captioned fields, buttons, a switch, a track of handles, a note, or a graph
  it draws — then Preview, Reset and Apply. Its body's least is the selector
  and its natural every part at full height, the dock scrolling what lies
  between (`OpPane::heights`, `Fit::Scrolled`). Nothing is modal: the tools,
  the panes and the menus stay live.
- **One preview across both panes**: a sample of a setting, Preview turned on,
  a pick or Auto opens the pane's operation (`OpPane::is_open`), and the window
  leaves the other pane's first (`View::leave_operation`). Leaving an operation
  — choosing another in its selector (`OpOutcome::Switched`), or opening the
  other pane's — applies it as one step where its settings were moved and it
  changes something (`OpPane::applying`), and drops it otherwise, an Invert
  chosen and never moved among them, so looking down a list applies nothing.
  Its pane keeps its settings until the step has landed, the operation chosen
  opening only then (`Then::Choose`): one refused stays open as it was set, and
  the other pane, opened meanwhile, goes back to its start
  (`View::operation_refused`). Slider samples preview; Apply alone records a
  step, and returns the pane to the operation's starting settings, closed
  (`OpPane::applied`), Add noise drawing afresh for the next; Apply in one pane
  while the other's operation is open applies that one first and this one once
  it has landed (`Then::Apply`). Reset, and Escape in an open pane, return the
  settings to where the operation started; Escape in a closed pane is the
  window's. An answer for a state of the layer painted on (`Layer::revision`)
  or a selection (`View::selection_epoch`) since moved is dropped and asked
  again (`operations::Basis`); how the layers show, their names and their order
  are not read, so a change to them keeps what was worked out. A job refused
  room on the worker's queue is asked again by the next input, never at once;
  one that ran short of memory, or could not be worked out, only once the
  picture or what is asked moves, the memory pressure falls or the window is
  laid out anew. A palette
  picture's adjustment is its palette mapped, worked out on the loop, and the
  Effects pane is withheld from it; an effect left open as the picture becomes
  one — an undo past a conversion — is closed, saying so, and one asked to
  apply to one is refused as needing a colour picture. The panes are withheld
  while a worker has the picture.
- **What applies it first**: marking a selection or a crop box, the view, the
  inks, the panes, choosing a tool, undo and redo, and how the layers show and
  in what order — an eye, a name, an opacity, a layer raised or lowered —
  leave it open, and where the picture moved beneath it what it shows and
  reads is asked again (`View::follow_picture`). Anything else that changes or
  reads the layer — a stroke, a fill, a transform, another layer or entry
  painted on however it is picked, a layer added, deleted or merged, a save, a
  copy — settles it first as leaving it does (`View::apply_adjustment`); one
  still being worked out is completed on the ordered queue and the action
  follows once it lands (`Settles::Adjusted`), a refusal naming a save or a
  close that waited on it as not done, but a press that found it unfinished
  paints nothing. A floating selection may lie over
  it: the operation applies to the layer beneath, as its preview showed.
- **The engine** (`tiled::run`, PT39): an operation is worked out over the
  layer painted on in blocks of tiles — at least four times its reach on a
  side, from two tiles to eight, so what a block reads is at most two and a half
  times what it writes and its window stays bounded (`tiled::block_side`) —
  each block reading the operation's whole reach about it
  wherever the picture extends (`effect::Window`), so every pixel is what one
  window over the whole picture would make of it: bit-identical however the
  work is cut and whichever block runs first. The blocks run across a
  `lib/parallel` runner, those of the tiles listed first first, a round at a
  time, the run stopping at the round a block halts in; a tile left as it was
  is answered as the very tile. An adjustment maps
  through a `tone::ToneMap` made once for the run.
- **The preview worker** (`preview`, PT40): previews, histograms, the Layers
  pane's thumbnails and the composite beneath a changing opacity run on
  Paint's own worker, never on the ordered queue, so no save waits behind them,
  across a pool of one participant per online CPU — the tiles a block at a
  time, a histogram a band of rows, the thumbnails a layer each and the
  composite a tile each. An Apply's completion, a merge or a flatten on the
  ordered queue shares the pool and takes it first (`run.rs`'s `Cores`): a
  preview gives way at its next block, band, thumbnail or tile while one is
  due, its ticket still holding so it is asked again; one asked while one is
  due is answered as given way at once, never waited on — without a thread of
  its own the preview worker is the event loop — and the loop asks none until
  that work has landed; so neither ever finds the pool taken and works its
  blocks out on one core. Its queue holds a job
  running and one waiting for each window, the newest winning
  (`preview::supersedes`, `preview::PREVIEW_ROOM`). The loop asks each window
  once a round what it wants next (`View::wanted_preview`) — the overlay's
  tiles, then a histogram, the composite and the thumbnails — one job out at a
  time and only the newest's answer taken. A window's `preview::Ticket` is
  bumped with every sample, every move of the picture and the window's going;
  a job asks before each block whether its stamp still holds and is abandoned
  between blocks where it does not, an answer for a ticket since bumped is
  dropped, and a closed window's waiting jobs are withdrawn. What lands
  gathers into a `preview::Overlay` for one operation over one state of the
  layer, the tiles meeting the view asked first and the rest after
  (`Overlay::next_batch`); a sample starts the next from it, the last preview
  showing until its own tiles land (`Overlay::followed_by`). Apply adopts a
  complete overlay as one step, a tile the operation left as it was being no
  change; an incomplete one is completed on the ordered queue
  (`tiled::complete`) and lands as one step. A refusal waits as the
  Adjustments bullet says. Severe memory pressure drops the overlay, and the
  next sample asks again, and the composite beneath an opacity changing, which
  is not asked again until that opacity settles.
- **Histograms** (PT19, `histogram::Histogram`): 256 levels of red, green, blue
  and Rec. 601 luma of the layer painted on, each pixel weighted by its opacity
  and by how much the selection chooses it, and its mean in linear light, taken
  from the counts; worked out on the preview worker a band of rows at a time,
  the bands' counts added so it is the same however the rows were cut, while a
  pane reads one and the picture has moved since — not when the adjustment
  does, which reads the same picture. A plot is drawn against its tallest level
  between black and white, so a spike of clipped pixels does not flatten the
  rest.
- **Levels** (PT20) for the composite and for each channel, a channel's mapping
  before the composite's: input black, grey and white handles on a track
  beneath the channel's histogram — the grey point placed where it maps to
  half, so `gamma = ln p / ln ½`, and kept as the gamma when black or white
  move — and output black and white on a second; every value in its field, the
  gamma to two places (`NumberField::with_decimals`); black, grey and white
  pickers that set each colour channel from a pixel of the layer beneath the
  preview; Auto, clipping 0.1% at each end of each channel (`tone::Levels`).
- **Curves** (PT21): up to 16 points per channel, through which the curve runs
  as a monotone cubic (Fritsch–Carlson), so it never overshoots between them;
  a press on the graph adds a point or takes one within reach, which follows
  the pointer until let go, and one carried out of the graph is taken away; the
  arrows nudge the chosen point, Delete takes it away, Page Up and Page Down
  choose the point before and after; the chosen point's input and output in
  fields; the channel's histogram behind (`curve_graph::CurveGraph`).
- **White balance** (PT22): the temperature the picture's light is taken to be,
  in kelvin from 2000 to 12000 and neutral at 6500, and its tint, each on a
  track swept with the cast a mid grey takes. A light is D65 — sRGB's own white
  — moved across the CIE 1960 uv plane as far as the named temperature and tint
  lie from 6500 K on the Planckian locus, so the neutral settings change
  nothing and a picked colour is corrected to a true grey; the correction is
  the von Kries ratio of D65 to that light in linear sRGB, a grey's luminance
  kept. The neutral picker solves both from a pixel that should be grey; Auto
  takes the picture's mean in linear light as that pixel.
- **Hue and saturation** (PT23) for the master and each of the six colour
  ranges, a range at full strength at its centre and fading to nothing at the
  next range's, 60° away, a range's lightness weighed by the colour's
  saturation so a grey keeps its own; the input and output hue spectra, the
  chosen range's reach marked. **Colour balance**: cyan–red, magenta–green and
  yellow–blue for the shadows, midtones and highlights, a lightness `l`
  weighed by the quadratic Bernstein basis — `(1 − l)²`, `2l(1 − l)`, `l²` —
  which sums to one, so a slider's end moves a channel half its range where
  its band weighs one (`tone::ColourBalance::shifts`), luminosity kept when
  asked. **Threshold**: its level a handle beneath the histogram.
- **Channel mixer**: each output channel weighed from red, green and blue,
  −200 to 200 percent each, and a constant of white, −100 to 100; monochrome
  makes every channel one grey, Rec. 601's weighing to start
  (`tone::ChannelMixer`). **Posterise**, **Desaturate** and **Invert** are one
  table or one grey each.
- **The effects** (`effect::Effect`):
  - *Gaussian blur*: three box passes of `lib/raster`'s one box blur over
    sixteen-bit premultiplied pixels (`Pixel16`, `box_blur16`), their radii
    matched to the standard deviation by Kovesi's widths after Wells
    (`effect::gaussian_radii`), so each pass rounds at sixteen bits and a clear
    pixel lends no colour.
  - *Unsharp mask*, replacing Sharpen: each channel pushed by its amount away
    from that blur where the two differ by its threshold or more.
  - *Median*: each premultiplied channel the median of the square about it,
    Perreault and Hébert's constant-time filter — column histograms slid a row
    at a time and the kernel's a column at a time, at coarse and fine levels —
    so a pixel costs the same at any radius and every colour channel stays
    within its alpha.
  - *Add noise*: uniform, or Gaussian as the sum of twelve uniform bytes
    (Irwin–Hall), each pixel's offsets drawn from the seed and its place alone
    in integers (`tairix_mix`), so they are the same on every target and
    however the picture is cut; monochrome moves the three channels alike.
  - *Find edges*: each channel's Sobel gradient strength. *Emboss*: a grey
    relief of luma weighed by alpha, lit from an angle and read a height ahead
    and behind. *Pixelate*: squares lined up on the picture's corner, each the
    premultiplied mean of what it covers.

## Settings (PT26, PT27)

- The icon bar's menu holds *Settings…* after *New window*; it opens Paint's
  settings window, or raises the one that is open (`settings::PaintSettings`,
  the host's shared `docapp::settings::SettingsWindow` over Paint's registry).
- The window is the application's own, not a document's: its categories down
  a sidebar list, each category's settings as groups of form rows beside it
  (`lib/controls`' `SettingsFrame`, `plans/GUI-CONTROLS-DESIGN.md` §11.52),
  and *Restore defaults* beneath with what the store last refused. A colour is
  a colour field: a swatch typed in hexadecimal.
  - **General**: the tool a new window starts with; what a picture opens at,
    fitted to the window (never past actual size) or at actual size.
  - **New picture**: width, height, format, the colours that format holds,
    and a transparent background where it can be clear — what *New picture*
    offers to start.
  - **Grid** (PT27): spacing and offset across and down, colour, opacity and
    style — lines, dashes, dots or crossings; whether a new window shows it;
    snapping to it; the zoom from which the pixel grid shows, or never.
  - **Canvas**: the checkerboard's square and its shades, the theme's or two
    chosen; what surrounds the picture, the theme's ground or a chosen colour.
  - **Panes**: the arrangement new windows open with — the front picture
    window's, taken, or the shipped one — and the one *Reset panes* returns to.
- The settings are one closed registry (`preferences::Preferences`) kept in
  Paint's private app-data store by the shared registry engine
  (`plans/APPDATA.md` AD11), read once at start. A change applies to every
  picture window as it is made — what a window draws with
  (`preferences::CanvasStyle`), snapping, and what *New picture* and *Reset
  panes* offer; the tool, the panes, the grid shown and the opening zoom are a
  new window's — and is written off the loop by the settings writer as the
  interaction settles, the store's answer adopted wherever the user has not
  edited since
  (`tairix_appdata::Publication`). A refused write is said in the settings
  window and on `stderr`, and the values it carried return to what the store
  holds; the last write is seen out before the process ends.
- **The two grids** (PT27, `grid::GridLines`): *View ▸ Grid* (Ctrl+') shows a
  window's grid, a line at every pixel boundary `offset + k × spacing` drawn in
  the screen column or row where the pixel after it starts, not drawn where a
  cell is under four pixels; *View ▸ Pixel grid* (G, the strip's button) shows
  the lines between pixels from the zoom the settings name.
- **Snapping** (PT27): with a window's grid shown and snapping on, a shape's or
  a marquee's or a crop box's box covers whole cells, the edges it covers on
  the lines nearest them whichever way it was dragged (never less than a cell);
  a line's ends, a gradient's ends and a polygon's corners land on the pixel
  at the nearest crossing; a dragged selection's top left lands on the nearest
  crossing; an edge a crop handle moves lands on the nearest line.

## The host (PT25, PT28)

`tairix_window::docapp` grows what a document application needs beyond its
document windows:

- **Icon-bar rows** (`DocumentApp::BAR_ROWS`): rows of the application's own
  after *New window*, each chosen row handed to `DocumentApp::bar_chosen`
  under the activation the choice grants.
- **App windows** (`AppView`, `Host::open_app_window`): windows of the
  application's own with no document, opened by it, routed by id, painted from
  what their view reports through `DocumentApp::render_app`, closed by their
  close mark, and closed by Quit only once the quit can no longer be turned
  down, never holding it back. A control of one that changes on its own — a
  held scroll arrow — is stepped on the deadline it names
  (`AppView::deadline_ns`, `AppView::advance`), folded into the loop's wait.
  An application with none says so with `NoAppView`.
- **The settings window** (`docapp::settings`, the `settings` feature): an
  application's preferences window over a closed registry implementing
  `docapp::settings::Preferences` — its categories and the groups each shows,
  how a row's action edits the record, and what reshapes a category. One
  event is one `SettingsRequest`: the edit it made, from what to what and
  whether it settled, then *Restore defaults* or an ask of the application's
  own; the application applies the edit, writes it off the loop on the
  settle, and hands what the store says back through `SettingsWindow::adopt`
  and what it refused through `SettingsWindow::say`
  (`tairix_appdata::Refusal::is_write`). Paint and Draw are its consumers.
- **Seeing work out** (`DocumentApp::leaving`): the process's end gives the
  application its turn to see its own workers out before the saves are.
- **Tool windows** (PT28, `DocumentView::tool_window`): a document window's
  transients, each showing a rectangle of the window's own drawing laid out
  beside it. After every round the host opens, resizes, retitles and closes
  them to match what the view asks for, asks the view once where a new one
  opens (`tool_opening`, carrying a held press when it says so), routes their
  input to the view translated into its drawing, hands their moves and close
  marks to it (`tool_moved`, `tool_gone`), and paints what the view reports
  inside each into that window alone. The view hears it has the keyboard while
  the window or any of its tool windows does, and of its going once the
  round's events are in, so moving between them is no change.
- **Showing a document's folder** (PT30): a secondary press on a document
  window's close button closes it and, once closed, has the desktop show its
  file in the file manager through a location grant on its descriptor, as
  `plans/TEXTEDIT.md` TE17 sets out; a refusal is stated in an alert.
- **Member windows** (PT31, `Host::open_member`): further windows onto a
  document window's document, each with an engine of its own. A member has no
  file: its Save, Save As and Open act on its primary, the window whose
  document it shows, and a Save As's new name and access reach every member.
  A member is never asked whether to save, by its close or by a quit; it
  closes with its primary — once the primary's own question is answered, so
  turning that question down closes nothing — and when the primary shows
  another document. After every outcome the host applies to a window it hands
  the application that window's index before carrying out what was asked
  (`DocumentApp::driven`), so what an engine changed reaches its document's
  other windows before a save of it is frozen.

## The loop

Paint runs in `tairix_window::docapp`'s host, which owns its windows, saves,
choosers and the queue saves are written on. Its own: a decode worker holding
the sandbox, released after each document so each document and each paste is
read by a fresh worker; on the queue beside saves, clipboard encodes, fills,
gradients, selections, what an Apply of an operation still has to work out,
transforms of every layer, merges and flattens, the Save As survey, and a
floating selection's putting down or clearing (`view::Compute`, each answer
landing as its `Lands` says); and the preview worker, with a pool of one
participant per online CPU that an Apply's completion, a merge or a flatten
on the queue takes ahead of it, for the speculative work — previews, histograms, the Layers pane's
thumbnails and the composite beneath a layer whose opacity changes — so a save
never waits behind it. A window's own work is held to its share of
the queue (`JOBS_PER_WINDOW`) and of the preview worker's
(`preview::PREVIEW_ROOM`), and a pass of the loop adopts only what had landed
when it began.

## Invariants

- No filesystem capability; nothing of a document or a pasted picture is
  parsed in the painter's address space.
- While a worker has the picture nothing changes the document, the entry
  showing or the layer painted on, and its answer is written only over the
  entry, layer and generation it was asked of.
- What a window shows is what a save writes: the layers are laid together by
  the one routine the paint and a flatten share.
- An operation's answer is the same to the bit however its work was cut into
  blocks and in whatever order they ran, so what Apply lands is what the
  preview showed.
- Save As offers only names Paint can write: the picker holds the name to the
  format the sheet chose before any file is made, and Paint writes that format.
- A paint reads nothing and costs what the window shows, not what the picture
  holds.
- Every allocation that grows with the picture is fallible; a refusal is
  stated in the window, never an abort.
- A kept sprite is written back exactly as it was read.
- A colour edited in the dock writes nothing until it settles, and lands as
  one step.
- An opacity changed in the Layers pane lands as one step however long it
  moves; picking a layer is no step.
- A tool's setting writes nothing: it is the window's own, applied as it
  changes.
- Closing a window or quitting with unsaved changes — an operation whose
  settings were moved and work still out on the picture among them — asks
  first, and Save there lands them before it saves; a sprite window's changes
  are its file window's, asked about there, its operation applied and what
  floats put down before it closes.
- No loop waits on another thread's work: a preview asked while the queue's
  work wants the cores is answered as given way.
- No tool, pane, tool window or settings window is modal: only a command's
  own question before it acts — saving, a new picture or page, a resize, the
  canvas size, a depth — takes the whole window.
- A worker's answer lands only on the layer, generation and selection it was
  asked of.
- Nothing in the settings window writes on the loop.
