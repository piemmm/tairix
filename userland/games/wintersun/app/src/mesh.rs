//! The ground as triangle meshes: a solved chunk at a stride, or the far
//! patch standing in for a chunk nobody has solved, each a grid of vertices
//! whose quads carry the plan their fragments are splatted from.
//!
//! A mesh is built once from ground the client already holds and drawn from
//! then on, so a steady view builds nothing. Each quad's plan is made when
//! the mesh is, which is what keeps a fragment's cost to a weight
//! interpolation, its texel reads and a blend.
//!
//! # Normals are the fine surface's
//!
//! A vertex's normal is read across the cells either side of it whatever the
//! stride, so a distant mesh keeps the light and shade of the relief it is
//! too coarse to carry, and two meshes meeting at a seam agree on the normal
//! there: a solved chunk's apron and a far patch's halo both hold what the
//! neighbour holds.
//!
//! # Skirts
//!
//! Two meshes at different strides do not share their edge vertices, and the
//! coarser edge can stand above the finer one. A mesh therefore hangs a skirt
//! from each edge whose neighbour differs from it in stride or source, or is
//! not drawn, as deep as its own spacing: the most a neighbour stands off it
//! on ground no steeper than one in one, so a crack there shows the edge's
//! own ground rather than the sky.

use alloc::vec::Vec;

use tairix_inline::bitset::BitSet256;
use tairix_wintersun_art::decal::{Bounds, Decal, Fray};
use tairix_wintersun_art::splat::{QuadPlan, Warp};
use tairix_wintersun_art::weight::WeightField;
use tairix_wintersun_figure::actor::Relief;
use tairix_wintersun_net::value::{ChunkCoord, WorldPoint};
use tairix_wintersun_world::blend::Kind;
use tairix_wintersun_world::chunk::Chunk;
use tairix_wintersun_world::far::FarPatch;
use tairix_wintersun_world::geom::{
    chunk_origin, Elevation, CELL_SUB_UNITS, CHUNK_CELLS, ELEVATION_SUB_UNITS,
};
use tairix_wintersun_world::ground::Ground;

use crate::error::ClientError;
use crate::terrain::{chunk_at, triangle_height};

/// World sub-units per elevation sub-unit.
pub const WORLD_PER_ELEVATION: i32 = {
    assert!(
        CELL_SUB_UNITS % ELEVATION_SUB_UNITS == 0,
        "a whole number of world sub-units to an elevation one"
    );
    CELL_SUB_UNITS / ELEVATION_SUB_UNITS
};

/// What a mesh is built from.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Source {
    /// A chunk the generator solved in full.
    Solved,
    /// Level 0's reading of a chunk nobody has solved.
    Far,
}

/// Which mesh: the chunk it covers, what it is built from, and the cells
/// between its vertices.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct MeshKey {
    /// The chunk.
    pub coord: ChunkCoord,
    /// What it is built from.
    pub source: Source,
    /// Cells between its vertices.
    pub stride: u32,
}

/// One vertex: the surface's height there and its normal.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Vertex {
    /// The surface's height, in world sub-units: the water where water
    /// stands, the ground elsewhere.
    pub height: i32,
    /// The surface normal's east and south components, out of
    /// [`NORMAL_ONE`]; it always points up, so these two are the whole of it.
    pub normal: [i8; 2],
    /// The material warp's displacement here, east and south, in world
    /// sub-units: a field smooth over many vertices, so a fragment reads it
    /// between its corners rather than evaluating the noise itself.
    pub warp: [i16; 2],
}

/// A unit normal component.
pub const NORMAL_ONE: i32 = 127;

/// The ground over one chunk as a grid of vertices and the plans of the quads
/// between them.
#[derive(Clone, Debug)]
pub struct TerrainMesh {
    key: MeshKey,
    origin: WorldPoint,
    spacing: i32,
    side: u32,
    vertices: Vec<Vertex>,
    plans: Vec<QuadPlan>,
    low: i32,
    high: i32,
    grounds: BitSet256,
}

