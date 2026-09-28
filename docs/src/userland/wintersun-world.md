# The WinterSun world generator

`userland/games/wintersun/world` (`tairix-wintersun-world`) answers what the
ground is at any point of a WinterSun realm, from the realm's `u64` seed and
a small validated parameter document and nothing else. It is
`plans/WINTERSUN.md` WS2 and WS25, and the second crate of the
`userland/games/` leaf subtree. Stability tier: **experimental**.

## Why the world is generated rather than transmitted

Terrain is public: a player can see the hills by walking to them, so sending
them would be paying bandwidth for something the seed already says. The
client and the realm server generate it independently and get the same
answer. The realm sends only what the seed cannot predict — entities, and the
deltas players caused — which is what makes a realm affordable in bandwidth
and in memory at any extent.

That only works if the two agree *exactly*. A one-sub-unit disagreement is a
player walking through a hill on one machine and around it on another, and no
amount of reconciliation recovers from it. Cross-architecture determinism is
therefore not a nicety here; it is the load-bearing property, and it is
tested as one.

Anything whose value *is* its concealment — a dungeon's interior layout, an
unopened container's contents, an undetected trap — is generated server-side
and streamed under interest management. It has no encoding in this crate at
all, which is a stronger guarantee than not sending it.

## Two scales, and why there have to be two

Hydrology, climate and road routing are not local questions. How much water
passes a point depends on every point that drains to it, which can be most of
a continent. A rain shadow is a record of everything the wind crossed before
it arrived. A road is only a road if it reaches the town at its far end.

A generator that answers those from a window centred on whichever chunk asked
produces a different answer per query — rivers that flow uphill across the
seam between two windows, and a road that stops at the edge of the window
that routed it. So the world is solved on two scales.

### The realm field

The whole world, solved once, coarsely and **globally**. Its stages run in
dependency order, each reading the one before it:

1. **Uplift.** Continental plates as a Voronoi partition over a jittered
   grid, each with a drift and a buoyancy. Convergence raises a belt along
   the seam two plates share; divergence opens a rift. This is what stops a
   heightfield looking like noise — a range gets a direction and a reason.
2. **Relief.** Domain-warped continental noise, plus the uplift field, plus
   ridged noise admitted only in proportion to belt strength, so ridges
   appear along belts and nowhere else. Sea level is then *cut* rather than
   chosen: the raw relief is sorted and cut at the requested quantile, so a
   realm's coastline is the submerged fraction the operator asked for.
3. **Hydrology.** Priority-Flood fills depressions from the outlets inward.
   Its pop order is itself a downstream-first ordering, which gives the
   filled surface (so lakes have a surface and an outflow), a
   guaranteed-acyclic routing, and the traversal order accumulation needs —
   all without an epsilon nudge or an iteration to convergence. Then bounded
   passes of stream-power incision (`E = K·√A·S`) with hillslope diffusion,
   which cut the valleys roads later follow and lay the flats settlements
   later use.
4. **Climate.** The realm spans a band of latitude, its two edges set by the
   parameter document. Temperature is a sea-level zonal curve by latitude,
   less an environmental lapse rate and a continentality cooling that grows
   toward the poles; its seasonal range grows with latitude and distance
   inland and shrinks over the sea. Precipitation follows four airflows — each
   hemisphere's westerlies and the easterly return flow beside them — each
   advected upwind-first once per solstice season with the belts shifted
   toward the summer pole, and weighed by how much it prevails at each
   latitude. Air loses much more where it is forced to rise, so the windward
   slope is wet and the lee is dry; over warm land it regains some of what
   fell. The rain season, summer rain less winter rain over the total, is what
   tells a Mediterranean coast from a savanna.
5. **Rock provinces.** A jittered-grid Voronoi partition four times finer than
   the plates, whose boundaries wander through a noise-perturbed query point.
   Its nearest site, like a plate's, is found exactly (`voronoi::nearest`,
   over sites `voronoi::site` places): the jitter lets a site two cells off be
   nearest, so the search widens past the nine cells around a point while one
   still could be. Each province's rock follows its tectonic setting, read
   through the relief's own continental warp: basalt where the ground is
   oceanic or floods from a fast-opening rift, granite or a metamorphic core
   in a strong belt, folded sediments in a weaker one, shield on old buoyant
   ground, platform sediments elsewhere.
