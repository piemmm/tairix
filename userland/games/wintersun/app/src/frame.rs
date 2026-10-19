//! One frame: described once, then drawn.
//!
//! A frame is described from the scene — the meshes it draws, the tiles its
//! grounds are read from, the figures standing in it, the camera and the
//! sun — and the description is all the passes read. Drawing it runs the
//! stages a GPU runs: the vertex stage and the scan conversion into the
//! geometry buffer, the light composite over it, then the figures, each
//! hidden wherever the ground stands nearer the eye.
//!
//! # Ask, then paint
//!
//! Everything a band needs is resolved before any band starts: the meshes
//! are built from ground already held, the material tiles are made resident
//! by the one mutable pass over the cache, and every figure is posed and
//! placed. The bands then only read, which is what lets them run on other
//! cores at all, and it is the same discipline that keeps a frame off the
//! filesystem: a paint reads nothing it has to wait for.
//!
//! # What a band is
//!
//! A full-width run of the target's rows. Every pass steps horizontally, so
//! a vertical cut would divide the row each is built around; the bands are
//! cut finer than the runner is wide so a core taken by another tenant holds
//! up one small piece rather than a whole share of the frame.

use alloc::vec::Vec;

use tairix_inline::bitset::BitSet256;
use tairix_parallel::{for_each, JobRunner};
use tairix_raster::surface::{RowBand, Surface};
use tairix_raster::view::Perspective;
use tairix_util::mathf;
use tairix_wintersun_art::cache::{MaterialCache, TileKey};
use tairix_wintersun_art::decal::{Decal, Fray};
use tairix_wintersun_art::material::{self, Mip, MIP_LEVELS};
use tairix_wintersun_art::splat::{Materials, Warp};
use tairix_wintersun_figure::paint::Brush;
use tairix_wintersun_net::value::{ChunkCoord, WorldPoint};
use tairix_wintersun_world::blend::Kind;
use tairix_wintersun_world::ground::Ground;

use crate::budget::{FrameTimes, Pass};
use crate::camera::{focal, Camera};
use crate::error::ClientError;
use crate::figures::{Cast, Framing, Stage};
use crate::light::{Atmosphere, Sky, Sun};
use crate::mesh::{MeshCache, MeshKey, Source, Surfaces, TerrainMesh};
use crate::quality::Detail;
use crate::raster::{self, Carried, Drawing, GBuffer};
use crate::terrain::{chunk_box, distance_squared, stride_for, HeldGround, VIEW_RANGE};
use crate::view::Viewport;

/// Where the renderer reads the time from.
///
/// Injected because the crate is `no_std` and asks the kernel nothing,
/// and because the per-pass measurement is the milestone's own exit
/// criterion: a host test drives it with a clock it controls, and the
/// real client passes the monotonic one.
pub trait Clock {
    /// Monotonic nanoseconds. Only differences are read.
    fn now_ns(&self) -> u64;
}

/// A clock that never advances.
///
/// For a caller that wants a frame and not a measurement — the digest
/// vertical, most of all, whose answer must not depend on how long the
/// guest took to produce it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Stopped;

impl Clock for Stopped {
    fn now_ns(&self) -> u64 {
        0
    }
}

/// Everything outside the renderer that one frame depends on.
#[derive(Copy, Clone)]
pub struct Scene<'a> {
    /// Where the player is looking from.
    pub camera: Camera,
    /// The ground the client holds.
    pub ground: &'a HeldGround,
    /// The roads stamped into the ground.
    pub decals: &'a [Decal<'a>],
    /// The realm's frayed decal edges.
    pub fray: &'a Fray,
    /// The realm's material warp.
    pub warp: &'a Warp,
    /// The light.
    pub sun: Sun,
    /// What the ground sits under.
    pub sky: Sky,
    /// The detail the frame is drawn at.
    pub detail: Detail,
    /// Everyone standing in it.
    pub cast: &'a Cast<'a>,
}

/// How many meshes one frame builds at most: a frame pays for the ground it
/// has just been handed over a few frames rather than all at once, drawing a
/// chunk at a stride it already holds until its own is built.
pub const MESH_BUILDS: usize = 8;

/// One frame, described: everything its passes draw, and nothing they read
/// besides.
pub struct Frame<'a> {
    camera: Perspective,
    size: (u32, u32),
    rows: u32,
    meshes: Vec<&'a TerrainMesh>,
    /// Which edges of each mesh hang a skirt.
    skirts: &'a [[bool; 4]],
    materials: Materials<'a>,
    atmosphere: &'a Atmosphere,
    stage: &'a Stage,
    times: FrameTimes,
}

