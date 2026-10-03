# tairix-raytrace

`lib/raytrace` is the ray tracer behind the desktop's ray-traced screensaver
(`docs/src/desktop/session.md`): scenes composed at random from a seed, and a
tracer that answers what one pixel of one shows. It is `no_std` + `alloc` and
forbids `unsafe`.

## Scenes

A `Draft::new(setting, seed, size, detail)` composes a scene in one of the
nineteen `Setting`s. Within a setting everything is drawn from the seed: which
pieces, where, in what, lit from where, at what hour and under what weather,
and seen from where — and every draw is bounded so the scene is lit and framed
to read.

A `Detail` sets how much a scene sets out, and nothing else: one table of
densities (`detail`) that composing and gathering read — how many objects a
scene holds, how many trees each wood may stand and so how far it is sown, and
how a radiosity record is gathered and laid. `Simple` is every setting
plainer; `Maximum` spends the whole budget. The land, the eye, the hour and the
weather are drawn before any wood grows, and each wood and the sward draw from
streams of their own keyed from one draw, so however many draws one wood
takes, a seed shows the same place at either detail.

- **Still lifes** stand on a plane: a checkerboard of spheres, gems, rings and
  stacks under the open sky; a studio's plinth under softboxes, high or low
  key; crystal clusters, or crystals grown out of a stone, on black glass at
  dusk, under display spots; glass and chrome among garden lamps under the
  full moon; soap bubbles drifting over a meadow. Only the studio is indoors:
  the others stand under the open sky's weather, dusk and night included.
- **Buildings** stand on a paved plaza, on tiles to the horizon or on open land
  with woods about it: a colonnade in three orders, a loggia of arches or a
  two-tier aqueduct across a valley, a domed rotunda, a ruined temple in long
  grass; a sculpture stands out in a meadow, on dunes or by a mountain lake.
- **Landscapes** are lands: rolling meadows, a forest, mountains over a lake,
  an island's coast, dunes or rocky desert, snow over a frozen pond, a lagoon
  at sunset, canyons between mesas, and a river valley crossed by a stone
  bridge.

The weather (`compose::weather`) sets the hour and the cloud. The sky is a
physical atmosphere over a round Earth (Hillaire, EGSR 2020), its sky, sun
colour and aerial perspective tabulated once per scene; clouds are volumetric
decks marched as the medium they are (Schneider 2015; Hillaire 2016), dimming
the sun and the sky they stand in front of. Each cell of a deck's weather map
bounds the heights its cloud can reach, so a march strides straight through
the air above and below them. A bank thins out over the last tenth of its
breadth rather than ending in a wall of cloud. Cirrus is a deck of its own, of
ice, in a bank high above the rest.

## The sun, the moon and the stars

Every light out of doors is measured, not chosen (`body`, `stars`,
`refraction`):

- **The sun** stands at infinity where it truly is, its disc 959″ in radius
  (the IAU solar radius over the astronomical unit) and its light the
  sunlight above the air, `SOLAR` in the scene's units, which the camera is
  balanced to. The disc darkens toward its limb as `μ^α`, α at each channel's
  wavelength from Neckel and Labs' profiles as Hestroffer and Magnan (1998)
  fit them, normalised so the disc still brings the whole sunlight.
- **The air bends it.** The index of refraction is standard dry air's
  (Ciddor 1996) over the 1976 Standard Atmosphere's density; the path a ray
  bends along out of the air is traced once per scene from every height in
  every direction (`atmosphere::Paths`), and the sunlight reaching a height is
  kept along that bent path from its true direction, spread as the path
  squashes the disc, and scattered by the air about the way it arrives. So a
  sun at the horizon
  stands lifted by some 33′ and squashed by the change of that lift across
  it, a little less light reaching from the squashed disc; sunset comes later
  than the geometry would have it, and the Earth's shadow edge is where the
  bent light leaves it. Each channel bends by its own refractivity, so a low
  sun's upper rim is blue and its lower red. The disc a ray sees is lit as
  limb darkening says at the true direction each channel's bent ray points,
  and dimmed along that ray, so the lower limb of a setting sun is redder
  than its upper. The eye's own rays, which cross only the level scene, stay
  straight.
