# tairix-raytrace

`lib/raytrace` is the ray tracer behind the desktop's ray-traced screensaver
(`docs/src/desktop/session.md`): scenes composed at random from a seed, and a
tracer that answers what one pixel of one shows. It is `no_std` + `alloc` and
forbids `unsafe`.

## Scenes

A `Draft::new(setting, seed, size, detail)` composes a scene in one of the
twenty-one `Setting`s. Within a setting everything is drawn from the seed: which
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
  moon; soap bubbles drifting over a meadow. Only the studio is indoors:
  the others stand under the open sky's weather, dusk and night included.
- **Buildings** stand on a paved plaza, on tiles to the horizon or on open land
  with woods about it: a colonnade in three orders, a loggia of arches in
  stone or in brick or a two-tier aqueduct across a valley, a domed rotunda,
  a ruined temple in long grass; a sculpture stands out in a meadow, on dunes
  or by a mountain lake. Every one is laid stone by stone (below).
- **An aqueduct** spans its valley in an upper tier of arches carrying its
  channel's covered conduit and, where the valley lies deep, great arches
  beneath each two of them. It is laid out on the land as it was sited and
  founded on the land as built wherever that lies lower, so no pier stands
  on air over a hollow the coarse land smoothed away. Beyond its arches the
  conduit runs on into either hill in a cutting dug into the land, until the
  hill stands a cutting's depth over it; there, where the hill as built
  covers the conduit, it tunnels through a portal whose headwall steps with
  the hill it holds back and whose arch's tympanum is walled up in rubble
  over the conduit, and where the hill is too low it runs on buried until
  the hill covers it.
- **Landscapes** are lands: rolling meadows, farmland seen from one of its
  lanes — fields in hedges, dry-stone walls and fences, gated, with woodlots
  among them (`lib/countryside`) — a forest, mountains over a lake,
  an island's coast, dunes or rocky desert, snow over a frozen pond, a lagoon
  at sunset, canyons between mesas, a river valley crossed by a stone
  bridge, and a stream running clear over its stones, seen from its edge.

The weather (`compose::weather`) sets the hour and the cloud. The sky is a
physical atmosphere over a round Earth (Hillaire, EGSR 2020), its sky, sun
colour and aerial perspective tabulated once per scene; clouds are volumetric
decks marched as the medium they are (Schneider 2015; Hillaire 2016), dimming
the sun and the sky they stand in front of. Each cell of a deck's weather map
bounds the heights its cloud can reach, so a march strides straight through
the air above and below them. Cirrus is a deck of its own, of ice, in a bank
high above the rest.

A deck runs on to the horizon. Its heights are taken over the Earth's curve
about its centre below the eye, as the air's are, so a deck seen low down
comes down to the horizon as far off as the curve lets it be seen — some
120 km for a cumulus base, nearly 380 km for the highest cirrus — and a ray
below the horizon meets the Earth before any of it. Its maps lie in levels
about the eye, each twice the last's breadth in as many columns (`cloud`,
after Losasso and Hoppe's clipmaps), so a column spans about as many pixels
far off as near; each level's weather is drawn only as finely as its columns
hold, its finer octaves settling to their mean (`noise::fbm2_resolved`), and
each level's rim is blended into the next level's own reading, so no seam
shows where they meet. Each place of cloud takes the sunlight the air brings
to it at the angle the sun stands there, so at dusk cloud far off toward the
set sun is still lit after the cloud overhead has greyed, and cloud far off
the other way darkens first. The aerial table reaches that far too, its first
slices where they always were: 32 within 60 km over the land, 88 to 454 km.

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
- **The moon** lights the night at its phase: full, 14 magnitudes fainter
  than the sun (−12.74 against −26.74), waning by Allen's phase law
  (0.026|α| + 4·10⁻⁹α⁴ magnitudes at a phase of α degrees), and warmer than
  sunlight, as the ROLO lunar model (Kieffer and Stone 2005) gives its
  reflectance across the channels. Its disc is lit by the Lommel–Seeliger
  law, μ₀/(μ₀ + μ), evenly at full and toward the sun otherwise, so a
  crescent's horns face the sun, and no part brighter than the full moon lit
  as it is; and all of it by the Earth's light — the full Earth's, waning as a
  Lambert sphere does, about a ten-thousandth of the sun's — bluer than its
  sunlit part, which a thin crescent's dark side shows. It is reckoned from
  the eye, nearer and so larger and brighter the higher it stands. By night
  the sun lies set beneath it, at least 15° down, as far as its phase has it;
  by day now and then, and at dusk as a young or an old crescent, a moon
  stands in the sky lighting nothing beside the sun, and no star shows
  through its disc. The night sky is the atmosphere lit by it.
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
  moon gives the faint light it does.

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
shade the ground.

