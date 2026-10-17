# tairix-land

Stability tier: **experimental**

The land generator, renderer-neutral, so the desktop's ray tracer
(`lib/raytrace`) and WinterSun's world build one land. `no_std` + `alloc`,
and `#![forbid(unsafe_code)]`.

## What it provides

- `relief`: the landforms — hills, mountains, dunes, an island, mesas, a
  river valley — and the relief a land lays one out as, on a regional slope,
  levelled in a clearing and settling to a rim where it does.
- `course`: the courses rivers, roads and paths run along, polylines carrying
  each vertex's level, breadth and depth, and the index that finds the
  nearest to a place in about as many steps as there are courses near it.
- `channel`: a river's channel along its course — pools and riffles, ledges,
  bars and cut banks, and how its water stands and runs in it.
- `snow`: the snow the wind lays, deep in the lee of every rise, scoured from
  crests, drifted against what stands in its way and carved into dunes and
  sastrugi.
- `grown`: what a farmed land grows in its season, and the way its rows run,
  each a byte a vertex carries.
- `attributes`: the channels each vertex of a land carries, and what a place
  on a land is like (`Lie`).
- `plan`: everything a land is made from — its relief, wear, rivers, road,
  countryside, snow and levels — the lattices it is built on, and the
  character and grain of its relief below the coarse step.
- `coarse`: the coarse land, built a bounded unit at a time: worn by its
  drainage, its rivers and lakes read, its road routed, its countryside laid
  out and its ways laid in; or brought from outside as a field a consumer
  solved itself.
- `level`: pieces of the levels refining the coarse land, each a square of a
  level's lattice, refined a bounded unit at a time.
- `ways`: a land's roads, lanes, tracks, paths, ditches and churned gateways.
- `Season`: the time of year a land is laid out in.

## Guarantees

- **Keyed by place.** Every level lies on a lattice fixed to the land, and a
  piece of a level is a pure function of the land and the piece: a region
  refined in pieces is the region refined whole, to the bit, whatever a store
  of worn ground keeps.
- **Deterministic.** Every function is computed in one fixed order through
  `tairix_util::mathf`, so a land is the same bit for bit on every Tier-1
  target, however its work is shared across a runner.
- **Fallible.** Every allocation is taken fallibly; a plan or a piece that does
  not fit, or a parent holding less than a piece reads, is refused.

## Tests

`cargo test -p tairix-land`: every landform keeps above its lowest and the
relief levels in its clearing, settles to its rim and falls along its tilt; a
course's index finds what a search of every segment finds; a river's channel
holds its water within its brims and its units where its marks put them, a
low river's bed is soaked and its banks thin toward it, a level course wanders
by its own run and a delta fans its silt; the coarse land's rivers run
downhill, its bridges clear their water, its square snaps to its lattice,
its build's progress only grows unit by unit, and a plan any of whose numbers
is not a land's is refused; a land brought from outside refines as the
crate's own, holds a lake where its drainage fills deep, and is refused when it
does not fit; level 0 and a finer level refined in pieces are the level refined
whole, a piece off the land or short of its parent is refused, a finer vertex
reads its parent to the land's last vertex, a land's grain shapes its relief, a
level too coarse for its band adds none of it, the sea floor shelves only its
shore's face, a piece keeping only its heights has the heights of one keeping
everything, a parent keeping only its heights reads as plain ground and is
refused where snow lies, and snow lies on a land as the wind drifted it; every growth keeps its byte and each
season stands each crop where it would be.

The design is in `docs/src/lib/land.md`.
