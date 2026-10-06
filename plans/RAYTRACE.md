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
| RT1 | Smooth reveal: a cubic B-spline over the reveal's grids, a blur coming into focus with no point a peak or a cross; each paint repaints and marks only what its steps change, the paints half a second apart while the picture forms and slowing with the share shown to at most 3 s, each change crossfaded in over the wait until the next, only the tiles it touches, until a twentieth of the picture is shown; the finished picture is every pixel's own trace | done |
| RT2 | Progress readout: *Generating scene... N%* while a scene is prepared, then *Rendering... N%*, small and mid-grey in the lower right, gone once the picture is whole | done |
| RT3 | Saving finished pictures: `screensaver.raytrace.save` keeps each whole picture as a PNG in the user's `Documents/Pictures/Raytracing/`, with no limit on how many | done |
| RT4 | Highest quality, always: no sample governor; a reconstruction filter and sampling rounds that leave no jagged or noisy edge | done |
| RT5 | A 160 s preparation budget, every unit of preparation bounded and parallel (closes D464), spent where it measurably buys realism: radiosity records, and woods reaching twice as far | done |
| RT6 | Wind on water without repetition: a spectrum of many wave components under gusting patches; the open sea's grid no longer tiles in view; detail finer than a pixel becomes roughness | done |
| RT7 | The water defects: ripples aliasing far off, reflections lost at grazing angles, the open sea stopping short of the horizon, and the noise seams under every pattern (D490–D494) | done |
| RT8 | Caustics on and under every body of water, from the light the surface itself focuses: beams over the water the picture shows, every one gathered, refracted onto beds and what stands in the water and reflected onto what stands over it | done |
| RT9 | Water's edge: reeds and bulrushes along banks, pondweed and water lilies in still water | done |
| RT10 | Streams close up: pools and riffles, ledges, bars and cut banks, running water over stones worn round, boulders, drift and weeds | done |
| RT11 | Deltas, beaches and eroding coasts: sand with driftwood, footprints, paw prints, shells, stones and wrack; cliffs, sandbanks, marram binding the dunes | planned |
| RT12 | Jetties, harbours and fishing villages; gulls and other birds; shoals of fish | planned |
| RT13 | `lib/countryside`: the renderer-neutral countryside layout — parcels, boundaries, gates, ways, land use, farmsteads, villages and plots — shared with WinterSun | planned |
| RT14 | Field boundaries as geometry: hedgerows with standard trees and repaired gaps, dry-stone walls, wooden fences kept and decaying, gates open, shut and broken | planned |
| RT15 | Ways between fields: green lanes between walls or hedges, tracks to gateways churned to mud where stock gather, roads graded by where they lead | planned |
| RT16 | Crops: maize, wheat, barley, oats, rapeseed and grass ley, in rows and tramlines by season; cut fields with one kind of bale or stack each | planned |
| RT17 | Pasture, orchards and vineyards: cow-pats with lusher grass about them, fruit trees in rows, vines on trellised rows | planned |
| RT18 | Overgrown ground: brambles, nettles, docks, scrub and tall weeds where land is left | planned |
| RT19 | Flowers and weeds by the score, in patches rather than an even sprinkle: clover, dandelions and their clocks shedding seed, thistles, buttercups, daisies, poppies, roses, ivy, wisteria and more | planned |
| RT20 | Masonry as geometry: rubble and coursed stone of individual imperfect stones in mortar; brick walls of individual bricks, reclaimed, marked, chipped and now and then missing | done |
| RT21 | Moss and lichen as geometry: fibrous moss cushions and crustose lichen on stone, wall, bark and roof | done |
| RT22 | Farm buildings: stone farmhouses under thatch, slate or tile; timber outbuildings weathered by age; vines, ivy and wisteria climbing the walls | planned |
| RT23 | Villages: houses on plots along their roads, front and back gardens, closeboard, post-and-rail or no fences with garden gates, chimneys with smoke now and then | planned |
| RT24 | Night in a village: lit windows, curtains drawn or left open, interiors lit behind them | planned |
| RT25 | Farmhouse interiors: kitchen, range, sink, table and chairs, dressers, stone floors and rugs, by day and by night, the air dusty enough to show a sunbeam | planned |
| RT26 | Bark in true relief: ridges, plates and corrugation as displaced geometry near the eye | done |
| RT27 | Stumps, sawn and splintered, some with new shoots at the foot | done |
| RT28 | Mud cracks as modelled geometry: polygonal plates with curled edges and real depth | done |
| RT29 | Snow that lies unevenly and drifts; snowmen never quite round | done |
| RT30 | Mountains near and far, ridged and eroded, some snow-capped | planned |
| RT31 | Weathered stone everywhere: erosion, chips, cracks, lichen and moss on every stone structure | done |
| RT32 | The moon by day or by night, at its phase and lit by earthshine, or not at all | done |
| RT33 | Aerial views: landscapes from altitude, and detailed abstract views | planned |
| RT34 | Planetary scenes: gas giants, ringed planets, earthlike and Mars-like worlds, views from icy moons, nebulae, stars, and the sun with its corona and spots | planned |
| RT35 | Macro shots: a leaf with a drop hanging from it, focused on the drop over a blurred landscape, in many variations | planned |
| RT36 | Caves: stalactites, stalagmites, pools, iridescent water, crystal outcrops that glow | planned |
| RT37 | Woods to the horizon: at *Maximum* a wood of trees reaches as far as its trees still span a pixel, stood one by one near the eye and hashed from cells past them, as thickly far off as near | done |
| RT38 | Stones in patches: boulder fields, scree below crags and pebbles in drifts, strewn by noise and slope from eight rocks a scene, and leaf litter as thick as the canopy sheds | done |
| RT39 | Scene detail: one generator at two profiles, *Simple* (low memory, every setting, the screensaver's default) and *Maximum* (up to 2 GB, all the realism the budget buys), chosen by `screensaver.raytrace.detail` | done |
| RT40 | One land generator, the tracer's and WinterSun's: the land's stages renderer-neutral, keyed by place and seam-free, in crates WinterSun builds its realm's land with at *Maximum* | planned |
| RT41 | Local adaptation, a gentle photographic HDR: a sky seen from a dark room or over a dark wood keeps its detail and the room its shadows, only the range a display cannot hold compressed, and a scene one exposure holds left exactly as it is | done |
| RT42 | Lights as measured: the sun at its true size with its limb darkened, bent by the standard atmosphere; the full moon at its true size, brightness and colour; Allen's stars with Tycho-2's colours; lamps in lumens and candelas; the dusk and night still lifes under the open sky; cirrus lit as ice | done |
| RT43 | Water clouds lit by multiple scattering that holds for thin cloud too, in place of the octave, powder and ambient approximations | done |
| RT44 | Cloud to the horizon: decks over the Earth's curve, mapped in levels about the eye out to as far as they can be seen, the air beneath them shaded by them, each place lit by the sun as it stands there | done |
| RT45 | No primitive stand-ins: every limb's foot flares into the roots it grows out of, every break torn, every end tapered, cut or broken as the real thing's is, every leaf and flower worn as the real one is — trees, snags, stumps, fallen trunks and their root plates, the snowman's carrot and sticks, saguaro ribs, spines and areoles, a palm's and a shrub's foot, a reed's tip and plume, a reedmace's spike, a lily's pads and flowers and a crowfoot's flower | done |

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
- **No primitive stand-ins.** Nothing a scene sets out reads as a capsule,
  cone, cylinder, sphere or box. A thing's ends, breaks and joins are
  modelled as the real one's form: wood snapped across its grain is torn and
  splintered, a root grows out of the flare its trunk swells in, a twig and a
  root taper to fine tips, a cut is a face with its bark cut through. Nature
  is never regular, and it ages: decay, rot, weathering and drying show.
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
in about as many paints: half a second while the coarse passes form the
picture, 1.5 s as the 2 px pass begins (every point of the 4 px grid shown),
and never more than 3 s; until the first steps come, the loop looks again a
scene frame later. Fine detail changes too little between paints for a
quicker cadence to show, and every paint costs a wake, a compositor pass and a
present whatever it carries. A buffer to lay afresh and a picture's last steps
are painted at once. Steps collected between paints wait for the next, and
those of a refused scene go with it.

Each paint's change is crossfaded in over the wait until the next
(`saver::raytrace::crossfade`), so the picture moves on continuously rather
than in steps, until a twentieth of the picture is shown: past it a change is
dots too small for a fade to show, and each paint is laid straight on. The
painter paints into a picture of the crossfade's own; a fade keeps what the
screen showed over the 16-pixel tiles its change touches and blends only
those, a scene frame at a time (`lib/raster`'s `mix_span`, one pass, about
half the cost of a copy and a composite), marking their cover. Its two
screen-sized pictures are taken only where the heap and the memory band let a
disposable cache of the machine's share hold them, and let go once the
twentieth is passed or the band wants the memory back, the change under way
first laid on whole; once the compositor lets the window's buffer go, which is
then painted afresh whole; and with a refused scene, whose rest is black.
Under reduced motion every change is laid straight on. Measured, a
fade frame blends about 2 ns a pixel on one core, 4.5 ms for a whole 1080p
screen before the pool shares it out.

## RT2 — Progress readout

`Draft::progress` answers how far preparation is, in thousandths, never
falling back and reaching a thousand only when the scene is ready. Each stage
carries a share of the whole measured over the settings; a land's build weighs
its own stages by their items times the measured cost of one, so its share
keeps pace whether droplets or wear dominate, and the sky counts the units of
its air's tables and its banks' levels as they are built. The shares are averages: a
setting whose land is most of its work (the desert) runs behind the clock in
its first half and catches up. Tracing reports steps traced of the reveal's
count. The engine reports both in the trace desk's status.

The readout is its own small window over the screensaver's, a transient of
it so it rises with it: *Generating scene... N%* while the scene is prepared,
*Rendering... N%* while it is traced, in mid-grey at the theme's caption size,
a margin in from the lower-right corner. It is brought up to date each time
the loop collects — at each paint and each frame of a fade — and four times a
second while a scene is prepared on its own thread,
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
128 until its samples agree. A pixel whose samples lean on a surface that
gathers its light by random rays — seen directly, or through clear water or
in a mirror — takes at least 64, as its first samples can all miss the few
paths that bring most of its light, and none stops while what it shows hangs
on its brightest sample; such a surface's bounce is never ended by Russian
roulette. Measured,
this costs 2–27% more tracing, most where shaded foliage and grass fill the
picture. Its samples are drawn from a Gaussian
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
- A land's fills and settle passes run four rows a core, its water grids
  about 8192 vertices a core, its droplets in turns of tiles across the
  runner (`lib/terrain`), its seal in bands; a stream's eye is sited with its
  marks weighed across the runner, its bed's stones are read, thinned and set
  out a few thousand a unit, and its flow solved about 16 384 points a core;
  a wood's trees are thinned 512 a unit; the
  sky's tables a row a core, a cloud bank's light a quarter layer a core;
  a radiosity record's hemisphere is gathered 64 rays a core a unit, and the
  records laid so far are indexed a slice at a time before the next grid
  looks for sites none of them holds for.
- A scene's objects' boxes are found across the runner a band at a time and
  its hierarchy built a slice at a time; the meter takes eight points, then
  sixteen adaptation samples, a core a unit.

Measured at 1920×1080 across 8 threads, no unit takes more than about 10 ms
at either detail but the one that begins a large scene's hierarchy, which can
take about 20 ms at *Maximum* (D517), and bounding them changed no pixel.

At *Maximum* (RT39) the budget is spent where it measurably buys realism.
Radiosity records hold a hemisphere of 1024 rays, are laid down to half the
radius *Simple* keeps and four times as many to a square, and hold within
fifteen degrees of turn, as their own rule says: the light a wood's trunks gather from about them, which records
spread over trunks too thin for them missed. Woods may stand six to a
hundred times as many trees one by one, out to 2–3 km where they stopped at
about 1.3 km, and a wood of trees carries on past them (RT37). Droplet erosion is not made denser:
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

## RT9, RT11, RT12 — Water's edge and the coast

- Banks and still water (RT9): reeds and reedmace stand in the shallows and
  on wet, level banks off any way; lilies and pondweed float where the water
  is still and as deep as each roots in; none stands where the woods' shade
  hides most of the sky, and the floating plants die back over winter. Depth
  and stillness are the land's fresh water against the ground and how steeply
  its surface falls (`Land::water_level`), or the lake a land's sea stands
  for. Each plant is grown into square patches (`waterside`) at statures from
  0.55 to 1.1 of its kind's, the shorter the thinner; a resumable job
  (`compose::waterside`) run between the woods' shade and the sward reads a
  lattice of 0.75 m cells within 45 m of the eye and of 3.75 m cells beyond to
  the detail's reach (`Simple` 250 m, at most 8000 patches; `Maximum` 700 m,
  40 000, the nearest the eye kept where more would stand), each cell a patch
  of the plant its place suits best or none, as likely as it suits it and as
  tall and thick, so beds thin toward their edges, and none whose square
  reaches a piece — a boulder, drift, a trunk — though ground kept open of
  pieces, a pond or the eye's own, bars no patch. A plant's clumps and beds
  are planned as prototypes only once a patch of one is set out, so a plant
  no water in the scene suits costs it none of the scene's 96, and past the
  96 a patch never planned is left out. A cell's patch is drawn from its place and the light there alone, so
  it is the same on any runner; the woods differ by detail, so where their
  shade falls the water's edge does too. Measured at 960×540 at *Simple*, a
  frozen pond's reeds about the eye trace 10% longer and a canyon's far beds
  14%, preparation takes about 0.1 s more, and a scene holds at most 5 MB
  more; at *Maximum* a winter scene prepares about 3 s longer, its radiosity
  records' rays crossing the reeds.
