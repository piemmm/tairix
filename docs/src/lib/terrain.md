# tairix-terrain

`lib/terrain` holds the grid algorithms a land is shaped by: where water runs,
how it and gravity wear the ground, and the cheapest way across it. The
desktop's ray tracer (`docs/src/lib/raytrace.md`) builds every landscape with
it, and WinterSun's world generator (`plans/WINTERSUN.md` WS2) its realm field,
so a river cut, a hillside's settling and a road's line are one definition
wherever they are drawn. It is `no_std` + `alloc` and forbids `unsafe`.

## The grid

A `Grid` is a square of samples stored row-major; each sample's eight
neighbours, and the `FlowDir` naming which of them it drains to, are its whole
vocabulary. Heights are the caller's: every pass reads and writes a slice of
one value a sample, and refuses one of any other length (`TerrainError::Shape`).

## Passes

- **Drainage** (Barnes, Lehman and Mulla, *Priority-Flood*, 2014) raises every
  pit to its spill point, then routes each sample to its steepest lower
  neighbour over the filled surface. The routing is acyclic by construction —
  the flood visits samples in the order water leaves them — so the area
  draining through each sample is one pass down that order.
- **Incision** lowers each sample by stream power, `K · Aᵐ · S`, along the
  routing: explicitly, or by Braun and Willett's implicit scheme (2013), which
  stays stable at time steps the explicit one cannot.
- **Hillslope** diffuses heights linearly, which rounds ridges and fills
  hollows, and slumps any slope steeper than its angle of repose back to it:
  talus.
- **Droplets** carry sediment down the slope they read, eroding where they
  speed and depositing where they slow, a bounded run of them each call.
- **Routes** are A\* over integer costs the caller prices step by step, so a
  road can prefer gentle ground, reuse an existing road, and bridge a river
  where it is narrowest; ties break on a sample's index.

## Bounds and determinism

A flood, a route and a run of droplets each advance a bounded number of samples
per call, so a caller answering a frame spreads them over as many calls as it
needs; run to its end, the pass gives exactly what one call would. A route the
heap refuses room to expand ends with `TerrainError::OutOfMemory`, never left
half-expanded to answer wrongly later. Arithmetic is
`f64` or `f32` through `lib/util::mathf` and every queue breaks its ties on a
sample's index, so a grid shaped on one Tier-1 target is bit-identical to the
same grid shaped on any other — the property WinterSun's cross-architecture
determinism vertical holds its realm to.
