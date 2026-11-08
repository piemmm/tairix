# DRAW — the desktop vector drawing editor (`Draw.app`)

Binding under `AGENTS.md`. What the vector editor is, the geometry and the
drawing model it is built on, the formats it reads and writes, and the seams
that keep parsing out of its address space and rendering off its loop.

Read first: `plans/PAINT.md` (the document host and the editor conventions
Draw shares), `plans/SVG.md` (the decoder whose grammar Draw's reader reuses),
`plans/SAVE-DIALOG.md`, `plans/FONT-SERVICE.md`, `plans/FIX-DESKTOP.md`.

## Ledger

| Id | Item | Status |
|---|---|---|
| DR1 | `lib/bezier`: points, the affine map (moved from `lib/raster`), bounds, line, quadratic and cubic segments, elliptical arcs, roots, flattening (moved from `lib/svg`), paths, winding and area, nearest points, arc length, intersections, curve fitting, offsets; `lib/svg`, `lib/icon` and `lib/raster` built on it | done |
| DR2 | `lib/bezier` boolean operations — union, intersection, difference, exclusion, divide — keeping curves, under either fill rule | done |
| DR3 | `lib/svg` exposes its path commands, shape rules and gradient definitions as values its own decode is built from; `lib/xml` takes each reader's own bounds; the XML escaper moves to `lib/xml` | done |
| DR4 | `lib/drawing`: the editable drawing — objects, styles, gradients, text, groups, clip groups, layers, the page — a persistent tree of shared nodes with stable identities and fixed bounds | done |
| DR5 | `lib/drawing` SVG reader and writer, with Draw's own namespace for layers, live shapes, guides, the grid and the page | done |
| DR6 | `lib/drawing` RISC OS Draw file reader and writer, read through the sandbox by content and claimed as `image/x-riscos-draw` | done |
| DR7 | `lib/drawing` PDF writer | done |
| DR8 | `lib/drawing` EPS writer: one Level 3 page through the walk PDF shares, transparency written opaque and said to be | done |
| DR9 | `lib/drawing` rendering: any device rectangle of a drawing at any scale through `lib/raster`'s one renderer; the outline view | done |
| DR10 | `lib/sandbox::drawedit`: the sandboxed read of a document or a pasted drawing — SVG or a Draw file, by its content — into the drawing, its checked wire form and fuzz harness | done |
| DR11 | The engine: the document, its history, the selection, the viewport, the tile cache and its render worker, the overlays, the canvas's scrollbars | done |
| DR12 | Tools: select and transform, nodes, pen, pencil, line, rectangle, ellipse, polygon and star, text, eyedropper, zoom, hand | done |
| DR13 | Commands: clipboard, duplicate, arrange, align and distribute, flip and turn, lock and hide, group and clip, the boolean operations and divide, stroke to path, object to path, outset and inset, simplify and reverse | done |
| DR14 | The inspector: Style, Layers, Object, Shape and Page; linear and radial gradients fitted to each object, their stop bar, spread and angle; the palette and the drawing's own swatches; the rotation field | done |
| DR15 | Rulers, guides, the grid and snapping | done |
| DR16 | Page setup: paper presets, a page of any size, orientation, units, the grid's spacing and subdivisions, and a ground of any colour, from the Page menu and the inspector's Page tab | done |
| DR17 | Files: Save and Save As in SVG, a RISC OS Draw file or PDF; Save a copy as PDF, EPS, PNG, JPEG, TIFF, SVG or a Draw file, raster at the drawing's own copy resolution; a drawing that lost content on reading opening read-only | done |
| DR18 | The settings window and Draw's preferences: a new window's tool, view and style, a new drawing's page and grid, the nudge, duplicate offset and pick distance; on the shared settings window (`docapp::settings`, `lib/controls`' `SettingsFrame` and `FieldColumn`, the colour field), Paint's and WinterSun's moved onto it | done |
| DR19 | The `Run` binary: the read worker, the render worker across the cores, the queue's work, and glyphs fetched only on workers | done |
| DR20 | The bundle: manifest, icon, Help in every required locale, docs, registration | done |
| DR21 | Shared chrome: the one status band (`StatusBand`) for Paint's two windows, TextEdit and Draw; number slots in the form rows (`FieldControl::Number`, settled as the keyboard leaves), Paint's size rows on them; the ruler (`Ruler`) — all in `lib/controls` | done |
| DR22 | Embedded raster images: Draw sprite and JPEG objects, SVG `<image>`, placing a picture | done |
| DR23 | Styled runs within one text object: family, weight, italic, size, letter spacing, baseline shift and a colour fill and stroke over a span, chosen by a selection while typing | done |
| DR24 | Text set along a path: set a text on a path and release it, its start along the path, read and written as SVG `<textPath>` | done |
| DR25 | The gradient tool: a drag lays a linear or radial gradient on the selection, in each object's own space, live until it lands | done |
| DR26 | A new zoom shown at once from the last zoom's tiles resampled, until its own land | done |
| DR27 | Fallible shared allocation: every node, name, text, dash list, guide list and swatch list a drawing shares is made fallibly through `lib/collections`' `try_arc`, `SharedSlice` and `SharedStr`, and so is the geometry rendering and writing it run through — `lib/bezier`'s path operations, booleans, outlines and dashes and `lib/svg`'s stroker — so memory refused is a refusal stated in the window, never an abort — `lib/drawing`, `lib/bezier`, `lib/sandbox::drawedit`'s records and Draw | done |
| DR28 | The parser sandbox's seam allocates fallibly on the editor's side — the request writer, the room a reply is received in, every client's decoding, the worker launch — a refusal of the editor's memory said as that rather than as a failed worker, and a worker allocating as it needs under its ceiling; held by an allocation sweep over every client | done |
| DR29 | A document as long as the reading worker's share of the machine holds, for every document reader: the bound derived from the worker's memory ceiling, never below 64 MiB | done |
| DR30 | A picture several objects show written once in SVG, through Draw's namespace over a `<use>` | done |
| DR31 | Opening a PDF: one page as a drawing through `lib/pdf`, a page asked for where it has several and its password where it needs one, in a dialog window of Draw's own (`plans/PDF.md` PDF8) | done |