impl TerrainMesh {
    /// The mesh of `chunk` at `stride` cells between vertices, with `decals`
    /// stamped into its ground and its materials displaced by `warp`.
    ///
    /// # Errors
    ///
    /// [`ClientError::Viewport`] for a stride that does not divide a chunk,
    /// and [`ClientError::OutOfMemory`] where the mesh does not fit.
    pub fn solved(
        chunk: &Chunk,
        stride: u32,
        decals: &[Decal<'_>],
        fray: &Fray,
        warp: &Warp,
    ) -> Result<Self, ClientError> {
        if stride == 0 || stride > CHUNK_CELLS || !CHUNK_CELLS.is_multiple_of(stride) {
            return Err(ClientError::Viewport);
        }
        let side = CHUNK_CELLS / stride + 1;
        let at = |i: u32, j: u32| (signed(i * stride), signed(j * stride));
        let height = |dx: i32, dy: i32| chunk.water_beside(dx, dy);
        let vertex = |i: u32, j: u32| {
            let (dx, dy) = at(i, j);
            let here = height(dx, dy).unwrap_or(Elevation::SEA_LEVEL);
            let across = |ox: i32, oy: i32| height(dx + ox, dy + oy).unwrap_or(here);
            Vertex {
                height: world_height(here),
                normal: normal(
                    [across(-1, 0), across(1, 0), across(0, -1), across(0, 1)],
                    1,
                ),
                warp: [0, 0],
            }
        };
        let field = |i: u32, j: u32| {
            let (dx, dy) = at(i, j);
            chunk
                .ground_beside(dx, dy)
                .map_or(WeightField::solid(Ground::Water), |blend| {
                    WeightField::from_blend(&blend)
                })
        };
        Self::build(
            MeshKey {
                coord: chunk.coord(),
                source: Source::Solved,
                stride,
            },
            side,
            vertex,
            field,
            decals,
            fray,
            warp,
        )
    }

    /// The mesh of `patch`, sampled every `stride` cells — its own step or a
    /// multiple of it — with `decals` stamped into its ground and its
    /// materials displaced by `warp`.
    ///
    /// # Errors
    ///
    /// [`ClientError::Viewport`] for a stride that is not a multiple of the
    /// patch's step dividing a chunk, and [`ClientError::OutOfMemory`] where
    /// the mesh does not fit.
    pub fn far(
        patch: &FarPatch,
        stride: u32,
        decals: &[Decal<'_>],
        fray: &Fray,
        warp: &Warp,
    ) -> Result<Self, ClientError> {
        let step = patch.step();
        if step == 0
            || !stride.is_multiple_of(step)
            || stride > CHUNK_CELLS
            || !CHUNK_CELLS.is_multiple_of(stride)
        {
            return Err(ClientError::Viewport);
        }
        let skip = stride / step;
        let side = CHUNK_CELLS / stride + 1;
        let vertex = |i: u32, j: u32| {
            let (pi, pj) = (i64::from(i * skip), i64::from(j * skip));
            let here = patch.surface_beside(pi, pj).unwrap_or(Elevation::SEA_LEVEL);
            let across = |oi: i64, oj: i64| patch.surface_beside(pi + oi, pj + oj).unwrap_or(here);
            Vertex {
                height: world_height(here),
                normal: normal(
                    [across(-1, 0), across(1, 0), across(0, -1), across(0, 1)],
                    signed(step),
                ),
                warp: [0, 0],
            }
        };
        let field = |i: u32, j: u32| {
            patch
                .ground(i * skip, j * skip)
                .map_or(WeightField::solid(Ground::Water), |blend| {
                    WeightField::from_blend(&blend)
                })
        };
        Self::build(
            MeshKey {
                coord: patch.coord(),
                source: Source::Far,
                stride,
            },
            side,
            vertex,
            field,
            decals,
            fray,
            warp,
        )
    }

    /// The mesh `key` names, `side` vertices along each edge, each `vertex`
    /// standing on ground `field` holds, warped by `warp`.
    pub(crate) fn build(
        key: MeshKey,
        side: u32,
        vertex: impl Fn(u32, u32) -> Vertex,
        field: impl Fn(u32, u32) -> WeightField,
        decals: &[Decal<'_>],
        fray: &Fray,
        warp: &Warp,
    ) -> Result<Self, ClientError> {
        let first = chunk_origin(key.coord)
            .centre()
            .ok_or(ClientError::Viewport)?;
        let spacing = signed(key.stride).saturating_mul(CELL_SUB_UNITS);
        let count = (side as usize) * (side as usize);
        let mut vertices = Vec::new();
        vertices
            .try_reserve(count)
            .map_err(|_| ClientError::OutOfMemory)?;
        let mut fields = Vec::new();
        fields
            .try_reserve(count)
            .map_err(|_| ClientError::OutOfMemory)?;
        let reach = {
            let far = spacing.saturating_mul(signed(side));
            Bounds {
                min_x: first.x,
                min_y: first.y,
                max_x: first.x.saturating_add(far),
                max_y: first.y.saturating_add(far),
            }
        };
        let mut touching: Vec<&Decal<'_>> = Vec::new();
        touching
            .try_reserve(decals.len())
            .map_err(|_| ClientError::OutOfMemory)?;
        touching.extend(
            decals
                .iter()
                .filter(|decal| decal.bounds().is_some_and(|b| b.overlaps(&reach))),
        );
        let (mut low, mut high) = (i32::MAX, i32::MIN);
        for j in 0..side {
            for i in 0..side {
                let at = WorldPoint {
                    x: first.x.saturating_add(signed(i).saturating_mul(spacing)),
                    y: first.y.saturating_add(signed(j).saturating_mul(spacing)),
                };
                let made = vertex(i, j);
                low = low.min(made.height);
                high = high.max(made.height);
                let (east, south) = warp.at(at);
                vertices.push(Vertex {
                    warp: [narrow(east), narrow(south)],
                    ..made
                });
                let mut weights = field(i, j);
                for decal in &touching {
                    decal.stamp(&mut weights, fray, at);
                }
                fields.push(weights);
            }
        }

        let quads = ((side - 1) as usize) * ((side - 1) as usize);
        let mut plans = Vec::new();
        plans
            .try_reserve(quads)
            .map_err(|_| ClientError::OutOfMemory)?;
        let mut grounds = BitSet256::EMPTY;
        let width = side as usize;
        for j in 0..width - 1 {
            for i in 0..width - 1 {
                let corner = |di: usize, dj: usize| &fields[(j + dj) * width + i + di];
                let plan = QuadPlan::new([corner(0, 0), corner(1, 0), corner(0, 1), corner(1, 1)]);
                for ground in plan.grounds() {
                    grounds.insert(u16::from(ground.id()));
                }
                plans.push(plan);
            }
        }
        Ok(Self {
            key,
            origin: first,
            spacing,
            side,
            vertices,
            plans,
            low: low.saturating_sub(spacing),
            high,
            grounds,
        })
    }

    /// Which mesh it is.
    #[must_use]
    pub const fn key(&self) -> MeshKey {
        self.key
    }

    /// The world position of vertex `(0, 0)`: the chunk's first cell centre.
    #[must_use]
    pub const fn origin(&self) -> WorldPoint {
        self.origin
    }

    /// World sub-units between vertices.
    #[must_use]
    pub const fn spacing(&self) -> i32 {
        self.spacing
    }

    /// How deep its skirt hangs below its edge: its own spacing, which meets
    /// a neighbour at another stride on ground no steeper than one in one.
    #[must_use]
    pub const fn skirt(&self) -> i32 {
        self.spacing
    }

    /// Vertices along each edge.
    #[must_use]
    pub const fn side(&self) -> u32 {
        self.side
    }

    /// Vertex `(i, j)`, or `None` past the grid.
    #[must_use]
    pub fn vertex(&self, i: u32, j: u32) -> Option<Vertex> {
        (i < self.side && j < self.side)
            .then(|| self.vertices[(j as usize) * (self.side as usize) + (i as usize)])
    }

    /// Every vertex, row by row.
    #[must_use]
    pub fn vertices(&self) -> &[Vertex] {
        &self.vertices
    }

    /// The plan of the quad whose north-west vertex is `(i, j)`.
    #[must_use]
    pub fn plan(&self, i: u32, j: u32) -> Option<&QuadPlan> {
        let quads = self.side.checked_sub(1)?;
        (i < quads && j < quads)
            .then(|| &self.plans[(j as usize) * (quads as usize) + (i as usize)])
    }

    /// The world position of vertex `(i, j)`'s foot.
    #[must_use]
    pub fn place(&self, i: u32, j: u32) -> WorldPoint {
        WorldPoint {
            x: self
                .origin
                .x
                .saturating_add(signed(i).saturating_mul(self.spacing)),
            y: self
                .origin
                .y
                .saturating_add(signed(j).saturating_mul(self.spacing)),
        }
    }

    /// The box it stands in, skirt included, in world sub-units: its west,
    /// north and lowest corner, then its east, south and highest.
    #[must_use]
    pub fn reach(&self) -> ([i32; 3], [i32; 3]) {
        let far = self.spacing.saturating_mul(signed(self.side - 1));
        (
            [self.origin.x, self.origin.y, self.low],
            [
                self.origin.x.saturating_add(far),
                self.origin.y.saturating_add(far),
                self.high,
            ],
        )
    }

    /// Every ground any of its quads draws.
    pub fn grounds(&self) -> impl Iterator<Item = Ground> + '_ {
        self.grounds
            .iter()
            .filter_map(|id| Ground::ALL.get(usize::from(id)).copied())
    }

    /// The surface's height under `at`, read off this mesh's own triangles,
    /// or `None` outside it.
    ///
    /// The triangles a quad is cut into are the ones the raster pass draws,
    /// so this is the height of the ground drawn there.
    #[must_use]
    pub fn height_at(&self, at: WorldPoint) -> Option<i32> {
        let spacing = i64::from(self.spacing);
        let quads = i64::from(self.side) - 1;
        let span = 0..=quads * spacing;
        let (x, y) = (
            i64::from(at.x) - i64::from(self.origin.x),
            i64::from(at.y) - i64::from(self.origin.y),
        );
        if quads < 1 || !span.contains(&x) || !span.contains(&y) {
            return None;
        }
        // A point on the far edge belongs to the last quad, not one past it.
        let (qi, qj) = ((x / spacing).min(quads - 1), (y / spacing).min(quads - 1));
        let (u, v) = (x - qi * spacing, y - qj * spacing);
        let corner = |di: i64, dj: i64| {
            let index = usize::try_from((qj + dj) * (quads + 1) + qi + di).ok()?;
            self.vertices
                .get(index)
                .map(|vertex| i64::from(vertex.height))
        };
        triangle_height(
            [corner(0, 0)?, corner(1, 0)?, corner(0, 1)?, corner(1, 1)?],
            (u, v),
            spacing,
        )
    }
}

/// A surface height as world sub-units.
fn world_height(elevation: Elevation) -> i32 {
    i32::from(elevation.0) * WORLD_PER_ELEVATION
}

/// The unit normal of a surface whose heights west, east, north and south of
/// a point are `around`, `cells` cells either side, as its east and south
/// components out of [`NORMAL_ONE`].
fn normal(around: [Elevation; 4], cells: i32) -> [i8; 2] {
    let [west, east, north, south] = around.map(|at| i64::from(at.0));
    // Rise against run, both in elevation sub-units.
    let run = 2 * i64::from(cells.max(1)) * i64::from(ELEVATION_SUB_UNITS);
    let (x, y) = (west - east, north - south);
    let length = (x * x + y * y + run * run).isqrt().max(1);
    let component = |value: i64| {
        let scaled = value * i64::from(NORMAL_ONE) / length;
        i8::try_from(scaled.clamp(-i64::from(NORMAL_ONE), i64::from(NORMAL_ONE))).unwrap_or(0)
    };
    [component(x), component(y)]
}

/// A warp displacement, which never exceeds the warp's amplitude, as the
/// vertex holds it.
fn narrow(displacement: i32) -> i16 {
    i16::try_from(displacement.clamp(i32::from(i16::MIN), i32::from(i16::MAX))).unwrap_or(0)
}

/// A grid index as the signed offset it is.
fn signed(index: u32) -> i32 {
    i32::try_from(index).unwrap_or(i32::MAX)
}

/// The surface a frame draws, as figures stand on it: each point's height
/// read off the mesh drawn over it, so a foot meets the triangles it is drawn
/// on whatever their stride.
#[derive(Debug, Default)]
pub struct Surfaces<'a> {
    /// The drawn meshes in key order, so a chunk's solved mesh is found
    /// before its far one.
    meshes: Vec<&'a TerrainMesh>,
}

impl<'a> Surfaces<'a> {
    /// The surface `meshes` draw.
    ///
    /// # Errors
    ///
    /// [`ClientError::OutOfMemory`] where the index does not fit.
    pub fn new(meshes: &[&'a TerrainMesh]) -> Result<Self, ClientError> {
        let mut sorted = Vec::new();
        sorted
            .try_reserve(meshes.len())
            .map_err(|_| ClientError::OutOfMemory)?;
        sorted.extend_from_slice(meshes);
        sorted.sort_unstable_by_key(|mesh| mesh.key);
        Ok(Self { meshes: sorted })
    }
}

impl Relief for Surfaces<'_> {
    fn height(&self, at: WorldPoint) -> Option<i32> {
        // A mesh's vertices sit on cell centres, so the chunk a point's mesh
        // covers is the one half a cell north-west of it falls in.
        let half = CELL_SUB_UNITS / 2;
        let coord = chunk_at(WorldPoint {
            x: at.x.saturating_sub(half),
            y: at.y.saturating_sub(half),
        });
        let first = self.meshes.partition_point(|mesh| mesh.key.coord < coord);
        self.meshes[first..]
            .iter()
            .take_while(|mesh| mesh.key.coord == coord)
            .find_map(|mesh| mesh.height_at(at))
    }
}

/// The meshes a frame draws, kept between frames so a steady view builds
/// none.
#[derive(Debug, Default)]
pub struct MeshCache {
    meshes: Vec<(TerrainMesh, bool)>,
}

impl MeshCache {
    /// Nothing built.
    #[must_use]
    pub const fn new() -> Self {
        Self { meshes: Vec::new() }
    }