- A clump (a patch no wider than 1.5 m) is drawn in full and a bed beyond
  plainer (`waterside::Grain`): a pad's mesh in 36 spokes or 14, a petal's in
  9 rows by 6 or 4 by 2, a lily's 56 stamens or 18, the stalks under the
  water only in a clump.
- Deltas and coasts (RT11): distributaries building lobes into still water;
  beaches graded by exposure — sand, shingle and stones — with driftwood and
  wrack along the tide line, shells, footprints and paw prints in tracks that
  wander; cliffs where relief meets the sea, slumping and undercut; dunes
  bound by marram clumps.
- Waterside life (RT12): jetties and piers of weathered timber; harbours with
  moorings; gulls, birds in flight, and shoals beneath clear water.

## RT10 — Streams close up

`Setting::Stream`: a stream a few metres across in the narrow valley it has
cut, seen from the margin its low water leaves — on the bar across from the
deep water three times in four — the eye 1.2–1.7 m up and looking along it
toward a ledge or a riffle where one lies 8–30 m ahead. The spot is ranked,
not filtered: near the land's middle where the stream runs 2.5–9 m across
first, then between dry banks, then falling between 0.3 % and 3 %, so a land
whose streams offer no such reach still gets the best they do; an eye that
stands by no stream looks over its dale with no brook laid.