- **The full moon** lights the night: the sunlight its grey reflects, 14
  magnitudes fainter than the sun (−12.74 against −26.74) and warmer, as the
  ROLO lunar model (Kieffer and Stone 2005) gives its reflectance across the
  channels. It is reckoned from the eye, nearer and so larger and brighter
  the higher it stands; lit square on, its disc is evenly bright. The night
  sky is the atmosphere lit by it.
- **The stars** are Allen's census, star by star to magnitude 12 — in three
  tiers by brightness, each cut into cube cells of its own, coarse for the few
  bright and fine for the many faint; each cell holds a Poisson draw of its
  solid angle's share of its tier, placed, made bright and coloured from its
  own key, with Tycho-2's colours turned to a tint by the colour index for
  blue against green and a blackbody's (Ballesteros 2012) red — and the light
  of the fainter, to magnitude 19.5, a glow. A ray sees a star through its
  footprint, a pixel's for the eye, as a Gaussian holding all of the star's
  light; a footprint broader than a tier's cells, as a scattered ray's, sees
  that tier's mean. A tier whose brightest star could not add a ten-thousandth
  to the sky it is seen against, about a hundredth of an eight-bit pixel's
  least step, is not looked for: by day most are not. The stars shine through
  the air, dimmed and lifted as it bends their light, and none below its
  horizon.
- **Lamps** shine in real units: a garden globe in lumens, a spotlight in
  candelas, turned to the scene's light by the sunlight's illuminance above
  the air (133 334 lx, Darula, Kittler and Gueymard 2005). Beside them the
  full moon gives the faint light it does.

What deep water glows with is scaled by the light falling on the scene's
level, measured channel by channel once the sky is built — the sun's disc
through the air and the cloud, and the sky above, cosine-weighted — so water
by moonlight glows only as much as moonlight lights it, and at sunset with
sunset's colour.

Cirrus is ice: its sunlight is scattered once exactly by rough ice crystals'
phase (asymmetry 0.75, Yang et al. 2013); the light scattered again and
again is Hillaire's isotropic series over the deck as its mean extinction
lays it out by height, scaled by `1 − g`; and the sky's light and the
ground's are scattered in by the share of the phase each hemisphere sends
the eye. Its bank's shadow dims the light the lower bank is lit by, and both
shade the ground. The water decks' octaves, powdered edges and ambient remain
the production approximations of Schneider and Wrenninge.

## Lands

A land is a landform worn by water through `lib/terrain`
(`docs/src/lib/terrain.md`): Priority-Flood drainage, stream-power incision
and hillslope creep over a coarse grid cut its valleys; its rivers, lakes and
roads are laid where the water and the ground let them run; and a far grid and
a finer grid about the eye refine it, droplets running over each for the rills
and fans no coarser pass makes, a turn of the land's tiles at a time across the
runner. Each vertex carries what the land is like
there — wet, worn or built up, on a road or a path, how much grows — which its
shading, its sward and its woods read. The rivers' and lakes' surface is a grid
of its own, wet a cell's diagonal past a river's banks, so a river however
narrow and however it slants across the grid keeps its water in every cell its
course crosses; what lies past the banks lies under the ground, unseen. Grass is laid over it in lawns of
coarser cells the further they lie, merging leaves finer than a pixel and
fading into the ground's own colour far off.

## Woods

A scene asks for its woods (`compose::woodland`) and they are grown once its
land stands, a bounded step at a time.

- **Patches, stands and gaps.** A wood covers its share of the ground in
  patches, noise thresholded so the share covered is the share asked. Within
  them, how closely it grows varies from stretch to stretch; its kinds each
  keep to the ground they take to (willows to the wet, pines to the dry and
  poor) in stands of their own, each stand as tall as it is old; and gaps open
  in the canopy where a tree or a stand of them fell, a lattice of them each
  holding one as the wood's share has it.
