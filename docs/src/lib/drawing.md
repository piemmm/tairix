# `tairix-drawing` — the editable vector drawing

`lib/drawing` is what the drawing editor (`plans/DRAW.md`) holds open: a
drawing as a persistent tree of shapes, text and groups, and the formats it
is read from and written to. Geometry is `lib/bezier`'s; the y axis points
down and the drawing's own unit is the pixel at 96 to the inch, SVG's user
unit.

## The tree

- A `Drawing` is a `Page`, its layers bottom first, its guides, its grid and
  its `Info` — a title of one line and a description, each trimmed.
  The page's size is held in its own `Unit`, as it was stated, so a page of
  210 by 297 millimetres is that exactly and `Page::size` gives its pixels.
  `PAPER` is the named sizes a new page is offered in, the default first.
- A layer is a group at the top of the tree. A `Node` is a shape, text or
  group; each carries an `ObjectId` issued by the drawing's `Ids`, an
  optional name, visibility, a lock, an opacity and a transform. A group may
  be clipped by a node of its own, in the group's space.
- Nodes are immutable once shared, behind `Arc`. An edit makes new nodes on
  the way to the root (`Drawing::with_node_at`) and shares everything else,
  so a save or a render takes the drawing whole for the price of a pointer,
  and an undo history holds only what left it.
- A group's `Children` are held in chunks of up to 128, each carrying the
  bounds, object and path-node counts and depth of what it holds: an edit
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
- `Text`: lines of one style — family, size, weight, italic, letter spacing,
  line height and alignment — from a baseline origin.

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
  lines becomes text.
- What a drawing cannot hold — filters, masks, patterns, markers other than
  Draw's own arrowheads, images, styled or positioned runs, text on a path,
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
| `draw:background` | root | the page's background colour |
| `draw:grid` | root | grid spacing and subdivisions |
| `<draw:guides>`, `<draw:guide axis position>` | root | guides |
| `draw:shape="arc"` with `draw:cx`/`cy`/`rx`/`ry`/`start`/`end`/`arc` | `path` | an ellipse's open arc, pie or chord |
| `draw:shape="star"` with `draw:cx`/`cy`/`points`/`outer`/`inner`/`rotation`/`rounding`/`polygon` | `path` | a polygon or star |
| `draw:nodes` | `path` | each node's join: `c` corner, `s` smooth, `z` symmetric |
| `draw:start-arrow`, `draw:end-arrow` | strokes | an arrowhead's shape, width and length |
| `draw:line-height` | `text` | the line height |
| `draw:role="background"`, `draw:marker` | the page fill, arrowhead markers | what the reader skips as its own |

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
  `Text::pen_at` places a caret where `set` would put the next character, by
  the same advances, alignment and spacing.
- `Mode::Outline` draws every outline as a hairline of one device pixel,
  setting paints, opacities and clips aside. The page's background is
  painted beneath everything.
- `Derived` is what rendering takes from a node alone — the path a shape
  traces and the dashes its stroke cuts — which the editor keeps between
  renders; `Afresh` derives it each time.

An object coming to more than `2²¹` points in one square is `TooComplex`.
Composited groups nest as deep as `lib/raster` draws them; deeper is left
undrawn, which `Surface::draw_artwork` reports.

## PDF

`pdf::pdf` writes a drawing as a one-page PDF 1.4 file the size of its page
(pixels at three quarters of a point), painted by one deflated content
stream:

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
- text as its glyphs' outlines, `Pdf::complete` saying whether every glyph
  was known;
- the page's background, and the drawing's title and description in the
  document information, as UTF-16 text strings.

## Fuzzing

`tests/fuzz_drawing.rs` mutates real documents, generates hostile nests of
elements and attributes, and feeds noise: reading never panics, whatever is
read writes, reads back without loss and writes again to the same bytes, and
renders in a random square at a random zoom, painted and as outlines.
