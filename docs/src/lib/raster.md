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
  and a fill's rows are built from, plus the one resampler.
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

`Depth` is reciprocal and reversed — larger is nearer, `Depth::CLEAR` lies
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

## The perspective

`view::Perspective` is the view a mesh is seen through: an eye, a forward, a
right and a down — any three independent directions, squared off against one
another — a focal length in pixels, a screen and a near distance. View space
has the eye at its origin; a point projects to the screen's centre plus the
focal length times its across and down over its distance, onto the converter's
own sub-unit grid, carrying `scale / distance` as its depth.

A triangle any corner of which does not project is **clipped**, not dropped:
`clip` cuts it to the near plane and to a guard band around the screen whose
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