- **The channel** (`channel`). A river's course is counted in units of pool
  and riffle (`Mark::phase`), six widths apart on a gentle stream and
  shortening toward a step every width or two as it steepens (Leopold and
  Wolman 1957; Montgomery and Buffington 1997), each mark carrying its run,
  its bend (`turn`, the curvature through it and its neighbours) and its
  brim's fall over 10 m either way. A unit is a crest, the riffle falling
  quickly below it over a quarter to two fifths of its length, and the pool
  after; where bedded rock holds a reach (`Rivers::ledges`: slate and
  limestone 0.35, sandstone 0.3, granite 0.15), two or three units' fall
  gathers at one ledge over a plunge pool scoured by its drop. The bed's long
  profile is a staircase, and at low water the water's follows it — as
  `1 − flowing⁴` of it over a ledge and `1 − flowing²` down a riffle, not at
  all in spate —
  so each pool stands ponded behind the crest below it; the mean water stands
  where its depth's share has it. In a pool the deepest line swings to one
  bank — along a straight reach to one side a unit's length along and to the
  other the next, over a ledge's units as a riffle's, and to the outside of a
  bend as it tightens — the bed rising steeply to the cut bank and gently up the bar on
  the other side; across a riffle the bed is flat but for its margins. The
  bed is lumped by a fifth of its depth at most, in lumps 2.5 m to 40 cm
  long, enveloped so it never leaves its deepest and its brim, and its
  breadth wanders 8 % either way, so its water's edge wanders as a stream's
  does. Along its thalweg the water carries what runs over the crest at
  Manning's speed down a typical riffle (`n` = 0.04), fast where shallow and
  slow where deep; across, Manning's law at each depth, never past Froude
  0.9 (Grant 1997).
- **Banks.** Each bank rises from the brim over a face broad on a bar's side
  and narrow and steep where a pool cuts it, broken by a bench a share of its
  height up, its top lifted into a levee or let down a little, slumped and
  bulging along its faces, and settling to the land beyond, or falling to it
  where the land lies below a perched river's brim. Sand lies up the
  bars and in the deep of the pools, in patches; a ledge's lip, tongue and
  the slab above it are bare rock, stained dark with the water's film; a
  bank's faces are its alluvium's earth, a cut face showing its beds of loam
  and gravel. The channel has the say over what was laid within its banks'
  faces, and stands there as it carved them against the droplets run over
  the land after, which only rill the land beyond.
- **The water as it runs.** A land's rivers may run below bankfull
  (`Rivers::flowing`, 0.28–0.5 here); bankfull depth follows hydraulic
  geometry, a fifth of the width to the two-thirds power. The bed's bare
  margin is soaked, its banks above it damp; nothing roots in what its floods
  scour or the fringe up its banks, where grass grows sparse and short, but
  pioneer plants take up to 0.3 of a bar's top. The ground's grass is painted
  only off the bed; gravel lies no steeper than it rests at, the steeper
  walls of a low channel showing the bank's earth.
- **Ground seen close.** The eye stands over a near grid of 4.7 cm cells. The
  ground's grain runs from crumbs and fine pebbles to fine sand (55, 170 and
  520 grains a metre), a scoured bed is gravel of the land's own rock (90
  granules a metre) mottled by cobbles too small to lay (14 a metre), sand
  mottled finer, and its relief runs five octaves from 7 cm to about a
  millimetre, sharing the grain's depth between them; each holds only as finely as the footprint resolves it and
  settles to its mean beyond, its relief lending its slope to the roughness.
  Bedded rock breaks along two sets of upright joints near square to each
  other, opening and closing along their strike, and parts along its beds.
  Scree keeps to drained ground. A sward's shade on the ground is blended
  between its cells, and a cell that barely thrives grows short as well as
  sparse.
- **Stones by the water that carried them** (`compose::stones`). The bed's
  median is the stone its bankfull flow just stirs (Shields, τ* = 0.045), at
  most 25 cm, the rest lognormal about it (σ 1–1.5 in φ). Each stone has come
  a distance drawn evenly from nought to the run above it, every stretch
  upstream feeding the bed alike, and is worn by that distance over its rock's
  rounding length (granite 8 km, slate 4 km, sandstone 3 km, limestone
  2.5 km); slate splits along its cleavage as fast as it rounds, so its wear
  stays at 0.08, flat and angular. A bed grows four shapes of its rock at four
  wears from fresh to the farthest-carried. Stones are drawn from a lattice per
  size class about the eye, a cell one stone or none, kept only where they span
  the detail's pixels at their distance (5 at *Simple*, 3 at *Maximum*), lie
  in a channel, on ground no steeper than gravel rests on, sparser on its
  sand and its bare rock; ranked largest first, each laid where it keeps clear
  of every stone laid before it by four fifths of its breadth, up to 40 000 at
  *Simple* and 160 000 at *Maximum*. Each lies on its flattest side, its
  length across the stream and its upstream end dipping 8–25° (imbrication),
  a fifth to two fifths of its height bedded, wet beneath the water and dry
  above. A rock's facets carry no material of their own, so each stone takes
  the one it is placed in.
- **Boulders and outcrops.** A lattice of 1.6 m cells holds, on a bank's face
  where its rock outcrops (`Rivers::outcrops`), a block of that rock
  0.6–1.6 m across standing square out of the face, buried more than half
  in it; elsewhere boulders fallen from the banks, likelier and larger where a
  pool cuts the bank or its rock outcrops, 0.35–1.3 m and fresh, each rolled
  down whatever is steeper over its own breadth than a boulder rests on (a
  rise of a half) until the ground holds it, often into the water at the
  bank's foot; now and then one lies out in the channel. Above the water both
  are mossed, moss mantling the faces turned to the sky in patches. Each
  claims its ground, so no drift is laid through it, and stands in the flow as
  long and as broad as it lies along and across the current. A bank's
  rock is never painted on its face: it stands out of the earth as these
  blocks, and only a ledge bares rock across the bed.
