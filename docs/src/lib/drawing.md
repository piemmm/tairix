# `tairix-drawing` — the editable vector drawing

`lib/drawing` is what the drawing editor (`plans/DRAW.md`) holds open: a
drawing as a persistent tree of shapes, text, pictures and groups, and the
formats it is read from and written to. Geometry is `lib/bezier`'s; the y axis points
down and the drawing's own unit is the pixel at 96 to the inch, SVG's user
unit.

## The tree

- A `Drawing` is a `Page`, its layers bottom first, its guides, its grid,
  its `Info` — a title of one line and a description, each trimmed — and the
  swatches kept with it, colours in the order they were kept, at most
  `MAX_SWATCHES` (1024).
  The page's size is held in its own `Unit`, as it was stated, so a page of
  210 by 297 millimetres is that exactly and `Page::size` gives its pixels.
  `PAPER` is the named sizes a new page is offered in, the default first;
  `Page::turned` stands a page on its side or upright, and
  `Page::measured_in` states it in another unit, its size unchanged.
- A layer is a group at the top of the tree. A `Node` is a shape, text, a
  picture or a group; each carries an `ObjectId` issued by the drawing's `Ids`, an
  optional name, visibility, a lock, an opacity and a transform. A group may
  be clipped by a node of its own, in the group's space.
- Nodes are immutable once shared, behind `Arc`. An edit makes new nodes on
  the way to the root (`Drawing::with_node_at`) and shares everything else,
  so a save or a render takes the drawing whole for the price of a pointer,
  and an undo history holds only what left it.
- A group's `Children` are held in chunks of up to 128, each carrying the
  bounds, object and path-node counts, picture pixels and depth of what it
  holds: an edit
  copies only the chunks it lands in, one edit or many at once
  (`with_inserted`, `appended`, `without`, `with_replaced`), and a chunk a
  removal leaves short of 32 joins its neighbour. `Children::meeting` skips
  every chunk a rectangle cannot meet.
- `Drawing::find_each` finds many objects by identity in one walk.
- A node's bounds are computed as it is made: its geometry inflated by what
  its stroke can reach (half the width, miters and arrowheads included), and
  text's from an estimate allowing each character the widest advance it is
  ever set with — a tab four spaces of it — letter spacing of either sign,
  and the ink a glyph may reach about its advance; a glyph inking further is
  drawn scaled down about its pen to fit, so no setting outgrows them.

## Values

- `Shape::Path` holds contours of `PathNode`s: a point, its optional incoming
  and outgoing handles, and a corner, smooth or symmetric join. A zero-length
  handle is no handle. `Shape::Rect` has independent corner radii;
  `Shape::Ellipse` an optional `ArcPart` drawing it as an open arc, a pie or
  a chord; `Shape::Star` points, an inner radius, a turn and rounding, or a
  polygon. Every live shape traces to a `lib/bezier` path.