Water cloud scatters the sun once exactly, through a droplet phase of a
forward lobe and a small backward one; every further order of its light is
the δ-Eddington two-stream solution (Joseph, Wiscombe and Weinman 1976,
`slab`). The cloud's light grids hold its optical depth toward the sun, away
from it, straight up, and level away from it. The sun's light is solved in a
slab facing its beam, as deep as the beam's way in and as thick as the cloud
runs on behind it, as a heap lit from aside is, and in the cloud's column,
as a deck is, through which it diffuses down whatever way it fell in —
the deck's taking over the further the cloud runs across than down. The
sky's light and the ground's are solved in the column, entering by the
nearest face the sky lights. The light the cloud scatters
toward the eye is each diffuse field's mean and its first moment, through the
full phase's asymmetry. A thin cloud thus takes little but its single
scattering, a heap's sunlit side shines and its base greys, its shaded side
is lit by the sky and by what of the sun diffuses through it, and a lossless
slab sends back or through all the sun it takes in, to a percent.

## Lands

A land is a landform worn by water through `lib/terrain`
(`docs/src/lib/terrain.md`): Priority-Flood drainage, stream-power incision
and hillslope creep over a coarse grid cut its valleys; its rivers, lakes and
roads are laid where the water and the ground let them run; and a far grid and
a finer grid about the eye refine it, droplets running over each for the rills
and fans no coarser pass makes, a turn of the land's tiles at a time across the
runner. Each vertex carries what the land is like
there — wet, worn or built up, how much of a road and of a path, how much
grows, each a quantity blended on its own — which its
shading, its sward and its woods read; the coarse grid running on to the
horizon carries it too, judged by its slope, its height and the sea as the far
land's is, so a dry land's distant ground grows no greener than its near. The rivers' and lakes' surface is a grid
of its own, wet a cell's diagonal past a river's banks, so a river however
narrow and however it slants across the grid keeps its water in every cell its
course crosses; what lies past the banks lies under the ground, unseen. Grass is laid over it in lawns of
coarser cells the further they lie, merging leaves finer than a pixel and
fading into the ground's own colour far off.

Snow on a land lies as the wind laid it (`snow`), never as an even mantle.
How sheltered each place stands is read once from the worn relief at the
coarse grid's samples, a band of rows a core, and carried to every finer
vertex through the same Catmull–Rom patch the relief is: the steepest slope up
to the ground within 100 m upwind, the mean of it over the wind's heading and
fifteen degrees either side (Winstral, Elder and Davis 2002), less what the
wind quickens by over a crest curving up 50 m about a place and up a slope it
climbs (MicroMet, Liston and Elder 2006). The fall, uneven by fifteen per cent
in patches some 70 m across, is moved by up to 1.4 of itself by that shelter:
deep in the lee of every rise, scoured to a crust on crests and up the slopes
the wind climbs, and sloughing off ground steeper than it rests on. On the
finer grids, where the wind has scoured it, it is cut into snow dunes 9 m
apart and sastrugi 1.6 m apart running along the wind, each only as fine as
the grid resolves and never through the snow (Filhol and Sturm 2015). Each
vertex carries the snow's depth beside what else the land is like, kept to
the millimetre about nought, where a few centimetres decide what shows: the
ground's colour shows where less than about 6 cm lies, in patches, and grass
grows only where it lies thinner than the grass stands, a shoot rooted on the
snow showing only what stands above it.

A valley's sides climb as steeply as its rock stands, its floor wandering
between spurs, and its view is taken from partway up one side with what it
looks at in sight. A work may dig cuttings into a land (`land::Build::site`):
each a level floor along a course, its sides climbing back to the land as
steeply as cut earth stands and ending square at the face of whatever it
leads to. A cutting only takes ground away, and it is held through the
droplets run over the finer grids after, as a river's channel is, so a level
trench never silts up.

Stones lie on a land as it would have them (`compose::strewn`), from eight
rocks of its own stone a scene and draws of their own, so a detail strewing
more shifts nothing else: gathered in boulder fields where a broad noise some
70 m across has them, each boulder among up to three smaller ones; fanned in
scree on the talus below a crag found within 22 m uphill, likelier the nearer
it and the larger stones the further they rolled; a few strays elsewhere;
and drifts of four to a dozen pebbles washed into the wet or silted hollows
within 15 m of the eye at `Simple` and 30 m at `Maximum`, which strews three
times the boulders too.