impl Frame<'_> {
    /// The view it is drawn through.
    #[must_use]
    pub const fn camera(&self) -> &Perspective {
        &self.camera
    }

    /// How many meshes it draws.
    #[must_use]
    pub fn meshes(&self) -> usize {
        self.meshes.len()
    }
}

/// What a frame is drawn through: the geometry buffer, each mesh's carried
/// vertices and each band's brush, held across frames so a frame drawn at
/// the size of the last makes none of them again.
#[derive(Debug, Default)]
pub struct Scratch {
    buffer: GBuffer,
    carried: Vec<Carried>,
    brushes: Vec<Brush>,
}

impl Scratch {
    /// Nothing allocated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The geometry buffer the last frame drew.
    #[must_use]
    pub const fn buffer(&self) -> &GBuffer {
        &self.buffer
    }
}

/// What describing a frame keeps between frames.
#[derive(Debug, Default)]
struct Describer {
    meshes: MeshCache,
    wanted: Vec<(u64, MeshKey)>,
    /// The meshes drawn, in coordinate order, and the skirts each hangs.
    drawn_keys: Vec<MeshKey>,
    skirts: Vec<[bool; 4]>,
    stage: Stage,
    atmosphere: Option<Atmosphere>,
    refused_tiles: usize,
    drawn: usize,
    /// Meshes the last frame wanted and drew at another stride or not at
    /// all, waiting to be built.
    missing: usize,
}

/// The reusable state one client's frames are described and drawn through.
#[derive(Debug, Default)]
pub struct Renderer {
    describer: Describer,
    scratch: Scratch,
}

impl Renderer {
    /// A renderer with nothing yet allocated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many figures the last frame drew.
    #[must_use]
    pub fn figures(&self) -> usize {
        self.describer.stage.placed()
    }

    /// How many meshes the last frame drew.
    #[must_use]
    pub const fn drawn(&self) -> usize {
        self.describer.drawn
    }

    /// Whether the last frame drew every mesh its view wanted, each at its
    /// own stride.
    #[must_use]
    pub const fn complete(&self) -> bool {
        self.describer.missing == 0
    }

    /// How many materials the last frame drew in a coarser tile or their
    /// flat tone because the cache would not admit a tile.
    #[must_use]
    pub const fn refused_tiles(&self) -> usize {
        self.describer.refused_tiles
    }

    /// What the last frame was drawn through.
    #[must_use]
    pub const fn scratch(&self) -> &Scratch {
        &self.scratch
    }

    /// Describe and draw one frame of `scene` into `target`.
    ///
    /// # Errors
    ///
    /// As [`Self::describe`] and [`execute`].
    pub fn render(
        &mut self,
        target: &mut Surface,
        view: &Viewport,
        scene: &Scene<'_>,
        cache: &mut MaterialCache,
        runner: &dyn JobRunner,
        clock: &dyn Clock,
    ) -> Result<FrameTimes, ClientError> {
        let frame = self
            .describer
            .describe(view, scene, cache, runner, clock, MESH_BUILDS)?;
        execute(&frame, &mut self.scratch, target, runner, clock)
    }

    /// Build every mesh the view of `scene` wants at its own stride, then
    /// describe and draw it once: a picture drawn once, rather than the first
    /// of a live view's frames, which builds its meshes over several.
    ///
    /// # Errors
    ///
    /// As [`Self::render`], and [`ClientError::OutOfMemory`] where the
    /// meshes do not all fit.
    pub fn render_whole(
        &mut self,
        target: &mut Surface,
        view: &Viewport,
        scene: &Scene<'_>,
        cache: &mut MaterialCache,
        runner: &dyn JobRunner,
        clock: &dyn Clock,
    ) -> Result<FrameTimes, ClientError> {
        self.describer
            .describe(view, scene, cache, runner, clock, usize::MAX)?;
        if !self.complete() {
            return Err(ClientError::OutOfMemory);
        }
        self.render(target, view, scene, cache, runner, clock)
    }

