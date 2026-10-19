//! The terrain pass: every drawn mesh carried through the perspective and
//! scan-converted into the geometry buffer, one band of rows to a core.
//!
//! A frame's ground is drawn as a GPU draws a mesh. The vertex stage carries
//! each mesh's vertices into view space once; each band then sets up the
//! triangles reaching its rows, clips those the eye stands too near to, and
//! shades every fragment that passes the depth test with the material splat.
//! What a fragment leaves is not a lit colour but what the light needs to
//! light it — its albedo, its normal, its height and how near its
//! triangle's edge is — so the light is taken once a pixel, after the
//! nearest surface has won it.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_inline::ArrayVec;
use tairix_parallel::{for_each, JobRunner};
use tairix_raster::color::Pixel;
use tairix_raster::mesh::{
    Depth, DepthBand, DepthBuffer, DepthTest, EdgeNear, EdgeRecord, MeshVertex, Triangle,
    MESH_SUB_UNITS, WEIGHT_ONE,
};
use tairix_raster::view::{Clipped, Perspective, MAX_CLIPPED};
use tairix_util::mathf;
use tairix_wintersun_art::splat::{self, Fragment, Materials, QuadPlan, QUAD_ONE};
use tairix_wintersun_net::value::WorldPoint;

use crate::error::ClientError;
use crate::mesh::{TerrainMesh, Vertex, NORMAL_ONE, WORLD_PER_ELEVATION};

/// A pixel nothing has been drawn at.
const BLANK: Pixel = Pixel {
    r: 0,
    g: 0,
    b: 0,
    a: 0,
};

/// What a fragment leaves beside its albedo: the surface's normal and its
/// height, which the light and the mist are taken from.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Texel {
    /// The normal's east and south components, out of [`NORMAL_ONE`].
    pub normal: [i8; 2],
    /// The surface's height, in elevation sub-units.
    pub elevation: i16,
}

/// Pixels across one segment of the coarse depth.
const SEGMENT: u32 = 8;

/// One row segment of the geometry buffer, coarsely: which of its pixels a
/// surface has been drawn at, and the farthest depth any was drawn with.
///
/// A pixel's depth only ever comes nearer, so the farthest drawn is never
/// nearer than any pixel's depth now: a polygon whose nearest point is no
/// nearer than it, over segments wholly drawn, would fail every depth test.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Coarse {
    drawn: u8,
    farthest: Depth,
}

impl Coarse {
    const EMPTY: Self = Self {
        drawn: 0,
        farthest: Depth::NEAREST,
    };
}

/// The geometry buffer: per pixel, the nearest surface's depth, albedo,
/// normal and height, and how near its triangle's edge lies; and per row
/// segment, the coarse depth a polygon wholly behind what is drawn is culled
/// against before it is set up.
#[derive(Debug, Default)]
pub struct GBuffer {
    depth: DepthBuffer,
    albedo: Vec<Pixel>,
    texels: Vec<Texel>,
    edges: Vec<EdgeNear>,
    coarse: Vec<Coarse>,
}

impl GBuffer {
    /// An empty buffer, sized on its first reset.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Make the buffer `width` × `height` with nothing drawn in it.
    ///
    /// The depth, its coarse bound and the edge records are cleared, so no
    /// pixel resolves an edge an earlier frame drew; the albedo and texels are
    /// read only where a surface was drawn, and drawing writes them.
    ///
    /// # Errors
    ///
    /// [`ClientError::OutOfMemory`] where it does not fit.
    pub fn reset(&mut self, width: u32, height: u32) -> Result<(), ClientError> {
        if !self.depth.reset(width, height) {
            return Err(ClientError::OutOfMemory);
        }
        let pixels = usize::try_from(u64::from(width) * u64::from(height))
            .map_err(|_| ClientError::OutOfMemory)?;
        fit(&mut self.albedo, pixels, BLANK)?;
        fit(&mut self.texels, pixels, Texel::default())?;
        fit(&mut self.edges, pixels, EdgeNear::NONE)?;
        self.edges.fill(EdgeNear::NONE);
        let segments = usize::try_from(u64::from(width.div_ceil(SEGMENT)) * u64::from(height))
            .map_err(|_| ClientError::OutOfMemory)?;
        fit(&mut self.coarse, segments, Coarse::EMPTY)?;
        self.coarse.fill(Coarse::EMPTY);
        Ok(())
    }

    /// The depth drawn.
    #[must_use]
    pub const fn depth(&self) -> &DepthBuffer {
        &self.depth
    }