Mud dries and cracks (`mud`, `compose::cracked`) on level, bare silt out of
the water — a canyon's or the badlands' washes, a valley's banks in summer
and autumn — within 18 m of the eye at `Simple` and 36 m at `Maximum`, in
patches. It is laid in 3 m tiles square to the land's own lattice, each the
plates of a Voronoi pattern: every plate curls toward its rim, its corners
the most, over a wall undercut beneath the curl, and the cracks between them
run from hairlines to wide ones, every side bent by a field that repeats with
the tile. Each tile shares the plates along its edges with every other, so
the few drawn meet in any order without a seam. A tile is laid only where
its ground lies within a centimetre of its plane, in the land's own silt,
bleached paler, over its earth gone damp beneath.

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
- **One reading, near and far** (`wood`). Whether a place grows a tree and
  what tree is read from the place and the ground there alone, by the one
  reading every wood is stood by: its patches and gaps first, then the
  ground's slope, wet, growth, roads and heights, then the kind that takes to
  it best and its stand's age.
- **Thinned tallest first.** The places a tree might stand are sown in rings
  about the eye — all the way round as far as the trees' shadows reach, then
  across the view out to the horizon, as far as a tree still spans a pixel or
  two, as far as the wood's most trees would fill, or as far as the detail's
  budget of places holds them, never sown coarser, which would thin the wood —
  and read across the runner a band at a time. Those that would grow are ranked by
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
  short and less branched; a dead one stands snapped off where its trunk was
  still thick, its biggest limbs broken back to a third of their length, as
  thick where they broke, every break torn.
- **Far off** (`far_wood`). At *Maximum* a wood of trees carries on past the
  trees it stands one by one, out to as far as its trees still span a pixel:
  its trees hashed from the cells of a lattice rather than stood, each cell's
  one place read by the same reading and kept as often as places sown as
  closely as the trees stood near, each growing as often as the ground suits
  it, come to once thinned to their crowns' room — the share of the ground
  their crowns fill matched to the trees stood near over the ring where they
  meet, so the wood stands as thickly far off as near. Once its trees'
  prototypes grow, its lattice is surveyed a few hundred blocks a step: every
  place's tree read once, a bit a cell kept for those that stand one, and for
  each block of 16 × 16 cells the lowest ground under it and the highest crown
  standing over it. Nothing else of it is stored, and it is traced as the
  tiles of its land a crown stands over, each boxed to those crowns. A ray
  passes a block it crosses over every crown, walks the rest a cell at a time
  meeting each tree that stands within reach once, and grows a tree only
  where it passes beneath the crowns about it and low enough to meet the
  tallest the place could grow; a tree met is placed again from its cell when
  it is shaded, so its pattern and its key are its own. Seen from the eye's
  height in the settings' own views, the trees stood near hide it; it shows
  from higher up, and costs a few per cent of a scene's preparation and
  tracing at *Maximum*. A wood of shrubs is low enough to be stood one by one
  as far as it is seen.
- **Beneath.** Shrubs, ferns and young trees are sown in the shade the canopy
  casts: next to none under a closed canopy, most in its gaps and along its
  edges, fewer again out in the open, where grass takes the ground.
- **Dead.** Fallen trunks — thrown with their roots or snapped at their feet,
  their limbs broken to stubs, in weathered bark taken by moss — lie about the
  eye, three times as thickly where the canopy has opened and those lying the
  way the wind threw them; stumps, sawn or snapped, and standing dead trees
  stand among the living, each stump's foot flaring into the roots it stands
  on. A thrown trunk tore up the plate of soil its roots held: a bulbous mass
  of crumbling clods standing on edge across its foot, the trunk flaring into
  it, its roots running out through its underside with their backs bared and
  broken off past its rim, those that sank deep broken short of its
  underside every way, a mat of fine roots bristling from it and fine roots
  hanging from its rim.