- **Thinned tallest first.** The places a tree might stand are sown in rings
  about the eye — all the way round as far as the trees' shadows reach, then
  across the view out to the land's edge, as far as a tree still spans a
  pixel or two, or as far as the wood's most trees would fill — and read
  across the runner a band at a time. Those that would grow are ranked by
  height, sorted in runs a core apiece and merged tallest first as they are
  taken, and thinned in that order: a tree stands only where its trunk keeps from every taller
  one's by their crowns' reaches together, times the wood's closure there, a
  much shorter tree standing under a taller one's crown more readily than
  beside a peer's, and a stand draws two in five of its trees overtopped or
  suppressed. No tree stands in the way of the view close ahead of the eye,
  nor where a piece claims the ground: a bridge's deck and a row of arches
  claim the whole strip beneath them, so nothing grows through a deck or in
  an arcade's bays.
- **Grown as they stood.** A kind grows four trees at heights from past its
  youngest to its tallest, and a place stands the one nearest the height it
  wants, scaled from three quarters of its own size to a third larger. A tree grown close among
  others sheds the limbs its neighbours shade and grows narrow; a young one is
  short and less branched; a dead one keeps its trunk, snapped short, and the
  stubs of its biggest limbs.
- **Beneath.** Shrubs, ferns and young trees are sown in the shade the canopy
  casts: next to none under a closed canopy, most in its gaps and along its
  edges, fewer again out in the open, where grass takes the ground.
- **Dead.** Fallen trunks — thrown with their roots or snapped at their feet,
  their limbs broken to stubs, in weathered bark taken by moss — lie about the
  eye, three times as thickly where the canopy has opened and those lying the
  way the wind threw them; stumps, sawn or snapped, and standing dead trees
  stand among the living.

Each crown's reach is recorded, and once the woods stand their **shade** is
cast over the land (`shade`), whether or not a sward is laid beneath them: how far under a crown a place lies, and how much
of the sky the crowns about it hide — the mean cover of the ground within a
dozen metres, twice box-filtered. It is cast a band of rows a core: the crowns
sorted into the bands their trunks stand in, each band covered from the crowns
that can reach it, then spread along its rows and, turned on its side, along
its columns. A lone tree hides little of the sky; a
closed wood nearly all of it. The sward thins with the sky hidden, to none
under a closed canopy, and weeds all but as soon; the ground under the crowns
is the leaves or needles they shed, fresh or browned by a year, over humus,
with carpets of moss, thickest under conifers and where the ground is damp;
and the air beneath the crowns is lit only by what gets through them, so a
wood's depths are dark rather than hazed as open air would be.

## Geometry

Spheres, domes, planes, rectangles, convex hulls (boxes, gems, crystals,
pyramids, obelisks), capped frusta, tori and their arcs, height grids, lawns,
and instances of prototypes. A hull's extent is found from its own corners,
each where three faces meet within the rest.

- **Height grids** hold `f32` heights at the vertices of a square grid of any
  number of cells and the bilinear patch between them. A ray walks a pyramid
  of maxima (Tevs, Ihrke and Seidel, *Maximum Mipmaps*, 2008), each level
  half the one below with the odd block out kept, and meets a cell's patch
  where a quadratic along it says. A grid is written as its rows are filled,
  never zeroed whole beforehand, and its pyramid is sealed a band of rows a
  core; a lawn's canopy grid is as large as its lawn and no larger; a grid that wraps tiles the open sea, a
  kilometre to a tile in metre cells, and past its 6 km reach lies at its
  mean level out to the horizon. A block's
  four children are crossed at once: the slab test is written once over a
  lane type, one box or four a lane each, which SSE2 and NEON take two lanes
  an instruction, with the same bits either way. A block with no surface is
  skipped before its box is built, never left to the slab test.
- **Prototypes** are built once and placed as often as a scene wants: trees
  grown after Weber and Penn (SIGGRAPH 1995), palms and ferns of fronds,
  saguaros, rocks cut from noised icospheres, fallen trunks and stumps. Each
  is a list of parts — tapering limbs with rounded ends, leaves cut to an
  outline, triangles — under a hierarchy of its own, built a slice at a time.
  A trunk holds its girth up its bole by its kind's form before narrowing into
  its crown, bows in one gentle sweep, swells at its foot over a metre or so —
  drawn in short segments so the swell curves — and is gripped by roots that
  spur out of the flare and run along the ground into the soil; its bare bole
  carries the stubs of the branches it shed as its crown rose.
