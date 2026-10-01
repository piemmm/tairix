# tairix-raytrace

Stability tier: **experimental**

The ray tracer behind the desktop's ray-traced screensaver: scenes composed at
random from a seed, and a tracer that answers what one pixel of one shows.
`no_std` + `alloc`, and `#![forbid(unsafe_code)]`.

## What it provides

- `Setting` — the nineteen settings a scene is set in: still lifes (a
  checkerboard of spheres and gems, a studio, crystals at dusk, lamps at
  night, soap bubbles), buildings (colonnades, arcades and aqueducts,
  rotundas, ruins), sculpture in a landscape, and landscapes (meadows, a
  forest, mountains over a lake, a coast, desert, snow, a lagoon, canyons, a
  river valley).
- `Draft` — a scene composed but not yet traceable: its lands built, its woods
  and swards grown, its trees and deadwood grown into prototypes, its grids and
  sky tables filled, its hierarchy built, and its radiosity gathered. `prepare`
  does a bounded unit of that work at a time across any
  `tairix_parallel::JobRunner`, so a caller on an interactive loop spreads it
  over frames; `finish` hands over the scene.
- `Scene` — read-only once finished, shared by every core tracing it.
- `Tracer` and `Quality` — what one pixel shows, at a cap of 8, 16, 32 or 64
  samples. A pixel is traced from its own index and the scene's key alone, so
  it comes out the same on whichever core takes it, in whatever order.
- `Encoder` — the display transform: ACES filmic tone, sRGB, and the
  desktop's ordered dither.
- `Reveal` and `Block` — the order a screensaver shows a picture in: coarse
  to fine, every pixel traced once, the first pass covering the whole picture
  in blocks at least eight to the shorter side, each later pass halving them,
  and each pass in a keyed, scattered order. A `Block` is a traced pixel and
  the part of the picture its colour stands for until finer steps reach it.

## Guarantees

- **Deterministic.** A setting, a seed and an aspect compose one scene; a
  pixel's samples are hashed from its index and the scene's key.
- **Fallible allocation.** Every buffer is reserved fallibly: a heap that will
  not hold a scene answers `None`, never an abort.
- **Bounded cost.** A scene holds at most 131 072 objects, 4096 hull faces,
  256 materials, 12 lights, 12 height grids, 96 prototypes, 16 lawns and 8
  woods; a pixel at most 64 samples, a path at most nine bounces. Every unit of
  preparation is bounded: a band of a grid's rows, a slice of a prototype's or
  the scene's hierarchy, a band of a wood's places.

## Tests

`cargo test -p tairix-raytrace`: every shape and grid met where it lies and
nearest first; trees of every kind grown sound however they stood, their
crowns as wide as reckoned, and a frame kept rigid through thousands of
turns; a trunk holding its girth, flared at its foot but not at a fork's
arm, gripped by roots and carrying its stubs; bark closing round its limb
with no seam, at its real size on any girth, darker in its fissures, its own
on every tree, white on a birch over its black foot, orange up a pine, and
settling far off to its mean, with its relief leaning the right way; the air
lit by the sun only where the sun reaches it, and a meter that holds a sky
below white but lets the sun blow out; a wood covering the share of ground asked, its gaps opening where its
lattice holds them, its trees spaced by their crowns with the short beneath
the tall, sown all about the eye near it and across the view beyond, none
walling off the view, and a forest standing thousands of trees and its fallen
among them clear of the eye; the shade crowns cast and the sky they hide, and
the air beneath them roofed; fallen trunks lying along the ground, thrown or
snapped, and stumps sawn or splintered; ferns arching from the ground; the
sampling, lights, materials and pigments; a land lying as its grids hold it,
its rivers running downhill and its road dry but where it bridges them; the
composer across every setting under many seeds (lit, sound, framed, the camera
in the open and above the water, crystals rooted in their rock, an Ionic
capital's scrolls in sight, a frozen pond's ice under its banks, and a coarse
render that reads on screen); a draft filled in bands across real threads
matching one filled alone; and the reveal order.

The design and the measurements behind its budgets are in
`docs/src/lib/raytrace.md`.
