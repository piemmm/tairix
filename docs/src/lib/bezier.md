# `tairix-bezier` — plane geometry for vector drawing

`lib/bezier` is the geometry every vector consumer shares: the SVG decoder,
the rasteriser's placement of artwork, the icon symbols, and the drawing
editor (`plans/DRAW.md`). One `Point`, one `Affine` (SVG's
`matrix(a b c d e f)`, which `lib/raster` re-exports), one `FillRule`, and the
line, quadratic and cubic segments a `Path`'s sub-paths are built from. The y
axis points down, so a positive cross product, a positive winding and a
positive signed area are all clockwise as drawn.

## Segments and paths

- `CubicBez` evaluates, splits by de Casteljau's construction, takes a
  sub-segment (backwards where asked), finds its tight bounds from its
  derivative's roots, its nearest point to another (samples bracketing each
  local minimum, refined by Newton's method), its length by adaptive
  Gauss–Legendre quadrature, and its signed area in closed form over the
  Bernstein basis. `ArcTable` measures a curve once so many points along it
  are each a lookup and a few Newton steps. `PathMeasure` measures a whole
  path so — a cubic's table beside each segment's length — and answers the
  point a distance along it and the unit direction it runs there, what dashes
  are cut at and text is set along.
- `QuadBez` elevates exactly to a cubic; `EllipseArc` converts SVG's endpoint
  arc to its centre form, growing radii too small to reach, and draws itself
  as cubics of at most a quarter turn.
- A `Path`'s winding number at a point counts exact crossings of the ray
  towards positive x over half-open spans of y, so a ray through a vertex
  counts it once and one grazing an extremum counts nothing.
- A copy (`try_clone`), a path of one sub-path (`Path::single`), a
  transform, a reversal and a flattened polyline allocate fallibly and answer
  the heap's refusal as `TryReserveError`; `Clone` copies infallibly, for
  code where an abort is contained.

## Flattening

The flattener is error bounded: a curve's segment count comes from its second
derivative and the tolerance, the chord over a step `h` departing from the
curve by at most `h²·max|C''|/8`. `MAX_SEGMENTS` bounds one curve; a caller
drawing curves far larger than an asset asks `cubic_segments_wanted` and
splits first. The arithmetic is exactly what the SVG decoder always used, so
every decoded asset keeps its bits.

## Intersections, fitting and offsets

- `intersect::meet` answers two segments' crossings, each a parameter on
  both, or the coincident run they share. Lines meet exactly; a line meets a
  cubic at the roots of the cubic's distance from it; two cubics meet where
  subdividing both leaves overlapping hulls too flat to tell from their
  chords, refined by Newton's method in both parameters, under a bound on
  the pairs examined.
- `fit::fit` is Schneider's least-squares cubic fitting with Newton
  reparameterisation, split at the worst point with a shared tangent and cut
  at corners, on an explicit stack.
- `offset::offset_cubic` approximates an offset cubic (ends moved along their
  normals, handles scaled by `1 − d·κ`), halving wherever it strays, and
  stops at the first piece its consumer refuses.
  `offset::outline` is a stroke's outline — both sides joined (miter within
  the limit, round, bevel) and capped (butt, round, square) — simplified;
  `offset::dashed` cuts a path into its dashes; `offset::grown` grows or
  shrinks a fill.

## Boolean operations

`boolean::boolean` takes both operands as fills under their own rules:
every segment is cut at its turning points so no piece crosses itself, every
pair of pieces whose bounds touch is intersected, the pieces cut at every
crossing and coincident run become the overlay's edges with their ends merged
within the tolerance, each edge is kept where the operation's result differs
across it — each operand's winding found just left of the edge by a ray, and
just right by adding the edge's own runs — and the kept edges are traced into
contours by the sharpest right turn, consecutive edges of one original curve
joined back into one. Every contour of a result is closed. A cut at either
zero, or before a piece's start, is that start's own, so the cuts along a
piece are in order as they are recorded and need no second sort. `divide`,
`simplified` and `components` build on it; `components` gives each hole the
smallest outer contour around it, asking only those whose bounds hold it.

## Bounds

Every algorithm is total over finite input. `MAX_FIT_POINTS`,
`MAX_BOOLEAN_SEGMENTS`, `MAX_BOOLEAN_SPLITS`, `MAX_DASHES`,
`intersect::MAX_CROSSINGS` and the subdivision depths are containment bounds:
work past one is `FitError::TooComplex` or `BooleanError::TooComplex`, never a
hang. Allocation that grows with the input is fallible and refused as
`OutOfMemory`, never an abort: `tests/allocation_failure.rs` refuses each
request in turn that the booleans, outlines, dashes, growing, fitting and the
path operations make. The fuzz harness (`tests/fuzz_bezier.rs`) drives every
algorithm over paths at scales from a millionth to a hundred million, with
coincident points, edges and outlines.
