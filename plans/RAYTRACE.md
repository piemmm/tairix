# RAYTRACE — photorealistic scenes for the ray-traced screensaver

Binding under `AGENTS.md`. This plan owns what `lib/raytrace` composes and how
it renders it: the scene families, the countryside, water, buildings and
nature it sets out, the sampling and reconstruction a pixel is traced with,
and the screensaver's reveal, progress readout and saved pictures.

Read first: `docs/src/lib/raytrace.md` (the tracer as built),
`plans/NEW-DESKTOP-SETTINGS.md` DS21 (the screensaver and its pane),
`plans/WINTERSUN.md` WS29, WS30 and WS35 (the world generator that shares the
countryside layout), `plans/FIX-DESKTOP.md` (no I/O on the serve loop) and
`plans/OPEN-DEFECTS.md` D462–D466 and D483–D487 (the tracer's open defects).

## Ledger

| ID | Item | Status |
|---|---|---|
| RT1 | Smooth reveal: a cubic B-spline over the reveal's grids, a blur coming into focus with no point a peak or a cross; each frame repaints and marks only what its steps change; the finished picture is every pixel's own trace | done |
| RT2 | Progress readout: *Generating scene... N%* while a scene is prepared, then *Rendering... N%*, small and mid-grey in the lower right, gone once the picture is whole | done |
| RT3 | Saving finished pictures: `screensaver.raytrace.save` keeps each whole picture as a PNG in the user's `Documents/Pictures/Raytracing/`, with no limit on how many | done |
| RT4 | Highest quality, always: no sample governor; a reconstruction filter and sampling rounds that leave no jagged or noisy edge | done |
| RT5 | A 160 s preparation budget spent on detail, with every unit of preparation bounded and parallel (closes D464) | planned |
| RT6 | Wind on water without repetition: a spectrum of many wave components under gusting patches; the open sea's grid no longer tiles in view; detail finer than a pixel becomes roughness | done |
| RT7 | The water defects: ripples aliasing far off, reflections lost at grazing angles, the open sea stopping short of the horizon, and the noise seams under every pattern (D490–D494) | done |
| RT8 | Caustics on and under every body of water, from the light the surface itself focuses | planned |
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
  machine preparing across 8 threads, and hold up to 500 MB (or the free
  memory, if less). Both are spent on detail. Every unit of preparation stays
  small enough for a caller answering a frame to stop after it; every
  allocation is fallible, so a smaller machine gets a plainer scene, never an
  abort.
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
point. A frame repaints the cells those reaches cover, each once, across the
compositor's pool, and marks a cover of 16-pixel tiles, their side doubled
until they make at most 128 rectangles: a compositor merges each rectangle
against the rest, so a scattered frame's own rectangles would cost more than
whole tiles, and the box it spans is the screen. A buffer the compositor lets
go is painted afresh from what the painter keeps, and nothing is traced
again.

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
a margin in from the lower-right corner. It is repainted only when the whole
percentage changes and taken down once the picture is whole, the scene is
refused, or the screensaver goes.

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

Up to 160 s of preparation buys density: droplet erosion, tree and plant
counts, radiosity records, scattered stones and litter, and the countryside's
pieces. D464's units are made bounded and parallel first: non-tree prototypes
grow one a unit across the runner, the woods' seedling sort and the far grid's
border blend are split into bands, and canopy grids are sized to their tiers
rather than rounded to a power of two.

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

Light refracted or reflected by a water surface converges and diverges as the
surface curves. A point beneath or beside water gathers the sun through the
surface it lies behind, its irradiance scaled by the ratio of areas the
surface's refraction maps between (the Jacobian of the mapping from surface
to receiver, from the surface's second derivatives), so the bright network on
a stream bed and the dancing light under a bridge are what the waves focus.

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

## RT32–RT36 — New scene families

Each is a `Setting` of its own, composed and traced by the same tracer:
the moon as a lit body in the sky; aerial vantages over the lands the
countryside lays out; planets with atmospheres from the existing physical
model at their own radius and composition, gas giants as banded flow fields,
rings as a thin particle disc casting and catching shadow; macro scenes with
a refracting drop and a shallow depth of field; caves as carved volumes with
speleothems, pools and emissive crystal.
