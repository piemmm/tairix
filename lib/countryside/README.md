# tairix-countryside

Stability tier: **experimental**

The countryside's layout, renderer-neutral. It covers:

- the holdings a land is farmed in;
- the villages and farmsteads on them;
- the highways, roads, lanes, tracks and paths between them, routed over the
  ground;
- the fields the land between ways and water is cut into;
- each field's boundaries, gates and use;
- a farmstead's yard and buildings, and a village's plots along its streets.

The desktop's ray tracer (`lib/raytrace`) sets these out as geometry, and
WinterSun (`userland/games/wintersun`) is to set them out as world objects.
Which kinds of settlement a world has, and what a building is, stay with each
consumer. `no_std` + `alloc`, and `#![forbid(unsafe_code)]`.

## What it provides

- `Countryside` and `Laying` — a region's layout, laid a bounded unit at a time
  across a `tairix_parallel::JobRunner`, ending in a `Layout`.
- `Layout` — every way, field, boundary, farmstead and village reaching the
  region, and what lies at any place in it (`side_at`, `parcel_at`).
- `Ground` — the land as its consumer knows it: height, standing water, and
  how wet, stony, wooded and fertile it lies.
- The records a consumer draws from: `route::Line`, `field::Field`,
  `boundary::Boundary` with its `Gap`s, `usage::Usage`, `farm::Farmstead`,
  `village::Village`.

## Guarantees

- **Seam-free random access.** Every draw is keyed by its purpose and its place,
  never drawn in sequence. Every feature is worked out over as much land about
  it as its making reads. So a region laid out in pieces lays every feature
  exactly as one laid out whole.
- **Deterministic everywhere.** Arithmetic is `f64` through `lib/util::mathf`,
  routes search integer costs, and every order breaks its ties on identity. A
  layout is therefore bit-identical on every Tier-1 target and on any runner.
- **Bounded steps, fallible memory.** A route's search settles a bounded number
  of points per unit, and only as many routes run at once as the runner is
  wide. Every buffer is reserved fallibly, and a refusal answers
  `Error::OutOfMemory`.

## Dependencies

`tairix-util` (`mathf`, `fallible`), `tairix-hash` (the keyed `FastHash` every
draw is a word of), `tairix-rng` (a word scaled to a unit draw),
`tairix-terrain` (the integer A\* every way is routed by) and
`tairix-parallel` (the runner a layout's work is shared across).