- **Decaying.** A kind's dead have lain as long as each other, a few seasons
  either way (`deadwood::Decay`). Their bark loosens once the wood has begun
  to rot and sloughs away in sheets, by their kind: an oak's thick corky bark
  stays on in its plates for years, a birch's outlasts the wood it wraps, a
  beech's falls early. The wood it bares lies sunk below it, tan where it fell
  lately and weathering grey-brown, its grain raised, checked along it and in
  places engraved by the galleries of the beetles that fed beneath the bark;
  where a sheet broke away its edge is dark. Moss takes dead wood the longer
  it lies, in crisp cushions that take its furrows before its crests. A sawn
  face, ringed with its years and weathering from tan to grey-brown, its bark
  cut through in a dark ring about its rim and following the lobes a low cut
  shows, dries into checks running in from its rim, more and wider the older
  it is, and a felled stump keeps the step from its notch to its back cut and
  the hinge it tore across, until the hinge rots away; past about half its
  span a heart rots hollow, a pit of crumbling brown in a stump's face and in
  a log's broken ends. Rot fruits in brackets the likelier the longer wood has
  lain — turkey tail's thin banded tiers, an artist's bracket's woody shelves,
  a birch's own polypore — shelving out level from a log's flanks or a stump's
  sides, pale margins round them and their pores beneath. A broadleaf's stump
  sends up shoots from below its cut or about its root collar, arching out
  and up toward the light, leafy but in winter; a pine's or a spruce's never
  does.

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

## The water's edge

Once the woods' shade is cast, and before the sward is laid in it, a scene
sets out the plants of its water's edge about the eye (`compose::waterside`).
Reeds and reedmace stand in the shallows and on banks wet, level and off any
way, reeds the higher up the bank and in the brisker water; water lilies and
pondweed float where the water lies still and as deep as each roots in,
pondweed the shallower and the less still. Each wants the light the woods
leave it, so none stands in a closed wood, pondweed bearing the most shade.
Depth and stillness are read from the land's fresh water — its level over the
ground, and how steeply its surface falls — or from the lake a land's sea
stands for. Over winter the floating plants die back and the reeds stand on,
dry.

Each plant is grown into square patches (`waterside`): reeds leafy up their
stems, each ending in its youngest leaf rolled into a spear or, once it
flowers, in a nodding branched plume tufted with silky spikelets; reedmace in
fans of strap leaves, its velvet spike blunt at both ends under the withered
spire of its male spike and bursting in fluff over winter; water lilies; and
rosettes of pondweed. A lily's pads (`lily`) are meshes, each cut to an
outline and worn as its own key and its age have it — oval and waved, its
lobes meeting, parted or overlapping, bronze and rolled as it unrolls, then
grazed by beetle larvae, bitten from its margin by moth larvae, split along
its veins, holed, spotted and frayed, yellowing and browning as it dies and
sinking at last — and its colour is drawn from the same key, so a wound dries
dark along the very cut. The pads are laid one after another, each resting on
those already floating beneath it. On one plant in eight in summer a flower
floats beside its pad at its own stage, from bud to spent: sepals and
spiralled petals cut to their own outlines, each bent, twisted and cupped its
own way and browning from the tip as it fades, stamens crowding a rayed
stigma. A crowfoot's small flower rings a knobbly head of carpels with broad
petals. A clump near the eye is drawn in full, its stalks running down under
the water; a bed beyond plainer. Each cell of a lattice of
0.75 m cells within about 45 m of the eye, and of 3.75 m cells beyond to the
detail's reach, holds a patch of the plant its place suits best or none, as
likely as the place suits it in that plant's own patches and gaps; the better
it suits it, the taller and thicker its patch, so a bed thins and shortens
toward its edges. A patch whose square reaches a piece already standing — a
boulder, drift, a trunk, a pier — is left out, though ground a scene keeps
open of pieces, a pond or the eye's own, is no bar to it (`Taken::Open`).
`Simple` sets them out to 250 m, at most 8000 patches;
`Maximum` to 700 m, at most 40 000 — where more would stand, those nearest
the eye, so the water's edge ends at a distance rather than part way across
the view. A plant's clumps and beds are planned as prototypes only once a
patch of one is set out, so a plant no water in the scene suits costs it
nothing; once the scene plans no more prototypes, a patch never planned is
left out rather than the scene refused.

## Streams

A land keeps its rivers, each mark carrying how far its water has run from
the divide, where it lies in its run of pools and riffles, how it bends and
how steeply its brim falls. Their channels (`channel`) are drawn from that
alone, so the land's grids, its water and a stream's flow read the same one.
A course is counted in units of pool and riffle some six widths apart,
shorter as it steepens toward steps; at low water each pool stands ponded
behind the crest below it and the water falls fast down the riffle after,
and where bedded rock holds a reach two or three units' fall gathers at one
ledge over a plunge pool. In a pool the deepest water swings to one bank —
from side to side unit by unit along a straight reach, to the outside of a
bend — cutting that
bank steep while a bar rises gently on the other, sanded in patches where the
slack water drops it. The bed is lumped by the gravel its floods heaped and
the hollows they scoured and its breadth wanders, so its water's edge does
too; its banks rise over faces of earth, broken by benches, slumped and
bulging, their tops lifted into levees or let down, to the land beyond, or
down to it where the land lies below a perched river's brim. The
channel stands as it carved it against the droplets run over the land after.
The water runs as fast over each crest as down a riffle, slow where it
deepens and fast where it shallows.

