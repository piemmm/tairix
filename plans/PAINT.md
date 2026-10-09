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
| PT15 | `lib/colour`: device CMYK, CIE XYZ, L\*a\*b\* and LCh(ab) under D65, and a correlated colour temperature with its tint | done |
| PT16 | The tool box in two columns: the shared `Toolbar` seats its tools in lanes across its breadth | done |
| PT17 | Panes: the left and right docks of mini-titled panes — Tools, Colour, Adjustment — each collapsed, closed, and moved within and between the docks by its header | done |
| PT18 | Docked adjustments: Adjust opens its choice in the Adjustment pane, previewed live and modal to nothing, under the preview rules in "Adjustments"; every worker answer lands only on the state it was asked of | done |
| PT19 | Histograms of the layer painted on, held to the selection, worked out on the worker for the panes that show them | done |
| PT20 | Levels: input and output levels for the composite and each channel over its histogram, black, grey and white point pickers, and Auto | done |
| PT21 | Curves: a tone curve for the composite and each channel, edited by its points over the histogram | done |
| PT22 | White balance: temperature and tint on tracks of the correction's colour, a neutral-point picker, and Auto | done |
| PT23 | Hue and saturation by colour range over the hue spectra; colour balance by tone range; threshold over the histogram | done |
| PT24 | The colour selector: the shared picker's square, wheel and slider views, its fields in RGB, HSV, HSL, CMYK, Lab, LCh or grey; the Colour pane's wells, recent colours and one-shot picture pick | done |
| PT25 | The document host: an application's own icon-bar rows and its app windows | done |
| PT26 | The settings window: Paint's own window from its icon bar's *Settings…* row, its settings by category, held in its private app-data store and applied to every window as they settle | done |
| PT27 | The grid: spacing, offset, colour, opacity and style; the pixel grid's threshold; snapping to the grid | done |
| PT28 | Tool windows: a pane dragged out of its dock floats in a tool window — the window manager's mini-titled transient (`plans/APPWIN.md` AW7, `plans/COMPOSITOR-WORK.md` stage M), the document host's tool windows of a document window — and docks again when dragged over a dock | done |
| PT29 | Help in every locale, the docs page and the README for all of it | done |

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
  A merge lays its layers together onto nothing, which keeps the look to
  within a level's rounding because source-over is associative; it merges
  only layers that show, a hidden one's pixels having nowhere to go. Adding, removing, moving and reshowing a layer
  are steps of their own (`Step::Layer*`) holding only what left the picture;
  a merge or flatten is a picture step. A tile step names its layer, and
  undoing one paints on that layer again (`Damage::Layers`).
- **Strokes and shapes**: coverage is composited from the state before the
  stroke (`stroke::Coverage`: a line or shape keeps the most any pass
  covered, a brush's dabs build up to its opacity, held in 65535ths so the
  faintest flow still reaches it), so a stroke never darkens where it crosses
  itself;
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
  flattens, shows and names layers (`Action::*Layer*`, `view_layers.rs`), the
  name and opacity through the layer form (`dialog::Form::layer`), which keeps
  the exact opacity until its slider moves.
- **The palette strip**: the whole palette in as few rows as hold every well
  at least 14 logical pixels across, a well up to 24; the grid's rows follow
  the layout (`SwatchGrid::set_columns`).
- **The floor**, declared once as a window opens and so held for any document:
  the view strip beside the widest setting, the bar's band at the most rows
  any tool wraps to there, and round a canvas the Tools pane showing a tool,
  the Colour pane at its width, and the strip of a full palette and its clear
  ink (`view::MOST_WELLS`).
- **The keyboard**: Tab walks the picture, the bar's settings, the palette, the
  Colour pane's buttons and choices, its picker and its recent colours, and the
  Adjustment pane's settings, and back to the picture; Shift+Tab the other way, passing a part
  with nothing that can act. Escape returns it to the picture. A field claims
  every key but Tab and its owner's chords (`tairix_controls::owner_chord`),
  and a chord settles the field before the window acts. A press elsewhere, or
  the menu, settles and releases the bar and the dock; the palette takes the
  keyboard only from Tab and gives it up to any press.
- **The pointer**: every part of the chrome sees every event, so a hover
  leaves and a press held on one part ends wherever it is let go; while a pane
  is dragged by its band nothing else takes the pointer.

## Panes (PT17)

- **Docks** run down the window's left and right edges between the bar and the
  status band, each holding panes stacked top down: by default the Tools pane
  on the left — the tool box, its tools in two columns — and on the right the
  Colour pane, with the Adjustment pane beneath it once an adjustment is opened
  or it is shown from View ▸ Panes.
  A dock is as wide as its widest pane and gives each its natural height in
  turn; a pane past the room left shows its header alone, and an empty dock
  takes no width.
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
  as each pane once, `pane:side` down each dock in turn and then the rest, a
  floating pane marked `:floating` and a hidden one `:hidden`
  (`Arrangement::spell`).

## Tool windows (PT28)

- A floating pane is a **tool window**: a transient of its document window that
  the window manager decorates with a mini title bar — the pane's name in the
  caption face, close alone — above its owner, off the icon bar and the window
  picker, hidden while its owner is minimised, closed with it, and given the
  keyboard only when pressed.
