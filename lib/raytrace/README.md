# tairix-raytrace

Stability tier: **experimental**

The ray tracer behind the desktop's ray-traced screensaver: scenes composed at
random from a seed, and a tracer that answers what one pixel of one shows.
`no_std` + `alloc`, and `#![forbid(unsafe_code)]`.

## What it provides

- `Setting` — the twenty settings a scene is set in: still lifes (a
  checkerboard of spheres and gems, a studio, crystals at dusk, lamps at
  night, soap bubbles), buildings (colonnades, arcades and aqueducts,
  rotundas, ruins), sculpture in a landscape, and landscapes (meadows, a
  forest, mountains over a lake, a coast, desert, snow, a lagoon, canyons, a
  river valley, a stream through its pools, riffles and ledges).
- `Detail` — how much a scene sets out: `Simple`, every setting plainer
  within about 512 MiB at its peak, or `Maximum`, all the realism a 2 GiB peak
  buys; `peak` states each one's budget for a caller weighing its memory.
- `Draft` — a scene composed at a `Detail` but not yet traceable: its lands
  built, its woods, water's edge and swards grown, its trees and deadwood grown into
  prototypes, its grids and sky tables filled, its hierarchy built, the beams
  its water bends the sun's light through laid, its radiosity gathered and its
  exposure and local adaptation metered. `prepare`
  does a bounded unit of that work at a time across any
  `tairix_parallel::JobRunner`, so a caller on an interactive loop spreads it
  over frames; `progress` says how far it has come, in thousandths; `finish`
  hands over the scene.
- `Scene` — read-only once finished, shared by every core tracing it.
- `Tracer` and `Quality` — what one pixel shows, at a cap of 8, 16, 32 or 128
  samples, drawn through a Gaussian reconstruction filter. A pixel is traced
  from its own index and the scene's key alone, so it comes out the same on
  whichever core takes it, in whatever order.
- `Encoder` — the display transform: ACES filmic tone, sRGB, and the
  desktop's ordered dither.
- `Reveal` and `Step` — the order a screensaver shows a picture in: coarse
  to fine, every pixel traced once, the first pass every point of a grid at
  least eight to the shorter side, each later pass halving the grid's spacing,
  and each pass in a keyed, scattered order. A `Step` is a traced pixel and the
  spacing of the grid it is a point of, which finer passes subdivide.
- `Setting::name` — what a setting is called, which a kept picture is named
  after.

## Guarantees

- **Deterministic.** A setting, a seed, a picture size and a detail compose
  one scene, and a seed shows the same land, eye, hour and weather at either
  detail; a pixel's samples are hashed from its index and the scene's key.
- **Fallible allocation.** Every buffer is reserved fallibly: a heap that will
  not hold a scene answers `None`, never an abort.
- **Bounded cost.** A scene holds at most 131 072 objects at `Simple` and
  2 097 152 at `Maximum`, 4096 hull faces,
  256 materials, 12 lights, 12 height grids, 96 prototypes, 16 lawns and 8
  woods, each carried on far off at `Maximum` as at most 576 tiles of its
  land, and beams over at most 2²¹ cells of water at `Simple` and 2²³ at
  `Maximum`; a pixel at most 128 samples, a path at most nine bounces, and a
  point under or over water gathers about 256 beams a level at most. Every unit of
  preparation is a fixed amount of work a core, whatever the scene holds: a
  band of a grid's rows or of a shade's, a turn of a land's droplet tiles, a
  slice of a prototype's or the scene's hierarchy, a band of a wood's places
  or a run of their ranking, a few rows of the water's edge's lattice, a band
  of the coarse grid's shelter from the wind where snow lies, a few thousand
  of a snowball's vertices shaped, a hundred or so of the caustics' survey
  points, a
  couple of thousand of their beams or a few thousand of their pyramids'
  nodes, 48 steps of a stone's wear, a few rows of a stream's flow, a few rows
  of a radiosity record's hemisphere, a few rows of a cloud
  level's weather, of its grid of the sun's depth or of its shadow, each
  level's maps reserved whole and written as their rows are filled.

## Tests