    /// Its width, in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.depth.width()
    }

    /// Its height, in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.depth.height()
    }

    /// What pixel `(x, y)` holds: its depth, albedo, texel and edge, or
    /// `None` outside the buffer. A depth of [`Depth::CLEAR`] is a pixel no
    /// surface was drawn at, whose other values mean nothing.
    #[must_use]
    pub fn at(&self, x: u32, y: u32) -> Option<(Depth, Pixel, Texel, EdgeNear)> {
        let width = self.width();
        if x >= width || y >= self.height() {
            return None;
        }
        let index = (y as usize) * (width as usize) + (x as usize);
        Some((
            self.depth.at(x, y),
            *self.albedo.get(index)?,
            *self.texels.get(index)?,
            *self.edges.get(index)?,
        ))
    }

    /// Row `y`, each of its planes a whole row's slice, or `None` outside the
    /// buffer.
    #[must_use]
    pub fn row(&self, y: u32) -> Option<GRow<'_>> {
        let width = self.width() as usize;
        if y >= self.height() {
            return None;
        }
        let start = (y as usize) * width;
        let span = start..start + width;
        Some(GRow {
            depth: self.depth.row(y),
            albedo: self.albedo.get(span.clone())?,
            texels: self.texels.get(span.clone())?,
            edges: self.edges.get(span)?,
        })
    }

    /// Draw a surface `depth` deep over the whole of row `y` from column
    /// `from` on, as a fragment would leave it.
    #[cfg(test)]
    pub(crate) fn paint_row(
        &mut self,
        y: u32,
        from: u32,
        depth: Depth,
        albedo: Pixel,
        texel: Texel,
        edge: EdgeNear,
    ) {
        let width = self.width();
        let sub = MESH_SUB_UNITS;
        let at = |x: u32, row: u32| MeshVertex {
            x: i32::try_from(x).unwrap_or(i32::MAX) * sub,
            y: i32::try_from(row).unwrap_or(i32::MAX) * sub,
            depth,
        };
        let corners = [
            [at(from, y), at(width, y), at(from, y + 1)],
            [at(width, y), at(width, y + 1), at(from, y + 1)],
        ];
        let height = self.height();
        for mut band in self.depth.bands_mut(height) {
            for corners in corners {
                if let Some(triangle) = Triangle::new(corners) {
                    let test = DepthTest {
                        compare: tairix_raster::mesh::DepthCompare::Always,
                        write: true,
                    };
                    triangle.fill(&mut band, test, EdgeRecord::Skip, |_| true);
                }
            }
        }
        for x in from..width {
            let index = (y as usize) * (width as usize) + (x as usize);
            self.albedo[index] = albedo;
            self.texels[index] = texel;
            self.edges[index] = edge;
        }
    }

    /// Every row in bands of `rows_per_band`, as a surface's rows are cut.
    ///
    /// # Errors
    ///
    /// [`ClientError::OutOfMemory`] where the band list does not fit.
    fn bands_mut(&mut self, rows_per_band: u32) -> Result<Vec<GBand<'_>>, ClientError> {
        let width = self.depth.width();
        let rows = rows_per_band.max(1) as usize;
        let per = rows.saturating_mul(width as usize).max(1);
        let segments = width.div_ceil(SEGMENT);
        let coarse = rows.saturating_mul(segments as usize).max(1);
        let mut bands = Vec::new();
        bands
            .try_reserve(self.depth.height().div_ceil(rows_per_band.max(1)) as usize)
            .map_err(|_| ClientError::OutOfMemory)?;
        let depth = self.depth.bands_mut(rows_per_band);
        bands.extend(
            depth
                .zip(self.albedo.chunks_mut(per))
                .zip(self.texels.chunks_mut(per))
                .zip(self.edges.chunks_mut(per))
                .zip(self.coarse.chunks_mut(coarse))
                .map(|((((depth, albedo), texels), edges), coarse)| GBand {
                    depth,
                    albedo,
                    texels,
                    edges,
                    coarse,
                    segments,
                }),
        );
        Ok(bands)
    }
}

/// One row of a [`GBuffer`], every plane of it a slice the row's width.
#[derive(Copy, Clone, Debug)]
pub struct GRow<'a> {
    /// The nearest surface's depth at each pixel.
    pub depth: &'a [Depth],
    /// Its albedo.
    pub albedo: &'a [Pixel],
    /// Its normal and height.
    pub texels: &'a [Texel],
    /// How near its triangle's edge lies.
    pub edges: &'a [EdgeNear],
}

/// One band of a [`GBuffer`]: whole rows, exclusively borrowed.
struct GBand<'a> {
    depth: DepthBand<'a>,
    albedo: &'a mut [Pixel],
    texels: &'a mut [Texel],
    edges: &'a mut [EdgeNear],
    coarse: &'a mut [Coarse],
    /// Row segments across the band.
    segments: u32,
}

/// The most row segments a polygon is checked against before it is set up:
/// past this a polygon is large on the screen, rarely hidden whole, and
/// cheaper drawn than probed.
const MOST_PROBED: u32 = 1024;