- **Drift.** The floods leave 7–14 pieces of the streamside trees' wood in
  the stretch the eye looks over, branches and trunks of their kind in
  weathered bark: stranded half in the water up a bar, jammed across the flow,
  waterlogged and sunk along the bed of a pool, fallen in from a bank with
  their tops swung downstream; and where the stream is under 6 m across, a
  trunk undercut from one bank spans it about one scene in three, the current
  piling branches against its upstream side more often than not. Each rests on
  the ground or the bed beneath its ends, and what lies in the water joins the
  flow as obstructions. A break is torn, not rounded: the trunk's tube is left
  open there (`Tube::opened`) and closed by a jagged face of weathered wood,
  bristling with splinters longest about the rim and hung with tatters of bark.
- **Weeds** (`compose::waterside`). The water's edge plants come to the
  stream: reeds and reedmace in its slack water and silt, pondweed in its
  pools, and water-crowfoot — stems streaming down the current just beneath
  the surface, tufted with thread-fine leaves, white-flowered in spring and
  summer — where it runs over gravel, all set out once the flow has shaped
  the water they float on. Nothing roots on ground the floods scour
  bare; the still-water plants root thicker in silt than over gravel, crowfoot
  the reverse; none stands within 2.5 m of the eye.
- **Wear** (`rock`). A rock is cut from its radius in every direction of a
  subdivided icosphere, its fractures and a slate's cleavage planes cut
  through it, then worn by the mean-curvature term of Bloore's flow (Bloore
  1977; Domokos and Gibbons 2012): the surface moves in as fast as it curves
  out and never out, so edges round first and hollows last, each vertex kept
  to its own ray so the facets hold their shape. The cotangent Laplacian
  (Meyer et al. 2003) is refreshed every eight steps, 48 steps a unit; its
  proportions are set after wear.
- **The surface** (`stream`). The water's own finer grid about the eye
  (`NearWater`, 15.2 m either way — whole cells of the far water grid — 1024
  cells at *Simple* and 2048 at *Maximum*) is left out of the far water grid
  entirely and meets it along its border, its own surface giving way to the
  far grid's over the 1.5 m within it (`land::seam`), so a ray meets one
  water's surface there and not two; the land reads its water within it from
  it. It is shaped by the flow's steady answer to its obstructions, solved
  linearly on a grid along the stream (Lamb §§ 246–247; Wehausen and Laitone
  1960) over the channel's sections read every 10 cm, 1.2 near-grid reaches
  behind the eye and 2.2 ahead of it whichever way it looks: potential flow over a finite depth with gravity and surface
  tension, a stone beneath the surface a rise in the bed (at most 0.6 of the
  depth) and one through it a Rankine half-body as broad as its waterline,
  Rayleigh damping keeping waves to the way the flow carries them and the
  eddies' viscosity (10⁻³ m²/s, of the order a stony stream's turbulence
  holds; Elder 1959) damping the short ones sooner, so a train of standing
  waves behind a log across the flow dies within a few of its lengths. The answer
  is worked at four depths and three speeds, six pairs of transforms, and each
  place takes the four about its own depth and speed. A stone parts the water
  in proportion to the stream's speed, so its answer falls away with the
  speed as the bed's does and slower water than the slowest answers as that
  does scaled by the square of its speed. The stones are laid into the grid a
  band of rows a core, each row taking them in one order. No place
  rises past the velocity head or falls within 0.15 of the bed. Foam comes
  only from the flow's own answer: where a wave stands steeper than 0.45, in
  the wake a stone through water faster than 0.5 m/s sheds, and in streaks
  where the water lands below a ledge, a quarter steeper within the 0.4 m its
  jet throws above than it runs there, its tongue pouring glassy; it bursts within
  0.8 s, whitening only the water an obstruction stirs. Then, unseen by its
  breaking, water nearing critical churns in boils as big as its depth (by
  6 % of it), and water shallower than 8 cm drapes over its gravel, but not
  over bare rock. The grid carries the foam, and the shading breaks it into
  bubbles and streaks. The caustics count the grid's own slopes in how far its
  beams stray.

Measured on Stream at 1920×1080 preparing across 8 threads, seeds 0–2: at
*Simple* in 7.0–9.3 s, holding at most 373 MB; at *Maximum* in 65–102 s, at
most 840 MB at its peak and once prepared.
The flow is solved on 2048 × 256 points at *Simple* and 4096 × 512 at
*Maximum*, holding at most 34 MB and 134 MB while it is solved, each grid
reserved only once the solve reaches it.

Tests: a valley along its compass heading; a hard cap wearing its hardness
times as slowly; a course counted in units by its width and its fall, its
bends read; water over the deepest line and under the brim; a stepped low
water and an even one in spate; pools deeper than crests and the bed keeping
its depth; a ledge's drop of bare rock into its plunge pool; a bend's swing,
cut bank and sandy bar; the water fastest where shallowest; a bank from brim
to land; a channel standing as carved against the droplets; a transform
matching the transform written out, undoing itself and the same on any
runner; a stone worn only inward along its rays, rounder the further carried,
slate flat and sharp-edged, made in what it is placed in; the linear
answer's long-wave dip, its silence to a ridge along the flow and its peak at
the still wave; lee waves only behind a stone, a pillow before one through
the surface and foam in its wake; a riffle stirred where a pool lies still;
fast water churning and a ledge pouring glassy down its tongue and breaking
white at its foot; a train of standing waves dying away behind a ridge;
nothing past what water can stand; a bed's median at Shields' stone and its wears from fresh to the
farthest; a broken end torn wood bristling with fibres; crowfoot streaming
where the water runs over gravel, and reeds off scoured ground; a perched
river's bank falling to its land; the deep water alternating from unit to
unit; a grid within the points it is allowed; a wake halfway between two
points still shedding; a stone's answer keeping pace with the bed's; a finer
water grid meeting the far one at its seam; a stream always having
somewhere to be looked at from, and a scene away from it composing without a
brook; a river's sand carrying no grit of its own; a grain's octaves sharing
its depth, and a grain made for its coarse relief keeping it; a water
planning only the patches it sets out; a stream's flow solved further the way
the eye looks; a boulder claiming its ground, never set through what already
stands, and standing in the flow as it lies; drift reaching the flow as one
obstacle; a crest held once to what water can stand; a course's way holding
past its ends; a water grid's steady fall told apart from its spread; a stream's
floating plants on its shaped water; and a stream's growing never reporting
less done.

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
foot, snow drifting against a wall's lee face and scoured from its windward
one where a land lies under snow (RT29); fences of posts and rails, true where
kept and leaning, moss-green and missing rails where not; gates hung on posts, open, shut or off a hinge, with
mud and hoof-churned ruts in a pasture's gateway. Crops are lawns of their own
blades and heads in drilled rows with tramlines; cut fields stand stubble and
bales of one kind. Flowers and weeds are drawn per species in patches seeded
by the field's history, never sprinkled evenly.

