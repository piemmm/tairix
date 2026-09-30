# tairix-raytrace

`lib/raytrace` is the ray tracer behind the desktop's ray-traced screensaver
(`docs/src/desktop/session.md`): scenes composed at random from a seed, and a
tracer that answers what one pixel of one shows. It is `no_std` + `alloc` and
forbids `unsafe`.

## Scenes

A `Draft::new(setting, seed, aspect)` composes a scene in one of the
seventeen `Setting`s. Within a setting everything is drawn from the seed:
which pieces, where, in what, lit from where, at what hour and under what
weather, and seen from where — and every draw is bounded so the scene is lit
and framed to read.

- **Still lifes** stand on a plane: a checkerboard of spheres, gems, rings
  and stacks under the open sky; a studio's plinth under softboxes, high or
  low key; crystal clusters, or crystals grown out of a stone, on black glass
  at dusk; glass and chrome among lamps at night; soap bubbles drifting over a
  meadow.
- **Buildings** stand on a paved plaza, on tiles to the horizon or on open
  land with trees about it: a colonnade (an avenue, a peristyle or a stoa, in
  three orders, each capital turned along the beam it carries), a loggia of
  arches or a two-tier aqueduct across a valley, a domed rotunda on stepped
  ground, or a ruined temple in long grass.
- **Landscapes** are height grids: rolling meadows, a forest glade, mountains
  over a lake, an island's coast with the sea running in, dunes or rocky
  desert, snow over a frozen pond, a lagoon at sunset, and canyons between
  mesas.

The weather (`compose::weather`) sets the hour — noon, day, golden hour,
sunset, dusk, or night by the moon — and the cloud: clear, fair cumulus, a
broken layer, overcast, or cirrus. The sun or moon is a light, its glow and
the sky's gradient the escaping ray's radiance, and the haze between the eye
and what it sees takes the horizon's colour and a share of the sun's glow.

## Geometry

Spheres, domes, planes, rectangles, convex hulls (boxes, gems, crystals,
boulders, pyramids, obelisks), capped frusta, tori and their arcs (arches),
height grids, crowns of leaves, and lawns of grass. A hull's extent is found
from its own corners, each where three faces meet within the rest.

- **Height grids** hold `f32` heights at the vertices of a square grid and the
  bilinear patch between them. A ray walks a pyramid of maxima (Tevs, Ihrke
  and Seidel, *Maximum Mipmaps*, 2008) and meets a cell's patch where a
  quadratic along it says, searched only while the ray is over that cell; a
  grid that wraps tiles the open sea. Land is a `Terrain` — a landform
  levelled in a clearing and settling to a rim — and the sea a sum of swells
  snapped to its tile, scaled to the significant wave height asked for. An eye
  over land stands on the lowest level ground among a few spots drawn, looking
  the way the land stays open furthest, and never beneath the water.
- **Crowns** place at most one leaf in each cell of a grid through an
  ellipsoid, hashed from the cell; a ray walks the cells in order
  (Amanatides and Woo, 1987), so the first leaf met is the nearest.
- **Lawns** root a few blades in each cell of a grid over the ground, each
  leaning no further than its cell's walls. A ray starts where it first comes
  within the blades' reach of the ground, found through the grid's pyramid,
  stops where it meets the ground, skips any cell whose ground it passes high
  above, and rejects a blade it passes wide of seen from above before testing
  the blade's two ribbons.

## Light

Distributed ray tracing (Cook, Porter and Carpenter, 1984): each sample draws
its own point in the pixel, on the lens, on each lamp and through each glossy
reflection. Glossy reflection draws GGX visible normals (Heitz, 2018),
anisotropic for brushed metal, and a lamp it finds is weighed against sampling
that lamp by the power heuristic (Veach and Guibas, 1995). Glass follows both
reflection and refraction near the eye, with Beer–Lambert absorption and the
glow of deep water; a dispersive gem traces one primary per sample. Films
interfere by the Airy sum over three wavelengths per primary; car paint has a
flaked metallic base under a mirror coat; leaves and blades are lit through
from behind. Clouds shadow the sun's light and dim the scene's scattered
light.

A pixel is sampled in rounds of 8, 16, 32 and 64, each a whole stratification
of every pair (Owen-scrambled Sobol, Burley 2020), and stops after any round
whose samples agree; `Quality` caps the rounds. The rounds and tolerance were
chosen against 256-sample references. Each sample is toned (ACES filmic,
Narkowicz) before the samples are averaged, then encoded to sRGB through a
lookup table and the desktop's ordered dither.

## Reveal order

`Reveal::new((width, height), key)` orders a picture's pixels coarse to fine,
each traced once. The first pass traces the top-left pixel of every block of
a grid whose side is the largest power of two leaving at least eight blocks
across the shorter side — 128 pixels on a 1080-line screen, so 135 pixels
cover it — and each `Block` names the part of the picture its colour stands
for, clipped to the picture. Every later pass halves the blocks and traces
the three pixels in four the grid of twice its side did not, down to single
pixels. A block covers no pixel an earlier step traced, so painting each step
over the last ends with every pixel showing its own trace. Within a pass the
steps follow a keyed bijection on the pass's range — odd multiplications and
right shifts on the smallest power-of-two range holding it, walked until they
land inside — so the whole picture sharpens at once. Nothing is stored beyond
one small record per pass, so a step is found from its index alone, on
whichever core traces it.

## Budgets

Measured on a 24-thread desktop at 640×360 and full quality, the settings
cost from about 1.5 µs a sample (the checkerboard, the lagoon) to about 28 µs
(the coast). Grass is what the grassy scenes spend most on: the
pyramid-bounded start and stop, the per-cell ground bound, the plan-view blade
rejection and the trig-free placement cut a meadow's cost about threefold.

On one core, composing a scene takes at most about 2 ms and building its
hierarchy under half a millisecond, so a caller can afford either within a
frame; filling a landscape's grids takes up to about 200 ms, which is why
`Draft::prepare` hands them out a bounded number of vertices at a time. A
scene's grids are at most 1025 vertices a side; a landscape holds about
10 MiB of heights and maxima while it is traced, and none once its picture is
whole.