    /// Describe one frame of `scene` as `view` sees it.
    ///
    /// # Errors
    ///
    /// [`ClientError::Viewport`] for a view no camera projects onto,
    /// [`ClientError::OutOfMemory`] when a mesh or a list the frame needs does
    /// not fit, and [`ClientError::Figure`] for a sun no figure can be lit
    /// by.
    pub fn describe<'r>(
        &'r mut self,
        view: &Viewport,
        scene: &Scene<'r>,
        cache: &'r mut MaterialCache,
        runner: &dyn JobRunner,
        clock: &dyn Clock,
    ) -> Result<Frame<'r>, ClientError> {
        self.describer
            .describe(view, scene, cache, runner, clock, MESH_BUILDS)
    }
}

impl Describer {
    /// Describe one frame of `scene` as `view` sees it, building up to
    /// `builds` of the meshes it wants that are not built.
    fn describe<'r>(
        &'r mut self,
        view: &Viewport,
        scene: &Scene<'r>,
        cache: &'r mut MaterialCache,
        runner: &dyn JobRunner,
        clock: &dyn Clock,
        builds: usize,
    ) -> Result<Frame<'r>, ClientError> {
        let mut times = FrameTimes::new();
        let started = clock.now_ns();
        let ground = scene.ground;
        let floor = ground.height_at(scene.camera.focus()).unwrap_or(0);
        let camera = scene
            .camera
            .perspective(view, floor, |at| ground.height_at(at))?;
        let focal = focal(view);

        self.want(&camera, ground, focal, floor)?;
        let (drawing, building) = self.gather(scene, clock, builds)?;
        let meshes = drawn(
            &self.meshes,
            &drawing,
            &mut self.drawn_keys,
            &mut self.skirts,
        )?;
        self.drawn = meshes.len();
        let (materials, refused, ensuring) = resident_tiles(
            &meshes,
            camera.eye(),
            focal,
            scene.detail.ground,
            cache,
            clock,
        );
        self.refused_tiles = refused;
        let resident = building.saturating_add(ensuring);
        times.record_warm(resident);
        times.record(
            Pass::Terrain,
            clock
                .now_ns()
                .saturating_sub(started)
                .saturating_sub(resident),
        );

        let lighting = clock.now_ns();
        let (width, height) = view.render();
        let atmosphere = &*self.atmosphere.insert(Atmosphere::new(
            &scene.sun,
            scene.sky,
            &camera,
            height,
            scene.detail.shadows.lit(),
        )?);
        times.record(Pass::Light, clock.now_ns().saturating_sub(lighting));

        let placing = clock.now_ns();
        let surfaces = Surfaces::new(&meshes)?;
        let framing = Framing {
            camera: &camera,
            light: scene.sun.light()?,
            shade: scene.detail.shadows.shade(),
        };
        self.stage
            .place(scene.cast, &framing, &surfaces, atmosphere, runner)?;
        times.record(Pass::Scenery, clock.now_ns().saturating_sub(placing));

        Ok(Frame {
            camera,
            size: (width, height),
            rows: view.band_rows(runner),
            meshes,
            skirts: &self.skirts,
            materials,
            atmosphere,
            stage: &self.stage,
            times,
        })
    }

    /// Gather the meshes `camera`, `focal` render pixels a unit from the
    /// eye, wants of `ground`, nearest first: every held chunk and far patch
    /// it can see, at the stride its distance asks for, a far patch only where
    /// no solved chunk covers its ground. The ground is taken to stand at
    /// `floor`, the focus's height, for how far the eye stands above it.
    fn want(
        &mut self,
        camera: &Perspective,
        ground: &HeldGround,
        focal: u32,
        floor: i32,
    ) -> Result<(), ClientError> {
        self.wanted.clear();
        let wanted = ground.solved().len() + ground.far().len();
        self.wanted
            .try_reserve(wanted)
            .map_err(|_| ClientError::OutOfMemory)?;
        let eye = camera.eye();
        let above = (i64::from(mathf::round_i32(eye[2])) - i64::from(floor)).unsigned_abs();
        let solved = ground
            .solved()
            .iter()
            .map(|chunk| (chunk.coord(), Source::Solved, 1));
        let far = ground
            .far()
            .iter()
            .filter(|patch| !ground.holds(patch.coord()))
            .map(|patch| (patch.coord(), Source::Far, patch.step()));
        for (coord, source, finest) in solved.chain(far) {
            let (low, high) = chunk_box(coord);
            if !camera.sees(low, high, f64::from(VIEW_RANGE)) {
                continue;
            }
            let distance = box_squared(coord, eye)
                .saturating_add(above.saturating_mul(above))
                .isqrt();
            let stride = stride_for(i64::try_from(distance).unwrap_or(i64::MAX), focal).max(finest);
            self.wanted.push((
                distance,
                MeshKey {
                    coord,
                    source,
                    stride,
                },
            ));
        }
        self.wanted.sort_unstable();
        Ok(())
    }
}