6. **Sites and roads.** Settlements on level, watered, defensible ground, kept
   apart. A minimum spanning tree over them, each edge routed by integer-cost
   A\* over a traversal cost field that prefers level ground, pays heavily to
   cross water, and **pays less to reuse a road already routed** — which is
   what makes a network braid and converge rather than run as parallel lines.
   Then landmark entrances.

Its resolution is a fixed sample count, never a step in world units. A realm
four chunks across and one four thousand chunks across get the same grid; the
larger simply has a coarser step. So the field's cost is the same on every
machine and for every realm, and nothing in it grows with the world's extent.

### The chunk

Fine detail, on demand. It reads the realm field, adds everything below the
coarse step, and depends on nothing outside a fixed ring of cells:

1. **Relief** — detail noise in *absolute* world units (texture does not
   scale with the realm the way structure does), ridged inside belts and
   billowed into dunes where it is dry.
2. **Water** — channels carved along the coarse drainage, widening with
   discharge; standing water only where the coarse field holds it — a hollow
   in the detail relief on dry ground is texture, not a basin. The sea stands
   at sea level and a lake at the coarse surface, and a cell is the sea's
   where the coarse water about it mostly is. Then the shore distance is
   measured.
3. **Structures** — settlements levelled and roads laid.
4. **Climate** — the coarse values corrected for the fine relief's departure
   from the coarse one.
5. **Biome** — the biome blend and the ground blend it grows.
6. **Scatter** — vegetation, rock and resource nodes, by biome.

Scatter is last because it reads the structure stamp: nothing grows on a road.

The build is a resumable phase machine, and the partially-built chunk is
readable throughout, so a client draws the coarse ground it already has rather
than blocking a frame on generation.

## The halo, and why it is exactly as wide as it is

Everything a chunk computes is either a read of the realm field — global, so
identical for every query — or a pure function of absolute position. Neither
depends on a neighbour. Two kinds of quantity do: the **shore distance**,
which is a distance transform, and the slope and drainage gradient, which are
stencils over the fine relief.

So the build works over the chunk *plus a ring* of `SHORE_CELLS`, runs the
transform over the whole working grid, and keeps only the chunk. A cell inside
the chunk therefore gets the same answer it would get if the whole world had
been transformed at once, and "a chunk generated alone equals the same chunk
generated as part of its neighbourhood" is a consequence of the construction
rather than a hope about the width. The transform carries which water is
nearest — a river, a lake or the sea, a tie going to the more standing — so a
coast reads the same whichever way it was reached.

Scatter reads one scatter step into the ring, so the ring is also as wide as
that step plus the widest band a footing reads — the shore reach and the
wetness stencil — and compile-time assertions hold it there. A footing in the
ring is computed by the same code as one in the chunk, from the same working
grid, the cleared flag of a levelled road included, and a channel whose bank
reaches into the ring is carved there at every coarse step.

Scatter obeys the same discipline by a different route: every scatter cell
offers exactly one candidate at a hashed position with a hashed priority, and
a candidate survives only if nothing within its exclusion radius outranks it.
Exclusion radii are bounded by the scatter step, so the outcome depends on
the eight neighbouring scatter cells and nothing further. Two chunks generated
a week apart agree on every item along their seam because neither ever looked
at the other.

## Biome and ground

A cell carries two blends: the **biome** — the living zone, which flora and
decoration read — and the **ground** it grows, which the splat draws. A
biome is not a surface: a boreal forest floor is needle litter, moss and
lichen, and a savanna is dry and tall grass over laterite. Each blend is a
weight vector over a closed vocabulary, kept to the four heaviest and
normalised to sum to exactly 255, so "the weights are normalised" is a
property of the type rather than a convention a consumer has to trust, and a
boundary is a gradient the renderer draws as one.

The classifier is a soft decision tree. Every split is a smooth threshold
whose two sides sum to one, so the tree is a partition of unity and its
totality holds by construction:

- **Cold** splits first on the warm season: below the snowline, ice sheet or
  polar desert by snowfall; between the snowline and the treeline, tundra,
  alpine tundra where the same place at sea level would grow trees, or heath
  and moor on an oceanic margin.
- **Dry** is Köppen's: effective moisture is precipitation over
  `20·(T + 7 + 7s)` mm, where `s` is the rain season, so a winter-wet climate
  keeps more of its rain than a summer-wet one.
