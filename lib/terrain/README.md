# tairix-terrain

Stability tier: **experimental**

Grid terrain: which way water leaves a height grid, how water and gravity wear
it down, and the cheapest way across it. The desktop's ray tracer
(`lib/raytrace`) builds its lands with it, and WinterSun's world generator
(`userland/games/wintersun/world`) its realm field, so both share one
definition of each algorithm. `no_std` + `alloc`, and
`#![forbid(unsafe_code)]`.

## What it provides

- `Grid` — a square grid of samples stored row-major, its neighbours, and the
  `FlowDir` a sample drains along.
- `drainage` — Priority-Flood (Barnes, Lehman and Mulla, 2014): the
  depression-filled surface, an acyclic steepest-descent routing over it, and
  the area draining through every sample.
- `incision` — fluvial incision by stream power along that routing, explicit
  or by the implicit scheme of Braun and Willett (2013).
- `hillslope` — linear diffusion, and talus: ground steeper than its angle of
  repose slumps to it.
- `droplet` — particle hydraulic erosion: droplets that carry sediment down the
  slope they read and lay it down where they slow, drawn tile by tile from
  streams keyed by each tile's pass and place on a lattice fixed to the land,
  over two staggered passes whose tiles run at once across a
  `tairix_parallel::JobRunner`. A piece of a land is worn from blocks of
  ground a caller's `Store` keeps where it keeps them, the rest filled from
  the land and worn by just the tiles they need, exactly as the land worn
  whole.
- `route` — A\* with integer costs: the least-cost path between two samples
  under a caller's pricing of each step, bounded by octile distance or by a
  tighter bound the caller supplies (`lib/countryside` routes every way by it).

## Guarantees

- **Bounded steps.** A pass whose cost grows with the grid — a flood, a route,
  a wearing — advances a bounded amount per call; a flood or a route run to
  its end gives exactly what one call would, and a wearing leaves the same
  ground however many calls and cores share it.
- **Keyed by place.** A block of worn ground is a pure function of the land
  and its place, so a land worn in pieces matches it worn whole, whatever a
  store keeps or drops.
- **Deterministic on every target.** Every answer is a pure function of its
  inputs: arithmetic goes through `lib/util::mathf`, and every queue breaks a
  tie on a sample's index, so a world is bit-identical on every Tier-1 target.
- **Fallible allocation.** Every working buffer is taken fallibly; a heap that
  refuses one answers `TerrainError::OutOfMemory`, and a buffer that does not
  hold one value per sample `TerrainError::Shape`.

## Tests

`cargo test -p tairix-terrain`: a pit fills to its outflow and every sample
drains to a sink without a cycle; a flood, a routing, an accumulation, a
diffusion and an implicit incision done in pieces each match the same done
whole; incision cuts a valley but never below its outlet or the floor, and an
implicit step lowers a sample toward its receiver and never past it; diffusion
spreads a spike and talus slumps a column to its repose, losing nothing;
droplets carry an upper slope down to its foot, leave level ground level and
gather water in the hollows they run through, leave the same ground and water
tracks on one core as across several, and change nothing past a tile's window;
a piece worn from its blocks is the land worn whole to the bit from any store,
a piece beside one worn re-runs only what its blocks need, a kept block of the
wrong size is made again and a block counts what it holds, and a refused unit
changes nothing, nor a law, rate or place a run cannot count; a route costs the
octile distance over even ground, goes round a wall and never through a closed
one; and a buffer of the wrong size is refused.

The design is in `docs/src/lib/terrain.md`.
