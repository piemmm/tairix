# tairix-drawing

Stability: **experimental**.

The editable vector drawing the drawing editor works on (`plans/DRAW.md`),
and the formats it is read from and written to.

- `Drawing` — a `Page` (its size held in its own unit, as stated; `PAPER` is
  the named sizes a new page is offered in), its layers bottom first, its
  guides and its grid.
- `Node` — a shape, text or group, each with a stable `ObjectId`, an optional
  name, visibility, a lock, an opacity and a transform; a group may be
  clipped by a node of its own. Nodes are immutable once shared: an edit
  replaces the nodes on the way to the root and shares the rest.
- `Children` — a container's nodes in chunks, each carrying the bounds,
  object and path-node counts and depth of what it holds, so an edit copies
  one chunk and a render or a hit test skips the chunks it cannot meet.
- `Shape` — a path of nodes with handles and corner, smooth or symmetric
  joins; a rectangle with corner radii; an ellipse, open arc, pie or chord; a
  polygon or star. `Style` — fill and stroke paints (none, a colour, a linear
  or radial `Gradient`), the fill rule, joins, caps, dashes, arrowheads and
  paint order. `Text` — lines of one style from a baseline origin.
- `svg::read` / `svg::write` — SVG, Draw's native format, read through
  `lib/svg`'s own grammar and cascade, with Draw's namespace for what SVG
  cannot say; what a document holds that a drawing cannot is listed as
  `Unkept`, never dropped silently. Every number is written exactly, so a
  saved drawing reopens as itself.
- `pdf::pdf` — the drawing as a one-page PDF, curves, gradients, alpha,
  transparency groups and clips kept.
- `render::artwork` — any square of device pixels at any zoom as
  `lib/raster` artwork, painted or as outlines, its cost following what the
  square shows rather than what the drawing holds.

Every value is checked as it enters the tree: the bounds in `limits` are
fixed containment bounds, and a value outside its range or past a bound is a
`DrawingError`, never carried. Allocation that grows with a drawing is
fallible.

Readers: the drawing editor and its sandboxed decode. See
`docs/src/lib/drawing.md`.