impl GBand<'_> {
    /// The pixels of segment `segment` across the screen.
    fn segment_mask(&self, segment: u32) -> u8 {
        let width = self.depth.width();
        let left = width.saturating_sub(segment * SEGMENT).min(SEGMENT);
        u8::try_from((1u32 << left) - 1).unwrap_or(u8::MAX)
    }

    /// Whether a polygon with corners `screen` can show in the band: it
    /// reaches the band's rows and the screen, and is not wholly behind what
    /// the band already holds.
    fn shows(&self, screen: &[MeshVertex]) -> bool {
        let extent = Extent::of(screen);
        let nearest = Depth(
            screen
                .iter()
                .map(|corner| corner.depth.0)
                .max()
                .unwrap_or(0),
        );
        extent.reaches(&self.depth.rows(), self.depth.width()) && !self.hides(extent, nearest)
    }

    /// Whether the band already holds, at every pixel a polygon spanning
    /// `extent` could reach in it, a surface no farther than `nearest`, the
    /// nearest the polygon comes: a polygon hidden there whole.
    fn hides(&self, extent: Extent, nearest: Depth) -> bool {
        let unit = i64::from(MESH_SUB_UNITS);
        let rows = self.depth.rows();
        let width = self.depth.width();
        let Extent {
            left,
            right,
            top,
            bottom,
        } = extent;
        let clamp = |value: i64, low: u32, high: u32| {
            u32::try_from(
                value
                    .div_euclid(unit)
                    .clamp(i64::from(low), i64::from(high)),
            )
            .unwrap_or(low)
        };
        let Some(last_row) = rows.end.checked_sub(1) else {
            return false;
        };
        let Some(last_column) = width.checked_sub(1) else {
            return false;
        };
        let (first_row, last_row) = (
            clamp(top, rows.start, last_row),
            clamp(bottom, rows.start, last_row),
        );
        let (first, last) = (
            clamp(left, 0, last_column) / SEGMENT,
            clamp(right, 0, last_column) / SEGMENT,
        );
        if (last_row - first_row + 1).saturating_mul(last - first + 1) > MOST_PROBED {
            return false;
        }
        (first_row..=last_row).all(|row| {
            (first..=last).all(|segment| {
                let at = ((row - rows.start) * self.segments + segment) as usize;
                self.coarse.get(at).is_some_and(|coarse| {
                    coarse.drawn == self.segment_mask(segment) && coarse.farthest.0 >= nearest.0
                })
            })
        })
    }
}

/// What one row of a mesh's vertices reaches this frame.
#[derive(Copy, Clone, Debug)]
struct Span {
    /// The pixel rows its vertices reach, as `(top, bottom)`, the whole screen
    /// where any of them does not project.
    rows: (i64, i64),
    /// Whether any of it lies at or past the near plane: a quad between two
    /// rows with none is wholly behind the eye.
    ahead: bool,
}

/// One mesh's vertices this frame: where each is in view space and, where it
/// projects, on the screen, and the rows the projected ones reach.
#[derive(Debug, Default)]
pub struct Carried {
    views: Vec<[f64; 3]>,
    screen: Vec<Option<MeshVertex>>,
    spans: Vec<Span>,
    /// Each edge's vertices hung by the skirt — north, south, west and east,
    /// in order along the edge — and the rows they reach.
    skirt: [Vec<([f64; 3], Option<MeshVertex>)>; 4],
    skirt_rows: (i64, i64),
    /// The rows anything of the mesh in front of the eye reaches.
    rows: (i64, i64),
}

impl Carried {
    /// Carry `mesh`'s vertices through `camera`, and the skirt it hangs from
    /// each edge `skirts` names.
    ///
    /// # Errors
    ///
    /// [`ClientError::OutOfMemory`] where the scratch does not fit.
    pub fn carry(
        &mut self,
        mesh: &TerrainMesh,
        skirts: [bool; 4],
        camera: &Perspective,
    ) -> Result<(), ClientError> {
        let side = mesh.side();
        let count = mesh.vertices().len();
        let near = camera.near();
        refill(&mut self.views, count)?;
        refill(&mut self.screen, count)?;
        refill(&mut self.spans, side as usize)?;
        for j in 0..side {
            let mut span = Span {
                rows: (i64::MAX, i64::MIN),
                ahead: false,
            };
            for i in 0..side {
                let height = mesh.vertex(i, j).map_or(0, |vertex| vertex.height);
                let view = camera.view(world(mesh.place(i, j), height));
                let projected = camera.vertex(view);
                span.rows = reach(span.rows, projected);
                span.ahead |= view[2] >= near;
                self.views.push(view);
                self.screen.push(projected);
            }
            self.spans.push(span);
        }
        self.skirt_rows = (i64::MAX, i64::MIN);
        let last = side - 1;
        for ((edge, hung), hangs) in self.skirt.iter_mut().enumerate().zip(skirts) {
            hung.clear();
            if !hangs {
                continue;
            }
            refill(hung, side as usize)?;
            for k in 0..side {
                let (i, j) = match edge {
                    0 => (k, 0),
                    1 => (k, last),
                    2 => (0, k),
                    _ => (last, k),
                };
                // A skirt quad spans its edge's own corners as well as the
                // hung ones, and either may reach rows the other does not.
                let top = self.screen.get((j * side + i) as usize).copied().flatten();
                self.skirt_rows = reach(self.skirt_rows, top);
                let height = mesh
                    .vertex(i, j)
                    .map_or(0, |vertex| vertex.height.saturating_sub(mesh.skirt()));
                let view = camera.view(world(mesh.place(i, j), height));
                let projected = camera.vertex(view);
                self.skirt_rows = reach(self.skirt_rows, projected);
                hung.push((view, projected));
            }
        }
        self.rows = (0..self.spans.len().saturating_sub(1))
            .filter_map(|j| self.quad_rows(j))
            .fold(self.skirt_rows, |(top, bottom), (above, below)| {
                (top.min(above), bottom.max(below))
            });
        Ok(())
    }

