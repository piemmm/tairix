# `tairix-raster` — software rasterisation

`lib/raster` is the one software rasteriser everything that draws pixels
shares: the window manager, the taskbar, the desktop's apps and the WinterSun
client. It is `no_std`, allocates fallibly, and decides every pixel with
integer coverage or with `f64` set up once and evaluated in a fixed order, so a
picture is the same bits on every target. Its README is the inventory of every
call; this page is what the parts are for and the rules they share.

## Three families

- **Coverage fills** — `Surface::fill_contours`, `fill_polygon`,
  `fill_polygon_subpixel` and the shapes in `shape` are one scan converter in
  `scan`. A pixel's alpha is the exact fraction of its area a shape covers,
  accumulated per row from each edge's signed extent; there is no sample grid
  to quantise an edge.
- **Compositing** — premultiplied `Pixel`s, the Porter–Duff `over`, and the
  span operations (`blend_span`, `mix_span`, `cover_span`) a compositor's blits
  and a fill's rows are built from, plus the one resampler and the one
  transformed blit. The transformed blit draws any `Picture` — a surface, or a
  `Mosaic` of tiles sampled across their seams as one picture — and a
  transform that only scales and moves takes the samples the general path
  would, to the bit, at a quarter of the cost, by working each column's and
  row's position out once and reading the source a row run at a time. Its
  framed form (`blit_framed`) covers exactly the source's rectangle, sampled
  within its edge pixels and anti-aliased by its edges' distance from each
  pixel's centre, which is how a
  document's picture is drawn: as an artwork tree's `Node::Image`, from the
  level of its `Pyramid` — the picture at every halving — that sets no more
  than two of its pixels across a destination pixel, so a picture drawn far
  smaller is averaged rather than aliased.
- **Meshes in depth** — `mesh` and `view`: triangles scan-converted against a
  depth buffer through a perspective, as a GPU draws them. WinterSun's 3D view
  is drawn through these, and nothing else in the tree needs a second
  rasteriser.

The span blend — a run laid, dissolved, covered or washed — the box blur and
the row scan a compositor splits a window row by have vector kernels — SSE2
and NEON — chosen per process on first use and taken only where they
reproduce the portable kernel exactly, so a picture is still the same bits on
every target; the README's *vector kernels* section lists them and the gates
that hold them to it. A masked wash's field answers a row at a time, so a
ramp or a glow is stepped along it rather than evaluated per pixel, and the
row is laid by one kernel.

The box blur averages eight-bit premultiplied `Pixel`s, one byte of coverage,
or `Pixel16`s — premultiplied at sixteen bits a channel, which an eight-bit
colour goes into and comes back out of exactly — so a Gaussian of three box
passes, as Paint's is, rounds each pass at sixteen bits rather than losing a
translucent pixel's colour to eight. Each pass's mean is an exact fixed-point
reciprocal of its window, at either width.

The fills draw into a `Surface` or one `RowBand` of it, and the mesh path into
a `DepthBuffer` or one `DepthBand` of it, handing each fragment to the caller's
shader, so a pass cut into bands on several cores draws the picture the whole
surface would. `Surface::paint_bands` cuts the bands for a runner, and
`BandScratch` gives each band a scan converter of its own. A pass whose work runs down its columns, as the ribbon of light
sums its, is cut into `ColumnBand`s instead, so no band repeats another's
column.

A surface's pixels are its own, or lent (`Surface::lent` over a
`PixelMemory`): a screensaver draws straight into the shared frame the desktop
reads, every drawing call unchanged and no copy between. The lender is asked
for its bytes once, as the surface is built, and is reached by nothing again
until the surface drops, so a lent surface's pixels borrow as cheaply as an
owned one's. The one view of those bytes as pixels is `lent`'s, and a column
band's view of its rows is `columns`'s, both interpreted by miri.

## The mesh path

A pixel belongs to a triangle when its *centre* does, decided by the same
crossings and tie rule the coverage fill uses for centre sampling, so two
triangles sharing an edge own every centre along it exactly once: a mesh has
neither cracks nor doubled pixels.

That rule is one integer half-plane per edge: a sample `(x, y)` lies at or
past an edge's crossing exactly where `a·x + b·y + c ≥ 0`, the rounding of the
crossing folded into `c`, and a triangle covers a sample where both of the
sides spanning its row hold it. A triangle's `Footprint` holds that rule:
`covers` asks it of one sample, and `block` of every sample of an
eight-pixel-square block at once — a
fragment tile — as a mask a sample, at the pixel centre or the four standard
positions of a four-sample image (`Lattice`). The row walker the fill steps by
and the block evaluator cover the same samples, and each of the four positions
covers what the centre covers of the triangle moved onto it, so a mesh owns
every sample of four exactly once too.