`cargo test -p tairix-raytrace`: every shape and grid met where it lies and
nearest first; trees of every kind grown sound however they stood, their
crowns as wide as reckoned, and a frame kept rigid through thousands of
turns; a trunk holding its girth, its foot swelling further toward each
root than between them, each root leaving from within its lobe with its back
above the ground and ending buried, and no foot spreading more roots than a
flare holds lobes; a flare swelling most toward a lobe at the ground, fading
smoothly to round, never past its most, rising no faster than its bound and
refusing what is not one; a bole's stubs and a snag's snapped top and limbs
ending torn, the snag thick where it broke; bark closing round its limb
with no seam, at its real size on any girth, darker in its fissures, its own
on every tree, white on a birch over its black foot, orange up a pine, and
settling far off to its mean, with its relief leaning the right way; bark
cut in true relief near the eye — never met outside its tube, sunk to its
depth, its outline ridged, every hit on the cut surface, a shadow wherever a
ray meets it, a bending limb's joint closed and its free end round, a flared
foot met where it swells from near, from far off and unseen, and a hit at an
open end's rim facing out of its side — and a limb far off or too thin to
have fissured its tube, each bark's steepest bounding how fast it rises; the
moon waning by its phase with its lit part toward the sun, its dark part
glowing bluer with the Earth's light, only the Earth's left at new moon, its
disc bringing its whole light at any phase, and no star shining through it; the air
lit by the sun only where the sun reaches it, and a meter that holds a sky
below white but lets the sun blow out; a wood covering the share of ground asked, its gaps opening where its
lattice holds them, its trees spaced by their crowns with the short beneath
the tall, sown all about the eye near it and across the view beyond, none
walling off the view, and a forest standing thousands of trees and its fallen
among them clear of the eye; the shade crowns cast and the sky they hide, and
the air beneath them roofed; fallen trunks lying along the ground, thrown or
snapped, each break torn wood bristling with laths, every stub and torn root
ending in a break, a thrown trunk's plate of soil with its roots torn off
past its rim, and stumps sawn or splintered, a sawn face ringed in the bark
it cut through and a snapped top torn jagged, never capped in bark; a break
fraying no further than its laths and tatters reach, climbing toward where
its fibres pulled out, facing out everywhere, and hollow and splinterless
when old; a sawn face checked from its rim and a felled one stepped from
notch to back cut, an old stump's heart rotted hollow, brackets shelving from
dead wood with their pores beneath, a broadleaf stump's shoots rising leafy
but in winter, and dead bark sloughing in sheets about as much as asked, the
wood it bares sunk below it, brown and checked dark;
stones gathered in fields and fanned in scree below crags, the larger
rolled the further, pebbles drifting only where water washed the ground, and
a richer detail strewing more without shifting another draw; leaf litter as
thick as the crowns above shed; snow drifting deep in the lee of a rise and scoured from its crest
and the slopes the wind climbs, sloughing off steep ground, cut along the wind
from the snow alone and only as finely as a grid resolves, a land under snow
standing above the same land bare by the depth its grids keep, the ground
showing and grass growing only where the snow lies thin, a shoot showing only
what stands above it; a snowman's balls never round, narrower along their
roll axis, ridged by the sheets they took up and streaked round their drums,
standing on a flat foot and carrying a flat seat, stacked foot to seat each
smaller than the last, leaning as hands set them, its face on its head, a
carrot and a stick ending in fine tips, never balls, and none built where
its prototypes would not fit; ferns arching from the ground; reeds and reedmace standing in the
shallows and on wet level banks off scoured ground, lilies and pondweed
floating on still water as deep as each roots in, and crowfoot streaming
where the water runs over gravel, none in a closed wood's gloom and the
floating and streaming ones gone over winter, each patch within its
square and its plants' reach, in its own materials and as tall as its
stature, a lily's pads round and flat on the water and its flowers one in
eight in summer, the near lattice filling the far one's hole on the land's own
grid, a lake's reeds and lilies standing only where they suit it, and a
scene's patches kept to its detail's most, those nearest the eye; a plant's
patch planned only once one is set out, and left out past the most a scene
plans; a strap leaf in pieces keeping one
outline and a pad round but for its slit; the
sampling, lights, materials and pigments; a limb narrowing by its level's
taper and a fork's arms carrying its narrowing on, a scaled limb met at its
placed girth, and a bending limb's bark starting round it alike either side of
a joint; a straight river wandering only sideways by its own length run; the
sky's mean weighed by its cosine over the hemisphere; a far grass cell's shoot
heading any way round; a coarse ray reaching cloud past any number of clear
columns; a deck running on to the horizon over the Earth's curve, its levels
meeting with no seam, every level's bands holding its cloud and a march
standing over the cell beneath each point on every level, far cloud lit by the
sun at its own place and shading the ground from a low sun, an overcast grey to
the horizon, and a point drawn along the air falling as its sunlight is
gathered; a bridge's deck and a row of arches keeping trees off the strip
beneath them, a strip claimed with no gap at its edges, a narrow slanting
river wet in every cell it crosses, and woods with nothing beneath them still
casting their shade; a seed showing the same place at either detail, `Simple` standing
fewer trees nearer the eye within its room, and both details' records the same
on any runner; local adaptation leaving a frame one exposure holds alone,
drawing a bright window down and lifting a dark wall alike to the edge between
them with no halo, keeping detail and letting the sun blow out, and the same
however its measurement was divided; a grid of any size met at the
nearest of its cells and sealed the same, its mean to the last bit, across any
number of cores, written only as its rows are filled, and a canopy grid as
large as its lawn and no larger; a shade cast in bands bit for bit the shade
cast whole, and sampled in bands as alone; a wood's places ranked tallest
first as one sort would; prototypes of every kind grown a core apiece a unit;
radiosity records laid the same however their rows fall across units, holding
within fifteen degrees of turn; a land lying as its grids hold it,
its rivers running downhill and its road dry but where it bridges them; the
composer across every setting under many seeds (lit, sound, framed, the camera
in the open and above the water, crystals rooted in their rock, an Ionic
capital's scrolls in sight, a frozen pond's ice under its banks, and a coarse
render that reads on screen); a draft filled in bands across real threads
matching one filled alone; a packet of eye rays finding, ray for ray, what
each finds alone, and a lawn crossed together meeting what each ray meets
alone; a walk taken an object at a time handing over what an unpaused one
visits; four boxes crossed in lanes bit for bit as each alone, and a grid's
blocks crossed four at a time reaching what one block at a time reaches,
never descending into one with no surface; an object whose box is not
finite tested by every ray; and the reveal order — every pass's grid whole
before the next begins; a pixel settling only on samples that tell its whole
light, and a floor seeing the sky only low down showing no black pixel, seen
bare, through glass or in a mirror; the Gaussian
filter's offsets distributed as their truncated Gaussian; a draft's progress
climbing steadily to its whole only once the scene is ready; water's waves
holding their slope variance whether they tilt the normal or roughen it,
never repeating across the water and gusting in patches, rippled water seen
low losing none of an even sky, and the open sea lying at its mean level past
its reach; noise continuous across every lattice wall; a land's eye clear
of the ground beneath it; the waves curving the surface as their slopes
change; a level surface's beams bringing every point exactly its light,
across tile seams and beyond the tiles laid, and bending and sharing the
sun's light as Snell's and Fresnel's laws have it; beams before a focus and
past it bringing what a million surface points' beams landing in the same box
bring, and the waves passing all the light they bend; the detail a point
gathers changing smoothly with its footprint; beams laid only where the
picture looks into rippled water, the same on any runner, and drawing a net
of light on a bed; sunlight under water bent and absorbed along its bent way;
a ceiling over water taking the sun the water reflects; a rough clear surface
widening the view beyond it; beams swept along a row of level waves being
those read afresh at each point, and read afresh where a texture frames the
waves off level; every pyramid bounding the beams beneath it however its
sealing is shared; a run of points asking for tiles just as each alone would,
and a tile asked for twice laid once for the most asked; a point seen over no
footprint taking a level surface's light; and a picture of a bed under
ripples showing the net that one with nothing laid does not; a valley along
its compass heading and a hard cap wearing its hardness times as slowly; a
transform matching the transform written out and the same on any runner; a
stone worn only inward and the rounder the further carried, slate staying
flat and sharp-edged; the flow's long-wave dip, lee waves standing only
behind a stone, a pillow before one through the surface and foam in its
wake, a stone's answer keeping pace with the bed's, a ledge breaking white
at its foot and a train of standing waves dying away behind a ridge, and
nothing standing past what water can; a perched river's bank falling to its land and its deep water
alternating from unit to unit; a finer water grid meeting the far one at its
seam; a stream always having somewhere to be looked at from, and a scene
away from it composing without a brook; its flow solved further the way the
eye looks, its boulders claiming their ground, never set through what already
stands and standing in the flow as they lie, its drift reaching the flow as
one obstacle, its crests held once to what water can stand, its floating
plants on its shaped water and its growing never reporting less done; a bed's
median at Shields' stone and its wears from fresh to the farthest-carried; a
course's way holding past its ends; a river's sand carrying no grit of its
own, a grain's octaves sharing its depth and a grain made for its coarse
relief keeping it; a water grid's steady fall told apart from its spread; and a
sward's shade on the ground blended between its cells, a large lawn's read
from its canopy grid.

The design and the measurements behind its budgets are in
`docs/src/lib/raytrace.md`.