## RT20, RT21, RT31 — Masonry, its cover and its weathering

Every structure a scene sets out is laid unit by unit by one mason
(`compose::courses::Mason`) into one prototype of `Part::Solid`s
(`solid`), raised as an instance; nothing built is a primitive stand-in.

- **Solids.** A unit is a block (a voussoir's ends leaning in by its fan), a
  drum (taper, entasis, flutes), a turned moulding or an Ionic volute, kept
  to 52 bytes. Its wear — rounded arrises, chips struck as shallow scallops,
  lumps, pits, a crack from one face — only takes stone away, is drawn from
  its key and its structure's age and stone, and shows only as far as each
  term spans pixels (1.5 to 3), so far off a unit is its exact dressed form.
  It is sphere traced against the steepest its surface can rise, never in
  steps under three quarters of a pixel, and its crossing narrowed by regula
  falsi; its bounds hold every hit, and a shadow sees what the eye does.
- **Laying.** Courses snap to an arch's springing; a stretch an opening
  leaves is faced at its ends with quoins laid through the wall; mortar
  cores are recessed behind the faces and overlap course to course so no
  coplanar seam shows; arches are voussoir rings or brick rowlocks; bricks
  keep their bond with headers closing each course and a few lost; columns
  rise in drums or break off, a ruin's lying along the way they fell; an
  opening walled up is rubble in mortar. Arches claim the ground beneath
  their bays as well as their piers.
- **Colour.** A unit's key gives it its shade and hue, as a quarry's beds
  differ; age greys and darkens it, rain streaks it, grime and biofilm
  blacken it in patches and along its joints, what shelters it crusts, a damp
  foot greens; bricks darken at their fired ends and a reclaimed one keeps its
  old mortar.
- **Cover** (`cover`), one reading carried from the solid's geometry to its
  pigment. Moss spreads from the joints in mats of packed cushions whose
  ragged margins break into cushions, with lone cushions lodged here and
  there, the most in joints, and none on a floor worn smooth; lichen in young
  lone colonies over the mosaics of old crusts, the orange species where birds
  perch on what faces the sky, each substrate its own species. Moss on bark
  (`cut`, `bark`) stands in relief near the eye by the same rule, its share
  matching its pigment's. A roof RT22 lays as solids carries the same cover.
- **Aqueducts.** Laid out on the land as sited and founded on the land as
  built wherever that lies lower; its conduit runs on beyond its arches in a
  cutting the land digs (`land::Build::site`, held through the droplets) to
  where the hill stands a cutting's depth over it, then through a portal whose
  headwall steps with the hill, where the hill as built covers the conduit by
  `COVER`, or else on buried until it does.

## RT22–RT25 — Farm buildings, villages and their interiors

Buildings are laid by the RT20 mason. Thatch is a deep layered roof of straw
over a ridge; timber is weatherboard silvering and splitting with age. Night
interiors are real rooms behind real glass, lit by lamps the scene holds, so
a window glows because a room is lit.

## RT26–RT30 — Nature detail

Mud cracks (RT28, `mud`, `compose::cracked`) are 3 m tiles of the plates of a
Voronoi desiccation pattern, 10–25 across: each plate curls toward its rim,
most at its corners and a few far more than most, dished at its middle and
undercut beneath the curl; cracks run from hairlines to a third of the
crust's thickness and more, every side bent by a field that repeats with the
tile. Every tile shares its feature points two cells in from its edges, so
four variants meet any other edge to edge in whole plates without a seam.
They are laid square to the land's own lattice out to 18 m (*Simple*) or 36 m
(*Maximum*) on level, bare silt out of the water — a canyon's or the
badlands' washes, a valley's banks in summer and autumn — in patches, and
only where a tile lies within a centimetre of its plane; coloured in the
land's own silt bleached paler over its earth gone damp beneath, some plates
faintly crazed.

Mountains (RT30) need an erosion law that carves as it gathers: channels
deepening where droplets converge and branching up every slope, the beds of
rivers and roads kept, and talus below crags — so that the budget can then be
spent on droplets, which today only smooth the land.

## RT26 — Bark in true relief

A limb is cut by its own bark (`cut`) where it stands near enough the eye for
the cut to show: ridges at its radius, fissures sunk the bark's depth
(`bark_depth`, 2.4 cm for oak and pine down to 2 mm for beech). Its girth
gates it, from 3 cm to 12 cm in radius, so twigs and young branches stay
smooth tubes; and how many pixels the cut spans fades it, in full from three
and none below one and a half, against the scene's `Viewpoint` (the eye and
the angle a pixel spans), which `Geometry` carries for every ray alike.