impl Describer {
    /// Take every mesh the view wants that is built, build up to `builds` of
    /// those that are not, and stand a built stride in for the rest: the keys
    /// drawn, nearest first, and the nanoseconds the builds took.
    fn gather(
        &mut self,
        scene: &Scene<'_>,
        clock: &dyn Clock,
        builds: usize,
    ) -> Result<(Vec<MeshKey>, u64), ClientError> {
        self.meshes.begin();
        let mut drawing = Vec::new();
        drawing
            .try_reserve(self.wanted.len())
            .map_err(|_| ClientError::OutOfMemory)?;
        let mut builds = builds;
        let mut building = 0u64;
        self.missing = 0;
        for &(_, key) in &self.wanted {
            if self.meshes.take(key) {
                drawing.push(key);
                continue;
            }
            if builds > 0 {
                builds -= 1;
                let started = clock.now_ns();
                let built = build(scene.ground, key, scene)?;
                building = building.saturating_add(clock.now_ns().saturating_sub(started));
                if let Some(mesh) = built {
                    self.meshes.adopt(mesh)?;
                    drawing.push(key);
                    continue;
                }
            }
            self.missing += 1;
            if let Some(stand_in) = self.meshes.stand_in(key) {
                drawing.push(stand_in);
            }
        }
        self.meshes.end();
        Ok((drawing, building))
    }
}

/// The meshes `drawing` names, as `meshes` holds them, each with the skirts
/// it hangs among them, `keys` and `skirts` refilled as the scratch that
/// takes.
fn drawn<'m>(
    meshes: &'m MeshCache,
    drawing: &[MeshKey],
    keys: &mut Vec<MeshKey>,
    skirts: &mut Vec<[bool; 4]>,
) -> Result<Vec<&'m TerrainMesh>, ClientError> {
    keys.clear();
    keys.try_reserve(drawing.len())
        .map_err(|_| ClientError::OutOfMemory)?;
    keys.extend_from_slice(drawing);
    keys.sort_unstable_by_key(|key| key.coord);
    skirts.clear();
    skirts
        .try_reserve(drawing.len())
        .map_err(|_| ClientError::OutOfMemory)?;
    let mut held = Vec::new();
    held.try_reserve(drawing.len())
        .map_err(|_| ClientError::OutOfMemory)?;
    for key in drawing {
        if let Some(mesh) = meshes.get(*key) {
            held.push(mesh);
            skirts.push(self::skirts(*key, keys));
        }
    }
    Ok(held)
}

/// Make resident, at `quality`, every level of every ground `meshes` draw
/// from the finest a pixel of the nearest of them reads on, seen from `eye`
/// `focal` pixels a unit away: the materials the frame reads, how many grounds
/// the cache would not hold whole, and the nanoseconds making them took.
fn resident_tiles<'c>(
    meshes: &[&TerrainMesh],
    eye: [f64; 3],
    focal: u32,
    quality: material::Quality,
    cache: &'c mut MaterialCache,
    clock: &dyn Clock,
) -> (Materials<'c>, usize, u64) {
    let mut grounds = BitSet256::EMPTY;
    let mut finest = [MIP_LEVELS; Ground::ALL.len()];
    for mesh in meshes {
        let nearest = nearest_squared(mesh, eye).isqrt();
        let footprint = u32::try_from(nearest / u64::from(focal)).unwrap_or(u32::MAX);
        for ground in mesh.grounds() {
            grounds.insert(u16::from(ground.id()));
            let level = Mip::at(material::params(ground).grain_shift, footprint, 0).level();
            let slot = &mut finest[usize::from(ground.id())];
            *slot = (*slot).min(level);
        }
    }
    let wanted = || {
        grounds
            .iter()
            .filter_map(|id| Ground::ALL.get(usize::from(id)).copied())
    };
    let levels =
        |ground: Ground| (finest[usize::from(ground.id())]..MIP_LEVELS).filter_map(Mip::new);
    let started = clock.now_ns();
    for ground in wanted() {
        for mip in levels(ground) {
            cache.ensure(quality, TileKey { ground, mip });
        }
    }
    let ensuring = clock.now_ns().saturating_sub(started);
    // Asked for, then counted: a tile the cache admitted can still have
    // given way to one asked for after it.
    let cache: &'c MaterialCache = cache;
    let refused = wanted()
        .filter(|ground| {
            levels(*ground).any(|mip| {
                cache
                    .peek(
                        quality,
                        &TileKey {
                            ground: *ground,
                            mip,
                        },
                    )
                    .is_none()
            })
        })
        .count();
    (
        Materials::resolve(cache, quality, wanted()),
        refused,
        ensuring,
    )
}