- **Bark** is laid on each limb along its stem and round its girth at real
  size, the circle round the limb carried onto a circle through the
  pattern's space so it closes with no seam, and each tree placed under its
  own key wears its own. Ridged barks are nets of fissures running up the
  trunk, parting and joining, their ridges shaped rounded or flat-topped and
  broken across here and there, knobbly and fibrous on their faces; pine
  thins from plates to orange flakes at a height each tree and each side of it
  reaches for itself, birch is white with lenticels over a black fissured
  foot, beech smooth, cherry banded, spruce scaled. Scars mark where limbs
  fell, lichen crusts what stands out, rain streaks it, soil splashes its foot,
  and its hollows darken with the walls about them. Detail finer than a pixel
  settles to its mean, in colour and in relief.
- **Lawns** root a few blades in each cell of a grid over the ground, each
  leaning no further than its cell's walls; a ray walks the cells it crosses
  low enough to reach anything, and the first thing met in the first cell is
  the nearest of all.

The scene's hierarchy over its objects is built a slice at a time too, each
step parting about a fixed share of the objects between children; the first
parts all of them once, in one linear pass. A walk through it hands over an
object at a time, so it can be left between objects and taken up again. An
object whose box is not finite is tested by every ray instead.

A pixel's eye rays are traced in packets of eight, a sampling round being a
whole number of packets. Each walks the hierarchy in its own order, testing
each object with its own reach as it stands; those that come to the same lawn
wait there for one another and cross its cells together, the rays standing in
one cell working out its ground, its stand and each shoot once between them
and each testing them as it would alone. Every ray still finds exactly what
it would alone; only what they share is worked out once.

## Water and relief

Relief tilts a surface's normal where its geometry is too fine to model: the
grain of honed stone, bark's ridges, and water's waves. Water's waves are a
spectrum of 96 travelling waves, one in each of 96 equal bands of log length
from the longest a breeze raises to the shortest, near-equally steep and
spread about the wind, scaled together to the slope variance asked of them —
Cox and Munk's fit for open water, less for sheltered — under a gust field
that lays them down in lulls and raises them in patches. A wave shorter than
about two of the pixel's footprints along the view lends its slope variance to
the surface's microfacet roughness instead of tilting the normal, so distant
water is a glossy sheet with the highlight its waves give it rather than
moiré, and glass turns a reflection its facet sends under the surface back
above it rather than losing it. The plane and solid noise every pattern is
drawn from is continuous across its lattice, the seed folded into each
corner's hash.

## Caustics

The light water's waves bend is gathered and spread as the surface curves
(`caustic`), beneath it onto the bed and whatever stands in the water, and
reflected onto whatever stands over it. The surface is cut into beams (Watt,
*Light-Water Interaction using Backward Beam Tracing*, 1990): each triangle of
a grid over it carries the sunlight crossing it, bent by the waves at its
corners, to any depth or height, where it covers a triangle of its own. A point
gathers the flux of every beam falling within the box the sun's disc and the
waves too fine to resolve blur a point into, each beam clipped to the box,
over the box's area, against what a level surface sends it. Over a level
surface the
beams tile the receiver, so every point takes exactly a level surface's light,
and the waves pass all of it on average; past a focus, where beams cross, every
one still counts, so the folds' bright lines are summed rather than missed.

Beams are laid only over the water whose light the picture shows. A survey of
it — every fourth pixel at `Simple`, every second at `Maximum` — finds what
the eye sees beneath water, over it and mirrored in it, and asks for the
two-metre tiles whose beams can reach each such point; a tile is laid only
where its waves move that light by a hundredth. A tile holds every level from
two cells a side to as many as 2⁹, a little under 4 mm, each level resolving the
waves six of its cells long and blurring the rest as the sun's disc does, and a
pyramid of each level's bounds — where the surface stands and how far its beams
drift either way — finds the beams about a point. A point gathers at the level
its own footprint asks, blended with the next, so the detail never steps from
tile to tile or with distance, but never at cells so fine that more than 256
beams lie in the patch of glints it gathers from: past a focus that patch grows with
depth and height, and what its cells cannot resolve the sun's disc blurs there
anyway. A scene's tiles hold at most 2²¹ cells at `Simple` and 2²³ at
`Maximum`, every footprint coarsened alike where they would not fit, and
their buffers are reserved whole and written a unit at a time. The survey
keeps each tile asked for once, in a hash map keyed by its place, a run of
points asking for the same tiles asking once. Along each row of a level the
waves' crests are swept a step at a time (`mathf::Phasor`) wherever the waves
lie level in the world, and read afresh at each point where a texture frames
them off level. Each level's pyramid is sealed from its foot, in bands of rows
across every core, then the rungs above.