    /// Start a frame: nothing is in use until it is asked for.
    pub fn begin(&mut self) {
        for (_, used) in &mut self.meshes {
            *used = false;
        }
    }

    /// Mark the mesh `key` names in use, answering whether it is built.
    pub fn take(&mut self, key: MeshKey) -> bool {
        let Ok(at) = self.position(key) else {
            return false;
        };
        self.meshes[at].1 = true;
        true
    }

    /// The built mesh of `wanted`'s chunk to draw while `wanted` is not yet
    /// built, marked in use: one from its own source before the other's, the
    /// fewest halvings or doublings from its stride, the finer of two as near.
    pub fn stand_in(&mut self, wanted: MeshKey) -> Option<MeshKey> {
        let level = wanted.stride.max(1).ilog2();
        let first = self
            .meshes
            .partition_point(|(mesh, _)| mesh.key.coord < wanted.coord);
        let (mesh, used) = self
            .meshes
            .get_mut(first..)?
            .iter_mut()
            .take_while(|(mesh, _)| mesh.key.coord == wanted.coord)
            .min_by_key(|(mesh, _)| {
                (
                    mesh.key.source != wanted.source,
                    mesh.key.stride.max(1).ilog2().abs_diff(level),
                )
            })?;
        *used = true;
        Some(mesh.key)
    }

