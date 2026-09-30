# tairix-raytrace

Stability tier: **experimental**

The ray tracer behind the desktop's ray-traced screensaver: scenes composed at
random from a seed, and a tracer that answers what one pixel of one shows.
`no_std` + `alloc`, and `#![forbid(unsafe_code)]`.

## What it provides

- `Setting` — the seventeen settings a scene is set in: still lifes (a
  checkerboard of spheres and gems, a studio, crystals at dusk, lamps at
  night, soap bubbles), buildings (colonnades, arcades and aqueducts,
  rotundas, ruins), and landscapes (meadows, a forest glade, mountains over a
  lake, a coast, desert, snow, a lagoon, canyons).
- `Draft` — a scene composed but not yet traceable: its land, sea and cloud
  grids still to fill. `prepare` fills about as many vertices as it is asked,
  whole rows at a time, across any `tairix_parallel::JobRunner`, so a caller
  on an interactive loop spreads the work over frames; `finish` builds the
  hierarchy.
- `Scene` — read-only once finished, shared by every core tracing it.
- `Tracer` and `Quality` — what one pixel shows, at a cap of 8, 16, 32 or 64
  samples. A pixel is traced from its own index and the scene's key alone, so
  it comes out the same on whichever core takes it, in whatever order.
- `Encoder` — the display transform: ACES filmic tone, sRGB, and the
  desktop's ordered dither.
- `Reveal` — a keyed bijection over the picture's pixels: the order a
  screensaver shows them in.

## Guarantees

- **Deterministic.** A setting, a seed and an aspect compose one scene; a
  pixel's samples are hashed from its index and the scene's key.
- **Fallible allocation.** Every buffer is reserved fallibly: a heap that will
  not hold a scene answers `None`, never an abort.
- **Bounded cost.** A scene holds at most 4096 objects, 4096 hull faces, 256
  materials, 12 lights and three height grids; a pixel at most 64 samples,
  a path at most nine bounces.

## Tests

`cargo test -p tairix-raytrace`: every shape and grid met where it lies and
nearest first, the sampling, lights, materials and pigments, the composer
across every setting under many seeds (lit, sound, framed, the camera in the
open and above the water, crystals rooted in their rock, an Ionic capital's
scrolls in sight, a frozen pond's ice under its banks, and a coarse render
that reads on screen), and a draft filled in bands across real threads
matching one filled alone.

The design and the measurements behind its budgets are in
`docs/src/lib/raytrace.md`.