## Light

Distributed ray tracing (Cook, Porter and Carpenter, 1984): each sample draws
its own point in the pixel, on the lens, on each lamp and through each glossy
reflection. Glossy reflection draws GGX visible normals (Heitz, 2018), and a
lamp it finds is weighed against sampling that lamp by the power heuristic
(Veach and Guibas, 1995). Glass follows reflection and refraction near the
eye; films interfere by the Airy sum; leaves and blades are lit through from
behind. The light diffuse surfaces gather from one another and the sky is
radiosity from an irradiance cache (Ward, Rubinstein and Clear, 1988) with its
gradients (Ward and Heckbert, 1992), laid over the picture coarse to fine
before its first pixel is traced. At `Maximum` a record gathers 1024 rays,
sixteen rows of equal cosine by sixty-four of azimuth, holds down to a 480th of
the picture's height, and a square of the picture holds at most 6400; at
`Simple`, 256 rays, eight rows by thirty-two, down to a 240th, and 1600 to a
square. Either way its rows are gathered a few a core at a time and it holds
within fifteen degrees of turn. A path that has scattered off a diffuse
surface drops the lamps' images and highlights it would otherwise find.

Sunlight reaches a point beneath water the way the surface above it bends it:
all but what a level surface reflects, absorbed along the bent way through the
water, and scaled by the caustics. A point over water facing it takes the sun
the water reflects, as much as a level surface's reflectance sends at the
sun's height, scaled by the reflected beams; a low sun's glitter, whose beams
swing too far along its way to resolve, reflects its mean. Either way a surface
takes that light diffusely, its highlight of it being the light's own image,
which its reflection finds. A path's footprint, which the patterns it meets
are averaged over, widens as the path passes through or glances off a rough
clear surface, a ray cone (Amanatides, 1984): through ripples a bed is seen,
and its caustics gathered, no finer than the ripples blur it.

The air between the eye and what it sees scatters the sun's light and the
sky's, kept apart in the aerial table: what stands in the sun's way shadows the
one, and the crowns roofing the air the other. Each of an eye ray's samples
tests one point drawn along it against the sun and its clouds, so the air in a
wood's shadow does not glow toward a low sun behind it, and shafts through its
gaps do.

A pixel's samples are drawn through a Gaussian reconstruction filter of half
a pixel's deviation, cut off at three deviations: each offset is the inverse
of the truncated filter's distribution (Giles' inverse error function), so
every sample weighs the same and the stratification survives, and detail
finer than a pixel is averaged rather than aliased. At its best a pixel is
sampled in rounds of 16, 32, 64 and 128, each a whole stratification of every
pair (Owen-scrambled Sobol, Burley 2020), and stops after any round whose
samples agree; `Quality` caps the rounds, and the screensaver always traces at
the best. Each sample is toned (ACES filmic, Narkowicz) before the samples are
averaged, then encoded to sRGB through a lookup table and the desktop's
ordered dither. A metered exposure
takes the frame's trimmed log mean to its key, then pulls down by up to two
stops where more than a twentieth of the frame would blow out: the sun and
its glints may, a sky or sunlit bark may not.