/// Build the mesh `key` names from `ground`, or `None` where the ground it
/// names is not held.
fn build(
    ground: &HeldGround,
    key: MeshKey,
    scene: &Scene<'_>,
) -> Result<Option<TerrainMesh>, ClientError> {
    match key.source {
        Source::Solved => {
            let Some(chunk) = ground.chunk(key.coord) else {
                return Ok(None);
            };
            TerrainMesh::solved(chunk, key.stride, scene.decals, scene.fray, scene.warp).map(Some)
        }
        Source::Far => {
            let Some(patch) = ground.patch(key.coord) else {
                return Ok(None);
            };
            TerrainMesh::far(patch, key.stride, scene.decals, scene.fray, scene.warp).map(Some)
        }
    }
}

/// Which edges of the mesh `key` names — north, south, west and east — hang a
/// skirt among the meshes `drawn`, in coordinate order: every edge but one a
/// neighbour drawn from the same source at the same stride meets, whose edge
/// vertices are its own, so no crack can open there.
fn skirts(key: MeshKey, drawn: &[MeshKey]) -> [bool; 4] {
    let ChunkCoord { x, y } = key.coord;
    [
        y.checked_sub(1).map(|north| ChunkCoord { x, y: north }),
        y.checked_add(1).map(|south| ChunkCoord { x, y: south }),
        x.checked_sub(1).map(|west| ChunkCoord { x: west, y }),
        x.checked_add(1).map(|east| ChunkCoord { x: east, y }),
    ]
    .map(|across| {
        across
            .and_then(|coord| drawn.binary_search_by_key(&coord, |other| other.coord).ok())
            .is_none_or(|at| drawn[at].source != key.source || drawn[at].stride != key.stride)
    })
}

/// How far across the ground, squared, the eye at `eye` stands from chunk
/// `coord`'s square.
fn box_squared(coord: ChunkCoord, eye: [f64; 3]) -> u64 {
    let at = WorldPoint {
        x: mathf::round_i32(eye[0]),
        y: mathf::round_i32(eye[1]),
    };
    u64::try_from(distance_squared(coord, at)).unwrap_or(u64::MAX)
}

/// How far, squared, the eye at `eye` stands from the nearest point of
/// `mesh`'s box.
fn nearest_squared(mesh: &TerrainMesh, eye: [f64; 3]) -> u64 {
    let (low, high) = mesh.reach();
    let at = [
        mathf::round_i32(eye[0]),
        mathf::round_i32(eye[1]),
        mathf::round_i32(eye[2]),
    ];
    let mut total = 0u64;
    for axis in 0..3 {
        let gap = (i64::from(low[axis]) - i64::from(at[axis]))
            .max(i64::from(at[axis]) - i64::from(high[axis]))
            .max(0)
            .unsigned_abs();
        total = total.saturating_add(gap.saturating_mul(gap));
    }
    total
}