Seen close, ground holds grain to the millimetre — crumbs, coarse and fine
sand, a scoured bed's gravel of the land's own rock and the cobbles in it,
relief in five octaves sharing a grain's depth — each only as finely as a
pixel resolves it,
settling to its mean beyond. A river's bare rock is stained dark by its
water; bedded rock breaks in blocks along two sets of upright joints and its
beds. Nothing roots in the bed its floods scour or the fringe up its banks,
but pioneer plants take the tops of its bars.

A stream's bed (`compose::stones`) is the stones its bankfull flow can move,
their median the stone that flow just stirs (Shields' criterion), the rest
spread lognormally about it. Each has come a distance drawn evenly from
nought to the run above it and is worn as far as it came, against its rock's
rounding length; slate stays flat and angular, splitting along its cleavage
as fast as it rounds. Stones are drawn from a lattice per size class about
the eye, kept where they span a few pixels at their distance and gravel can
rest, sparser on sand and bare rock, laid largest first where none already
lies, each on its flattest side, its length across the stream, its upstream
end dipping and its foot bedded in the gravel. Where a bank's rock outcrops it
stands out of the earth as blocks; boulders fallen from the banks, most where
a pool cuts one or its rock outcrops, roll down whatever is too steep to hold
them, often into the water at the bank's foot; both are mossed above the
water, and stand in its flow as they lie across it. The floods leave the streamside trees' wood lodged in the
stream — branches stranded at the edge, jammed across the flow, sunk in the
pools or fallen in from the banks, and where the stream is narrow a trunk
across it with branches piled against it, never through a boulder — each
break torn and splintered, never rounded.

The water's edge plants (`compose::waterside`) take only its slack water and
silt — reeds, reedmace, pondweed in its pools — and water-crowfoot streams
down the current where it runs over gravel, each set out on the water as
the flow has shaped it.

A rock (`rock`) is worn by the mean-curvature term of Bloore's flow: its
surface moves in as fast as it curves out and never out, so its edges round
first and its hollows last, each vertex kept to its own ray from the middle.

The stream's surface about the eye is its own grid, meeting the far water's
along its border, where it gives way to it over a metre and a half, so a ray
meets one surface of water there and not two. It is shaped by the flow's
steady answer to the stones and the wood in it (`stream`), over the stretch
the eye looks along, the more of it ahead whichever way it looks: linear potential
flow over a finite depth with gravity and surface tension, solved by Fourier
transforms of a grid along the stream (`fourier`) at four depths and three
speeds, each place taking the answers about its own, a stone parting the
water in proportion to its speed. Over a stone the water
dips or humps, lee waves stand behind it fanned in a V, and before a stone
through the surface it piles up, its wake falling away behind; the pools lie
glassy and the riffles stand in waves. No place rises past the stream's
velocity head. Foam comes only where the water breaks about an obstruction —
a wave standing steeper than water can, the wake a stone through brisk water
sheds, the pool where a ledge's glassy tongue lands — and bursts within a
second; then fast water
churns and shallow water drapes over its gravel, texture its breaking never
sees. The foam is held on the grid and broken into bubbles and streaks where
it is shaded.

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
  saguaros, rocks cut from noised icospheres, fallen trunks and stumps, and
  structures laid unit by unit. Each is a list of parts — tapering limbs with
  rounded or open ends, a limb's foot flaring, leaves cut to an outline,
  triangles, solids — under a hierarchy of its own, built a slice at a time. A triangle with no material of its own
  takes the one its placing is made in, so one rock is laid wet, dry or
  mossed. A mesh may be mapped: its vertices carry where on its own surface
  they lie, in metres, and its triangles its key, its size and the outline it
  is cut to, so a pad or a petal shaped in the round is cut to an edge finer
  than its mesh and shaded by where on itself a ray met it.
- **Saguaros** (`cactus`) are ribbed flesh, their ribs counted by their
  girth, each crest wandering and grooved between, felted areoles along each
  crest and spines from them coloured by their age — red-brown at the apex,
  then tan and grey, weathered pale toward the corked base — and pinched where
  droughts constricted them; a saguaro is built a share of its areoles a
  step. A palm stands on a swollen foot matted with short curved roots, some
  dead and snapped; a shrub grows from a buried stool, its stems swelling
  where they leave it.
  A trunk holds its girth up its bole by its kind's form before narrowing into
  its crown and bows in one gentle sweep. Its foot (`foot`) swells all round
  and out toward each of its roots as a buttress, the trunk deforming toward
  each, and each root runs on out of its lobe where the lobe comes down to
  the root's back, just under half buried, wandering to one side as it
  narrows, forking now and then, and diving into the soil, a rootlet carrying
  it on down, so no end of it shows. Its bare bole carries the stubs of the
  branches it shed as its crown rose, each broken off torn.
- **Flares** (`flare`). A limb's foot swells by a share of its radius all
  round and by a lobe toward each root, each falling away up the limb and the
  whole faded to round before the limb's flare ends, so the limb above meets
  it in round. A flared limb is met at any distance by the march that cuts
  bark (below), its radius the flare's at each height and angle; its bark is
  the limb's own, laid at the swollen girth, so its pattern runs on down into
  the lobes. Its bounds are read round it where its lobes are fullest, widened
  by as much as they could rise between readings.
- **Breaks** (`fracture`). Wood snapped across its grain is torn, never
  rounded. Bent until it gave, it failed in tension on the side bent away
  from and crushed on the other: the face climbs toward a crest where its
  fibres pulled out, ragged in fibres a centimetre apart, the more so on its
  pulled side; laths stand from it, slabs split along the wood's rays with
  sharp faces, leaning out a little, narrowing and splitting at their tips,
  most where the wood was pulled and toward the rim, and those torn from the
  rim keep the bark's dark edge on their outer face; the bark tears about
  level, a thin dark edge about the rim, and hangs in a tatter or two. A break
  that has lain long has lost its finer splinters and its heart has rotted
  hollow. However broad, a break is torn as deep as its wood is thick across
  its narrowest. Stumps, fallen trunks' ends, their limbs' stubs, a bole's
  stubs, a dead tree's top and limbs, and the roots a thrown trunk tore up all
  end in one.
- **Snowmen** are built as children build them (`snowman`): two balls or
  three, each smaller than the last, rolled from the snow, so none is round.
  A rolled ball is a drum, narrower along its roll axis than round it, lumped,
  wrapped in the sheets it took up — each sheet's end a lip a centimetre or so
  proud, thinning behind it — and streaked round its drum with the earth, dead
  grass and leaf it picked up; a head is as often packed by hand, rounder and
  lumpier with no sheets. Each is dented where it was patted, flattened where
  it was set down and where the next was pressed onto it, snow packed about
  the join, settled under its weight, and stacked leaning a few degrees and set
  a little off its seat. Lumps of coal, broken fresh along their fractures, are
  pressed in for its eyes, a mouth and buttons; a carrot ringed where its fine
  roots grew, tapering away to the thread of root it ends in, is pushed in for
  its nose, drooping as it may; and forked sticks narrowing out to fine twig
  tips are pushed into its sides for arms. Each ball is a mesh about a centimetre to
  a facet, shaped a few thousand vertices a unit.
- **Bark** is laid on each limb along its stem and round its girth at real
  size, the circle round the limb carried onto a circle through the
  pattern's space so it closes with no seam, and each tree placed under its
  own key wears its own. Ridged barks are nets of fissures running up the
  trunk, parting and joining, their ridges broken across into blocks: an
  oak's, a willow's or a poplar's furrows are sharp at their floors and
  red-brown down their walls, climbing to narrow crests that crumble into
  corky scales, cracked along their grain and knobbly; pine
  thins from plates to orange flakes at a height each tree and each side of it
  reaches for itself, birch is white with lenticels over a black fissured
  foot, beech smooth, cherry banded, spruce scaled. Scars mark where limbs
  fell, lichen crusts what stands out, rain streaks it, soil splashes its foot,
  and its hollows darken with the walls about them. Detail finer than a pixel
  settles to its mean, in colour and in relief.
- **Bark in true relief** (`cut`). Near the eye a limb thick enough to have
  fissured — from 3 cm in radius, fully by 12 cm — is cut by its own bark:
  its ridges stand at its radius and its fissures are sunk the bark's depth
  into it, so its outline is ridged and its furrows shadow one another. It is
  cut in full where the cut spans three pixels or more, fading to none where
  it would span one and a half; farther off it is its tube again, its bark
  tilting the light. The cut surface depends only on where a point lies and
  where the eye stands, so every ray meets the same one: it is found by sphere
  tracing (Hart 1996) against twice the steepest each bark was found to rise,
  never in steps shorter than three quarters of a pixel there, and the
  crossing bisected to a hundredth of that; a shadow ray stops at the first
  crossing. Past each end it rounds, the cut limb runs on within that end's
  sphere, so a bending limb's joints stay closed and a free end stays round.
  Beyond the bark's outer surface, and deeper than its cut, the bark cannot
  change which side of the cut surface a point stands, so it is read only in
  the shell between, and outside it the march steps by the limb's own far
  gentler rise. Its normal is the gradient of the surface it crossed, read
  either side of the hit, so it faces whatever ray crossed into it however
  steep the bark's walls or the flare's lobes; an end, rounded or open, is not
  read, so a hit at an open end's rim faces out of the limb's side. Moss on
  the bark is cut in with it, filling the fissures before it takes the
  crests and standing proud of them by as much as its cushions rise; the
  limb's bounds and the shell the bark is read in reach as far out as it
  stands, and the share of bark the march found it covering is the share its
  colour shows.
- **Lawns** root a few blades in each cell of a grid over the ground, each
  leaning no further than its cell's walls; a ray walks the cells it crosses
  low enough to reach anything, and the first thing met in the first cell is
  the nearest of all. Fallen leaves lie only where the ground is gentler than
  they stay on.
- **Solids** (`solid`) are a structure's units: a block, its ends leaning in
  as a voussoir's do; a drum, tapering and swelling in its entasis and cut in
  flutes; a turned moulding; an Ionic volute channelled between its turns; a
  field stone, a slab broken along a few planes: the flat bed it lies on, a
  face tilted about where a wall sets it by and broken again across at a
  slant, and sides cut square to the face at angles spread round it, so its
  face is an irregular polygon edged straight, its arrises blunted by the
  weather.
  Each is worn as long as its structure stood — its arrises rounded, chips
  struck from them as shallow scallops (a field stone's spalled from its
  broken faces), its faces lumped where split, hammer-dressed or broken and
  pitted as they erode, now and then a crack running in from one face — and
  wear only ever takes stone away. A solid is found by sphere tracing against
  the steepest its surface can rise, its crossing narrowed by regula falsi,
  its wear and its cover shown only as far as each spans pixels and settling
  to its exact form far off; lumps too shallow to stand out of a face still
  turn the light on it for as long as they are broad enough to see, so a
  rough face never shades as a plane. A block's dressed face is read off its
  rounded box rather than sampled.
- **Masonry** (`compose::courses`). A mason lays a wall in courses, each
  course's top brought to an arch's springing, its stones' lengths drawn and
  their joints broken course on course; each stretch an opening leaves is
  faced at its ends with quoins laid through the wall, long and short by
  turns, and the core between its faces is mortar recessed behind them. An
  arch is a ring of voussoirs, its keystone proud, or rows of brick rowlocks;
  a brick wall is laid in stretcher, English, Flemish or garden bond, headers
  closing each course, a few bricks lost; columns rise in drums to their
  capitals or break off; a round building's courses are rings of stones; a
  walled-up opening is filled with rubble in mortar. Each unit's colour
  (`masonry`) is its own shade and hue, as a quarry's beds differ, a field
  stone's further still, blotched and now and then stained rust as it lay
  half buried in the land and weathered for a while of its own; old stone
  is greyed and darkened, streaked below where rain runs off, blackened with
  grime and biofilm in patches and along the joints water creeps into,
  crusted under what shelters it and greened at a damp wall's foot; a brick
  is darker at its fired ends, some burnt through, and a reclaimed one keeps
  the lime mortar of the wall it came from.

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

## Moss and lichen

What grows on a structure's stone (`cover`) is read once where a ray meets a
solid and carried to its colour, so the cushion of moss standing proud near
the eye and the green it settles to far off lie in the same places; near the
eye it is the solid's own relief, found by the same march in a shell outside
the stone. Moss mantles what faces the sky and climbs a damp wall's shaded
side and its splashed foot, spreading out of the joints where grit and water
gather: mats of packed cushions in patches ragged at every scale by a warp,
tapering at their margins until they break into the cushions they are made
of, each mat one moss or another, hoary where dry and exposed, browning in
a dry season; beyond them a lone cushion lodges here and there, most of all
in a joint. Nothing clings to a floor worn smooth by feet. Lichen colonises
over decades: young colonies stand alone, most of them small, their margins
lobed, and where it grows thick the old crusts have spread until they meet,
a mosaic whose joins wander, each crust cracked into areoles and edged in
its dark prothallus; the orange lichens that feed on what birds leave keep
to where they perch on what faces the sky, and lime-rich stone and acid
stone each carry their own species.

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
gaps do. The point is drawn where the share of the air's sunlight its sample
was drawn with has been gathered along the ray, so a far stretch dimmed by the
near one counts for as little as it shows. The same holds for the air beneath
the clouds: each stretch of it up to a cloud, or to where the ray leaves the
highest bank, is judged at one such point against the banks' shadows, so the
air under an overcast is grey to the horizon rather than lit as a clear day's
haze, and sunlight between clouds shows as shafts.

A pixel's samples are drawn through a Gaussian reconstruction filter of half
a pixel's deviation, cut off at three deviations: each offset is the inverse
of the truncated filter's distribution (Giles' inverse error function), so
every sample weighs the same and the stratification survives, and detail
finer than a pixel is averaged rather than aliased. At its best a pixel is
sampled in rounds of 16, 32, 64 and 128, each a whole stratification of every
pair (Owen-scrambled Sobol, Burley 2020), and stops after any round whose
samples agree; `Quality` caps the rounds, and the screensaver always traces at
the best. Agreeing is not enough where a surface gathers its light by a
random ray, as a leaf always does and a matte surface does where no radiosity
record holds: deep in a crown or under a roof most such rays find nothing and
a few find the sky, so a pixel's first samples can all miss it and agree on
black. A pixel whose samples lean on such a surface — seen directly, or
through clear water or in a mirror, whose light is a quarter or more of the
pixel's — takes at least 64 samples, and no pixel stops while what it shows
would move without its brightest sample; such a surface's bounce is always
followed, never ended by Russian roulette. Each sample is toned (ACES filmic, Narkowicz) before the samples are
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