## What it is

`Draw.app` (`name = "Draw"`, `title = "Draw"`, `id = os.tairix.draw`,
`kind = "application"`, `library = "Graphics"`): vector drawing in layers,
from a page of paths, shapes, text and groups. It claims `image/svg+xml`, the
format it reads back what it writes, `image/x-riscos-draw` and
`application/pdf`, one page of which opens as a drawing (`plans/PDF.md`);
the double-click order between Draw and the viewer follows the store's, as
Paint's does. The manifest requests `CAP_CONSOLE_WRITE`, `CAP_SHM`,
`CAP_SANDBOX_SPAWN` and `CAP_LOG_EMIT`, and no filesystem capability: every
document is one the user handed over, and every write goes through it.

## Geometry (`lib/bezier`, DR1, DR2)

- **One geometry currency.** `Point` (a vector with the arithmetic geometry
  needs) and the affine map (`Affine`, SVG's `matrix(a b c d e f)`, moved here
  from `lib/raster`, which re-exports it). Below rasterisation and below SVG,
  so the decoder, the rasteriser and the editor share one definition.
- **Segments**: line, quadratic (elevated exactly to a cubic where a cubic is
  wanted), cubic: evaluation, derivative, split at a parameter, sub-segment,
  tight bounds from the derivative's roots, nearest point, arc length and its
  inverse, reversal.
- **Arcs**: SVG's endpoint form to its centre form (radii grown where too
  small, degenerate arcs a line) and a centre arc to cubics, at most a quarter
  turn each.
- **Flattening** is the one flattener every consumer draws through, moved from
  `lib/svg` with its arithmetic unchanged, so every decoded asset keeps its
  bits.
- **Paths** are sub-paths of segments, open or closed: bounds, transform,
  reversal, winding number at a point by exact curve crossings, signed area by
  Green's theorem in closed form per segment, containment under either rule.
- **Intersections**: line–line exactly; line–curve as the roots of one
  polynomial; curve–curve by subdivision of bounding hulls refined by Newton
  steps; a curve's own crossing.
- **Fitting**: Schneider's least-squares cubic fit with Newton
  reparameterisation, split at the worst point, corners kept — the pencil and
  Simplify.