- **Met exactly.** The cut limb is the limb's cone run on within each rounded
  end's sphere, its surface `reach = radius − cut · (1 − height)`. A ray's
  stretch within the cone is sphere-traced (Hart 1996): each step the height
  above the surface over 1.25 × (1 + taper + depth × `Bark::steepest`), twice
  the steepest each bark kind rises over a dense sampling (a test holds every
  kind but a palm's stepped rings to it), never shorter than ¾ of a pixel at
  the point or 0.4 mm, and at most 256 steps to its stretch; a crossing is
  bisected to a hundredth of the shortest step. A shadow ray (`Seeking::Any`)
  stops at the first crossing. A ray leaving the bark from within a hollow it
  rounds off is let out before it looks again.
- **Marched lean.** Beyond the bark's outer surface or deeper than its cut the
  bark cannot change which side of the cut surface a point stands, so it is
  read only within the shell, and outside it a step is the height over the
  limb's own bound, without the bark's steepness.
- **Shaded.** A cut hit's normal is the gradient of the surface the march
  crossed, read four bisection tolerances either side, so it faces whatever
  ray crossed into it however steep the bark; the ends are not read, so a hit
  at an open end's rim faces out of the side. `Hit::relieved` keeps the
  material's relief from tilting it again.
- **Flared feet** are met the same way at any distance (RT45), the round cone
  swollen by the flare and run on within the cone of its most.
- **Measured.** A furrowed height costs about 550 ns against the plainer
  pattern's 265 ns, mostly its corky scales' cellular lookup, which is read
  only where a pixel is finer than about a centimetre; reading the bark only
  within its shell takes a fifth off a cut ray (12.7 µs to 10.5 µs), so a
  near-eye cut ray costs about 1.7 times the plainer bark's, a trunk filling a
  close-up half again as long to trace, and a forest about as long as before.

## RT27 — Stumps and decay

Dead wood decays (`deadwood`). A kind's dead in a wood have lain an age
drawn from 0.15 to 0.95 (`plants::Rotting`), each piece within 0.2 of it;
drift a stream carried, 0.1 to 0.4.

- **Bark** sloughs (`Bark::bare`, `sloughs(kind) × smoothstep(0.2, 0.9, age)`:
  0.1 for birch, 0.25 for oak and olive, up to 0.75 for beech) in sheets of
  two octaves of noise thresholded to the share asked, sharp-edged and dark
  at their edges. The wood it bares lies at a bark height of 0.12, tan sapwood
  weathering grey-brown, its grain raised, checked along it and engraved by
  beetles' galleries in patches; moss, `0.5 × smoothstep(0.3, 0.95, age)`,
  lies in crisp cushions taking the furrows before the crests.
- **Stumps** stand on a foot flared toward five to eight roots (`foot`), its
  flare carrying on past a low cut so the face follows its lobes, the bole a
  flared limb from a fifth of its radius below the ground.
- **Sawn faces** are a polar mesh of eight rings and 32 spokes, with every
  check's two edges and floor and the notch's two ends among them, the last
  band, from 0.9 of the outline, the bark cut through in its dark edge. Felled
  (three in five) the back cut stands 0.12–0.25 of the radius above the notch
  over a chord 4–8 % of the radius from the middle, the two faces meshed on
  their own and joined by a riser, the hinge torn across along the riser's top
  in a narrow unbarked break of laths until the heart rots. Each face checks
  `1 + 6 × age` times, from 0.15–0.45 of the radius out, opening at the rim
  0.4–1.2 % of the radius times `1 + age` and cut 6–20 % times `0.5 + age`
  deep. Its wood weathers from tan to grey-brown by `smoothstep(0, 0.6, age)`.
- **Breaks** (`fracture`) tear snapped stumps, a log's ends, its limbs' stubs,
  a bole's stubs, a snag's top and limbs and a plate's roots: a polar face of
  ten rings and 10–56 spokes climbing up to its narrowest radius toward its
  tension side, ragged in fibres 1.1 cm apart, falling to a bark rim ragged by
  6 % of it, the last 3 % the bark's dark edge; 2–22 laths scaled to that
  narrowest, those beyond 0.85 of the way out barked on their outer face; up
  to two tatters. Past an age of 0.55 its heart hollows out to 0.3–0.75 of the
  radius, and it loses three quarters of its laths by the last.
- **Fallen trunks** are ten tubes sunk 18 % into the ground, each limb stub an
  open limb narrowing to three quarters and torn, shorter the longer it has
  lain. Thrown, the foot flares into its root plate: a slab 2.4–3.6 radii out
  and 1.6–2.6 deep at the trunk, its rim uneven by two octaves, its faces 30
  rings and 144 spokes of crumbling clods in speckled topsoil; its five to
  eight lateral roots, one to four branches each, run along its underside
  half bared and are torn past its rim; 18–32 sinker roots torn short every
  way, 30–60 fine roots bristling and 6–14 hanging from the rim.
- **Brackets** fruit with a chance of `0.15 + 0.6 × age`: tiers of three to
  seven thin shelves 2.5–6 cm out, or one to three thick ones 6–18 cm, on one
  to three of a log's flanks or a stump's sides, level, a shelf 1.3–1.9 times
  as broad as it reaches, its margin uneven, ridged in 4–10 zones, a thick
  one hoof-shaped, a thin one waving, banded to a pale margin, pores beneath.
- **Shoots** rise from a broadleaf's stump (not a pine's, a spruce's, a
  palm's, a saguaro's or a fern's), half of sawn ones and three in ten
  snapped, fewer as it rots: four to eleven 0.35–1.6 m long, from just below
  the cut or about the root collar, arching toward the light, in young
  smooth bark, leaves of their kind's own shape a third larger along their
  upper three quarters (`tree::leaf`, which a tree's twigs bear theirs by too),
  none in winter.

## RT29 — Snow and snowmen

- **Drifted** (`snow`). A land under snow (`Plan::snowpack`) carries a fall
  0.2–0.6 m deep moved by one wind. How sheltered each place stands is read
  at the coarse grid's samples, a band of rows a core (`Step::Shelter`), and
  read back through the relief's own Catmull–Rom patch: the steepest slope up
  to the ground 4–100 m upwind, the mean over the wind's heading and 15°
  either side (Winstral, Elder and Davis 2002), less 2.0 times the curvature
  over 50 m and 0.3 times the slope the wind climbs (MicroMet, Liston and
  Elder 2006). The depth is the fall, uneven by 15 % in 70 m patches, times
  `1 + 1.4 tanh(shelter / 0.2)`, never under the 6 % crust the wind cannot
  lift, and none on slopes past 1.1. The far grid adds it to the ground under
  it everywhere but beneath a clearing's water; the finer grids cut snow dunes
  (9 m apart, 22 cm deep) and sastrugi (1.6 m, 16 cm) along the wind where it
  scoured the snow, each only where the grid resolves it, never through the
  snow (Filhol and Sturm 2015).
- **Carried.** A land's grid keeps five channels a vertex, the fifth the
  snow's depth, square-root coded to 6 m so a few millimetres tell about
  nought. Snow buries what grows thinner than the 14 cm of winter grass; the
  ground's colour shows through less than about 6 cm, in patches; a lawn's
  shoot rooted on the snow shows only what stands above it. Winter's ground
  beneath is frozen earth and dead grass, and its sward a winter upland's.
- **Measured.** At 8 threads a coarse sample's shelter costs about 260 ns, its
  longest unit 4.5 ms; a far vertex under snow 51 ns against 38 bare.
- **Snowmen** (`snowman`, `compose::snowman`). Two balls (one in five) or
  three, 0.3–0.5 m the foot's radius, each 0.55–0.8 of the one beneath. A
  rolled ball is a drum (0.06–0.17 narrower along its roll axis, a gentler
  second roll across it), lumped by 3–6 % of its radius in three octaves,
  wrapped in two to four sheets whose lips stand 1.2–3.5 % of its radius
  proud, and streaked round its drum with earth, dead grass and leaf; a head
  is packed by hand as often as rolled, with no sheets. It is patted into three
  to ten dents a palm or a few fingers broad, flattened where it was set down
  (10–18 % of its radius at the foot) and where the next was pressed on, the
  join packed with snow, settled to 0.9–0.98 of its height, and stacked
  leaning 1.5–7° and set off its seat by up to a quarter of the seat's
  breadth; the lowest is pressed into the snow and down a slope far enough to
  leave no gap. Its eyes, mouth and buttons are lumps of one coal prototype
  broken along six fresh fractures, pressed a third to a half in; its nose a
  carrot 9–16 cm long, its skin ringed every few millimetres where its fine
  roots grew (`BarkKind::Taproot`); its arms two forked sticks pushed into the
  body's sides. Each ball is a mesh about a centimetre to a facet, shaped 8192
  vertices a unit. A snowman needs seven prototypes, and is not built where
  they would not fit.

## RT37, RT38 — Woods and stones by the budget

- Woods to the horizon (RT37): at *Maximum* a wood of trees reaches as far
  as its trees still span a pixel. Its trees are stood one by one at its own
  spacing over every grid the land is traced on, out to where its most trees
  fill, its places fit the budget (about 80 bytes a place) or its trees stop
  spanning a pixel; never sown coarser, which would thin it. Past them it
  carries on hashed from cells (`far_wood`): one place a cell on a lattice as
  fine as its middling trees stand apart where it grows thickest, read by the
  one reading every wood is stood by (`wood::Reader`), and kept as often as
  places sown as closely as the trees stood one by one, each growing a tree as
  often as the ground suits one, come to once thinned to their crowns' room.
  The share of the ground those crowns fill is matched, over the ring short of
  where it begins, to the trees stood there, so it stands as thickly far off
  as near on any other ground. Once its trees' prototypes grow, the ring is
  read for that match 16 384 places a step and its lattice surveyed 256 blocks
  a step, across the runner (`far_wood::Matching`, `far_wood::Surveying`): over
  the ring across the view and a crown's reach beyond it, every place's tree is
  read once, keeping a bit a cell for those that stand one and, a block of
  16 × 16 cells at a time, the lowest ground under it and the highest crown
  that stands over it, its neighbours' reaching over included. Nothing else of
  it is stored: about 12 bytes a block and 32 more where a tree stands in it.
  It is traced as the tiles of its square a crown stands over, each boxed to
  those crowns. A ray passes a block it crosses over every crown, walks the
  rest a cell at a time meeting each tree that stands within reach once,
  reads the ground for a tree only where it passes beneath the crowns about
  it, and grows the tree only where it passes low enough to meet the tallest
  its place could grow; a tree it meets is placed again from its cell when it
  is shaded. Shrubs are
  low enough to be stood one by one as far as they are seen, so a wood of them
  carries on no further; a moorland's heather and gorse and a mountain forest
  stand as many as the detail's table allows. The pieces and trunks a scene
  sets out are kept apart over a grid hashed by cell, so each is asked of only
  its neighbours however far out it stands, and the land's coarse grid out to
  the horizon carries what grows there as its far land does.
- Stones in patches (RT38, `compose::strewn`): eight rocks a scene, strewn
  from draws of their own. A place takes a stone with a chance of
  `0.04 + 0.96 × max(field, scree)`: `field` the boulder fields, a 70 m noise
  thresholded from 0.1 to 0.45, each boulder there among up to three stones a
  quarter to two thirds its size; `scree` up to 0.85 on talus (its normal's
  upward part 0.62–0.92) below a crag (under 0.6) within 22 m uphill, less
  the further the crag, its stones graded larger the further they rolled.
  Pebbles 4–14 cm lie in drifts of four to twelve within 0.6 m, washed into
  ground at least a quarter wet or silted, in 8 m patches, out to 15 m from
  the eye at *Simple* (40 drifts looked for) and 30 m at *Maximum* (160),
  which also strews three times the boulders. Leaf litter is as thick as the
  crowns above shed, the open keeping 8 % of it, what the wind carried out.

## RT42, RT43 — Lights as measured

Every light out of doors comes of a published measurement, nothing tuned
(`docs/src/lib/raytrace.md`, *The sun, the moon and the stars*):

- The sun's disc is 959″ in radius and brings the sunlight above the air; its
  limb darkens as Neckel and Labs measured it. The air bends its light as
  standard dry air over the 1976 Standard Atmosphere does: the atmosphere's
  transmittance is kept along bent paths from the true direction, the
  sunlight a height takes spread as the path squashes the disc and scattered
  about the way it arrives, and the same paths say where a ray seen leaving a
  point comes from, with the solid angle's change for a sample's density.
  Each channel bends by its own refractivity.
- By night the light is the moon's at its phase (RT32): its true size from
  the eye, 14 magnitudes below the sun at full, the ROLO model's colour.
- The stars are Allen's census to magnitude 12 in three tiers by brightness,
  each a cube cell's Poisson share of its tier, their colours Tycho-2's, the
  fainter a glow; seen through the ray's footprint, through the air and never
  below its horizon. A tier that could not show against the sky a ray sees it
  over, as by day, is not searched.
- Lamps are lumens and candelas through the sunlight's illuminance, 133 334
  lx. The dusk crystals and the night garden stand under the weather's own
  dusk and full moon; the studio alone keeps a room's walls for its sky.
- Water's glow is scaled by the light measured falling on the scene's level.
- Cirrus is ice in a high bank of its own: single scattering by its phase,
  Hillaire's series for the rest, the sky and ground in by the phase's share,
  its shadow on the bank beneath.

- Water decks (RT43, `slab`): the sun scattered once exactly through a
  droplet phase of a forward lobe (g 0.9) and a twentieth's backward one
  (g −0.3); every further order by the δ-Eddington two-stream solution, its
  albedo 0.99998 and asymmetry the lobes' mean. The light grids hold the
  cloud's optical depth toward the sun, away from it, straight up, and level
  away from it (the last two in steps growing from 10 m out to 32 km). The
  sun's light is solved in a slab facing its beam, as deep as its way in and
  as thick as the cloud runs on behind, as a heap lit from aside is; and in
  its column, as a deck is, through which the light diffuses down whatever
  way it fell in — blended toward the deck's the further the cloud runs
  across than down, from four times to twelve. The sky's light and the
  ground's are solved in the column, entering by its nearest face the sky
  lights. The scattered
  light toward the eye is each diffuse field's mean and first moment through
  the full phase's asymmetry. Energy holds to a percent in a lossless slab, a
  thin cloud takes little but its single scattering, a shaded side greys
  under the sky's light, and nothing overflows however thick the cloud.

## RT44 — Cloud to the horizon

A bank of cloud reaches as far as any of it can be seen, so no band of clear
sky ever lies between the clouds and the horizon (`cloud.rs`).

- **The Earth's curve.** Heights are taken over the Earth about its centre
  straight below the eye, as the air's are, the scene's level at the sea's
  distance from it plus its own height (`Air::ground`). A deck seen low down
  runs down to the horizon as the curve carries it: a cumulus base meets the
  eye's level some 120 km off, the highest cirrus nearly 380 km off. A ray
  stops at the sea's sphere, beyond which the Earth hides the cloud.
- **Levels.** A bank stands about the eye (`Cloudbank::stand`) in levels, each
  twice the last's breadth in as many columns, the coarsest reaching where the
  eye's sight grazing the sea runs on to the ceiling, the finest no broader
  than 28 km for the low decks and 60 km for cirrus. Each level holds every
  deck's weather and bands, the sun's optical depth, the bank above's
  sunlight and the shadow; the cell counts are multiples of four, so a level's
  square lies on whole cells of the next. A level's weather is drawn only as
  finely as its columns hold, octaves finer than two columns settling to
  their mean (`fbm2_resolved`), and its outer eighth is blended into the next
  coarser level's own reading, reaching it at the edge, so no field steps
  where the levels meet. A march begins where its ray rises through the
  bank's floor, beneath which no cloud stands, and walks the finest level
  holding each point, onto the next past a level's edge and back within a
  finer one's square.
  Every map is reserved whole and written row by row, so no unit fills more
  than its own rows; a unit is a few rows a core, some 6 ms.
- **Light by place.** The sunlight on cloud is tabulated by height and by the
  cosine of the sun's angle from the vertical at the place, over the range the
  bank's places see, so cloud far off toward a low sun takes it higher and
  cloud far off the other way lower or not at all.
- **The air beneath.** The aerial table carries on to 454 km in 88 slices,
  the first 32 where they were, within 60 km. The air before each cloud a ray
  meets, and on to where it leaves the highest bank, is lit by what of the sun
  the banks let through, judged at one point of each stretch drawn where its
  sample's share of the stretch's sunlit air has been gathered
  (`Sight::drawn`), the land's aerial perspective drawing its point the same
  way. An overcast's air is grey to the horizon.

Measured on Coast at 960×540, Simple, 24 threads: preparation about 0.3 s
longer (2.0–2.2 s to 2.3–2.6 s), the longest unit unchanged at about 6 ms, the
readout's longest hold 0.2 s, the peak 15–30 MB higher; tracing 10–16% longer,
the far cloud's march and the shaded air between them, of which the shade's
noise costs some sampling rounds. At *Maximum* a meadow or a valley prepares
3–4% longer, its radiosity records' rays looking out to far cloud.

Tests: a deck running on to the horizon however low a ray, the further the
lower, past 60 km at the horizon, and none below it; heights over the curve
and the points they lie at agreeing; the levels meeting with no seam in
weather, light or shadow; every level's bands holding its cloud; a march
standing over the finest level and cell beneath each point, inward and
outward; far cloud lit by the sun at its own place; far cloud shading the
ground from a low sun; an overcast grey to the horizon; the aerial table
keeping its near slices; a point drawn along the air falling as its sunlight
is gathered; and a resolved sum keeping only the octaves its footprint holds.

## RT45 — No primitive stand-ins

- **Saguaros** (`cactus`) are ribbed flesh: ribs counted by girth, each crest
  wandering, felted areoles set along it with spines by age, red-brown at the
  apex to grey and pale toward the corked base, constrictions where droughts
  pinched it; built a share of areoles a step (`Bristling`).
- **A palm's foot** swells into a dense mat of short curved roots at the soil,
  some dead and snapped; **a shrub** grows from a buried stool, its stems
  swelling where they leave it.
- **Lily pads** (`lily::Pad`, `waterside`) are meshes each cut to an outline
  its key draws — oval, waved, its lobes meeting, parted or overlapping, frayed,
  bitten, split and holed the more the older it is — laid one after another so
  each rests on those floating beneath it; young pads still rolled and bronze,
  crowded ones raised, dying ones sinking; coloured by the same draw, so a
  wound's dark rim follows the cut.
- **Flowers** sit at every stage from bud to spent: sepals and petals are
  sheets cut to their own outlines, each bent, twisted and cupped its own way,
  spiralled round the ovary, browning from the tip as they fade and shed onto
  the water; stamens are narrow straps crowding a rayed, horned stigma. A
  crowfoot's broad petals ring a knobbly head of carpels.
- **Reeds** end in a spear of rolled leaf or a branched plume of silky tufts;
  **a reedmace's spike** swells between blunt ends under a withered spire and
  bursts in fluff over winter.

## RT32 — The moon

`body::moon` sets the moon toward any direction with the sun toward another.
Its light is the full moon's (−12.74 against the sun's −26.74, nearer and so
larger and brighter with height) times Allen's phase law,
`0.026|α| + 4·10⁻⁹α⁴` magnitudes at a phase of α degrees, plus the Earth's
light: the full Earth's at the moon (geometric albedo 0.367 over the
Earth–moon distance squared, about 10⁻⁴ of the sun's), waning as a Lambert
sphere's phase, `(sin ψ + (π − ψ) cos ψ)/π`, bluer (0.86, 1, 1.24). Its disc
(`Limb::Phase`) is each channel the sunlit share times the Lommel–Seeliger
law `μ₀/(μ₀ + μ)` plus the earthlit share, both as shares of its mean; the
law's own disc mean is half its integrated phase function,
`1 − sin(α/2) tan(α/2) ln cot(α/4)`, and the sunlit share is capped where
Allen's law would light a part brighter than the full moon lit as it is.

The weather sets it (`compose::weather`): by night the moon is the light,
20–50° up, its phase drawn as `150u²` degrees, capped so the sun lies at
least 15° below the horizon; by day three scenes in ten, 45–150° from the
sun and 8° up, and at sunset or dusk nearly half, a young or old crescent
12–55° from where the sun went down and 3° up, lighting nothing beside the
sun. A star never shows through its disc (`Seeing::occulted`).

## RT33–RT36 — New scene families

Each is a `Setting` of its own, composed and traced by the same tracer:
aerial vantages over the lands the
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
| objects a scene holds | 131 072 | 2 097 152 |
| trees a meadow's, a valley's or a building's wood stands | 20 000 | 360 000 |
| a forest's | 90 000 | 900 000 |
| a winter wood's | 40 000 | 480 000 |
| a canyon's | 4000 | 120 000 |
| a mountain forest's, and a moorland's heather and gorse | 60 000 and 2500 | 360 000 and 250 000 |
| a rocky desert's saguaros and scrub | 600 and 900 | 18 000 and 27 000 |
| places a wood's trees are sown over at once | 1 200 000 | 3 500 000 |
| a wood of trees carried on past them, hashed | no | to where its trees span a pixel |
| rays a radiosity record gathers | 256, eight rows by 32 | 1024, sixteen by 64 |
| the least a record holds for, of the picture's height | a 240th | a 480th |
| records a square of the picture, and pixels a record | 1600 and 64 | 6400 and 16 |

A wood's caps also bound how far its trees are stood one by one: a *Simple*
wood's reach about 1.3 km and a *Maximum* one's 2–3 km, past which a wood of
trees carries on hashed (RT37). A profile changes how much
is set out, never what a setting is: the land, the eye, the hour and the
weather are drawn before any wood grows, and each wood and the sward draw from
streams of their own keyed from one draw, so however many draws one wood takes
a seed shows the same place — woods and sward included — at either.
`Detail::peak` states each profile's budget, 512 MiB and 2 GiB, which the
session weighs against the memory band (`plans/NEW-DESKTOP-SETTINGS.md`
DS24); each profile has its own measured progress shares.

Measured at 1920×1080 on a 24-thread desktop preparing across 8 threads, a
landscape prepares at *Simple* in 0.35–9.3 s, holding at most 475 MB at its
peak and once prepared, a mountain lake's, its tree prototypes and the
clouds' light grids most of it; and at *Maximum* in 1.0–102 s, at most
840 MB at its peak and once prepared, a stream's — most of *Maximum*'s time
its radiosity records. A wood carried on far off adds its survey, about 4 s
of a mountain forest's 50 at *Maximum*, and a few per cent to gathering and
tracing. RT38's stones and what later items buy are *Maximum*'s to spend the
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
