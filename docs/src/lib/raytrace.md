# tairix-raytrace

`lib/raytrace` is the ray tracer behind the desktop's ray-traced screensaver
(`docs/src/desktop/session.md`): scenes composed at random from a seed, and a
tracer that answers what one pixel of one shows. It is `no_std` + `alloc` and
forbids `unsafe`.

## Scenes

A `Draft::new(setting, seed, size)` composes a scene in one of the nineteen
`Setting`s. Within a setting everything is drawn from the seed: which pieces,
where, in what, lit from where, at what hour and under what weather, and seen
from where — and every draw is bounded so the scene is lit and framed to read.

- **Still lifes** stand on a plane: a checkerboard of spheres, gems, rings and
  stacks under the open sky; a studio's plinth under softboxes, high or low
  key; crystal clusters, or crystals grown out of a stone, on black glass at
  dusk; glass and chrome among lamps at night; soap bubbles drifting over a
  meadow.
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
the sun and the sky they stand in front of.

## Lands

A land is a landform worn by water through `lib/terrain`
(`docs/src/lib/terrain.md`): Priority-Flood drainage, stream-power incision
and hillslope creep over a coarse grid cut its valleys; its rivers, lakes and
roads are laid where the water and the ground let them run; and a far grid and
a finer grid about the eye refine it, droplets running over each for the rills
and fans no coarser pass makes. Each vertex carries what the land is like
there — wet, worn or built up, on a road or a path, how much grows — which its
shading, its sward and its woods read. Grass is laid over it in lawns of
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
  across the runner a band at a time. Those that would grow are thinned in
  order of height: a tree stands only where its trunk keeps from every taller
  one's by their crowns' reaches together, times the wood's closure there, a
  much shorter tree standing under a taller one's crown more readily than
  beside a peer's, and a stand draws two in five of its trees overtopped or
  suppressed. No tree stands in the way of the view close ahead of the eye.
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
cast over the land (`shade`): how far under a crown a place lies, and how much
of the sky the crowns about it hide — the mean cover of the ground within a
dozen metres, twice box-filtered. A lone tree hides little of the sky; a
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

- **Height grids** hold `f32` heights at the vertices of a square grid and the
  bilinear patch between them. A ray walks a pyramid of maxima (Tevs, Ihrke
  and Seidel, *Maximum Mipmaps*, 2008) and meets a cell's patch where a
  quadratic along it says; a grid that wraps tiles the open sea, a
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
before its first pixel is traced; a path that has scattered off a diffuse
surface drops the lamps' images and highlights it would otherwise find.

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

## Reveal order

`Reveal::new((width, height), key)` orders a picture's pixels coarse to fine,
each traced once. Pass by pass it traces the points of a grid whose spacing
halves each time: the first pass every point of a grid whose spacing is the
largest power of two leaving at least eight points across the shorter side
(`Reveal::coarsest`; 128 pixels on a 1080-line screen, so 135 points span it),
each later pass the three points in four the grid of twice its spacing did not
hold, down to single pixels. A `Step` names its pixel and its pass's spacing.
The grid twice as coarse is whole when a pass begins, so every point of a
pass's grid is either traced or lies between traced points, and within a cell
of a pass's grid only its top-left corner can already be traced. Within a
pass the steps follow a keyed bijection on the pass's range, so the whole
picture sharpens at once, and a step is found from its index alone.

## Progress

`Draft::progress` answers how far a draft's work has come in thousandths,
never falling back and reaching a thousand only once the scene is ready. Each
stage of the work holds a share of the whole measured over the settings —
composing (on a land, the land's build, its planting, then the grids,
prototypes and sky its look queued), the hierarchy, the radiosity records and
the meter — and reports how far through itself it is. A land's build weighs
each of its own stages by its items (vertices filled, coarse samples worn,
droplets run, vertices settled) times the measured cost of one, so the readout
keeps pace with a desert's droplets as with a valley's wear.

## Budgets

A scene holds at most 131 072 objects (a forest's trees, understory and
deadwood among them, each 320 bytes), 4096 hull faces, 256 materials, 12
lights, 12 height grids, 96 prototypes, 16 lawns and 8 woods, and a path at
most nine bounces.

A scene may take up to 160 s to prepare on a desktop-class machine across 8
threads and hold up to 500 MB (`plans/RAYTRACE.md`). Measured on a 24-thread
desktop preparing across 8 threads: a forest stands
some 75 000 plants over its land and prepares in about 6 s — the land about
1.4 s, its woods and sward 0.8 s in steps of a few milliseconds, its
prototypes 0.4 s, and its radiosity most of the rest — and holds about
260–440 MB while it is traced. Traced on one of its cores, built for the
x86-64 baseline (SSE2), a sample costs from about 2.4 µs (the checkerboard,
mostly its clouds) to about 19 µs (a meadow, mostly its eye rays over grass)
at 640×360 and full quality: a forest about 17 µs, a valley 12, a colonnade
7. Every unit of preparation is bounded — a band of a grid's rows, a
slice of a hierarchy, a band of a wood's places, a lawn — so `Draft::prepare`
answers within a frame's slice however large the scene.
