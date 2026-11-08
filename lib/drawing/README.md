# tairix-drawing

Stability: **experimental**.

The editable vector drawing the drawing editor works on (`plans/DRAW.md`),
and the formats it is read from and written to.

- `Drawing` — a `Page` (its size held in its own unit, as stated; `PAPER` is
  the named sizes a new page is offered in), its layers bottom first, its
  guides and its grid.
- `Node` — a shape, text, picture or group, each with a stable `ObjectId`,
  an optional name, visibility, a lock, an opacity and a transform; a group
  may be clipped by a node of its own. Nodes are immutable once shared: an edit
  replaces the nodes on the way to the root and shares the rest.
- `Children` — a container's nodes in chunks, each carrying the bounds,
  object and path-node counts, picture pixels and depth of what it holds, so
  an edit copies one chunk and a render or a hit test skips the chunks it
  cannot meet.
- `Shape` — a path of nodes with handles and corner, smooth or symmetric
  joins; a rectangle with corner radii; an ellipse, open arc, pie or chord; a
  polygon or star. `Style` — fill and stroke paints (none, a colour, a linear
  or radial `Gradient`), the fill rule, joins, caps, dashes, arrowheads and
  paint order. `Text` — lines from an origin or one line along a path, its
  runs setting spans of it in other faces, sizes, raises and colours. `Image` —
  a raster picture over a rectangle, its pixels a shared
  `tairix_raster::Pyramid`, kept with the bytes it was read from where a
  writer can pass them on (`Source`, its `Encoding` stating what they hold).
  A drawing's pictures together hold at most `MAX_TOTAL_IMAGE_PIXELS`, each
  counted once (`pictures_fit`), as every reader counts them, and
  `decode_placed` decodes a picture file to place, in the sandbox.
- `svg::read` / `svg::write` — SVG, Draw's native format, read through
  `lib/svg`'s own grammar and cascade, with Draw's namespace for what SVG
  cannot say, pictures as data URLs and clips written as the shapes a clip
  path holds; what a document holds that a drawing cannot is listed as
  `Unkept`, never dropped silently. Every number is written exactly, so a
  saved drawing reopens as itself.
- `pdf::pdf` — the drawing as a one-page PDF, curves, gradients, alpha,
  transparency groups, clips and pictures kept, a JPEG passed on as it came.
  `pdf::read` reads one page of a PDF through `lib/pdf` into a drawing of one
  layer, the page and password a `pdf::Request` names, and `pdf::pages` lists
  a document's pages for a chooser.
- `eps::eps` — the same as one page of Level 3 EPS, transparency written
  opaque and said to be, a baseline JPEG passed on as it came.
- `riscos::read` / `riscos::write` — RISC OS Draw files (`&AFF`), sprites
  and JPEGs included: read into one layer, what the drawing cannot keep
  listed as `Unkept`; written with every object on the page drawn as the
  drawing draws it, and what the format cannot hold reported as `Dropped`.
- `read` — whichever of the three a document is, told apart by its content.
- `render::artwork` — any square of device pixels at any zoom as
  `lib/raster` artwork, painted or as outlines, its cost following what the
  square shows rather than what the drawing holds.

Every value is checked as it enters the tree: the bounds in `limits` are
fixed containment bounds, and a value outside its range or past a bound is a
`DrawingError`, never carried. Everything that grows with a drawing — the
nodes, strings and lists it shares as well as its working buffers — is
allocated fallibly, so a refusing heap is `DrawingError::OutOfMemory`, never
an abort (`tests/allocation_failure.rs`).

Readers: the drawing editor and its sandboxed decode. See
`docs/src/lib/drawing.md`.