    /// The rows a quad between vertex rows `j` and `j + 1` can cover, or
    /// `None` where every vertex of both lies behind the eye.
    fn quad_rows(&self, j: usize) -> Option<(i64, i64)> {
        let (a, b) = (self.spans[j], self.spans[j + 1]);
        (a.ahead || b.ahead).then(|| (a.rows.0.min(b.rows.0), a.rows.1.max(b.rows.1)))
    }
}

/// `span` widened to the row `projected` falls in, or to the whole screen
/// where it does not project.
fn reach(span: (i64, i64), projected: Option<MeshVertex>) -> (i64, i64) {
    match projected {
        Some(vertex) => {
            let row = i64::from(vertex.y).div_euclid(i64::from(MESH_SUB_UNITS));
            (span.0.min(row), span.1.max(row + 1))
        }
        None => (i64::MIN, i64::MAX),
    }
}

/// Empty `buffer` and make room for `len` values in it.
fn refill<T>(buffer: &mut Vec<T>, len: usize) -> Result<(), ClientError> {
    buffer.clear();
    buffer
        .try_reserve(len)
        .map_err(|_| ClientError::OutOfMemory)
}

/// A mesh drawn this frame, with its vertices carried.
#[derive(Copy, Clone, Debug)]
pub struct Drawing<'a> {
    /// The mesh.
    pub mesh: &'a TerrainMesh,
    /// Its vertices this frame.
    pub carried: &'a Carried,
    /// Which of its edges, north, south, west and east, hang a skirt: those
    /// a neighbour drawn with other vertices meets, where a crack can open.
    pub skirts: [bool; 4],
}

/// Everything the terrain pass reads besides the meshes.
#[derive(Copy, Clone)]
pub struct Pass<'a> {
    /// The view.
    pub camera: &'a Perspective,
    /// The tiles the frame made resident.
    pub materials: &'a Materials<'a>,
}

/// Draw `drawings`, nearest first so the depth test spares the shading of
/// what they hide, into `buffer` in bands of `rows` rows across `runner`.
///
/// # Errors
///
/// [`ClientError::OutOfMemory`] where the band list does not fit.
pub fn draw(
    buffer: &mut GBuffer,
    drawings: &[Drawing<'_>],
    pass: &Pass<'_>,
    rows: u32,
    runner: &dyn JobRunner,
) -> Result<(), ClientError> {
    let mut bands = buffer.bands_mut(rows)?;
    for_each(runner, &mut bands, &|band: &mut GBand<'_>| {
        let mut clipped = ArrayVec::<Clipped, MAX_CLIPPED>::new();
        for drawing in drawings {
            draw_mesh(band, *drawing, pass, &mut clipped);
        }
    });
    Ok(())
}

/// The corners of a quad's two triangles, as `(across, down)` vertex offsets
/// from its north-west corner: cut from north-west to south-east.
const HALVES: [[(u32, u32); 3]; 2] = [[(0, 0), (1, 0), (1, 1)], [(0, 0), (1, 1), (0, 1)]];

/// Draw every triangle of `drawing` reaching `band`'s rows.
fn draw_mesh(
    band: &mut GBand<'_>,
    drawing: Drawing<'_>,
    pass: &Pass<'_>,
    clipped: &mut ArrayVec<Clipped, MAX_CLIPPED>,
) {
    let rows = band.depth.rows();
    let (top, bottom) = (i64::from(rows.start), i64::from(rows.end));
    let (first, last) = drawing.carried.rows;
    if last <= top || first >= bottom {
        return;
    }
    let mesh = drawing.mesh;
    let side = mesh.side();
    let carried = drawing.carried;
    let eye = pass.camera.eye();
    let (ci, cj) = (
        quad_under(mesh_axis(mesh, eye[0], true), mesh.spacing(), side),
        quad_under(mesh_axis(mesh, eye[1], false), mesh.spacing(), side),
    );
    for j in outward(cj, side - 1) {
        let Some((above, below)) = carried.quad_rows(j as usize) else {
            continue;
        };
        if below <= top || above >= bottom {
            continue;
        }
        for i in outward(ci, side - 1) {
            if !quad_shows(carried, side, (i, j), band, pass.camera.near()) {
                continue;
            }
            let Some(plan) = mesh.plan(i, j) else {
                continue;
            };
            for half in HALVES {
                let corners = half.map(|(di, dj)| corner(mesh, carried, i + di, j + dj, (di, dj)));
                draw_triangle(band, pass, mesh, plan, mesh.place(i, j), corners, clipped);
            }
        }
    }
    draw_skirt(band, pass, drawing, clipped);
}

/// How far along one axis `at` lies from `mesh`'s first vertex, in world
/// sub-units: across where `across`, else down.
fn mesh_axis(mesh: &TerrainMesh, at: f64, across: bool) -> i64 {
    let first = mesh.place(0, 0);
    let origin = if across { first.x } else { first.y };
    i64::from(mathf::round_i32(at)) - i64::from(origin)
}

