# tairix-shaders

Stability tier: **experimental**

The pinned shader set (`plans/GPU.md` GD6 and GP4, `plans/SHADER.md` SH3):
the desktop's passes as GPU programs, each written in Rust through
`lib/spirv`'s builder and committed under `spv/` as the SPIR-V the desktop
loads.

## What it provides

- The frost's four passes — `frost::reduce`, `frost::blur`, `frost::spread`,
  `frost::mix` — and the compositor's `compose::over`, `compose::copy`,
  `compose::fill`, `compose::encode` and `compose::resample`. Each module
  names its bindings, its `Push` and the words it takes (`WORDS`), and its
  committed `SPIRV`.
- The compositor's chrome: `chrome::body`, one window's client cut to its
  shape, its plate and its furniture laid at its opacity, and
  `chrome::shadow`, the drop shadow it casts at either level and easing
  between them. Each lays over a rectangle its `chrome::Push` names: the body
  from a `body::Layout` in a storage buffer, the shadow from the levels'
  `shadow::Tables`, laid out once, and its caster in the push
  (`Tables::push`). A description is refused (`Refused`) where it passes a
  bound that keeps the pass in 32-bit words: `MAX_PASS_RADIUS`, `MAX_PASS_COORDINATE`, and `lib/raster`'s
  drawing extent and surface size. `compose::resample::taps` lays out a
  `ResamplePlan`'s taps the same way.
- The furniture painter's primitives (`paint`): `plate` (a rounded rectangle
  over or replacing), `wash` (a colour ramp at a weight a pixel from up to
  two planes), `ring` (washed solid or lit, or framed), `coverage` (a shape's
  alpha from the converter's pieces of each row, `coverage::Shape`), `mask`
  (an ink through a coverage plane) and `picture` (rounded or toned), each
  described in its push by its own type and refused past its bounds.
- `indirect::counts`: the check a hardware backend runs on an indirect
  dispatch's counts before the dispatch reads them, zeroing counts past the
  device's and setting the slot's fault (`plans/GPU.md` GP5.4).
- `SET`: every module's name, the source that builds it, its committed bytes,
  for `cargo xtask shaders` to hold together, and a `Sample` of it run on a
  known input, which `gpucheck` holds every device to.
- `GROUP` and `groups`: the square workgroup every pass runs in, and the
  dispatch that covers a grid.

## Guarantees

- Each pass is `lib/raster`'s own arithmetic: its conformance cases run the
  committed module on `lib/gpu`'s software device and compare every pixel with
  the software path, bit for bit — the chrome against `Body` laid through
  `Pixel::laid_over`, `shadow_row`'s blend, and `resample_window_into`; the
  painter's passes against the `Surface` call each is.
- Every pass computes in 32-bit integers, so a backend without 64-bit integer
  arithmetic runs it unchanged.
- The dither biases and the rounding point are read from `lib/raster` when a
  module is built, never restated.
- `cargo xtask shaders` (in `ci`) fails closed on a committed module that has
  drifted from its source, a missing one, or a file naming no module.

## Adding a pass

Write its module with its source, enter it in `SET`, commit an empty
`spv/<name>.spv` (the set compiles its committed bytes in), then run
`cargo xtask shaders --write`.

## Tests

`cargo test -p tairix-shaders`: every committed module admitted and needing
exactly the push words and bindings its pass states; the frost's passes equal
to `halved_reference` and, mixed back, to `Surface::frost_halved_from`; the
spread laid only at its place; the over equal to `blend_span` dithered and
not, at full, partial and no factor; copy and fill laying their rectangles
alone; the encode writing both display orders as `ChannelOrder::encode_run`
does; the body over random windows — decorated and plain, cut and square,
presented short, translucent, partly off the frame, their clients' pixels
premultiplied or not — equal to `Body`, and exact at `MAX_PASS_RADIUS` with
clients far past their cut; the shadow at either level and between them
equal to `shadow_row`'s rows; the resample reducing, enlarging and copying
equal to `resample_window_into`; plates, washes, rings, covered and washed
shapes, strokes, tinted masks and rounded and toned pictures equal to the
`Surface` calls they are; each description refused just past each bound;
the indirect counts' check zeroing a slot past the device's and keeping one
within; and every sample changing what it is given.
