# tairix-shaders

Stability tier: **experimental**

The pinned shader set (`plans/GPU.md` GD6, `plans/SHADER.md` SH3): the
desktop's passes as GPU programs, each written in Rust through `lib/spirv`'s
builder and committed under `spv/` as the SPIR-V the desktop loads.

## What it provides

- The frost's four passes — `frost::reduce`, `frost::blur`, `frost::spread`,
  `frost::mix` — and the compositor's `compose::over`, `compose::copy`,
  `compose::fill` and `compose::encode`. Each module names its bindings, its
  `Push` and the words it takes (`WORDS`), and its committed `SPIRV`.
- `SET`: every module's name, the source that builds it, and its committed
  bytes, for `cargo xtask shaders` to hold together.
- `GROUP` and `groups`: the square workgroup every pass runs in, and the
  dispatch that covers a grid.

## Guarantees

- Each pass is `lib/raster`'s own arithmetic: its conformance cases run the
  committed module on `lib/gpu`'s software device and compare every pixel with
  the software path, bit for bit.
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
does.