- **Offsets**: a curve offset by Tiller–Hanson, subdivided until within
  tolerance — Outline stroke and Offset.
- **Booleans** (DR2): every curve split where it meets another, coincident
  runs merged, each piece kept or dropped by the winding of the two operands
  on either side of it, and the kept pieces traced into closed sub-paths:
  union, intersection, difference, exclusion and divide, curves kept as curves.
- **Bounds**: every algorithm is total over finite input and bounded in the
  work it does: recursion depth, intersections per pair and output segments
  are fixed bounds, and a result past one is a typed refusal.

## The drawing (`lib/drawing`, DR4)

- **Objects**: a path (sub-paths of nodes, each a point with optional incoming
  and outgoing handles and a corner, smooth or symmetric constraint), a
  rectangle with corner radii, an ellipse with an optional arc as an open arc,
  a pie or a chord, a polygon or star (points, inner radius, rounding, turn),
  text, and groups — a group may be clipped by a path of its own. Every object
  carries an identity, an optional name, visibility, a lock, an opacity and a
  transform.
- **Style**: fill and stroke paints — none, a colour with alpha, or a linear
  or radial gradient (stops, spread, focal point, in the object's own space) —
  the fill rule, stroke width, joins with their miter limit, caps, a dash
  pattern and its offset, arrowheads at either end, and which of fill and
  stroke paints first.
- **Text**: lines of one style — family, weight, italic, size, letter spacing,
  line height, alignment — from a baseline origin. Glyphs are geometry
  fetched from the font service, never part of the drawing.
- **Layers** are the drawing's top-level groups, each named or not, shown or
  hidden, locked or not, with an opacity. **The page**: its size held in its
  own unit as it was stated (so A4 is exactly 210 × 297 mm), and an optional
  background colour; the named paper and screen sizes are one table. Guides,
  the grid, the drawing's one-line title and description, and its swatches
  (`draw:swatches`, at most 1024) belong to the drawing.
- **Persistence**: every node is immutable once shared, behind `Arc`; an edit
  replaces the nodes on the way to the root and shares the rest, so a save or
  a render takes the drawing whole for the price of a pointer and the history
  holds only what left it. A container's children are held in chunks, each
  carrying the bounds of what it holds, so an edit copies only the chunks it
  lands in — one edit or many at once, insertions, removals and replacements
  alike — and a render or a hit test skips chunks it cannot meet. A chunk a
  removal leaves short joins its neighbour, so the list never decays into
  slivers.
- **Identity**: every object keeps its identity across edits, and many are
  found in one walk (`Drawing::find_each`), so an editor keeps its selection
  on the same objects when the drawing beneath it is taken back.
- **Bounds by construction**: a node's conservative bounds are computed as it
  is made; text's from an estimate no character is set wider than, a tab as
  its spaces, letter spacing of either sign counted, and no glyph inks past —
  one that would is drawn scaled down about its pen. Exact bounds are asked of the node with the text measured, and
  say whether any text in them waited on a glyph not yet known.
- **Fixed bounds**: objects, nesting, nodes per path, dash entries, gradient
  stops and text length are containment bounds, as `lib/svg`'s are, sized for
  documents rather than for assets.

## Files (DR5–DR8, DR17)

- **SVG** is the native form: clean SVG 1.1 with Draw's namespace carrying
  what SVG cannot say — layers, live shapes, node constraints, arrowheads,
  guides, the grid and the page's background — and every number spelled
  exactly, so a saved drawing reopens as itself. Read through `lib/svg`'s own grammar and cascade: the
  path, number, length, colour, transform and `viewBox` grammars, the
  stylesheet and the presentation-property cascade, `use` and `symbol`
  expanded, gradients with their `href` chains and bounding-box units, clip
  paths as clip groups, `<text>` and its `<tspan>` lines as text, `<image>`
  as a picture. What the drawing cannot hold — filters, masks, patterns,
  markers other than arrowheads, pictures it cannot read, objects' titles and
  metadata — is listed as the document is read (`Unkept`), and a document
  that lost anything opens read-only, as Paint's does. A reference cycle is malformed, and reading
  descends no deeper than a drawing may hold.
- **RISC OS Draw files** (`&AFF`, named `,aff` or by the filetype a volume
  records — `.aff` is Hunspell's): read into one layer — paths with their
  gaps as the Draw module draws them, text and transformed text, groups, the
  object a tagged object wraps, the options' paper and grid — in draw units
  of 1/640 point and RISC OS's Latin-1, the font table and options found
  first wherever they stand; RISC OS font names mapped to and from the
  installed families by their generic kind (Trinity serif, Homerton
  sans-serif, Corpus monospace); caps the two ends do not share, and triangle
  caps, as arrowheads. Written on the smallest A-series paper that holds the
  page, every object drawn as the drawing draws it: a stroke Draw cannot
  stroke so (stretched, or cut otherwise by Draw's fixed mitre limit) as its
  area, an arrowhead its caps cannot draw as a shape, what the format cannot
  hold reported as `Dropped` and named in the save's or copy's message.
- **Save** writes the format the file's name, or the filetype its volume
  records for the document's own file, says (`SaveFormat::of`): SVG, a Draw
  file, or PDF, which opens again as one layer whose text is outlines, and
  the save says so (`SaveFormat::keeps_editing`). Save As
  offers the three, opening on the document's own; any other name is refused
  before the file is touched. A save is written by the copy encoders
  (`copy::copy`), so a save and a copy in one format are the same bytes, and
  every note is a sentence the message reads on with.
- **Save a copy** (the document host's `Request::SaveCopy`) writes a copy and
  leaves the document's own file, name and changes as they are: PDF (paths,
  gradients as shadings, opacity, clips, text as outlines, streams
  compressed), EPS (the same walk through a PostScript sink — `vector` — its
  transparency written opaque and said to be), PNG, JPEG or TIFF (the page
  rendered by the one renderer at the drawing's copy resolution, held in the
  drawing as `draw:resolution` and set in the Page tab, lowered where it must
  be to fit 8192 pixels, the file recording the resolution made at), SVG
  or a Draw file; its format is the one media registry's reading of the
  name, or of the filetype the volume records.
- **Reading** runs in the parser sandbox (`lib/sandbox::drawedit`, DR10): the
  bytes go to a capability-empty worker, which reads them as a Draw file or as
  SVG by their content, the drawing comes back in a checked
  wire form a batch at a time, and every value is validated as the drawing is
  rebuilt from it.

## Pictures (DR22)

- **The model**: a picture is a node of its own — pixels premultiplied over
  a rectangle of its own space, held as a `tairix_raster::Pyramid` of every
  halving and shared behind `Arc`, so a copy, a duplicate or a step of
  history shares them. It may keep the bytes it was read from (`Source`) with
  only the facts a writer needs of them (`Encoding`: PNG; JPEG with its
  stored raster, components, progressive or not, EXIF orientation and
  density; a sprite with its eigen factors), which no writer parses. It has
  no style: it clips, joins a boolean operation and is outlined by its
  rectangle.
- **Reading**: SVG `<image>` from a data URL, known by content, laid as
  `preserveAspectRatio` lays it and cut by a clip where it slices; RISC OS
  sprite, transformed sprite and JPEG objects, the JPEG's raster as RISC OS
  draws it. One document's pictures are decoded once each, by their bytes,
  and hold at most `MAX_TOTAL_IMAGE_PIXELS` together; a picture that will not
  decode is `Lost::Images`.
- **Writing**: SVG embeds the bytes read or a PNG of the pixels, once per
  picture: a picture several objects show — the same pixels, or pixels
  written as the same bytes — is a `<symbol>` in `<defs>` marked
  `draw:picture`, each object a `<use>` of it over its own rectangle, which
  Draw reads back as that picture object and any viewer draws as SVG does;
  PDF an image object of each, a JPEG passed through `DCTDecode`
  and turned by its matrix, other pixels deflated with a soft mask where
  translucent and clear pixels bled from their neighbours; EPS the same
  through ASCII85, a baseline JPEG passed on, a masked image where binary and
  flattened where partly clear; a Draw file a baseline JPEG object, a sprite
  it was read from, or a 32-bit sprite. Clip paths are written as the shapes
  they hold, a nested clip as a chained clip path per member, so every viewer
  clips as Draw does.
- **Rendering**: `Node::Image` draws from the level its narrowest scale
  calls for through `Surface::blit_framed`, covering exactly its rectangle.
- **Placing**: *Place picture…* asks the picker for an import
  (`Request::Import`, `PickPurpose::Import`), delegated read-only whatever
  Draw may edit; the sandbox decodes any still picture by content
  (`decode_placed`, the drawedit place operation) and the load worker builds
  its levels. The sandbox's picture wire holds each read's pictures to a
  drawing's pixels and the bytes of the document handed over. A picture
  dragged from the file manager or off the desktop and let go over the canvas
  is placed the same way, centred where it was let go: the window answers the session's file
  offer (`DocumentView::places`) for a type the image library reads whole or a
  sprite file, over the canvas alone, and the file arrives read-only through
  the document host's `import_chosen` with the drop point
  (`plans/NEW-TASKBAR.md` T21). A drop point the canvas no longer holds places
  it as the menu does.
- **The combined bound**: every edit that brings pictures in (`edit::insert`,
  `edit::append`) holds the drawing to `pictures_fit` — pixels counted once
  per picture, as every reader counts them — answered from the node
  summary's per-object count unless the drawing repeats pictures past it, so
  no reader refuses what Draw writes for its pictures' pixels. A file written
  longer than this computer's reading worker may be handed is still written,
  and said to be.

## Lettering (DR23, DR24)

- **Runs**: a text sets spans of its content otherwise than its own style.
  A run is a byte range on character boundaries and what it overrides —
  family, weight, italic, size, letter spacing, a baseline shift in ems of
  its own size, and a fill and a stroke that are each a colour or none (a
  gradient paints a whole object). Runs are ordered, disjoint and non-empty,
  each overrides something, and neighbours that set the same are one run; a
  text holds at most `MAX_TEXT_RUNS` of them naming at most
  `MAX_TEXT_FAMILIES` families. Lines keep the text's own baseline grid — its
  size and line height — so a larger run does not move them. Each glyph of a
  setting knows its run, so a renderer, a writer and Object to path paint
  consecutive glyphs of one paint together, in order; estimated bounds take
  every character at its own run's size, spacing and shift.
- **Editing**: while typing, a selection — shift with the arrow keys, Home
  and End, or a drag across the text — is a byte range. A style change made
  with a selection applies to it as runs (family, size, bold, italic, letter
  spacing, baseline shift, a fill or stroke colour); without one it applies
  to the whole object and clears the same from its runs, as a paint given a
  whole object — a gradient among them — clears its runs' paint. Typed text
  takes the style of the character before it, a selection typed over the
  style of its first character (`Text::run_for`); the inspector shows the
  style where the selection starts. A press puts the caret at the nearest
  character boundary (`Text::offset_at`), and the caret and the selection's
  mark stand across the line, turned with a path the text follows.
- **On a path**: a text's baseline is lines from an origin or a path — an
  editable path in the text's own space and how far along it the alignment
  point stands. On a path a text is one line: each glyph's advance midpoint
  lies on the path at its distance along it, turned to the path's direction,
  and a glyph whose midpoint falls off either end is not drawn. *Set text on
  path* takes a selected text and path, the path joining the text; *Release
  from path* sets the text in lines from the path's start and gives the path
  back as an object.
- **Files**: SVG writes runs as `<tspan>`s within each line, and a path
  text as a `<textPath>` over a path in `<defs>`; both read back, with
  `startOffset` lengths and percentages and `baseline-shift`. A RISC OS Draw
  file takes one text object for each run of a line and one transformed text
  object for each glyph of a path text; PDF and EPS paint each run's glyphs
  in its own paint. `lib/svg` draws `baseline-shift` and `<textPath>`
  (`plans/SVG.md` S50, S26), so every reader of SVG shows what Draw writes.

## The window (DR11, DR14–DR16, DR21)

- The tool box down the left, rulers along the canvas's top and left and
  scrollbars along its right and foot, the inspector down the right — tabs
  over Style, Layers, Object, Shape and Page, a tab taller than the panel
  scrolling — and the status band along the bottom; nothing across the top,
  as there is no menu bar. The band and the rulers are `lib/controls`' own
  (`StatusBand`, `Ruler`): the band says what the last command did or, while
  it says nothing, how the tool in hand is used (`Tool::hint`), then the
  pointer and selection readings, each in a slot measured for values to
  ±99 999.99 units, beyond which a reading elides, and the zoom, which opens
  the zoom menu.
- The overlay — the selection's frame and handles, a drag's preview, the
  marquee, nodes, the caret — is stroked in the raster's sub-pixel units
  through one pen (`paint::Pen`), a hairline one logical pixel wide laid on
  whole pixels; translucent marks (the page's shadow, the grid, guides, a text
  selection) are blended over the canvas, never written as holes in the
  window. A
  secondary press opens the one window menu: File, Page, Edit, Object, Path,
  Layer, View and Tools.
- A command's row — picture, label, chord, when it may be chosen — is stated
  once (`Command::spec`), and the menu and the keyboard both read it.
- **Gradients in the inspector** (DR14) are fitted to each object's own box
  (`gradient::fitted`): linear across it at an angle so its ends meet the
  farthest corners, radial from its centre to its corners. An edit to the
  stops, their colours or the spread gives every selected object the stops
  shown and keeps where each one's gradient lies; a new form or angle fits
  it again. A style in hand holding a gradient is fitted to each object drawn
  in it, and to new text when its typing lands. The gradient tool (DR25)
  lays one along a drag instead, mapped into each selected object's own
  space through its whole transform — radial about the drag's start with
  Alt or where it is radial already — and the eyedropper fits a gradient it
  takes to each object given it. The swatch grid is a fixed palette followed
  by the drawing's own swatches; Rotation turns one object to an angle read
  from its transform, several together by it.
- The canvas shows the page on the pasteboard at a continuous zoom from 1% to
  25600%, anchored on the pointer, by the wheel, pinch, the zoom tool and the
  view menu; the outline view draws every path as a hairline. A scrollbar's
  span is derived from the viewport whenever it is wanted. The view is never
  taken further from the drawing's origin than twice the largest coordinate a
  drawing holds, so tile indices, rulers and the grid stay exact and bounded
  whatever a document's transforms make of its bounds.
- What is under way — typing, a field of the inspector, a colour being
  chosen — lands before anything that acts on the drawing: Undo and Redo, a
  command, a paste, a save or a copy. A change made in the inspector while
  text is being typed joins the typing's one step. Undo and Redo keep the
  selection on the same objects, found by identity where they went; an object
  in a hidden or locked layer leaves the selection, and nothing new goes into
  such a layer.

## Settings (DR18)

- *Settings…* on the icon bar's menu opens Draw's settings window, or raises
  the one open (`settings::DrawSettings`): the document host's shared settings
  window over Draw's closed registry (`preferences::Preferences`), kept in
  Draw's private app-data store by the shared engine and read once at start.
- **General**: the tool a new window starts with, and whether it shows the
  grid, snaps and draws outlines only. **New drawing**: the paper, upright or
  on its side, the unit, and the grid's spacing — typed in that unit, kept in
  drawing pixels — and subdivisions. **Style**: the fill and the stroke new
  objects are drawn in, each none or a colour, and the stroke's width.
  **Editing**: the nudge (ten times with Shift), the duplicate's offset, and
  the pick distance a handle, a node and a guide are picked and snapped within.
- The tool, view and style are a new window's (`View::begin`) and the page and
  grid a new drawing's (`document::blank`); the editing settings apply to every
  window as they are made (`View::adopt`). Draw's own defaults are
  `Preferences::default`, the one definition a window starts from.
- Lengths are kept in hundredths of a drawing pixel and a paint as `none` or
  its hex digits, a paper as its name in lower-case words — a settings store
  spells no fraction and starts a comment at `#`.
- A change is written off the loop by the settings writer as its interaction
  settles and the store's answer adopted wherever the user has not edited
  since; a refused write is said in the window and on `stderr`, and the last
  is seen out before the process ends.

## Rendering and the loop (DR9, DR11, DR19)

- The drawing is rendered in tiles of the device grid at the current zoom by
  a render worker, a job's tiles spread across the cores — each core taking
  the next tile as it finishes one, with the glyphs it has made shared
  across them — each job holding the drawing's root, so the loop renders
  nothing itself. A window has one job out at a time, whatever changes while
  it is out. Tiles hold what is drawn and not the page's ground, which the
  canvas lays beneath them — over white paper, so a translucent ground
  leaves no hole — so a change to the page alone renders no tile again. A
  tile not yet rendered at a new zoom is stood in for by the tiles of the
  last view shown whole — what there was of the view just left where none
  is kept — held as the backdrop and drawn through `lib/raster`'s
  transformed blit as one `Mosaic`, placed as the tile will be; the backdrop
  goes once the view's own tiles are all held, on a change of mode, and at
  severe pressure. A tile a render could not make is not asked again until the
  drawing, the mode or the memory pressure changes; a job the renderer could
  not take is asked for again with the next. A window holds the tiles in
  view and twice as many again, once at moderate pressure and none beyond
  the view at severe.
- An edit invalidates only the tiles its old and new bounds meet. A drag
  previews the moving objects over the tiles each frame — at full fidelity
  under a budget of points, by their bounds past it, a path past what is
  left never built — and lands once, when it ends; a node drag previews the
  path and its handles where the pointer has them.
- Glyph outlines are fetched by the workers that draw and trace text, a
  text's characters in one run (`Glyphs::prepare`), into the font client's
  bounded outline cache, which answers a run it holds whole without asking
  the service; the loop reads only what has arrived
  (`tairix_font::cached_outline`), so text is picked, framed and given its
  caret by the glyphs it is drawn with once they have landed, and by its
  estimated box until then. Text whose glyphs could not all be had is never
  traced into paths without them. The window's own labels are drawn without
  waiting on the font service at all: the document host fetches what was
  drawn provisionally on a thread of its own and lays the windows out again
  when it lands.
- The host's queue writes saves and exports and runs what grows with the
  drawing: boolean operations and division, outlines, outsets and insets and
  simplification over a large selection. Divide cuts the selection, traced in
  the drawing's space, into the pieces its overlaps make, each painted as the
  uppermost object over it was. Outset and inset grow or shrink an outline by
  a distance in the drawing's units through `lib/bezier`'s offsets, and
  simplify flattens it finely and fits it again (`fit::fitted`, the pencil's
  own fit) within a tolerance, corners kept; both distances live in the Shape
  tab. Clip makes the uppermost selected object the clip of a group holding
  the rest; releasing it puts the clip back on top of what it held. Reverse
  runs a path the other way, so its arrowheads and dashes follow it.

## The sandbox seam (DR28, DR29)

- The editor's side of every sandbox exchange allocates fallibly: a request is
  built in a refusable writer and sent whole or not at all, a reply is
  received into room reserved fallibly and decoded by borrowing and copying
  fallibly, and the launch — the worker's path, its spawn block, the machine's
  RAM read through `sysinfo` — allocates nothing it cannot refuse. A refusal
  of the editor's memory is said as want of memory and blames no worker. A
  worker allocates as it needs under its kernel ceiling; one that runs out
  ends with the runtime's out-of-memory exit, said as want of memory too.
  Every client is held to this by an allocation sweep.
- A document may be as long as the reading worker's ceiling holds beside its
  frames (`WorkerMemory::document_bytes`): a quarter of the machine less its
  frames for a document reader, never below 64 MiB; the fixed 64 MiB for a
  pooled artwork reader. The launcher states it (`document_room`), the editor
  refuses a longer one as too large before a byte is sent, and every claim a
  worker makes about what it read from a document — a picture's source, a
  kept sprite — is held to the length handed over.

## Invariants

- No filesystem capability; nothing of a document or a pasted drawing is
  parsed in the editor's address space.
- What a window shows is what an export renders: one renderer.
- A paint reads nothing and costs what the window shows, not what the drawing
  holds.
- Everything that grows with the drawing — its shared values and working
  buffers, and the geometry its rendering and writing run through — is
  allocated fallibly; a refusal is stated in the window, never an abort.
- A drag, a slider or a typed value lands as one step when it settles.
- A document that lost content on reading is never written back over its own
  file.
- A drawing's pictures never hold more pixels together than a reader
  accepts, and a file written larger than Draw opens is said to be.
- Closing or quitting with unsaved changes asks first.
