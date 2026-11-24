# `tairix-shaders` — the pinned shader set

The desktop's passes as GPU programs (`plans/GPU.md` GD6 and GP4,
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
| `resample` | Filters a window of a picture drawn at another size through its plan's taps, or copies it where the plan only copies | `Surface::resample_window_into` |
| `body` | Lays one window over a rectangle of the frame: its client's pixels within their drawn extent, the plate past them, its furniture, the client's rounded cut and its opacity | `Body` laid through `Pixel::laid_over` |
| `shadow` | Lays the drop shadow one caster casts at either level, or crossfaded while it eases between them | `shadow_row`'s `blend_into` |
| `plate` | Lays a rounded rectangle over a surface, or replacing what it covers | `Surface::fill_round_rect`, `Surface::set_round_rect` |
| `wash` | Washes a colour ramped down a rectangle's rows over it, at a weight a pixel from up to two planes | `Surface::fill_vertical_gradient`, `Surface::wash_region`, `Surface::wash_polygon_subpixel` |
| `ring` | Washes a ring solid or lit by the key light, or lays it as a plate's rim | `Surface::wash_ring`, `Surface::frame_ring` |
| `coverage` | Writes a scan-converted shape's alpha a pixel to a plane, from the converter's pieces of each row | `CoverageRows::row` |
| `mask` | Lays an ink through a plane of coverage, at nearest | `cover_span`, `Surface::blit_tinted`, `Surface::fill_polygon_subpixel` and `fill_polygon` (with `coverage`), `Surface::stroke_polyline` (each of `stroke_quads` in turn) |
| `picture` | Lays a picture cut to a rounded rectangle, or toned toward grey and weakened | `Surface::blit_rounded`, `Surface::blit_desaturated` |
| `indirect_counts` | Holds an indirect dispatch's three counts to the device's: past them on any axis, every axis zeroed and the slot's fault word set, else both kept | what the software device judges as the dispatch runs |

Every pass but `indirect_counts`, a single invocation a hardware backend runs
before an indirect dispatch (`plans/GPU.md` GP5.4), is a compute entry point
`main` over workgroups `GROUP` (8) invocations square, one a pixel or cell;
`groups(width, height)` covers a grid. Its buffers are words at set 0 — a pixel is one word, red in its low
byte, as `tairix_raster::Pixel` lies in memory — and its push constants are
its `Push`'s `WORDS` words. An invocation outside the extent its push names
reads and writes nothing. The modules are SPIR-V 1.3, the Vulkan 1.1
baseline, so every backend takes them unchanged.

The chrome passes and the resample are described in storage buffers of
words the CPU fills: a window's `body::Layout`, more than portable push
constants hold; the shadow's `shadow::Tables`, both levels' kernels and notch
tiles as `lib/raster`'s `DropShadow`s build them, laid out once for as long
as the theme and scale stand, with each caster's geometry and its levels'
places in the pass's push (`Tables::push`); and a `ResamplePlan`'s `taps`,
whose plans take 128-bit arithmetic and so are made on the CPU. Every pass computes in 32-bit integers, so every backend runs it
unchanged; a description that would carry it past a word — a corner radius
past `MAX_PASS_RADIUS`, a caster's edge past `MAX_PASS_COORDINATE` of the
origin, an extent past `MAX_DRAWING_EXTENT`, a run of pixels past
`MAX_SURFACE_PIXELS` — is refused (`Refused`), never wrapped. A source pixel
whose alpha is nought lays nothing, whatever colour it carries, as in the
compositor's every row.

The furniture painter's passes (`paint`) each lay over a rectangle of a
surface within the shape they paint, described in their push by
`plate::Plate`, `wash::Wash`, `ring::Placed`, `mask::Mask` and
`picture::Picture`, and a shape's coverage by `coverage::Shape`, the
converter's pieces of each row laid out by the CPU, since an edge's crossing
of a row boundary takes 64-bit products. A plane — a mask, a shape's coverage
— is one word a pixel, its value in the low byte. A rectangle fill, a plain or
faded picture and an overwrite are GD6's `fill`, `over` and `copy` already.

The passes are `lib/raster`'s arithmetic to the bit: the tolerance their
conformance cases state is zero. The ordered dither's biases and the rounding
point are read from `lib/raster` as each module is built, so the two cannot
disagree about a constant. A device runs the frost as `frost_reduce`, then
`frost_blur` along the rows and down the columns at half the frost's radius,
then `frost_spread` into a field and `frost_mix` over the surface; the
desktop drawing through them on a device is `plans/GPU.md` GD11.

## Samples

Each entry of `SET` carries `sample`: the pass run on a known input — every
binding's words, its push and its workgroups — built by its description
type, a pass laying over a surface laying over one of 24 × 20 premultiplied
pixels. `gpucheck` runs every sample on each served GPU and on the software
device and holds the two to the bit.

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
  the encode against `ChannelOrder::encode_run` in both orders; the body over
  random windows — decorated and plain, cut and square, presented short,
  translucent, partly off the frame, their clients' pixels premultiplied or
  not — against `Body`, and at `MAX_PASS_RADIUS` across a cut's arc, at its
  corners and far past its edges; the shadow over random casters at either
  level and easing between them, rounded and square, with and without its
  notch tiles, against `shadow_row`'s rows; the resample reducing, enlarging
  and copying, a window and the whole, against `resample_window_into`.
- The furniture painter's passes over random rectangles, partly off the
  surface: plates over and replacing at radii up to twelve; gradients and
  masked washes; rings solid, lit and framed, their top and bottom radii
  apart; shapes of several contours, self-crossing, under both fill rules,
  covered against `CoverageRows::row`, and laid as fills, strokes (each quad
  in turn) and design-grid fills, and washed through a mask; tinted masks and
  pictures rounded and toned, partly off the surface, against the `Surface`
  calls each is.
- Each description is refused just past each bound and taken at it.
- `indirect_counts` keeps counts at the device's and zeroes every axis of a
  slot past them on any one, setting its fault, never clearing an earlier
  fault, and touching no other slot.
- Every sample runs on the software device and changes what it was given.

## Stability

Experimental.