`Depth` is reciprocal and reversed — larger is nearer, so a `DepthTest`'s
`CompareOp::Greater` is the reversed-Z configuration of the one comparison
every depth, stencil and sampler test reads — `Depth::CLEAR` lies
beyond anything drawn and `Depth::NEAREST` is the near plane. The reciprocal
is affine across the screen, so it interpolates exactly, and it is the
perspective weight: a fragment's `Weights` are its corners' perspective-correct
barycentric coordinates, in `WEIGHT_ONE`ths, so whatever a caller interpolates
across a triangle it interpolates as a GPU would.

A `Triangle` is set up once and filled into any band of a `DepthBuffer`, cut as
a surface's rows are. The fill tests each covered centre under a `DepthTest`,
hands each fragment that passes to the caller's shader — which answers whether
it keeps it — and writes the depth of a kept one. Asked to (`EdgeRecord`), it
records for each fragment how near its triangle's edge lies and which way
(`EdgeNear`), which is what a silhouette pass resolves aliasing against
afterwards without a multisampled buffer.

A fill that is not a mesh can still stand behind one: `DepthPlane` is a
polygon's own reciprocal depth across the screen and `Occlusion` tests it
against a buffer a mesh drew, so `Canvas::fill_polygon_subpixel_occluded` draws
a figure's strips hidden wherever the ground is nearer. The buffer is read and
never written, so the figure's own parts still cover one another by their
paint order alone.

## The general pipeline's stages

A draw's triangles are clipped, set up and tested by the same stages, in
`f64` for geometry and `f32` for colour, each a fixed order of operations so
the software device draws the same bits on every target.

- **Clipping** (`clip`). One cut, every edge from its inside end, serves the
  view space `Perspective` clips in and the homogeneous clip space a draw's
  vertex stage writes. A `Viewport` — a rectangle, its height negative where
  the draw flips it, and the depths its near and far planes map to — cuts a
  triangle to a positive `w`, near and far (unless depth is clamped) and a guard
  band a quarter of the converter's extent about its centre, and maps what
  remains onto the
  converter's grid: each `Mapped` corner with its depth, its reciprocal `w`,
  and its share of the triangle the draw named, in clip space and across the
  screen. At most `MAX_CLIP_CORNERS` corners remain. `Viewport::clip_line` cuts
  a segment by the same planes and maps its two ends, shared between the
  segment's own; a point is the segment of one vertex, dropped when it is
  clipped.
- **Set-up and interpolation** (`primitive`). A `Primitive` over three mapped
  corners covers through its `Footprint`, carries its depth as a plane across
  the screen with its steepest slope a pixel, and gives each sample the weights
  of the draw's own corners under an `Interpolation`: perspective-correct,
  linear across the screen, or flat at the provoking corner, and its
  reciprocal `w` at any point (`inverse_w`), a fragment's `FragCoord.w`. A
  value interpolated across any piece of a clipped triangle is the one the
  whole triangle gives there.
- **Depth** (`depth`). `DepthFormat` stores a depth in `0..=1` as sixteen- or
  twenty-four-bit unsigned normalised or as a float, its words ordering as the
  depths they hold, and states its resolution: a normalised step, or a float's
  unit in the last place at a polygon's deepest exponent. `DepthBias` is the
  slope and constant offset, held to its clamp either way.
- **Stencil** (`stencil`). A `StencilFace` compares its reference with the value
  held through the compare mask and writes each `Outcome`'s `StencilOp` through
  the write mask.
- **Blending** (`blend`). `Blend` is the device's pair of equations over the
  source, a second source, the colour held and the blend constant, every value
  and factor held to a normalised attachment's `BlendRange` first; `LogicOp`
  replaces it on stored bits. The desktop's own composite stays `Pixel::over`.

## The perspective

`view::Perspective` is the view a mesh is seen through: an eye, a forward, a
right and a down — any three independent directions, squared off against one
another — a focal length in pixels, a screen and a near distance. View space
has the eye at its origin; a point projects to the screen's centre plus the
focal length times its across and down over its distance, onto the converter's
own sub-unit grid, carrying `scale / distance` as its depth.

A triangle any corner of which does not project is **clipped**, not dropped:
`clip` cuts it, through the cut every clip shares, to the near plane and to a
guard band around the screen whose
every corner projects within the converter's exact range. `vertex` refuses
exactly the corners the clip would cut, so a triangle is drawn whole only where
the clip would leave it whole, and meets a clipped neighbour along their edge.
Each edge is cut from its inside end and the cut put exactly on the plane, so
two triangles sharing an edge cut it at the same point and stay watertight;
every corner of what remains comes projected and carries its share of the
triangle's own three, so a caller's attributes follow the clip.

`sees` tests a world box against the view out to a reach: outside if it lies
wholly past one of the view's planes, or if the view lies wholly past one of the
box's faces, which is what culls a tall box standing behind the eye. `ray` is
the direction through a pixel.

Everything is set up and evaluated in `f64` basic operations in a fixed order
with nothing fused, so a point projects to the same bits on every target.

## Stability tier

**experimental**: the mesh path and the perspective grow with the 3D view, and
the 2D families with the desktop.
