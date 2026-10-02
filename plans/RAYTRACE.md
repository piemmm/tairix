# RAYTRACE — photorealistic scenes for the ray-traced screensaver

Binding under `AGENTS.md`. This plan owns what `lib/raytrace` composes and how
it renders it: the scene families, the countryside, water, buildings and
nature it sets out, the sampling and reconstruction a pixel is traced with,
and the screensaver's reveal, progress readout and saved pictures.

Read first: `docs/src/lib/raytrace.md` (the tracer as built),
`plans/NEW-DESKTOP-SETTINGS.md` DS21 and DS24 (the screensaver and its pane),
`plans/WINTERSUN.md` WS25–WS31 and WS35 (the world generator that is to share
the land generator and the countryside layout), `plans/FIX-DESKTOP.md` (no I/O
on the serve loop) and `plans/OPEN-DEFECTS.md` D463, D465, D466, D483–D485,
D503, D505 and D514–D520 (the tracer's open defects).

## Ledger

| ID | Item | Status |
|---|---|---|
| RT1 | Smooth reveal: a cubic B-spline over the reveal's grids, a blur coming into focus with no point a peak or a cross; each paint repaints and marks only what its steps change, the paints a scene frame apart while the picture forms and slowing with the share shown to at most 3 s; the finished picture is every pixel's own trace | done |
| RT2 | Progress readout: *Generating scene... N%* while a scene is prepared, then *Rendering... N%*, small and mid-grey in the lower right, gone once the picture is whole | done |
| RT3 | Saving finished pictures: `screensaver.raytrace.save` keeps each whole picture as a PNG in the user's `Documents/Pictures/Raytracing/`, with no limit on how many | done |
| RT4 | Highest quality, always: no sample governor; a reconstruction filter and sampling rounds that leave no jagged or noisy edge | done |
| RT5 | A 160 s preparation budget, every unit of preparation bounded and parallel (closes D464), spent where it measurably buys realism: radiosity records, and woods reaching twice as far | done |
| RT6 | Wind on water without repetition: a spectrum of many wave components under gusting patches; the open sea's grid no longer tiles in view; detail finer than a pixel becomes roughness | done |
| RT7 | The water defects: ripples aliasing far off, reflections lost at grazing angles, the open sea stopping short of the horizon, and the noise seams under every pattern (D490–D494) | done |
| RT8 | Caustics on and under every body of water, from the light the surface itself focuses: beams over the water the picture shows, every one gathered, refracted onto beds and what stands in the water and reflected onto what stands over it | done |
| RT9 | Water's edge: reeds and bulrushes along banks, pondweed and water lilies in still water | planned |
| RT10 | Streams close up: running water over stones worn round, slate the angular exception | planned |
| RT11 | Deltas, beaches and eroding coasts: sand with driftwood, footprints, paw prints, shells, stones and wrack; cliffs, sandbanks, marram binding the dunes | planned |
| RT12 | Jetties, harbours and fishing villages; gulls and other birds; shoals of fish | planned |
| RT13 | `lib/countryside`: the renderer-neutral countryside layout — parcels, boundaries, gates, ways, land use, farmsteads, villages and plots — shared with WinterSun | planned |
| RT14 | Field boundaries as geometry: hedgerows with standard trees and repaired gaps, dry-stone walls, wooden fences kept and decaying, gates open, shut and broken | planned |
| RT15 | Ways between fields: green lanes between walls or hedges, tracks to gateways churned to mud where stock gather, roads graded by where they lead | planned |
| RT16 | Crops: maize, wheat, barley, oats, rapeseed and grass ley, in rows and tramlines by season; cut fields with one kind of bale or stack each | planned |
| RT17 | Pasture, orchards and vineyards: cow-pats with lusher grass about them, fruit trees in rows, vines on trellised rows | planned |
| RT18 | Overgrown ground: brambles, nettles, docks, scrub and tall weeds where land is left | planned |
| RT19 | Flowers and weeds by the score, in patches rather than an even sprinkle: clover, dandelions and their clocks shedding seed, thistles, buttercups, daisies, poppies, roses, ivy, wisteria and more | planned |
| RT20 | Masonry as geometry: rubble and coursed stone of individual imperfect stones in mortar; brick walls of individual bricks, reclaimed, marked, chipped and now and then missing | planned |
| RT21 | Moss and lichen as geometry: fibrous moss cushions and crustose lichen on stone, wall, bark and roof | planned |
| RT22 | Farm buildings: stone farmhouses under thatch, slate or tile; timber outbuildings weathered by age; vines, ivy and wisteria climbing the walls | planned |
| RT23 | Villages: houses on plots along their roads, front and back gardens, closeboard, post-and-rail or no fences with garden gates, chimneys with smoke now and then | planned |
| RT24 | Night in a village: lit windows, curtains drawn or left open, interiors lit behind them | planned |
| RT25 | Farmhouse interiors: kitchen, range, sink, table and chairs, dressers, stone floors and rugs, by day and by night, the air dusty enough to show a sunbeam | planned |
| RT26 | Bark in true relief: ridges, plates and corrugation as displaced geometry near the eye | planned |
| RT27 | Stumps, sawn and splintered, some with new shoots at the foot | planned |
| RT28 | Mud cracks as modelled geometry: polygonal plates with curled edges and real depth | planned |
| RT29 | Snow that lies unevenly and drifts; snowmen never quite round | planned |
| RT30 | Mountains near and far, ridged and eroded, some snow-capped | planned |
| RT31 | Weathered stone everywhere: erosion, chips, cracks, lichen and moss on every stone structure | planned |
| RT32 | The moon by day or by night, at its phase and lit by earthshine, or not at all | planned |
| RT33 | Aerial views: landscapes from altitude, and detailed abstract views | planned |
| RT34 | Planetary scenes: gas giants, ringed planets, earthlike and Mars-like worlds, views from icy moons, nebulae, stars, and the sun with its corona and spots | planned |
| RT35 | Macro shots: a leaf with a drop hanging from it, focused on the drop over a blurred landscape, in many variations | planned |
| RT36 | Caves: stalactites, stalagmites, pools, iridescent water, crystal outcrops that glow | planned |
| RT37 | Woods to the horizon: at *Maximum* a wood reaches as far as its trees still span a pixel, its places sown coarser the further they lie wherever they would not otherwise fit the budget | planned |
| RT38 | Stones in patches: boulder fields, scree below crags and pebbles in drifts, strewn by noise and slope from eight rocks a scene, and leaf litter as thick as the canopy sheds | planned |
| RT39 | Scene detail: one generator at two profiles, *Simple* (low memory, every setting, the screensaver's default) and *Maximum* (up to 2 GB, all the realism the budget buys), chosen by `screensaver.raytrace.detail` | done |
| RT40 | One land generator, the tracer's and WinterSun's: the land's stages renderer-neutral, keyed by place and seam-free, in crates WinterSun builds its realm's land with at *Maximum* | planned |
| RT41 | Local adaptation, a gentle photographic HDR: a sky seen from a dark room or over a dark wood keeps its detail and the room its shadows, only the range a display cannot hold compressed, and a scene one exposure holds left exactly as it is | done |

## Standing rules

These bind every item, done or planned.

- **Physics, not effects.** Nothing is painted on that light would not
  produce. A sunbeam through a dusty window is single scattering in a
  participating medium the scene really holds; a caustic is light the water
  surface really focuses; a gleam is a reflection the tracer really finds. If
  a phenomenon does not appear, the scene or the light transport is wrong, and
  that is what gets fixed.
- **Geometry over texture.** Anything a viewer reads as having shape is
  modelled: a wall is stones, a brick wall is bricks and recessed mortar, moss
  is fibres, a mud crack is a gap between plates, bark relief near the eye is
  displaced. A pattern may stand in only where the detail is finer than a
  pixel, and there it settles to its mean in colour *and* in relief.
- **Variety by construction.** Every repeated element — stone, brick, plant,
  wave, cloud — is drawn from its own key, so no two match. Nothing tiles in
  view, and no scene is uniformly covered by any one thing: cover comes in
  patches, gradients and gaps.
- **Budgets.** A scene may take up to 160 s to prepare on a desktop-class
  machine preparing across 8 threads, and hold at its peak up to 2 GB (or the
  free memory, if less) at *Maximum* detail, far less at *Simple* (RT39). Both
  are spent on detail and never wasted: a buffer is sized to what it holds, a
  stage's working set is let go when the stage ends, and where an efficient
  structure serves as well it is the one used. Every unit of preparation stays
  small enough for a caller answering a frame to stop after it; every
  allocation is fallible, so a smaller machine gets a plainer scene, never an
  abort.
- **Shared with WinterSun.** The land generator is to build WinterSun's land
  too, at *Maximum* (RT40), so work on it keeps to WinterSun's world rules
  (`plans/WINTERSUN.md` WS25–WS31, WS35): a pure function of its inputs, bit
  for bit the same on every Tier-1 target and however many cores share it,
  renderer-neutral, and bounded by its working set rather than the land it
  covers.
- **Deterministic.** A setting, a seed and a picture size compose one scene,
  and a pixel's samples are hashed from its index and the scene's key.

## RT1 — Smooth reveal

`Reveal` traces coarse to fine: each pass traces the points of a grid whose
spacing halves pass by pass that no coarser pass reached, each point once. A
`Step` names its pixel and its pass's spacing.

The picture shown (`saver::raytrace::preview`) is a uniform cubic B-spline
over the grid the current pass traces, its controls drawn from every pixel
traced so far, which the painter keeps. The basis is smooth through its
second derivative, so a coarse grid reads as a soft blur, with no point a
peak or a cross, and the picture comes into focus as the passes halve the
spacing. A pass's grid begins as the coarser grid refined (Lane and
Riesenfeld), which is the same surface, so no pass jumps. A point's control
then moves from that refinement to its own trace by its settled share — the
traced share of the pass's points about it, itself among them — so detail
fades in as samples gather rather than at each new point; the first pass
refines nothing, so the picture fades in from black. In the last pass each
pixel settles onto its own trace by the same share, so the finished picture
is every pixel's own trace. The grid mirrors across its first column and row;
past its last, the first grid repeats its edge and every later grid keeps the
two columns and rows refinement carries down, so the edges agree with
refining too.

A step changes the picture only within three of its pass's spacings of its
point. A paint repaints the cells the reaches of every step since the last
cover, each once, across the compositor's pool, and marks a cover of 16-pixel
tiles, their side doubled until they make at most 128 rectangles: a compositor
merges each rectangle against the rest, so a scattered paint's own rectangles
would cost more than whole tiles, and the box it spans is the screen. A buffer
the compositor lets go is painted afresh from what the painter keeps, and
nothing is traced again.

Paints come further apart as the picture fills in
(`saver::raytrace::paint_wait`). The wait after a paint is in proportion to
the steps shown, so each pass, four times as long as the one before, is shown
in about as many paints: a scene frame while the coarse passes form the
picture, 1.5 s as the 2 px pass begins (every point of the 4 px grid shown),
and never more than 3 s. Fine detail changes too little between paints for a quicker cadence to
show, and every paint costs a wake, a compositor pass and a present whatever
it carries; over a 1080p reveal this cuts the paints about sixty-fold and,
traced across 8 cores or more, the pixels composited eighteen- to fortyfold.
A buffer to lay afresh and a picture's last steps are painted at once. Steps
collected between paints wait for the next, and those of a refused scene go
with it.

## RT2 — Progress readout

`Draft::progress` answers how far preparation is, in thousandths, never
falling back and reaching a thousand only when the scene is ready. Each stage
carries a share of the whole measured over the settings; a land's build weighs
its own stages by their items times the measured cost of one, so its share
keeps pace whether droplets or wear dominate. The shares are averages: a
setting whose land is most of its work (the desert) runs behind the clock in
its first half and catches up. Tracing reports steps traced of the reveal's
count. The engine reports both in the trace desk's status.

The readout is its own small window over the screensaver's, a transient of
it so it rises with it: *Generating scene... N%* while the scene is prepared,
*Rendering... N%* while it is traced, in mid-grey at the theme's caption size,
a margin in from the lower-right corner. It is brought up to date at each
paint, and four times a second while a scene is prepared on its own thread,
which wakes the serve loop through the session's worker wake the moment the
scene is ready, so its first passes are shown as they come. It is repainted
only when the whole percentage changes and taken down once the picture is
whole, the scene is refused, or the screensaver goes.

## RT3 — Saving finished pictures

`screensaver.raytrace.save` (`true`/`false`, off by default) is a *Save
pictures* row on the screensaver pane. The option travels to the tracing host
at launch; an engine told to keep pictures copies each traced pixel into a
picture of its own as it traces, on the thread that will write it.

Once the picture is whole and laid down for the loop, the tracing thread —
never the serve loop — encodes it with `lib/image`'s PNG encoder (the smallest
exact colour type, 8-bit RGB for an opaque picture) and writes it into
`<home>/Documents/Pictures/Raytracing/` through the `Keeper` and
`PictureFiles` seams, making each folder of the way as needed. A file is
created exclusively and never through a link, named for the setting and the
UTC moment it was finished (its scene's seed when the clock is not set), with
a numbered suffix if that name is taken, so nothing is overwritten and no
count is enforced; a write cut short is removed. A refusal — no home, no
space, a picture the heap would not copy — is stated on `stderr` and never
stops the screensaver. With no tracing thread granted there is nowhere off the
loop to write from, so nothing is kept and the launch says so.

## RT4 — Highest quality, always

The screensaver traces every pixel at the tracer's best quality; the governor
that cut samples to keep a reveal under four minutes is gone. A pixel takes
at least 16 samples, four by four over the filter, then rounds of 32, 64 and
128 until its samples agree. Its samples are drawn from a Gaussian
reconstruction filter of half a pixel's deviation, cut off at three, through
the inverse of its distribution, so every sample weighs the same. Measured,
this is about twice the samples and the time of the old best quality.

## RT5 — The preparation budget

Every unit of preparation is a fixed amount of work a core, whatever the
scene holds, so a caller answering a frame stops within a few milliseconds:

- A height grid of any number of cells reserves its buffers whole and is
  written as its rows are filled, so no unit zeroes a whole grid; it is
  sealed a band of rows a core, its mean summed in bands of a fixed size so
  it comes out the same on any runner. A canopy grid is its tier's size.
- Prototypes grow as many at once as the runner is wide: a tree's stems a few
  at a time, any other kind's parts in one core's unit, every hierarchy a
  slice at a time.
- A wood's places are sown a ring at a time, a unit bounded by the places
  drawn rather than those that fall on the land; those that would grow are
  kept in runs of 16 384 that never move as more are added, each run sorted
  by a core and merged tallest first as they are thinned, 1024 a unit.
- The shade crowns cast is cast in bands: the crowns sorted into bands of
  rows 16 384 a unit, each band covered from the crowns that can reach it,
  then spread along its rows and, turned, along its columns, a band a core —
  bit for bit the shade cast whole. A lawn samples its shade the same way.
- A land's fills and settle passes run four rows a core, its droplets in
  turns of tiles across the runner (`lib/terrain`), its seal in bands; the
  sky's tables a row a core, a cloud bank's light a quarter layer a core;
  a radiosity record's hemisphere is gathered 64 rays a core a unit, and the
  records laid so far are indexed a slice at a time before the next grid
  looks for sites none of them holds for.
- A scene's objects' boxes are found across the runner a band at a time and
  its hierarchy built a slice at a time; the meter takes eight points, then
  sixteen adaptation samples, a core a unit.

Measured at 1920×1080 across 8 threads, no unit takes more than about 10 ms
at either detail, and bounding them changed no pixel.

At *Maximum* (RT39) the budget is spent where it measurably buys realism.
Radiosity records hold a hemisphere of 1024 rays, are laid down to half the
radius *Simple* keeps and four times as many to a square, and hold within
fifteen degrees of turn, as their own rule says: the light a wood's trunks gather from about them, which records
spread over trunks too thin for them missed. Woods may stand three to ten
times as many trees, so they carry on to 2–4 km where they stopped at about
1.3 km and a forest fills its land. Droplet erosion is not made denser:
measured, more droplets under the present law wear the finer grids' relief
smooth and silt the river and road beds they run through, so denser erosion
waits on the law RT30 brings.

## RT6, RT7 — Water

Fine water relief (`Relief::Waves`) is a spectrum of 96 travelling waves, one
in each of 96 equal bands of log length from the longest to the shortest a
breeze raises, about as steep as each other, their directions spread about
the wind and shorter waves straying further; scaled together to the slope
variance asked of them — Cox and Munk's fit for the open sea, far less for
sheltered water. Measured, the slope at one place correlates with the slope
anywhere else below a quarter, where six swells correlated past a half. A gust
field of smooth noise scales their height from a lull's share to the whole,
the catspaws a breeze draws.

A wave shorter than about two of the pixel's footprints along the view is not
evaluated: its slope variance widens the surface's microfacet roughness
instead, so far water turns into a glossy sheet with the highlight spread its
waves give it, never moiré. A reflection a facet sends under the surface is
turned back above it rather than lost. The open sea's grid repeats every
kilometre in metre cells, no swell shorter than four of them, and past its
6 km reach the sea lies at the grid's mean level to the horizon.

## RT8 — Caustics

Sunlight crossing water is bent by its waves' slopes, gathered where the
surface curves one way and spread where it curves the other (`caustic.rs`).
The surface is cut into beams (Watt, 1990): each triangle of a grid over it
carries the sunlight crossing it, bent at its corners, to any depth or height,
where it covers a triangle of its own. A point gathers the flux of every beam
falling within the box the sun's disc and the waves too fine to resolve blur a
point into, over the box's area, against what a level surface sends it. Over a
level surface the beams tile the receiver, so every point takes exactly the
level light; past a focus, where beams cross, every one still counts, so the
folds are summed, never missed.

- **Where.** A survey of the picture — every fourth pixel at *Simple*, every
  second at *Maximum* — finds what the eye sees beneath water, over it, and
  mirrored in it, and asks for the 2 m tiles of water whose beams can reach
  each point. A tile is laid only where its waves move that light by a
  hundredth; a point so deep or so high that no tile could resolve a wave for
  it asks for none.
- **How finely.** A tile holds every level from two cells a side to as many as
  2⁹, a little under 4 mm, each level resolving only the waves six of its
  cells long and blurring the rest as the sun's disc does. A point gathers at
  the level its own footprint asks, blending it with the next, so the detail
  never steps from tile to tile or with distance; coarser than a tile, it
  takes a level surface's light. It never asks for cells so fine that more
  than 256 beams lie in the patch of glints it gathers from: past a focus that
  patch grows with depth, and what its cells cannot resolve the sun's disc
  already blurs there.
- **Room.** At most 2²¹ cells at *Simple* and 2²³ at *Maximum*; a plan past
  its room coarsens every footprint alike. Its buffers are reserved whole and
  written a unit at a time. The survey keeps each tile asked for once, in a
  hash map keyed by its place; a level's rows are filled by sweeping each
  wave's crest a step at a time (`mathf::Phasor`) wherever the waves lie level,
  read afresh where a texture frames them off level; each pyramid's foot is
  sealed in bands of rows across the cores, then the rungs above it.
- **The tracer.** Sunlight beneath water comes the way the surface above bends
  it: all but what a level surface reflects, absorbed along the bent way, and
  scaled by the beams' factor. A surface over water facing it takes the sun
  the water reflects, as much as a level surface's reflectance sends, scaled by
  the reflected beams'. Either way the surface takes that light diffusely: its
  highlight of it is the light's own image, which the surface's reflection
  finds. A low sun's reflected glitter, whose beams swing too far along its way
  to resolve, reflects its mean.
- **The ray cone.** A path's footprint widens as it passes through or glances
  off a rough clear surface (Amanatides, 1984), so what is seen through ripples
  is filtered over what the ripples blur it to, and its caustics are gathered
  no finer than that.

Measured at 1920×1080 across 8 threads, laying a scene's caustics takes up to
0.25 s at *Simple* and 0.95 s at *Maximum* and holds up to about 75 MB and
330 MB, a mountain lake's the most; a unit of it takes at most about 6 ms.
Beneath and over water the caustics add about 2.5 µs to a sample.

Tests: a level surface's beams bringing every point exactly its light, across
tile seams and beyond the laid tiles; beams before a focus and past it
bringing what a million surface points' beams landing in the same box bring;
the waves passing all the light they bend; the detail changing smoothly with
the footprint; the survey laying beams only where the picture looks into
rippled water, the same on any runner; real waves drawing a net of light on a
bed; sunlight under water bent and absorbed along its bent way; a ceiling over
water taking the sun the water reflects; rows of beams swept along level waves
being those read afresh, and read afresh under waves framed off level; every
pyramid bounding the beams beneath it however its sealing is shared; a run of
points asking for tiles as each alone would; a point seen over no footprint
taking a level surface's light; and a picture of the bed showing the net that
one with nothing laid does not.

## RT9–RT12 — Water's edge and the coast

- Banks and still water (RT9): reeds and bulrushes stand in the shallows and
  along banks where the land is wet and level; pondweed and lilies float where
  the flow is still and the water shallow, both read from the land's water
  attributes.
- Streams close up (RT10): a close vantage on a stream; its stones rounded in
  proportion to how far water has carried them, angular only where the bed is
  slate; the water's surface shaped by the flow over them, standing waves and
  all.
- Deltas and coasts (RT11): distributaries building lobes into still water;
  beaches graded by exposure — sand, shingle and stones — with driftwood and
  wrack along the tide line, shells, footprints and paw prints in tracks that
  wander; cliffs where relief meets the sea, slumping and undercut; dunes
  bound by marram clumps.
- Waterside life (RT12): jetties and piers of weathered timber; harbours with
  moorings; gulls, birds in flight, and shoals beneath clear water.

## RT13 — `lib/countryside`

A new `no_std` crate holding the countryside's *layout*, with no notion of
how it is drawn: lib/raytrace sets its records out as geometry, and WinterSun
(WS30, WS35) as world objects. It obeys WinterSun's world rules, which are
the stricter: random access keyed by place rather than drawn in sequence, so
a region laid out in pieces matches one laid out whole and neighbouring tiles
meet without a seam; placement by priority rule, not first come; `f64` with
`mathf` only and integer tie-breaks. Vocabularies with meaning in a world —
which settlement kinds exist, what a building is — stay with each consumer;
the crate holds the geometry and the algorithms.

- **Ways**: a looped network (relative-neighbourhood graph) between
  settlements, farmsteads and gateways, ranked road, lane, track and path,
  routed over the terrain by `lib/terrain`'s router with each rank's pricing.
- **Parcels**: the land between ways and water split into fields by recursive
  cuts biased to the contour and to the ways, sized by rank and terrain;
  never crossing water or a way.
- **Boundaries**: each parcel edge a hedge, wall, fence or ditch by region and
  rock, with gates where a track meets it and the gaps a gate leaves.
- **Land use**: arable by crop, pasture, meadow, orchard, vineyard, woodlot,
  left to grow over; one bale kind per cut field.
- **Settlements**: farmsteads of a house and outbuildings about a yard;
  villages of plots by frontage along their streets, with front and back
  gardens and their fences.

## RT14–RT19 — Fields and what grows in them

Boundaries are instanced geometry along their polylines: hedges as dense
shrub masses of leaf-bearing prototypes, varying in height and width, with a
standard tree now and then and a repaired gap of post and rail where one
collapsed; dry-stone walls coursed from individual stones with a batter and
coping, collapsing here and there, lichen on their faces and grass at their
foot; fences of posts and rails, true where kept and leaning, moss-green and
missing rails where not; gates hung on posts, open, shut or off a hinge, with
mud and hoof-churned ruts in a pasture's gateway. Crops are lawns of their own
blades and heads in drilled rows with tramlines; cut fields stand stubble and
bales of one kind. Flowers and weeds are drawn per species in patches seeded
by the field's history, never sprinkled evenly.

## RT20–RT25 — Buildings and settlements

Masonry is built from individual stones or bricks, each a prototype drawn
from its own key and placed in courses with mortar recessed between them;
weathering is geometry and pigment together (RT31). Thatch is a deep layered
roof of straw over a ridge; timber is weatherboard silvering and splitting
with age. Night interiors are real rooms behind real glass, lit by lamps the
scene holds, so a window glows because a room is lit.

## RT26–RT31 — Nature detail

Bark near the eye becomes a displaced shell over its limb, intersected
exactly, so ridges catch light at the silhouette; far off the existing
normal-only relief settles to its mean. Mud cracks are plates of a Voronoi
desiccation pattern with curled, undercut rims. Snow is accumulated by wind
over the land's shape, drifting into lee hollows and against walls.

Mountains (RT30) need an erosion law that carves as it gathers: channels
deepening where droplets converge and branching up every slope, the beds of
rivers and roads kept, and talus below crags — so that the budget can then be
spent on droplets, which today only smooth the land.

## RT37, RT38 — Woods and stones by the budget

- Woods to the horizon (RT37): at *Maximum* a wood stands out to as far as
  its trees still span a pixel. Its places are sown at its own spacing near
  the eye and coarser beyond only where their candidates — about 80 bytes a
  place, a dozen places a tree — would not otherwise fit the budget, and never
  coarser than its trees keep apart.
- Stones in patches (RT38): eight rocks a scene, strewn where a noise field
  and the slope say — boulder fields, scree fanning below crags, pebbles in
  the hollows — never an even sprinkle; leaf litter as thick as the crowns
  above shed.

## RT32–RT36 — New scene families

Each is a `Setting` of its own, composed and traced by the same tracer:
the moon as a lit body in the sky; aerial vantages over the lands the
countryside lays out; planets with atmospheres from the existing physical
model at their own radius and composition, gas giants as banded flow fields,
rings as a thin particle disc casting and catching shadow; macro scenes with
a refracting drop and a shallow depth of field; caves as carved volumes with
speleothems, pools and emissive crystal.

## RT39 — Scene detail

One generator, two profiles: `Draft::new(setting, seed, size, detail)` takes a
`Detail`, which picks one table of densities (`detail.rs`) that composing and
gathering read, and nothing else:

| | *Simple* | *Maximum* |
|---|---|---|
| objects a scene holds | 131 072 | 524 288 |
| trees a meadow's, a valley's or a building's wood stands | 20 000 | 120 000 |
| a forest's | 90 000 | 270 000 |
| a winter wood's | 40 000 | 160 000 |
| a canyon's | 4000 | 40 000 |
| a rocky desert's saguaros and scrub | 600 and 900 | 6000 and 9000 |
| rays a radiosity record gathers | 256, eight rows by 32 | 1024, sixteen by 64 |
| the least a record holds for, of the picture's height | a 240th | a 480th |
| records a square of the picture, and pixels a record | 1600 and 64 | 6400 and 16 |

A wood's cap also bounds how far it is sown, so a *Simple* wood reaches about
1.3 km where a *Maximum* one carries on to 2–4 km. A profile changes how much
is set out, never what a setting is: the land, the eye, the hour and the
weather are drawn before any wood grows, and each wood and the sward draw from
streams of their own keyed from one draw, so however many draws one wood takes
a seed shows the same place — woods and sward included — at either.
`Detail::peak` states each profile's budget, 384 MiB and 2 GiB, which the
session weighs against the memory band (`plans/NEW-DESKTOP-SETTINGS.md`
DS24); each profile has its own measured progress shares.

Measured at 1920×1080 on a 24-thread desktop preparing across 8 threads, a
landscape prepares at *Simple* in 0.3–4.0 s, holding at most 324 MB at its
peak and 323 MB once prepared, and at *Maximum* in 0.8–33 s, at most 583 MB and
580 MB — most of *Maximum*'s time its radiosity records, and the most memory
a mountain lake's, its caustics (RT8) beside its land. RT37's woods to the
horizon, RT38's stones, and what later items buy are *Maximum*'s to spend the
rest of its budget on.

Tests: a seed showing the same land, eye, sun, weather and sward at either
detail; *Simple* standing fewer trees, nearer the eye, within its room of
objects; both profiles' records laid the same on any runner; every setting
preparing at *Simple*; and a draft's progress climbing steadily at either.

## RT40 — One land generator, the tracer's and WinterSun's

WinterSun is to build its realm's land with this generator at Maximum detail
(`plans/WINTERSUN.md` WS25–WS31, WS35), so the land's stages move out of
`lib/raytrace` into renderer-neutral crates beside `lib/terrain` and
`lib/countryside` (RT13), held to WinterSun's world rules: a pure function of
the seed and the place, bit for bit on every Tier-1 target and across any
runner, a region laid out in pieces matching one laid out whole, and bounded
by its working set, its digest folded into WinterSun's.

- Relief, wear and droplets run over tiles whose seams neighbouring pieces
  agree on; `lib/terrain`'s droplets already leave the same ground on any
  runner, and seams across pieces are the part still to build.
- Woods are thinned by a keyed priority rule — a place stands unless a taller
  place within its room outranks it — rather than in one sequence, so a
  district's trees need only its neighbours' places, as WinterSun's scatter
  needs.
- The grids refined about the eye become the tracer's choice of which pieces
  to refine, not the generator's.
- The tracer keeps what is its own: the eye, the picture, shading, light and
  the reveal.

## RT41 — Local adaptation

One exposure holds about the range a display shows; a dark room with a sunlit
window, a wood's depths under a bright sky, or snow beside a shaded wall hold
far more. The tracer compresses the excess as a photographer would, gently,
and only there (Reinhard et al., "Photographic Tone Reproduction for Digital
Images", 2002): the camera's correction, like the filmic curve, adding and
moving no light (`adapt.rs`).

- **Measured with the meter.** Once the global meter has set the exposure and
  the glare — unchanged, so a scene keeps the exposure it had — the meter
  traces about 9216 film points at the picture's shape (128 by 72 on a
  widescreen picture; one to sixteen pixels in a small one), four samples
  apiece, exposed and with the glare as an eye sample has them. They are
  splatted into a bilateral grid over the film and log luminance (Chen,
  Paris and Durand, 2007) — a cell eight points a side, a layer a stop deep,
  twelve stops either side of the key, clamped there — and blurred 1 4 6 4 1
  along each axis: for every place and brightness, the mean log luminance of
  the like-lit ground about it, the base layer of Durand and Dorsey's
  decomposition (2002). A window and the room about it fall in different
  layers, so neither haloes the other.
- **Applied per sample, in any order.** A sample reads the grid at its own
  film position and exposed luminance, before the filmic curve, so a pixel
  comes out the same on any core in any order and every step of the reveal is
  its pixel's own. A read is a weighed blend of the bases about it and the
  key, a sample's worth of weight pulling an unmeasured base back to the key.
- **Only where needed.** Within a stop and a half of the key the correction
  is nought. Beyond, it eases in over half a stop to half the excess and
  settles toward at most two stops down for a highlight and one up for a
  shadow (a hyperbolic tangent), so a sky keeps its clouds and a room its
  dark corners. A grid none of whose bases lie that far out is not kept, and
  the scene is traced exactly as without one.
- **Detail kept.** The correction follows the base alone, so texture within a
  region keeps its contrast; the sun's disc and the glints off water and glass
  still blow out.

Measured on 640 by 360 renders, the change is gentle: a forest's shaded trunks
lift by about a quarter in their encoded level and its sky draws down a few
levels; a valley under a low sun shows its cirrus and its blue more clearly
with no halo at the tree line; an overcast meadow barely moves. Every outdoor
setting keeps a grid, its sky lying past what one exposure holds; the studio,
whose lamps are set by hand, and the night and dusk still lifes keep none.

Tests: a frame within a display's range keeping no grid and so traced as
without one; a dark wall's lift and a bright window's darkening each alike up
to the edge between them, with no band of halo either side, detail kept
within the window and the sun still blowing out; the correction within its
bounds, easing in without a step; and the grid the same however the meter's
work was divided.