/// The quad of a mesh `side` vertices wide whose column or row holds a point
/// `offset` sub-units from its first vertex, clamped onto the mesh.
fn quad_under(offset: i64, spacing: i32, side: u32) -> u32 {
    let last = i64::from(side.saturating_sub(2));
    u32::try_from(offset.div_euclid(i64::from(spacing.max(1))).clamp(0, last)).unwrap_or(0)
}

/// `0..len` ordered by distance from `centre`, nearest first: a mesh's quads
/// drawn outward from the eye, so the nearest surface takes each pixel before
/// what it hides is shaded.
fn outward(centre: u32, len: u32) -> impl Iterator<Item = u32> {
    let centre = centre.min(len.saturating_sub(1));
    (0..len).flat_map(move |distance| {
        let after = centre.checked_add(distance).filter(|&at| at < len);
        let before = centre.checked_sub(distance).filter(|_| distance > 0);
        after.into_iter().chain(before)
    })
}

/// The screen box a polygon's corners span, in sub-units.
#[derive(Copy, Clone, Debug)]
struct Extent {
    left: i64,
    right: i64,
    top: i64,
    bottom: i64,
}

impl Extent {
    fn of(screen: &[MeshVertex]) -> Self {
        screen.iter().fold(
            Self {
                left: i64::MAX,
                right: i64::MIN,
                top: i64::MAX,
                bottom: i64::MIN,
            },
            |extent, corner| {
                let (x, y) = (i64::from(corner.x), i64::from(corner.y));
                Self {
                    left: extent.left.min(x),
                    right: extent.right.max(x),
                    top: extent.top.min(y),
                    bottom: extent.bottom.max(y),
                }
            },
        )
    }

    /// Whether it can reach a pixel centre of `rows`, `width` pixels across:
    /// a polygon wholly beside the band or the screen is not set up at all.
    fn reaches(self, rows: &Range<u32>, width: u32) -> bool {
        let unit = i64::from(MESH_SUB_UNITS);
        self.bottom >= i64::from(rows.start) * unit
            && self.top < i64::from(rows.end) * unit
            && self.right >= 0
            && self.left < i64::from(width) * unit
    }
}

/// Whether the quad whose north-west vertex is `(i, j)` of a mesh `side`
/// vertices wide can show in `band`: not where every corner is behind the
/// near plane, nor where every corner projects and the four lie beside the
/// band or wholly behind what it already holds. A quad some corner of which
/// does not project is left to its triangles' clip.
fn quad_shows(
    carried: &Carried,
    side: u32,
    (column, row): (u32, u32),
    band: &GBand<'_>,
    near: f64,
) -> bool {
    let (side, first) = (
        side as usize,
        (row as usize) * (side as usize) + (column as usize),
    );
    let corners = [first, first + 1, first + side, first + side + 1];
    if corners.iter().all(|&at| carried.views[at][2] < near) {
        return false;
    }
    match corners.map(|at| carried.screen[at]) {
        [Some(north_west), Some(north_east), Some(south_west), Some(south_east)] => {
            band.shows(&[north_west, north_east, south_west, south_east])
        }
        _ => true,
    }
}

/// One corner of a triangle about to be drawn.
#[derive(Copy, Clone, Debug)]
struct Corner {
    view: [f64; 3],
    screen: Option<MeshVertex>,
    /// Where it sits across and down its quad, out of [`QUAD_ONE`].
    across: u16,
    down: u16,
    vertex: Vertex,
}

/// Vertex `(i, j)` of `mesh` as the corner `offset` of its quad.
fn corner(mesh: &TerrainMesh, carried: &Carried, i: u32, j: u32, offset: (u32, u32)) -> Corner {
    let index = (j as usize) * (mesh.side() as usize) + (i as usize);
    Corner {
        view: carried.views[index],
        screen: carried.screen[index],
        across: if offset.0 == 0 { 0 } else { QUAD_ONE },
        down: if offset.1 == 0 { 0 } else { QUAD_ONE },
        vertex: mesh.vertices()[index],
    }
}

/// The normal of the skirt hung from each edge — north, south, west and
/// east — facing out from the mesh, east and south out of [`NORMAL_ONE`].
const WALLS: [[i8; 2]; 4] = {
    assert!(
        NORMAL_ONE <= i8::MAX as i32,
        "a normal's component fits its byte"
    );
    #[allow(clippy::cast_possible_truncation, reason = "checked to fit just above")]
    let one = NORMAL_ONE as i8;
    [[0, -one], [0, one], [-one, 0], [one, 0]]
};