- `Style`: fill and stroke `Paint` (none, a colour with alpha, or a
  `Gradient` — linear or radial with a focal point, its stops in order, a
  spread and a transform into the object's space), the fill rule, the stroke's
  width, join and miter limit, cap, dashes and their offset, an `Arrowhead` at
  either end (drawn only where the stroke paints), and which of fill and
  stroke paints first.
- `Text`: its content and its own style — family, size, weight, italic,
  letter spacing and alignment — set on a `Baseline`: lines from an origin at
  a line height, or one line along a `TextPath`, an editable path in the
  text's own space and how far along it the alignment point stands. Its
  `Run`s are spans of the content set otherwise: ordered, disjoint, non-empty
  byte ranges on character boundaries, each a `RunStyle` overriding family,
  size, weight, posture, letter spacing, a baseline shift in ems of its own
  size, and a fill and a stroke that are each a colour or none — never a
  gradient, which paints a whole object — no two touching neighbours setting
  the same, at most `MAX_TEXT_RUNS` naming at most `MAX_TEXT_FAMILIES`
  families. Lines keep the text's own size and line height whatever a run
  sets. `Text::replaced` and `Text::restyled` edit the content and the runs
  together, keeping that form.
- `Image`: a raster picture laid over a rectangle of its own space, its
  pixels held premultiplied as a `tairix_raster::Pyramid` — the picture and
  each halving of it — shared behind `Arc`, so a render, a copy or a step of
  history takes it for a pointer. Beside them it may keep the bytes it was
  read from (`Source`) with what they state (`Encoding`): a PNG; a JPEG with
  its stored raster, components, whether it is progressive, its EXIF
  orientation and its density (`tairix_image::JpegFrame`); or a RISC OS
  sprite with its eigen factors. The bytes are never parsed outside the
  reader: a writer passes them on, and states of them only what the reader
  recorded. A picture is at most `MAX_IMAGE_SIDE` (65 536) pixels on a side
  and what a surface holds, its source at most `MAX_IMAGE_SOURCE_BYTES`, and
  a source whose facts its pixels contradict is refused. A drawing's
  pictures hold at most `MAX_TOTAL_IMAGE_PIXELS` (four surfaces) together,
  each counted once however many objects show it, as every reader counts
  them; `pictures_fit` is what an edit bringing pictures in holds itself to,
  so no reader refuses a drawing written for its pictures' pixels — the size
  of the file written is bounded by nothing in the drawing. A node's summary
  counts a picture for every object showing it, so that answer is a
  comparison unless the drawing shows its pictures more often than the
  bound, when they are listed and each counted once. Every picture is
  decoded under the same limits, so which a document loses never depends on
  the order they come in.

Every value is checked as it enters: a coordinate past `MAX_COORDINATE`
(10⁹ pixels) or not finite, a scale past `MAX_SCALE`, a negative width,
stops out of order, an empty or over-long name, a tree past `MAX_DEPTH`
(128) or `MAX_OBJECTS` (2²⁰), a path past `MAX_PATH_NODES` — each is
refused with a `DrawingError`, never carried. These are containment bounds,
sized for documents.

## SVG

SVG is Draw's native format.

**Reading** (`svg::read`) goes through `lib/svg`'s own grammar and cascade —
its path commands, shapes, lengths, colours, transforms and `viewBox`, its
stylesheet at document bounds (`SheetLimits::DOCUMENT`), and its gradient
definitions with their `href` chains — over `lib/xml` held to a document's
bounds (`Limits::DOCUMENT`).

- The root's `width` and `height` give the page in the unit they are stated
  in; a document stating no size at all is laid out on A4. The root's first
  `<title>` and `<desc>` are the drawing's own; a title's spaces are
  collapsed onto one line. The `viewBox` is
  mapped onto the page as `preserveAspectRatio` says, and that map is placed
  in each layer's transform.
- Layers are the top-level groups Draw or Inkscape marks as layers; each run
  of other top-level content between them becomes a layer of its own, so
  their order is kept. A layer keeps the name its document gave it, or none.
- `use` and `symbol` are expanded, `switch` takes the first child whose
  conditions are met, nested
  `svg` and `symbol` viewports are clipped unless `overflow` shows what
  spills, gradients in bounding-box units are placed in the shape's own space
  and every reference to one gradient shares it, a `clipPath` becomes the
  clip of a group holding what it clips, and `<text>` with its `<tspan>`
  lines becomes text: a span set otherwise than the text a run of what it
  overrides — family, size, weight, posture, letter spacing, a
  `baseline-shift` in ems of its size, and a colour or none for its fill and
  stroke — and one over the whole content the text's own style; a
  `<textPath>` holding all its characters a text along its own `path` data or
  the path or basic shape it names, its start read from `startOffset`.
- `<image>` is a picture: a PNG or JPEG its link carries as a data URL —
  known by its content, the media type the URL states not believed — laid
  in the rectangle the element states as `preserveAspectRatio` lays it and,
  where it slices, cut to that rectangle by a clip; a side left unstated is
  the picture's own, or what its ratio makes of the side stated. The same
  link met again is decoded once and shared (`image::Pictures`), and a
  document's pictures hold at most `MAX_TOTAL_IMAGE_PIXELS` together. A
  `clipPath`'s shapes clip by their `clip-rule`, and a picture inside one is
  no part of it, as SVG has it.
- What a drawing cannot hold — filters, masks, patterns, markers other than
  Draw's own arrowheads, pictures linked from elsewhere or of another kind,
  text styling no run holds (a gradient in a span, a span's own stroke width,
  dashes, stretch or word spacing), positioned runs, a text path a text cannot
  hold — a second one, one beside text off it, one past its bound or broken,
  or one the glyphs bend along —
  non-scaling strokes, foreign content, animation, scripts, links, the
  unchosen branches of a `switch`, objects' titles and descriptions, and
  metadata — is listed in `Unkept` as the document is read, never dropped
  silently. Only the rectangle Draw paints a page's background with is
  skipped, and only where the page has the background it stands for.
- A document that is not UTF-8 is `NotText`; one whose root is not `svg` is
  `NotSvg`; a broken one, or one with a reference back to itself (a `use`
  inside what it uses, a clip clipped by itself — an error in SVG), is
  `Malformed`. Work is bounded: every element visited counts against
  `4 × MAX_OBJECTS` visits, `use` nests at most 16 deep, and reading descends
  no deeper than a drawing may hold — every `use` and clip expansion counted
  — so no document can recurse the reader past its stack. Past a bound is
  `TooComplex`.

**Writing** (`svg::write`) writes clean SVG 1.1 that any viewer draws, and
Draw's namespace (`urn:tairix:draw`, prefix `draw`) for what SVG cannot say:

| Attribute | On | Carries |
|---|---|---|
| `draw:layer`, `draw:name`, `draw:locked` | groups, objects | layers, names, locks (Inkscape's `groupmode` and `label` too) |
| `draw:background` | root | the page's background colour, its alpha too |
| `draw:grid` | root | grid spacing and subdivisions |
| `draw:resolution` | root | the resolution raster copies are made at, where it is not 96 |
| `<draw:guides>`, `<draw:guide axis position>` | root | guides |
| `<draw:swatches>`, `<draw:swatch colour>` | root | the drawing's swatches |
| `draw:shape="arc"` with `draw:cx`/`cy`/`rx`/`ry`/`start`/`end`/`arc` | `path` | an ellipse's open arc, pie or chord |
| `draw:shape="star"` with `draw:cx`/`cy`/`points`/`outer`/`inner`/`rotation`/`rounding`/`polygon` | `path` | a polygon or star |
| `draw:nodes` | `path` | each node's join: `c` corner, `s` smooth, `z` symmetric |
| `draw:start-arrow`, `draw:end-arrow` | strokes | an arrowhead's shape, width and length |
| `draw:line-height` | `text` | the line height of a text set in lines |
| `draw:role="background"`, `draw:marker` | the page fill, arrowhead markers | what the reader skips as its own |
| `draw:picture="true"` | `symbol` | a picture written once for every object that uses it |

A picture is an `<image>` of its rectangle, its bytes in a base64 data URL —
the PNG or JPEG it was read from as it came, any other its pixels as a PNG,
encoded once however many objects show it — `xmlns:xlink` declared only where
one is written. A picture more than one object shows — the same pixels, or
pixels written as the same bytes — is written once, a `<symbol>` in `<defs>`
over its pixels marked `draw:picture`, and each object a `<use>` of it over
its own rectangle; Draw reads such a use back as the picture object itself,
every one sharing one decode, where any other symbol is expanded as SVG
expands it. A stretched picture's rectangle is read as it is stated, never
scaled to the pixels and back. SVG's clip paths hold shapes and text but no groups, so a
clip is written as its members, each placed as its groups place it; a group
within a clip that is clipped itself calls for that clip by a clip path of
each member's own, written in the member's space and clipped in turn by the
next group out, so every viewer clips exactly as Draw does. A picture in a
clip clips by its rectangle and is written as one, and a member clips by its
fill rule, written as `clip-rule`. A text in lines is a `<tspan>` for each
line, placed on its baseline; a text along a path is a `<textPath>` naming
the path, written once in `<defs>` in the text's own space, its start as
`startOffset`. Within either, a run is a `<tspan>` of what it overrides, its
baseline shift in `em` so it reads back exactly and its paint's opacity said
wherever it differs from what the text's own leaves it to inherit.

Every number is spelled exactly — the shortest text that reads back as the
same value — so a saved drawing reopens as itself and nothing the writer
decides from a value can come out differently on the next save; an alpha is
the decimal of fewest places reading back as the same 8-bit value. Live
shapes are written as the path they trace, with their parameters beside it,
so every viewer draws them. Gradients are written in user space; arrowheads
as markers coloured as their stroke, one per shape, size, colour and end.
Definitions are named in the order the document first refers to them,
through an index sorted by key, so naming a reference is a search however
many there are. The document is measured and then written into a string
reserved once to its length, and a refused reservation is
`DrawingError::OutOfMemory`.

## Rendering

`render::artwork` turns a square of device pixels — a tile of the window, or
of an export — into `lib/raster` artwork on a design grid spanning it, so a
window and an export draw through the one renderer.

- Each node is flattened and stroked in its own space, to 0.2 of a device
  pixel, so a stroke scales and shears with its object as SVG draws it.
  Strokes go through `lib/svg`'s stroker; gradients through its
  `placed_gradient`, so a drawing paints exactly as the SVG decoder paints
  the same document.
- Only what can reach the square is worked on. A node whose bounds miss it is
  passed over (a group's children a chunk at a time); a sub-path whose bounds
  miss it is dropped; a curve whose control hull misses it is drawn as its
  chord, which changes no winding inside; a stroke is cut into the runs of
  segments that can reach it. What is left is clipped to the square and a
  two-pixel margin (Sutherland–Hodgman, which keeps winding inside a convex
  window) before it is placed, so no zoom pushes a vertex off the grid and
  the cost of a square follows what it shows, not what the drawing holds.
- Dashes are cut along the curves themselves (`lib/bezier`'s dasher), so
  every square and zoom cuts them alike; a dash of no length is drawn as its
  caps. Arrowheads are drawn after the stroke in `Stroke::head_colour`, as a
  viewer draws the markers the SVG writer writes, and only where the stroke
  paints.
- Fill and stroke paint in the style's order. A group is composited as one
  only where its opacity or clip changes the picture; a clip is an alpha mask
  of its shapes' fills. A shape painting one layer folds its opacity into the
  paint rather than isolating it.
- Text is set from `Glyphs` (`Text::set`), which is told first which
  characters are coming (`Glyphs::prepare`) so a source that must fetch them
  fetches them together; a glyph not yet known is left out and
  `Artwork::complete` says so, so the editor draws again when it lands.
  Each character is set in its run's face (`Text::face`), every glyph knowing
  the run it belongs to and the byte it starts at; a text along a path stands
  each glyph's advance midpoint on the path at its distance along it
  (`tairix_bezier::PathMeasure`), turned to its direction, and leaves out one
  whose midpoint falls off either end. `Text::pens` places the pen before each
  character of a line by the same advances, alignment and spacing — what
  `Text::caret` and `Text::offset_at` answer from — and `Setting::painted`
  parts the glyphs into consecutive runs painted alike, which the renderer, the
  vector writers and Object to path paint one after another, a text painted
  more than one way composited as one.
- A picture is drawn as `tairix_raster::Node::Image`, from the first level
  of its pyramid that sets no more than two of its pixels across a device
  pixel along its narrowest axis, through `Surface::blit_framed`: sampled
  within its own edge pixels and covering exactly its rectangle, so a
  picture shown far larger keeps a sharp edge and one shown far smaller is
  averaged rather than aliased. Its opacity is the blit's strength, with no
  isolated layer.
- `Mode::Outline` draws every outline as a hairline of one device pixel,
  setting paints, opacities and clips aside. In `Mode::Painted` the page's
  background is painted beneath everything where the view asks for it
  (`View::ground`): an export does, and a window that lays the ground
  beneath its tiles itself does not, so a change to the page alone draws no
  tile again.
- `Derived` is what rendering takes from a node alone — the path a shape
  traces and the dashes its stroke cuts — which the editor keeps between
  renders; `Afresh` derives it each time.

An object coming to more than `2²¹` points in one square is `TooComplex`.
Composited groups nest as deep as `lib/raster` draws them; deeper is left
undrawn, which `Surface::draw_artwork` reports.

## PDF and EPS

The vector formats share one walk of the drawing (`vector`), each painting
through a sink of its own. `pdf::pdf` writes a drawing as a one-page PDF 1.4
file the size of its page (pixels at three quarters of a point), painted by
one deflated content stream:

- every path as curves, filled under its own rule and stroked with its
  width, joins, caps, miter limit and dashes; arrowheads filled after the
  stroke;
- colours with their alpha through graphics states (`/ca`, `/CA`);
- gradients as axial or radial shadings, through a function stitched
  between each two stops that stand apart; a stop's alpha through a
  luminosity soft mask painting the same shading in grey; a reflected or
  repeated spread stitched period by period over the range the shape needs
  (at most 256 periods, past which the ramp's ends carry on); a gradient
  stroke as its outline filled;
- an opacity that changes the picture — a group, or a shape painting more
  than once over itself — as a transparency group form painted through it,
  and otherwise folded into the one paint;
- a clip as its shapes' exact union (`lib/bezier`'s booleans), a nested clip
  intersected with it;
- text as its glyphs' outlines, each run of them painted alike in its own
  paint, `Pdf::complete` saying whether every glyph was known;
- a picture as an image object painted over its rectangle, written once
  however many objects show it: a JPEG it was read from as it came, through
  `DCTDecode`, turned by the matrix it is drawn through as its EXIF
  orientation says, since a PDF viewer reads none; any other its pixels
  deflated a row at a time, grey where every pixel is, with a soft mask of
  its alpha where any pixel lets something through. A wholly clear pixel is
  written in the colour of the pixels it borders, weighted by how much each
  shows, so a viewer that smooths a picture blends its edge with its own
  colours rather than with black; its opacity is the fill alpha it is painted
  at;
- the page's background, and the drawing's title and description in the
  document information, as UTF-16 text strings.

`eps::eps` writes the same walk as one page of Encapsulated PostScript,
Level 3 and 7-bit clean, its lines within the conventions' 255 characters:
gradients through the same shading dictionaries painted by `shfill`, the
title a string literal with what is not printable ASCII in octal, pictures as
`image` in ASCII base-85, each line opening with a space so none reads as a
comment the conventions name: a baseline JPEG as it came through `DCTDecode`
(LanguageLevel 3 is not held to read a progressive one), any other its pixels
deflated — once however many objects show it, though PostScript draws it
where each is — and a masked image interleaved by sample where any pixel lets
something through. PostScript paints no transparency, so a translucent
colour, gradient, opacity or pixel is written opaque — a pixel shown where it
is more than half shown — and `Eps::flattened` says it was.

`pdf::read` reads one page of a PDF into a drawing of one layer through
`lib/pdf`, the page and the password asked for in a `pdf::Request` with the
bytes the document may decode to, and `pdf::pages` counts a document's pages
for a chooser and sizes, in points, as many of the first as it is asked
for. The page's crop box is the
drawing's page, in points. Each item of the page's display list becomes an
object in order: a fill and the stroke of the same path that follows it one
shape in their style — a stroke as thin as a device draws read as a pixel of
the page — a clip a group clipped to its path, a transparency group a group at
its opacity, a picture a picture, its pixels held once however often it is
shown and a JPEG kept with its bytes. An axial shading is a linear gradient
and a radial one a radial gradient, focused on its first circle's centre; one
that does not extend past an end paints nothing there, and a radial shading
whose first circle has a radius is read as concentric, recorded as a shading
no gradient holds where it is not. The document's title and subject become
the drawing's (`Info`). What the page holds that the drawing cannot — soft
masks, blend modes, tiling patterns, other shadings, unreadable pictures and
text, annotations, damage — is recorded as `Unkept`, as is what the document
holds beyond the page: its other pages (`Lost::OtherPages`), its bookmarks,
attachments and other parts (`Lost::DocumentParts`), and metadata besides
its title and subject. A PDF Draw wrote holds none of these, so it opens
writable again. A PDF needing a password, given a wrong one or asked for a
page it lacks is a `ReadError` of its own. `crate::read` reads a PDF by its
header (`pdf::recognised`), as it reads a Draw file by its signature, unless
the bytes begin as markup, whose text may hold a header's characters
anywhere.

A drawing also holds the resolution its raster copies are made at
(`Drawing::resolution`, `PIXELS_PER_INCH` until it is set, at most
`MAX_RESOLUTION`), which its own formats never use.

## RISC OS Draw files

`riscos` reads and writes the Draw file format of the RISC OS Programmer's
Reference Manual (volume 4, appendix E): draw units of 1/640 point, measured
up from the paper's foot, and RISC OS's Latin-1 for text and names
(`lib/fsmeta`'s `acorn::latin1_char`). The crate's own `read` reads a
document as a Draw file when it begins with the format's signature and as SVG otherwise,
so a file is read by what it holds, never by its name.

`riscos::read` reads a file into one layer, finding its font table and
options first, as later RISC OS lets them follow the objects that need
them:

- the options' A-series paper, turned or not, and the grid, its page shown
  in inches or millimetres as the grid is measured; a file without options
  is on A4;
- paths with their fill and outline colours, width, join, dashes and winding
  rule, stroked at the format's fixed mitre limit of ten. A cap both ends
  share is the stroke's cap; ends that differ, or a triangle cap, are butt
  ends bearing arrowheads — a disc for a round cap, a square for a square
  one, a triangle as its style sizes it. The thinnest outline a device can
  draw is read a pixel wide and `Lost::NonScalingStrokes` says so;
- a path's gaps as the Draw module draws them: a stroke breaks at a gap and
  leaves a sub-path closed by one open, while a fill runs along it, so a path
  both filled and stroked across a gap is the two, grouped;
- text and transformed text in the font table's face — Trinity as serif,
  Homerton as sans-serif, Corpus as monospace, any other family by its name
  before sans-serif, and the system font as monospace — a font wider or
  narrower than its height stretched across;
- groups with their names, and the object a tagged object wraps;
- sprites, filling the object's bounds; transformed sprites, their matrix
  mapping the space their pixels span — `2^eig` OS units each — from its
  foot; and JPEGs, their matrix mapping the width and height the object
  states from its foot, the raster drawn as stored whatever turn its EXIF
  asks for, as RISC OS draws it. Each is kept with its bytes — a sprite's own
  extent, a JPEG's data — and the same bytes met again are decoded once.

What a drawing cannot hold is listed rather than dropped: a picture whose
bytes will not decode (`Lost::Images`), text areas (`Lost::TextAreas`), a tagged object's
tag and data (`Lost::Annotations`), text set right to left
(`Lost::TextPositioning`) and an object of a type the format does not name
(`Lost::ForeignContent`). A file that breaks the format anywhere — an object
overrunning its file or not a whole number of words, a path drawing with no
sub-path open, an unknown component or join, a text with no height, groups
nested past the drawing's depth — is refused whole.

`riscos::write` writes the drawing on the smallest A-series paper that
holds its page, its top-left corner the paper's so a file read back holds
every object where it stood, after the font table and the options. Draw
holds no transform, gradient, translucency or clip, so every object is
written in the paper's coordinates, flat and opaque, and `Dropped` says
what that lost — gradients (painted in their average colour), transparency,
clipping, text outlines, letter spacing, characters outside RISC OS's
Latin-1 (written `?`), font families RISC OS has none of, and a page no
A-series paper is. What Draw can draw exactly it draws exactly:

- a stroke Draw strokes as the drawing does is Draw's own; one its object's
  transform stretches, or one whose corners Draw's fixed mitre limit would
  cut otherwise, is the area it paints, filled;
- an arrowhead Draw's caps draw exactly — a triangle, or a disc or square a
  stroke's width across, on the butt end of one open, solid sub-path — is
  Draw's cap; any other is a shape of its own;
- a stroke painted beneath its fill is two objects, grouped, as are a
  shape's parts and a text's lines;
- text is written a run of a line at a time, each where the drawing sets it,
  in RISC OS's own font of its family's kind and weight and in its run's
  colour, and a text along a path a glyph at a time, each a transformed text
  object turned as the path turns it;
- a picture read from a baseline JPEG is a JPEG object of those bytes, at
  the size its density states (ninety to the inch where it states none), its
  matrix turning the stored raster as its EXIF orientation says; one read
  from a sprite is that sprite; any other is its pixels as a 32-bit sprite at
  ninety to the inch, masked where any pixel lets something through — by a
  bit a pixel where each is wholly shown or hidden, by eight where any is
  partly. A sprite standing upright fills
  a sprite object's bounds; any other is a transformed sprite.

Hidden objects, layers, guides, swatches and what a drawing says of itself
are not part of a Draw file and are not written, as an export leaves them.

## Memory

Everything that grows with a drawing is allocated fallibly, so a heap that
refuses is `DrawingError::OutOfMemory` (`ReadError::OutOfMemory` reading),
never an abort. That holds for the values the tree shares as well as its
working buffers: a node is shared
through `try_arc`, and a name, a text, a dash list, the guides and the
swatches through `lib/collections`' `SharedStr` and `SharedSlice`, each one
block made only fallibly. The geometry beneath — `lib/bezier`'s path copies,
transforms, booleans, outlines and dashes, and `lib/svg`'s stroker — refuses
the same way, as do a picture's decode, its levels and every writer's
encoding of it. `tests/allocation_failure.rs` refuses each request in turn
that rendering, writing every format and reading a Draw file make, over a
drawing holding a translucent PNG, a JPEG and a turned picture, with glyphs
known and without, and holds each refusal to an answer that leaves nothing
behind.

## Fuzzing

`tests/fuzz_drawing.rs` mutates real documents — one of them pictures, sliced,
turned, clipping and used — generates hostile nests of elements and
attributes, and feeds noise: reading never panics, whatever is
read writes, reads back without loss and writes again to the same bytes, and
renders in a random square at a random zoom, painted and as outlines. A
second sweep does the same for Draw files — the documents written as Draw
files and mutated, files of generated objects of every type, and noise after
the signature — and holds that whatever is read writes a Draw file that
reads back with nothing lost.
