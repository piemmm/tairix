# tairix-bezier

Stability: **experimental**.

Plane geometry for vector drawing, below both rasterisation and SVG: the one
`Point`, the one `Affine` map (`lib/raster` re-exports it), `Bounds`, the
`FillRule`, the `Line`, `QuadBez` and `CubicBez` segments, the `EllipseArc`,
and the `Path` of sub-paths built from them.

- `flatten` — the one flattener: error-bounded subdivision to a tolerance,
  which the SVG decoder, the icon symbols and the drawing editor draw through.
- `cubic`, `quad`, `line`, `arc` — evaluation, derivatives, splitting, tight
  bounds, nearest points, arc length and its inverse (`ArcTable`), SVG's
  endpoint arc in centre form and as cubics.
- `path` — winding numbers by exact curve crossings, containment under either
  rule, signed area in closed form, reversal, transforms.
- `roots` — quadratic and cubic roots written for Bézier coefficients.
- `intersect` — crossings and coincident runs of any two segments.
- `fit` — Schneider's cubic fitting, the pencil and Simplify.
- `offset` — offset curves, a stroke's outline as a path, dashes, and a fill
  grown or shrunk.
- `boolean` — union, intersection, difference, exclusion and divide, keeping
  curves as curves.

Every algorithm is total over finite input and bounded in the work it does;
the bounds (`MAX_SEGMENTS`, `MAX_FIT_POINTS`, `MAX_BOOLEAN_SEGMENTS`,
`MAX_BOOLEAN_SPLITS`, `MAX_DASHES`, `intersect::MAX_CROSSINGS`) are fixed
containment bounds, and work past one is a typed refusal. Allocation that
grows with the input is fallible.

Readers: `lib/svg`, `lib/raster`, `lib/icon`, and the drawing editor
(`plans/DRAW.md`). See `docs/src/lib/bezier.md`.
