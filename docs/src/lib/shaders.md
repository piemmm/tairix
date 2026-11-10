# `tairix-shaders` — the pinned shader set

The desktop's passes as GPU programs (`plans/GPU.md` GD6,
`plans/SHADER.md` SH3). Each module is written in Rust through
[`lib/spirv`](./spirv.md)'s builder, which answers a module only once the
validator admits it, and committed under `lib/shaders/spv/` as the SPIR-V the
desktop loads, so no runtime compiler is ever reached and a change to a pass
is a change to its committed bytes.

## The passes

| Module | What it does | The software path it equals |
|---|---|---|
| `frost_reduce` | Averages a rectangle of a surface two by two into cells, edges replicated | `halved_reference`'s cells |
| `frost_blur` | One axis of the box blur over the cells, `2 · reach + 1` of them, edges replicated | its two passes |
| `frost_spread` | Lays each pixel back from the two cells nearest its centre along each axis, weighted in sixteenths | its spread |
| `frost_mix` | Mixes the blur over a surface at each pixel's coverage, rounded at the surface's dither | `Surface::frost_halved_from` |
| `over` | Composites a source over a surface, scaled by a factor, dithered or to nearest | `blend_span` |
| `copy` | Copies a rectangle between surfaces | the rows it moves |
| `fill` | Fills a rectangle with one pixel | the rows it lays |
| `encode` | Writes pixels in a display's byte order | `ChannelOrder::encode_run` |

Every pass is a compute entry point `main` over workgroups `GROUP` (8)
invocations square, one a pixel or cell; `groups(width, height)` covers a
grid. Its buffers are words at set 0 — a pixel is one word, red in its low
byte, as `tairix_raster::Pixel` lies in memory — and its push constants are
its `Push`'s `WORDS` words. An invocation outside the extent its push names
reads and writes nothing. The modules are SPIR-V 1.3, the Vulkan 1.1
baseline, so every backend takes them unchanged.

The passes are `lib/raster`'s arithmetic to the bit: the tolerance their
conformance cases state is zero. The ordered dither's biases and the rounding
point are read from `lib/raster` as each module is built, so the two cannot
disagree about a constant. A device runs the frost as `frost_reduce`, then
`frost_blur` along the rows and down the columns at half the frost's radius,
then `frost_spread` into a field and `frost_mix` over the surface; the
desktop drawing through them on a device is `plans/GPU.md` GD11.

## The drift guard

`cargo xtask shaders`, among `ci`'s static gates, builds every module of
`SET` from its source and fails closed where a committed file has drifted,
is missing, or names no module. `--write` commits what the sources build. A
new pass's file is committed empty first, since the set compiles its committed
bytes in.

## Tests

- Every committed module is admitted, its workgroup the square `GROUP`, its
  push constants and bindings exactly what its pass states.
- The conformance cases run each committed module on `lib/gpu`'s software
  device over pictures drawn from the run's seed: the frost passes against
  `halved_reference` at sizes down to one pixel and radii from 1 to 43, and
  mixed back against `Surface::frost_halved_from` over rectangles inside and
  at the edges of a larger surface; the spread placed inside a larger surface
  and nowhere else; the over against `blend_span` at full, partial and no
  factor, dithered and not; copy and fill against the rectangles they lay;
  the encode against `ChannelOrder::encode_run` in both orders.

## Stability

Experimental.
