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
  speed and depositing where they slow (Beyer, 2015). A run (`droplet::Tiles`)
  lies on a lattice fixed to the land, not to the grid that holds the ground:
  its droplets start tile by tile, each tile drawing them from a stream keyed
  by its pass and place, over two tilings, the second staggered half a tile
  from the first. A tile is a third again as broad as a droplet reaches.
  Within a pass every tile's droplets run on its window of the ground as the
  pass found it, so a pass's tiles run at once across a runner, and their
  changes are summed in the tiles' row-major order; the second pass reads the
  first's sum. The droplets run only on the land's own square, the domain.
- **Wearing a piece.** A piece of a land, a rectangle of its lattice
  (`droplet::Placed`), is worn (`droplet::Wearing`) from blocks of ground 64
  samples a side, one set for the ground before the run and one after each
  pass (`droplet::Block`). A block a store keeps is read; the rest are filled
  from the land (`droplet::Source`) and worn by just the tiles reaching them.
  So the ground a piece needs lies within the run's halo of it
  (`Tiles::halo`): its blocks, and each pass's tiles and a droplet's reach
  either side, 815 samples at the tracer's law. Each block is a pure function
  of the land and its place, so a store (`droplet::Store`) may keep or drop
  any block it is handed, a piece worn from any of them is the land worn
  whole to the bit, and a piece beside one already worn wears only the
  blocks no earlier piece reached. Each step fills a quarter of a block's
  rows a core, a block part filled carried to the next; gathers the windows
  of a batch of a pass's tiles as wide as the runner; or runs 512 more
  droplets of each of the batch's tiles, a tile's run resuming where the
  last step left it (`Tiles::run_on`), the batch summed once all its tiles
  have run, so a step costs a core the same whatever a tile starts. Measured against one run in order, two
  staggered passes wear a land within a few per cent and show no tiling.
- **Routes** are A\* over integer costs the caller prices step by step, so a
  road can prefer gentle ground, reuse an existing road, and bridge a river
  where it is narrowest; ties break on a sample's index. The rest of the way is
  bounded by octile distance times the cheapest step, or by a tighter bound the
  caller knows (`advance_guided`): it must never overestimate, nor fall by more
  than a step costs, and then settles far fewer samples where most of the grid
  costs far more than its cheapest steps. `lib/countryside`
  (`docs/src/lib/countryside.md`) routes every way of a countryside this way.

## Bounds and determinism

A flood, a route and a run of droplets each advance a bounded amount per call,
so a caller answering a frame spreads them over as many calls as it needs; run
to its end, a flood or a route gives exactly what one call would, and a
wearing the same ground however many calls and cores shared it and whatever
its store kept. A
route the heap refuses room to expand ends with `TerrainError::OutOfMemory`,
never left half-expanded to answer wrongly later, and a wearing refused room,
or a step its land refuses, is left where it was. Its
arithmetic is checked: a law, a rate or a lattice place it could not count
exactly is refused as `TerrainError::Shape`. Arithmetic is
`f64` or `f32` through `lib/util::mathf` and every queue breaks its ties on a
sample's index, so a grid shaped on one Tier-1 target is bit-identical to the
same grid shaped on any other — the property WinterSun's cross-architecture
determinism vertical holds its realm to.