/// Draw `frame` into `target` through `scratch`.
///
/// Reads nothing but the frame: drawing one description twice gives the same
/// picture.
///
/// # Errors
///
/// [`ClientError::Viewport`] when `target` is not the frame's render extent
/// or a clip window cuts it, and [`ClientError::OutOfMemory`] when a buffer
/// the frame needs does not fit.
pub fn execute(
    frame: &Frame<'_>,
    scratch: &mut Scratch,
    target: &mut Surface,
    runner: &dyn JobRunner,
    clock: &dyn Clock,
) -> Result<FrameTimes, ClientError> {
    let (width, height) = frame.size;
    if !whole(target, width, height) {
        return Err(ClientError::Viewport);
    }
    let mut times = frame.times;

    let started = clock.now_ns();
    scratch.buffer.reset(width, height)?;
    let count = frame.meshes.len();
    if scratch.carried.len() < count {
        scratch
            .carried
            .try_reserve(count - scratch.carried.len())
            .map_err(|_| ClientError::OutOfMemory)?;
        scratch.carried.resize_with(count, Carried::default);
    }
    let mut carrying: Vec<(&TerrainMesh, &mut Carried, [bool; 4], bool)> = Vec::new();
    carrying
        .try_reserve(count)
        .map_err(|_| ClientError::OutOfMemory)?;
    for ((mesh, carried), &skirts) in frame
        .meshes
        .iter()
        .zip(scratch.carried.iter_mut())
        .zip(frame.skirts)
    {
        carrying.push((mesh, carried, skirts, true));
    }
    let camera = &frame.camera;
    for_each(runner, &mut carrying, &|(mesh, carried, skirts, fits)| {
        *fits = carried.carry(mesh, *skirts, camera).is_ok();
    });
    if carrying.iter().any(|(_, _, _, fits)| !fits) {
        return Err(ClientError::OutOfMemory);
    }
    drop(carrying);
    let mut drawings = Vec::new();
    drawings
        .try_reserve(count)
        .map_err(|_| ClientError::OutOfMemory)?;
    for ((mesh, carried), skirts) in frame
        .meshes
        .iter()
        .zip(scratch.carried.iter())
        .zip(frame.skirts)
    {
        drawings.push(Drawing {
            mesh,
            carried,
            skirts: *skirts,
        });
    }
    let pass = raster::Pass {
        camera,
        materials: &frame.materials,
    };
    raster::draw(&mut scratch.buffer, &drawings, &pass, frame.rows, runner)?;
    times.record(Pass::Terrain, clock.now_ns().saturating_sub(started));

    let lit = clock.now_ns();
    ensure_brushes(&mut scratch.brushes, height.div_ceil(frame.rows.max(1)))?;
    let (buffer, brushes) = (&scratch.buffer, &mut scratch.brushes);
    let atmosphere = frame.atmosphere;
    dispatch(target, brushes, frame.rows, runner, &|band, _| {
        atmosphere.composite(buffer, band);
    })?;
    times.record(Pass::Light, clock.now_ns().saturating_sub(lit));

    let painting = clock.now_ns();
    if frame.stage.placed() > 0 {
        let stage = frame.stage;
        let depth = buffer.depth();
        dispatch(target, brushes, frame.rows, runner, &|band, brush| {
            stage.paint(band, depth, brush);
        })?;
    }
    times.record(Pass::Scenery, clock.now_ns().saturating_sub(painting));
    Ok(times)
}

/// Whether `target` admits every pixel of a `width` × `height` frame: it is
/// that size, and no clip window or origin cuts any of it.
///
/// Every pass paints whole rows from the target's left edge, so a target it
/// could reach only part of is refused rather than drawn wrongly.
fn whole(target: &mut Surface, width: u32, height: u32) -> bool {
    if target.width() != width || target.height() != height {
        return false;
    }
    let Some(mut band) = target.row_bands_mut(0..height, height).next() else {
        return false;
    };
    band.rows() == (0..height)
        && band
            .row_span_mut(0, 0, width)
            .is_some_and(|(first, span)| first == 0 && span.len() == width as usize)
}

/// Make sure there is a brush for each of `bands` bands.
fn ensure_brushes(brushes: &mut Vec<Brush>, bands: u32) -> Result<(), ClientError> {
    let bands = usize::try_from(bands).map_err(|_| ClientError::OutOfMemory)?;
    if brushes.len() < bands {
        brushes
            .try_reserve(bands - brushes.len())
            .map_err(|_| ClientError::OutOfMemory)?;
        brushes.resize_with(bands, Brush::default);
    }
    Ok(())
}

/// Cut `target` into bands of `rows` rows and run `visit` over them, each
/// with its own brush.
fn dispatch(
    target: &mut Surface,
    brushes: &mut [Brush],
    rows: u32,
    runner: &dyn JobRunner,
    visit: &(dyn Fn(&mut RowBand<'_>, &mut Brush) + Sync),
) -> Result<(), ClientError> {
    let bands = target.row_bands_mut(0..target.height(), rows);
    let mut work: Vec<(RowBand<'_>, &mut Brush)> = Vec::new();
    work.try_reserve(bands.len())
        .map_err(|_| ClientError::OutOfMemory)?;
    for (band, brush) in bands.zip(brushes.iter_mut()) {
        work.push((band, brush));
    }
    for_each(runner, &mut work, &|(band, brush)| visit(band, brush));
    Ok(())
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod tests;