What one exposure cannot hold is then compressed locally, gently, as a
photographer would (`adapt`). Once the exposure and the glare are set, the
meter traces about 9216 more film points at the picture's shape (128 by 72 on
a widescreen picture), four samples apiece, into a bilateral grid over the
film and log luminance (Chen, Paris and Durand, 2007): cells eight points a
side and a stop deep, twelve stops either side of the key, blurred 1 4 6 4 1
along each axis, so each holds the mean log luminance of the like-lit ground
about it — Durand and Dorsey's base layer (2002). Each eye sample reads the
grid at its own film position and exposed luminance before the filmic curve,
so a pixel still traces the same on any core in any order. Within a stop and a
half of the key nothing changes; beyond, the correction eases in to half the
excess and settles toward two stops down for a highlight and one up for a
shadow. A window and the dark room about it lie in different layers, so
neither haloes the other; texture keeps its contrast; the sun still blows out.
A scene none of whose bases lie that far from its key keeps no grid, and is
traced exactly as without one.

## Reveal order

`Reveal::new((width, height), key)` orders a picture's pixels coarse to fine,
each traced once. Pass by pass it traces the points of a grid whose spacing
halves each time: the first pass every point of a grid whose spacing is the
largest power of two leaving at least eight points across the shorter side
(`Reveal::coarsest`; 128 pixels on a 1080-line screen, so 135 points span it),
each later pass the three points in four the grid of twice its spacing did not
hold, down to single pixels. A `Step` names its pixel and its pass's spacing.
The grid twice as coarse is whole when a pass begins, so every point of a
pass's grid lies within one of its spacings, each way, of a traced point.
Within a pass the
steps follow a keyed bijection on the pass's range, so the whole
picture sharpens at once, and a step is found from its index alone.

## Progress

`Draft::progress` answers how far a draft's work has come in thousandths,
never falling back and reaching a thousand only once the scene is ready. Each
stage of the work holds a share of the whole measured over the settings at
each detail —
composing (on a land, the land's build, its planting, then the grids,
prototypes and sky its look queued), the hierarchy, the caustics, the radiosity
records and the meter and its adaptation — and reports how far through itself
it is. A land's build weighs
each of its own stages by its items (vertices filled, coarse samples worn,
droplets run, vertices settled) times the measured cost of one, so the readout
keeps pace with a desert's droplets as with a valley's wear.

## Budgets

A scene holds at most 131 072 objects at `Simple` and 524 288 at `Maximum` (a
forest's trees, understory and deadwood among them, each 320 bytes), 4096 hull
faces, 256 materials, 12 lights, 12 height grids, 96 prototypes, 16 lawns and
8 woods, and a path at most nine bounces.

A scene may take up to 160 s to prepare on a desktop-class machine across 8
threads and hold up to 2 GB at its peak at `Maximum`, far less at `Simple`
(`plans/RAYTRACE.md`). Measured at 1920×1080 on a 24-thread desktop preparing
across 8 threads, a landscape prepares at `Maximum` in 0.8–33 s — a meadow
in 24–31 s, a forest in 33 s, a desert in 6 s, a lagoon in 0.8–1.8 s, most of
each its radiosity records and up to half of a lagoon's its caustics — holding
at most about 580 MB at its peak and once prepared, a mountain lake's, whose
beams are many; at `Simple` it prepares in 0.3–4.0 s, holding at most 324 MB
and 323 MB. Laying a scene's caustics takes up to 0.25 s at `Simple` and
0.95 s at `Maximum`, and holds up to about 75 MB and 330 MB. Traced on one of its cores,
built for the x86-64 baseline (SSE2), a sample costs from about 2.4 µs (the
checkerboard, mostly its clouds) to about 19 µs (a meadow, mostly its eye rays
over grass) at 640×360 and full quality: a forest about 17 µs, a valley 12, a
colonnade 7; the local adaptation's correction costs a sample about one per
cent more, and beneath or over water the caustics about 2.5 µs.

Every unit of preparation is a fixed amount of work a core — a band of a
grid's rows or of a shade's, a turn of a land's droplet tiles, a band of the
objects' boxes or a slice of a hierarchy, a ring's places drawn or a run of
their ranking, a few rows of a radiosity record's hemisphere, a handful of
the meter's samples, a hundred or so of the caustics' survey points, a couple
of thousand of their beams or a few thousand of their pyramids' nodes — and a
wood's places are kept in runs that never move as
more are added, so `Draft::prepare` answers within about a frame however large
the scene: measured, no unit takes more than about 10 ms at either detail. Under the
screensaver's `idle` setting the preparation runs on one core and takes some
eight times as long.