/// Hang `drawing`'s skirt from its four edges.
fn draw_skirt(
    band: &mut GBand<'_>,
    pass: &Pass<'_>,
    drawing: Drawing<'_>,
    clipped: &mut ArrayVec<Clipped, MAX_CLIPPED>,
) {
    let (top, bottom) = {
        let rows = band.depth.rows();
        (i64::from(rows.start), i64::from(rows.end))
    };
    let (first, last_row) = drawing.carried.skirt_rows;
    if last_row <= top || first >= bottom {
        return;
    }
    let mesh = drawing.mesh;
    let carried = drawing.carried;
    let last = mesh.side() - 1;
    for k in 0..last {
        // Each edge as its two ends, their places along the edge, and the
        // quad they belong to.
        let edges = [
            (0, (k, 0), (k + 1, 0), (k, k + 1), (k, 0)),
            (1, (k + 1, last), (k, last), (k + 1, k), (k, last - 1)),
            (2, (0, k + 1), (0, k), (k + 1, k), (0, k)),
            (3, (last, k), (last, k + 1), (k, k + 1), (last - 1, k)),
        ];
        for (edge, from, to, along, quad) in edges {
            if !drawing.skirts[edge] {
                continue;
            }
            let Some(plan) = mesh.plan(quad.0, quad.1) else {
                continue;
            };
            // A skirt is a wall, so every corner of it faces out from its edge.
            let at = |end: (u32, u32)| {
                let top = corner(
                    mesh,
                    carried,
                    end.0,
                    end.1,
                    (end.0 - quad.0, end.1 - quad.1),
                );
                Corner {
                    vertex: Vertex {
                        normal: WALLS[edge],
                        ..top.vertex
                    },
                    ..top
                }
            };
            let (top_from, top_to) = (at(from), at(to));
            let hang = |top: Corner, place: u32| {
                let (view, screen) = carried.skirt[edge][place as usize];
                Corner {
                    view,
                    screen,
                    vertex: Vertex {
                        height: top.vertex.height.saturating_sub(mesh.skirt()),
                        ..top.vertex
                    },
                    ..top
                }
            };
            let (low_from, low_to) = (hang(top_from, along.0), hang(top_to, along.1));
            if let [Some(a), Some(b), Some(c), Some(d)] =
                [top_from, top_to, low_to, low_from].map(|corner| corner.screen)
            {
                if !band.shows(&[a, b, c, d]) {
                    continue;
                }
            }
            let origin = mesh.place(quad.0, quad.1);
            for corners in [[top_from, top_to, low_to], [top_from, low_to, low_from]] {
                draw_triangle(band, pass, mesh, plan, origin, corners, clipped);
            }
        }
    }
}

/// A world position in the `f64` space the perspective takes.
fn world(at: WorldPoint, height: i32) -> [f64; 3] {
    [f64::from(at.x), f64::from(at.y), f64::from(height)]
}

/// What one triangle's fragments are shaded from.
struct Shader<'a> {
    pass: &'a Pass<'a>,
    plan: &'a QuadPlan,
    origin: WorldPoint,
    spacing: i64,
    /// Each interpolated attribute at the triangle's three corners.
    attributes: Attributes,
    /// Ground a pixel spans at the reference depth, and that depth.
    footprint: (u64, u64),
}

/// The attributes a fragment interpolates: where it lies across and down its
/// quad, the surface's height, the normal's two components and the warp's
/// two, each at a triangle's three corners.
#[derive(Copy, Clone, Debug)]
struct Attributes([[i64; 3]; 7]);

impl Attributes {
    fn of(corners: &[Corner; 3]) -> Self {
        let each = |value: fn(&Corner) -> i64| corners.map(|corner| value(&corner));
        Self([
            each(|corner| i64::from(corner.across)),
            each(|corner| i64::from(corner.down)),
            each(|corner| i64::from(corner.vertex.height)),
            each(|corner| i64::from(corner.vertex.normal[0])),
            each(|corner| i64::from(corner.vertex.normal[1])),
            each(|corner| i64::from(corner.vertex.warp[0])),
            each(|corner| i64::from(corner.vertex.warp[1])),
        ])
    }

    /// Each attribute at a fragment lying `weights` among the corners.
    fn at(&self, weights: [u32; 3]) -> [i64; 7] {
        let [a, b, c] = weights.map(i64::from);
        self.0
            .map(|[first, second, third]| (a * first + b * second + c * third) >> 16)
    }
}

/// Draw one triangle of a quad whose north-west corner stands at `origin`,
/// clipping it first where any corner does not project.
fn draw_triangle(
    band: &mut GBand<'_>,
    pass: &Pass<'_>,
    mesh: &TerrainMesh,
    plan: &QuadPlan,
    origin: WorldPoint,
    corners: [Corner; 3],
    clipped: &mut ArrayVec<Clipped, MAX_CLIPPED>,
) {
    let near = pass.camera.near();
    if corners.iter().all(|corner| corner.view[2] < near) {
        return;
    }
    if let [Some(a), Some(b), Some(c)] = corners.map(|corner| corner.screen) {
        let screen = [a, b, c];
        if !Extent::of(&screen).reaches(&band.depth.rows(), band.depth.width()) {
            return;
        }
        let Some(triangle) = Triangle::new(screen) else {
            return;
        };
        let shader = Shader {
            pass,
            plan,
            origin,
            spacing: i64::from(mesh.spacing()),
            attributes: Attributes::of(&corners),
            footprint: footprint(corners.map(|corner| corner.view), screen),
        };
        fill(band, &triangle, &shader, None);
        return;
    }
    pass.camera.clip(corners.map(|corner| corner.view), clipped);
    let Some((first, rest)) = clipped.split_first() else {
        return;
    };
    for pair in rest.windows(2) {
        let pieces = [*first, pair[0], pair[1]];
        let [a, b, c] = pieces.map(|piece| piece.vertex);
        if !Extent::of(&[a, b, c]).reaches(&band.depth.rows(), band.depth.width()) {
            continue;
        }
        let Some(triangle) = Triangle::new([a, b, c]) else {
            continue;
        };
        let shader = Shader {
            pass,
            plan,
            origin,
            spacing: i64::from(mesh.spacing()),
            attributes: Attributes::of(&corners),
            footprint: footprint(pieces.map(|piece| piece.view), [a, b, c]),
        };
        fill(band, &triangle, &shader, Some(shares(pieces)));
    }
}