- Carried to the window's edge — as far as a held press's pointer is reported
  once it has left the window — a pane opens in a tool window under the
  pointer at the place along its band the press held its header, and the
  window manager takes the press on as its own move, so the pane leaves
  without the press being let go. Let go in the window away from a dock, it
  opens where it was dropped, its band where the header would have been. A
  pane that starts floating, from the settings, opens under the top band
  against the edge of its home side.
- A floating pane is laid out at its own breadth and open height in a
  rectangle of the window's drawing beside the window's own area
  (`Layout::floating`), so one drawing and one set of hit tests serve the
  window and its tool windows: the host translates, and an open list in a
  floating pane is held within that pane's own window (`Layout::pane_window`).
- While a tool window is moved, the window manager says where the pointer is
  over its owner's client area, and nothing about the screen; over a dock the
  landing place is marked, and let go there the pane docks and the tool window
  closes.
- Its close mark hides the pane, as a docked pane's does; a tool window the
  desktop refuses to open docks its pane at the foot of its home side. A key
  the pane does not take acts on the document as if typed in its window, and
  a question the window asks veils its floating panes with it.

## Adjustments (PT18–PT23)

- **Docked, not modal**: Adjust opens its choice in the Adjustment pane
  (`adjust::AdjustPane`, `view_adjust.rs`), docked or floating wherever it
  last was, and nothing else is taken from the window: the tools, the panes and
  the menus stay live. The same adjustment chosen again shows the pane where it
  is. Invert, Desaturate and Find edges, which have no settings, apply at once.
  With nothing open the pane offers the list of adjustments to open one from.
- **The pane** is one `panel::Panel` each adjustment declares its rows into — a
  list, a number with its slider or a swept track, captioned fields, buttons, a
  switch, a track of handles, a note, or a graph it draws — under Preview,
  Reset and Apply. One routine lays it out for the paint, every hit test and
  the keyboard; a pane shorter than its rows takes the room from its graphs,
  down to a quarter of their height, before a control is left out. A dock that
  cannot give each pane all it wants gives the adjustment its room first
  (`PaneKind::BY_CLAIM`).
- **The preview** is worked out on the worker from the layer painted on, held
  to the selection, one job at a time between it and the histogram
  (`view_adjust::Looks`): settings moved while one works are asked again once
  it lands, and an answer for an entry, layer, generation or selection
  (`View::selection_epoch`) that has since moved is dropped and asked again.
  A refused or failed job is asked again by the next input, never at once. A
  palette picture's preview is its palette mapped, worked out on the loop.
  *Preview* turns it off and on to compare, the worked preview kept meanwhile.
  Apply lands it as one step and closes it, the pane going back to its list;
  Reset, and Escape in the pane, return it to its starting settings; closing
  the pane drops it. The pane is withheld while a worker has the picture.
- **What applies it first**: marking a selection or a crop box, the view, the
  inks, the panes, choosing a tool, undo and redo leave it open, and where the
  picture moved beneath it what it shows and reads is asked again
  (`View::follow_picture`). Anything else that changes or reads the layer — a
  stroke, a fill, a transform, a layer or entry change, a save, a copy, another
  adjustment — applies it first. Applying a preview that has landed is
  immediate, so what asked carries on; one still being worked out applies on a
  worker and the action follows it once it lands (`Settles::Adjusted`), but a
  press that found it unfinished paints nothing. A floating selection may lie
  over it: the adjustment applies to the layer beneath, as its preview showed.
- **Histograms** (PT19, `histogram::Histogram`): 256 levels of red, green, blue
  and Rec. 601 luma of the layer painted on, each pixel weighted by its opacity
  and by how much the selection chooses it, and its mean in linear light;
  worked out on the worker while the pane reads one and the picture has moved
  since. A plot is drawn against its tallest level between black and white, so
  a spike of clipped pixels does not flatten the rest.
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
  yellow–blue for the shadows, midtones and highlights through GIMP's band
  masks, luminosity kept when asked. **Threshold**: its level a handle beneath
  the histogram.

## Settings (PT26, PT27)

- The icon bar's menu holds *Settings…* after *New window*; it opens Paint's
  settings window, or raises the one that is open (`settings::SettingsWindow`).
- The window is the application's own, not a document's: its categories down
  a sidebar list, each category's settings in a panel beside it, and *Restore
  defaults* beneath with what the store last refused.
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
  picture window as the interaction settles — what a window draws with
  (`preferences::CanvasStyle`), snapping, and what *New picture* and *Reset
  panes* offer; the tool, the panes, the grid shown and the opening zoom are a
  new window's — and is written off the loop by the settings writer, the
  store's answer adopted wherever the user has not edited since
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
  down, never holding it back. An application with none says so with
  `NoAppView`.
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
- No tool, pane, tool window or settings window is modal: only a command's
  own question before it acts — saving, a new picture or page, a resize, the
  canvas size, a depth, a layer's properties — takes the whole window.
- A worker's answer lands only on the layer, generation and selection it was
  asked of.
- Nothing in the settings window writes on the loop.
