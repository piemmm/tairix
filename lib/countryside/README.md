# tairix-countryside

Stability tier: **experimental**

The countryside's layout, renderer-neutral. It covers:

- the holdings a land is farmed in;
- the villages and farmsteads on them;
- the roads, lanes, tracks and paths between them, routed over the ground,
  beside the highways the consumer brings, kept as given;
- the fields the land between ways and water is cut into;
- each field's boundaries, gates and use;
- a farmstead's yard and buildings, and a village's plots along its streets.

The desktop's ray tracer (`lib/raytrace`) sets these out as geometry, and
WinterSun (`userland/games/wintersun`) is to set them out as world objects.
Which kinds of settlement a world has, and what a building is, stay with each
consumer. `no_std` + `alloc`, and `#![forbid(unsafe_code)]`.

## What it provides

- `Countryside` and `Laying` — a region's layout, laid a step at a time
  across a `tairix_parallel::JobRunner`, ending in a `Layout`.
- `Layout` — every way, field, boundary, farmstead and village reaching the
  region, and what lies at any place in it (`side_at`, `parcel_at`).
- `Waters` and `Ground` — the land as its consumer knows it: its height and
  the water standing on it, which is all asking what lies at a place reads,
  and for laying a layout out, how wet, stony, wooded and fertile it lies.
- The records a consumer draws from: `route::Line`, `field::Field`,
  `boundary::Boundary` with its `Gap`s and height, `usage::Usage`,
  `farm::Farmstead`, `village::Village`; and what a boundary's kind is like
  (`Kind::stands`, `Kind::porosity`) and the stretches its gaps leave standing
  (`boundary::standing`).

## Guarantees

- **Seam-free random access.** Every feature's draws are words keyed by its
  purpose and its place, and every feature is worked out over as much land
  about it as its making reads. So a region laid out in pieces lays every
  feature exactly as one laid out whole.
- **Deterministic everywhere.** Transcendental maths goes through
  `lib/util::mathf`, routes search integer costs, and every order breaks its
  ties on identity. A layout is therefore bit-identical on every Tier-1 target
  and on any runner.
- **Steps sized to the runner, fallible memory.** Each step settles a bounded
  number of route points or lays a bounded batch of holdings, ways or fields a
  core, at most 64 routes searching at once; the step between two phases is
  one pass over the region's holdings or ways, a few milliseconds for the
  lands the ray tracer lays. The layout holds every holding, way, boundary and
  field of its region, so its memory grows with the region. Every buffer is
  reserved fallibly, and a refusal answers `Error::OutOfMemory`.

## Dependencies

`tairix-util` (`mathf`, `fallible`), `tairix-hash` (the keyed `FastHash` every
draw is a word of), `tairix-rng` (a word scaled to a unit draw),
`tairix-terrain` (the integer A\* every way is routed by) and
`tairix-parallel` (the runner a layout's work is shared across).