/// Each clipped corner's share of the triangle's own three, in
/// [`WEIGHT_ONE`]ths.
#[allow(
    clippy::float_arithmetic,
    reason = "a clipped corner's shares come out of the perspective's clip in \
              `f64`; they are rounded once per clipped triangle, and every \
              fragment of it composes them in integers"
)]
fn shares(pieces: [Clipped; 3]) -> [[u32; 3]; 3] {
    pieces.map(|piece| {
        piece.weights.map(|share| {
            let scaled = mathf::round_i32(share * f64::from(WEIGHT_ONE));
            u32::try_from(scaled.clamp(0, i32::try_from(WEIGHT_ONE).unwrap_or(i32::MAX)))
                .unwrap_or(0)
        })
    })
}

/// The ground one pixel spans across a triangle whose corners are `views` in
/// view space and `screen` on it, and the reciprocal depth that holds at: the
/// square root of its area over the area it covers, at the mean of its
/// corners' depths.
#[allow(
    clippy::float_arithmetic,
    reason = "taken once a triangle from the view-space corners the \
              perspective works in; each fragment then scales it by its own \
              depth in integers"
)]
fn footprint(views: [[f64; 3]; 3], screen: [MeshVertex; 3]) -> (u64, u64) {
    let edge = |a: [f64; 3], b: [f64; 3]| [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let (u, v) = (edge(views[0], views[1]), edge(views[0], views[2]));
    let cross = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let ground = mathf::sqrt(cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]);
    let at = screen.map(|corner| (i64::from(corner.x), i64::from(corner.y)));
    let twice = ((at[1].0 - at[0].0) * (at[2].1 - at[0].1)
        - (at[2].0 - at[0].0) * (at[1].1 - at[0].1))
        .unsigned_abs();
    let sub = f64::from(MESH_SUB_UNITS);
    #[allow(
        clippy::cast_precision_loss,
        reason = "twice a triangle's area in sub-units is below 2^60, and a \
                  footprint wants its order of magnitude"
    )]
    let pixels = twice as f64 / (sub * sub);
    let span = if pixels > 0.0 {
        mathf::sqrt(ground / pixels)
    } else {
        0.0
    };
    let depth = screen
        .iter()
        .map(|corner| u64::from(corner.depth.0))
        .sum::<u64>()
        / 3;
    let span = u64::try_from(mathf::round_i32(span).max(1)).unwrap_or(1);
    (span, depth.max(1))
}

/// Fill `triangle` into `band`, each fragment shaded by `shader`; `shares`
/// carries a clipped piece's corners back to its triangle's own.
fn fill(
    band: &mut GBand<'_>,
    triangle: &Triangle,
    shader: &Shader<'_>,
    shares: Option<[[u32; 3]; 3]>,
) {
    let width = band.depth.width() as usize;
    let first = band.depth.rows().start;
    let GBand {
        depth,
        albedo,
        texels,
        edges,
        coarse,
        segments,
    } = band;
    let segments = *segments as usize;
    triangle.fill(depth, DepthTest::OPAQUE, EdgeRecord::Keep, |fragment| {
        let own = fragment.weights.0;
        let weights = match shares {
            None => own,
            Some(pieces) => compose(own, pieces),
        };
        let Some(index) = fragment
            .y
            .checked_sub(first)
            .map(|row| (row as usize) * width + fragment.x as usize)
        else {
            return false;
        };
        let (Some(albedo), Some(texel), Some(edge)) = (
            albedo.get_mut(index),
            texels.get_mut(index),
            edges.get_mut(index),
        ) else {
            return false;
        };
        let (colour, surface) = shader.shade(weights, fragment.x, fragment.y, fragment.depth);
        *albedo = colour;
        *texel = surface;
        *edge = fragment.edge;
        let row = (fragment.y - first) as usize;
        if let Some(coarse) = coarse.get_mut(row * segments + (fragment.x / SEGMENT) as usize) {
            coarse.drawn |= 1 << (fragment.x % SEGMENT);
            coarse.farthest = Depth(coarse.farthest.0.min(fragment.depth.0));
        }
        true
    });
}