- **The rest** divides by warmth, moisture, seasonality and continentality
  into the forests, woodlands, grasslands, shrublands and deserts from boreal
  to tropical.

Then terrain takes shares of that partition for its own biomes: rift waste in
torn lowland, volcanic barren on fresh basalt, badlands on gullied soft rock,
wetlands where drainage gathers, and the shores of lakes and the sea — a
river's bank is no coast. Each takes a share and scales the rest to make room,
so the sum is kept. Drainage wetness is specific catchment against the local
gradient in the monotone form `a/(a + K·tanβ)`, because `mathf` has no
logarithm; a marsh sits in a poorly drained flat, not wherever it rains. The
catchment leaves out a coarse sample's own area, so a ridge top drains
nothing, and it is counted in cells, so a threshold on it names the same
river at any coarse step. The classifier and the palettes read dry ground
only: where water covers a cell, it is open water.

Each biome grows a palette of grounds, modulated by moisture, wetness, the
cell's soil and rock, a slow patch field, and slope, and the ground blend is
the palettes weighed by the biome blend. Soils are a soft partition over
parent rock, climate and floodplain. A steep face turns to its own rock, with
scree gathering below it, whatever grows around it.

## Determinism, and how it is proven

The arithmetic is written to be target-independent:

- Randomness is a **keyed hash over coordinates**, not a sequence, so no
  value depends on the order anything was traversed in.
- Arithmetic is IEEE-754 `f64` restricted to the exactly-specified operations
  plus `lib/util::mathf`, TAIRiX's own libm. Rust contracts no fused
  multiply-add, so `a * b + c` stays two operations.
- Everything stored is a quantised integer, so a stored value is a bit
  pattern rather than a rounding.
- Every sort, priority queue and traversal has a total order with an index
  tiebreak. An `f64` comparator has none, so the flood's heap, the wind sweep
  and the road search all order integers.

"Written to be" is not evidence, so the claim is staked on one constant,
`digest::REFERENCE_DIGEST`: a fixed realm is solved, its coarse field and six
spread-out chunks are folded into a digest, and that digest must equal the
constant. The host suite asserts it, and so does one vertical per Tier-1
target — `tests/integration/world_determinism_qemu_{aarch64,riscv64,x86_64}`
under QEMU, and `tests/integration/world_determinism_wasm32` under a real
WebAssembly engine. Agreement between targets follows from each agreeing with
the constant, so a target that has never been run cannot pass by accident.

The `wasm32` leg needs no browser, unlike the other wasm32 verticals: its
subject is arithmetic, and requiring a headless Chrome would make the fourth
leg runnable in fewer places than the claim it defends applies to. It is also
the leg most likely to catch a real divergence, being the only Tier-1 target
with a 32-bit `usize`.

## Memory

Resident cost is the working set, never the world's extent:

- The realm field is a fixed grid, a few mebibytes at the resolution ceiling
  and the same on every machine.
- Chunks live in a `lib/reclaim` cache whose budget is derived from the memory
  the machine reported — supplied by the caller from the System Information
  API, because a library picking a capacity for itself would be the
  hand-chosen ceiling the charter forbids — and released through the system's
  pressure bands. Its generation token is the realm parameter document, so
  changing any of it invalidates every entry.
- Nothing is written to disk. Recomputing a chunk is cheaper than reading it
  back, so only player-caused change is stored, and a realm is O(changes) on
  disk and O(working set) in RAM whatever its extent.

Allocation is fallible throughout: a machine too short to hold the field gets
a typed refusal, never an abort.

## Bounds

Every field of the parameter document is bounded, because a client is handed
its realm's parameters *by the realm*, and a realm is no more trusted by a
client than a client is by a realm. The document is `RealmSpec`, the wire item
`wintersun/net` carries in `Welcome`; `RealmParams` wraps it behind one
validating constructor, so an out-of-range extent, a resolution that would not
tile, or an edge latitude off the planet is refused with a reason rather than
clamped into something the two ends might disagree about.

This crate decodes no bytes. The wire encoding belongs with the rest of the
protocol in `wintersun/net`, which owns bounded decode and fuzzes every
`Welcome`, so there is no untrusted-input parser here and no fuzz target of
its own.