A scene holds at most 131 072 objects at `Simple` and 2 097 152 at `Maximum`
(a forest's trees, understory and deadwood among them, each 320 bytes), 4096
hull faces, 256 materials, 12 lights, 12 height grids, 96 prototypes, 16 lawns
and 8 woods, each carried on far off at `Maximum` as at most 576 tiles of its
land, and a path at most nine bounces.

A scene may take up to 160 s to prepare on a desktop-class machine across 8
threads and hold up to 2 GB at its peak at `Maximum`, far less at `Simple`
(`plans/RAYTRACE.md`). Measured at 1920×1080 on a 24-thread desktop preparing
across 8 threads, a landscape prepares at `Maximum` in 1.0–102 s — a stream
in 65–102 s, a meadow in 48–64 s, a forest in 65–74 s, a mountain forest over
a lake in 44–55 s, snow over a frozen pond in 72–76 s, a desert in 7–20 s, a
lagoon in 1.0–4.6 s, most of each its radiosity records and up to half of a
lagoon's its caustics — holding at most about 840 MB at its peak and once
prepared, a stream's; at `Simple` it prepares in 0.35–9.3 s, holding at most
475 MB.
Laying a scene's caustics takes up to 0.25 s at `Simple` and
0.95 s at `Maximum`, and holds up to about 75 MB and 330 MB. Traced on one of its cores,
built for the x86-64 baseline (SSE2), a sample costs from about 2.4 µs (the
checkerboard, mostly its clouds) to about 19 µs (a meadow, mostly its eye rays
over grass) at 640×360 and full quality: a forest about 17 µs, a valley 12, a
colonnade 7; the local adaptation's correction costs a sample about one per
cent more, and beneath or over water the caustics about 2.5 µs.

Every unit of preparation is a fixed amount of work a core — a band of a
grid's rows or of a shade's, a turn of a land's droplet tiles, a band of the
objects' boxes or a slice of a hierarchy, a run of a far wood's ring read or
a few hundred of its blocks surveyed, a ring's places drawn or a run of
their ranking, a few rows of the water's edge's lattice, a bed's stones read,
thinned or set out a few thousand at a time, a stream's flow solved a few
thousand of its points a core, a few rows of a
radiosity record's hemisphere, a handful of
the meter's samples, a hundred or so of the caustics' survey points, a couple
of thousand of their beams or a few thousand of their pyramids' nodes — and a
wood's places are kept in runs that never move as
more are added, so `Draft::prepare` answers within about a frame however large
the scene: measured, no unit takes more than about 10 ms at either detail, but
for the one that begins a large scene's hierarchy, finding every object's box
at once, which can take about 20 ms at `Maximum`. Under the
screensaver's `idle` setting the preparation runs on one core and takes some
eight times as long.