/// A fragment's shares of a clipped piece's corners, carried back to the
/// shares of the triangle's own.
fn compose(own: [u32; 3], pieces: [[u32; 3]; 3]) -> [u32; 3] {
    let mut out = [0u32; 3];
    for (slot, total) in out.iter_mut().enumerate() {
        let sum: u64 = own
            .iter()
            .zip(&pieces)
            .map(|(weight, piece)| u64::from(*weight) * u64::from(piece[slot]))
            .sum();
        *total = u32::try_from(sum >> 16).unwrap_or(WEIGHT_ONE);
    }
    out
}

/// A 4×4 ordered dither, sixteenths: what each pixel's mip choice is raised
/// by, so neighbouring pixels between two levels split between them.
const BAYER: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

impl Shader<'_> {
    /// The albedo and texel of the fragment at `(x, y)`, `depth` deep, lying
    /// `weights` among the triangle's corners.
    fn shade(&self, weights: [u32; 3], x: u32, y: u32, depth: Depth) -> (Pixel, Texel) {
        let [across, down, height, east, south, dx, dy] = self.attributes.at(weights);
        let normal = [east, south];
        let one = i64::from(QUAD_ONE);
        let place = WorldPoint {
            x: saturate(i64::from(self.origin.x) + across * self.spacing / one),
            y: saturate(i64::from(self.origin.y) + down * self.spacing / one),
        };
        let (span, reference) = self.footprint;
        let scaled = span.saturating_mul(reference) / u64::from(depth.0.max(1));
        let dither = BAYER[((y & 3) * 4 + (x & 3)) as usize];
        let (first, second) = match Reading::of(normal, dither) {
            Reading::Ground => (i64::from(place.x), i64::from(place.y)),
            Reading::Meridian => (i64::from(place.y), -height),
            Reading::Parallel => (i64::from(place.x), -height),
        };
        let colour = splat::fragment(
            self.plan,
            self.pass.materials,
            Fragment {
                sample: WorldPoint {
                    x: saturate(first + dx),
                    y: saturate(second + dy),
                },
                across: u16::try_from(across.clamp(0, one)).unwrap_or(QUAD_ONE),
                down: u16::try_from(down.clamp(0, one)).unwrap_or(QUAD_ONE),
                footprint: u32::try_from(scaled).unwrap_or(u32::MAX),
                dither,
            },
        );
        let component = |value: i64| i8::try_from(value.clamp(-127, 127)).unwrap_or(0);
        let elevation = (height / i64::from(WORLD_PER_ELEVATION))
            .clamp(i64::from(i16::MIN), i64::from(i16::MAX));
        (
            colour,
            Texel {
                normal: [component(normal[0]), component(normal[1])],
                elevation: i16::try_from(elevation).unwrap_or(0),
            },
        )
    }
}

/// The plane a fragment reads its materials in: the ground plane where the
/// surface is gentle, and on a steep face the vertical plane it most nearly
/// faces, so a cliff's texture keeps its scale rather than streaking down it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Reading {
    /// East and south.
    Ground,
    /// South and down: the plane a face toward east or west lies in.
    Meridian,
    /// East and down: the plane a face toward north or south lies in.
    Parallel,
}

/// The steepest a surface may be, as its normal's horizontal part squared out
/// of [`NORMAL_ONE`] squared, and still read only the ground plane: thirty
/// degrees, where a vertical plane would take one dithered pixel in eighty.
const GENTLE: i32 = NORMAL_ONE * NORMAL_ONE / 4;

impl Reading {
    /// The plane a surface facing `normal` — east and south, out of
    /// [`NORMAL_ONE`] — reads in at a pixel `dither` sixteenths into the
    /// ordered dither.
    ///
    /// Each plane is weighted by the normal's component across it to the
    /// eighth power and the dither picks between them, so the planes give way
    /// to one another over a band about forty-five degrees, not at a seam.
    fn of(normal: [i64; 2], dither: u8) -> Self {
        let [east, south] = normal.map(|component| component * component);
        if east + south < i64::from(GENTLE) {
            return Self::Ground;
        }
        let up = (i64::from(NORMAL_ONE * NORMAL_ONE) - east - south).max(0);
        let sharpen = |weight: i64| {
            let squared = weight.unsigned_abs() * weight.unsigned_abs();
            squared * squared
        };
        let (ground, meridian, parallel) = (sharpen(up), sharpen(east), sharpen(south));
        let pick = (ground + meridian + parallel) / 32 * (2 * u64::from(dither & 15) + 1);
        if pick < ground {
            Self::Ground
        } else if pick < ground + meridian {
            Self::Meridian
        } else {
            Self::Parallel
        }
    }
}

/// A 64-bit world coordinate as the wire's 32-bit one, saturating.
fn saturate(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}

/// Resize `buffer` to `len` values of `blank`, refusing rather than
/// panicking when the allocation does not fit.
fn fit<T: Clone>(buffer: &mut Vec<T>, len: usize, blank: T) -> Result<(), ClientError> {
    if buffer.len() == len {
        return Ok(());
    }
    buffer.clear();
    buffer
        .try_reserve(len)
        .map_err(|_| ClientError::OutOfMemory)?;
    buffer.resize(len, blank);
    Ok(())
}

#[cfg(test)]
#[path = "raster_tests.rs"]
mod tests;