    /// Hold `mesh`, marked in use.
    ///
    /// # Errors
    ///
    /// [`ClientError::OutOfMemory`] where there is no room for it.
    pub fn adopt(&mut self, mesh: TerrainMesh) -> Result<(), ClientError> {
        match self.position(mesh.key) {
            Ok(at) => self.meshes[at] = (mesh, true),
            Err(at) => {
                self.meshes
                    .try_reserve(1)
                    .map_err(|_| ClientError::OutOfMemory)?;
                self.meshes.insert(at, (mesh, true));
            }
        }
        Ok(())
    }

    /// Give back every mesh the frame did not draw.
    pub fn end(&mut self) {
        self.meshes.retain(|(_, used)| *used);
    }

    /// The mesh `key` names, if it is built.
    #[must_use]
    pub fn get(&self, key: MeshKey) -> Option<&TerrainMesh> {
        self.position(key).ok().map(|at| &self.meshes[at].0)
    }

    /// How many meshes are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.meshes.len()
    }

    /// Whether none is.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.meshes.is_empty()
    }

    fn position(&self, key: MeshKey) -> Result<usize, usize> {
        self.meshes.binary_search_by(|(mesh, _)| mesh.key.cmp(&key))
    }
}

#[cfg(test)]
#[path = "mesh_tests.rs"]
mod tests;
