//! The ground a client holds, and which of it a view wants.
//!
//! Two kinds of ground: chunks the generator solved in full, close about the
//! focus, and far patches — level 0's reading of a chunk — out to where the
//! fog closes the view. Solving a chunk is a large part of a second of work
//! and a far patch a small part of a millisecond, so a long view across a
//! valley is drawn from patches while the chunks about the player arrive.
//!
//! # Nothing here reads the world
//!
//! The frame loop asks a worker for ground and adopts what it answers; a
//! frame draws only what is held. Ground not yet held is not drawn — the sky
//! shows where it would stand — rather than fetched by the frame that wants
//! it, which would be the frame-loop read the charter forbids.

use alloc::vec::Vec;

use tairix_inline::ArrayVec;
use tairix_parallel::{JobRunner, SERIAL};
use tairix_raster::view::Perspective;
use tairix_util::defer::JobDesk;
use tairix_util::mathf;
use tairix_wintersun_art::decal::Decal;
use tairix_wintersun_net::value::{ChunkCoord, WorldPoint};
use tairix_wintersun_world::chunk::{Chunk, ChunkBuild};
use tairix_wintersun_world::error::WorldError;
use tairix_wintersun_world::far::FarPatch;
use tairix_wintersun_world::geom::{CELL_SUB_UNITS, CHUNK_CELLS, CHUNK_CELLS_LOG2};
use tairix_wintersun_world::ground::Ground;
use tairix_wintersun_world::land::{Keeping, RealmLand, Store};
use tairix_wintersun_world::params::RealmParams;
use tairix_wintersun_world::realm::RealmField;

use crate::camera::Camera;
use crate::error::ClientError;
use crate::mesh::WORLD_PER_ELEVATION;
use crate::view::Viewport;

/// World sub-units along a chunk's edge.
pub const CHUNK_SUB_UNITS: i32 = CELL_SUB_UNITS << CHUNK_CELLS_LOG2;

/// How many chunks either side of the focus's own a client solves in full:
/// the ground a figure stands and walks on near the player.
pub const SOLVE_RADIUS: i32 = 2;

/// How far from the eye ground is drawn, in world sub-units: where the fog
/// has all but closed the view (`light`'s fog depth).
pub const VIEW_RANGE: i32 = 3200 * CELL_SUB_UNITS;

/// How many far patches one ask carries: each is a fraction of a
/// millisecond, so the worker's round trip is paid once for several.
pub const FAR_BATCH: usize = 16;

/// Half the carriageway a road covers completely, in world sub-units.
///
/// The same 1.6 cells the generator levelled the ground over, so the
/// worn surface and the levelled ground are the same road rather than
/// two that nearly coincide.
const ROAD_HALF_WIDTH: u32 = (CELL_SUB_UNITS as u32) * 8 / 5;

/// How far a road's surface frays out into what it crosses.
const ROAD_FEATHER: u32 = CELL_SUB_UNITS as u32;

/// How much of the ground a road takes at its centreline, out of the
/// weight field's total.
///
/// Not all of it: letting a little of what it crosses through is what
/// makes a track over heath look unlike the same track over sand.
const ROAD_COVERAGE: u16 = 220;

/// A realm's roads as paths the decal stamp can read.
///
/// Converted once per realm rather than per frame: the generator answers
/// in cells and a stamp works in sub-units, and a road does not move.
#[derive(Debug, Default)]
pub struct RoadDecals {
    paths: Vec<Vec<WorldPoint>>,
}

impl RoadDecals {
    /// Convert every road the realm routed.
    ///
    /// # Errors
    ///
    /// [`ClientError::OutOfMemory`] if the paths do not fit.
    pub fn from_realm(field: &RealmField) -> Result<Self, ClientError> {
        let mut paths = Vec::new();
        paths
            .try_reserve(field.roads().len())
            .map_err(|_| ClientError::OutOfMemory)?;
        for road in field.roads() {
            let mut path = Vec::new();
            path.try_reserve(road.path.len())
                .map_err(|_| ClientError::OutOfMemory)?;
            path.extend(road.path.iter().filter_map(|cell| cell.centre()));
            if path.len() >= 2 {
                paths.push(path);
            }
        }
        Ok(Self { paths })
    }

    /// The decal for each road, borrowing the paths.
    ///
    /// # Errors
    ///
    /// [`ClientError::OutOfMemory`] if the list does not fit.
    pub fn decals(&self) -> Result<Vec<Decal<'_>>, ClientError> {
        let mut out = Vec::new();
        out.try_reserve(self.paths.len())
            .map_err(|_| ClientError::OutOfMemory)?;
        out.extend(self.paths.iter().map(|path| Decal {
            ground: Ground::Gravel,
            path,
            half_width: ROAD_HALF_WIDTH,
            feather: ROAD_FEATHER,
            coverage: ROAD_COVERAGE,
        }));
        Ok(out)
    }

    /// How many roads were converted.
    #[must_use]
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Whether the realm routed no roads at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// What the frame loop asks the ground worker for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Ask {
    /// Solve this chunk in full.
    Solve(ChunkCoord),
    /// Read level 0 over these chunks.
    Far(ArrayVec<ChunkCoord, FAR_BATCH>),
}

/// The hand-off between the frame loop and the worker that makes ground.
///
/// No answer is ever superseded — another chunk is not a newer version of
/// it — yet [`JobDesk::deliver`] drops an answer whenever an ask is waiting,
/// a second answer replaces one not yet collected, and the loop asks every
/// frame. So an ask is declined while one is in flight or its answer waits,
/// which is what gets every answer to the loop.
pub struct ChunkDesk<A> {
    desk: JobDesk<Ask, A>,
    landed: bool,
}

impl<A> Default for ChunkDesk<A> {
    fn default() -> Self {
        Self::new()
    }
}

impl<A> ChunkDesk<A> {
    /// Nothing asked for, being made, or made.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            desk: JobDesk::new(),
            landed: false,
        }
    }

    /// Whether an ask would be taken: none is in flight, and no answer waits
    /// to be collected.
    #[must_use]
    pub const fn takes_asks(&self) -> bool {
        !self.desk.in_flight() && !self.landed
    }

    /// Ask for `ask`, replacing an ask the worker has not taken; declined
    /// while one is in flight or its answer waits to be collected.
    ///
    /// Returns whether the worker should be woken.
    pub fn ask(&mut self, ask: Ask) -> bool {
        self.takes_asks() && self.desk.submit(ask).wake
    }

    /// Take the waiting ask, or `None` when there is nothing to make.
    pub fn next_job(&mut self) -> Option<Ask> {
        self.desk.next_job()
    }

    /// Record the answer to the ask in flight, returning whether the loop
    /// should be woken to collect it.
    pub fn deliver(&mut self, answer: A) -> bool {
        let owed = self.desk.deliver(answer);
        self.landed |= owed;
        owed
    }

    /// Take the landed answer, once.
    pub fn collect(&mut self) -> Option<A> {
        self.landed = false;
        self.desk.collect()
    }

    /// Stop handing out work, so a parked worker leaves.
    pub fn stop(&mut self) {
        self.desk.stop();
    }

    /// Whether the worker has been asked to leave.
    #[must_use]
    pub const fn stopping(&self) -> bool {
        self.desk.stopping()
    }
}

/// The world box chunk `coord`'s meshes lie within: its square reaching a
/// cell further east and south, since its vertices stand on cell centres and
/// its last on the next chunk's first, at every height ground can stand.
#[must_use]
pub fn chunk_box(coord: ChunkCoord) -> ([f64; 3], [f64; 3]) {
    let edge = |at: i32| at.saturating_mul(CHUNK_SUB_UNITS);
    let past = |at: i32| f64::from(edge(at.saturating_add(1)).saturating_add(CELL_SUB_UNITS));
    (
        [
            f64::from(edge(coord.x)),
            f64::from(edge(coord.y)),
            f64::from(i32::from(i16::MIN) * WORLD_PER_ELEVATION),
        ],
        [
            past(coord.x),
            past(coord.y),
            f64::from(i32::from(i16::MAX) * WORLD_PER_ELEVATION),
        ],
    )
}

/// What the ground worker made of an [`Ask`].
#[derive(Debug)]
pub enum Made {
    /// A chunk, solved.
    Solved(Chunk),
    /// The far patches made, the chunks among those asked for the generator
    /// refused, and whether the heap had no room for the rest.
    Far {
        /// The patches made.
        patches: Vec<FarPatch>,
        /// The chunks refused.
        refused: Vec<ChunkCoord>,
        /// Whether the heap ran short before every chunk was read.
        short: bool,
    },
    /// The generator refused to solve the chunk.
    Refused(ChunkCoord),
    /// The heap had no room to solve the chunk.
    Short,
}

impl Made {
    /// Solve `coord` over `land`, wearing its ground through `store`.
    ///
    /// A heap shortfall is [`Self::Short`], never a refusal: more room ends
    /// it, where a refusal is the generator's lasting answer.
    #[must_use]
    pub fn solved(coord: ChunkCoord, land: &RealmLand, store: &mut dyn Store) -> Self {
        match ChunkBuild::new(coord).and_then(|build| build.finish(land, store, &SERIAL)) {
            Ok(chunk) => Self::Solved(chunk),
            Err(WorldError::OutOfMemory) => Self::Short,
            Err(_) => Self::Refused(coord),
        }
    }

    /// Read `coords` off `land`'s level 0, stopping at the first the heap
    /// has no room for.
    #[must_use]
    pub fn far(coords: &[ChunkCoord], land: &RealmLand) -> Self {
        let (mut patches, mut refused) = (Vec::new(), Vec::new());
        for &coord in coords {
            let room = match land.far(coord) {
                Ok(patch) => {
                    let room = patches.try_reserve(1).is_ok();
                    if room {
                        patches.push(patch);
                    }
                    room
                }
                Err(WorldError::OutOfMemory) => false,
                Err(_) => {
                    let room = refused.try_reserve(1).is_ok();
                    if room {
                        refused.push(coord);
                    }
                    room
                }
            };
            if !room {
                return Self::Far {
                    patches,
                    refused,
                    short: true,
                };
            }
        }
        Self::Far {
            patches,
            refused,
            short: false,
        }
    }
}

/// What a live client says when solved ground finds no room.
const SHORT_SOLVED: &str =
    "no memory for solved ground; it is drawn from far ground until some is given back";

/// What a live client says when far ground finds no room.
const SHORT_FAR: &str = "no memory for far ground; it is not drawn until some is given back";

/// The ground a live client holds, and what it has learned of asking for
/// more: the chunks the generator refuses, whether the heap is short of room
/// for either kind until ground is given back or the pressure band moves, and
/// the view its last fruitless scan looked through.
#[derive(Debug, Default)]
pub struct Holding {
    ground: HeldGround,
    refused: Vec<ChunkCoord>,
    short_far: bool,
    short_solved: bool,
    /// The focus's chunk and the eye's when distant ground was last given
    /// back: nothing more is distant until one of them moves.
    released_at: Option<(ChunkCoord, ChunkCoord)>,
    /// The view a scan last found nothing to ask for through.
    settled: Option<Perspective>,
}

impl Holding {
    /// Nothing held.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ground: HeldGround::new(),
            refused: Vec::new(),
            short_far: false,
            short_solved: false,
            released_at: None,
            settled: None,
        }
    }

    /// The ground held.
    #[must_use]
    pub const fn ground(&self) -> &HeldGround {
        &self.ground
    }

    /// Give back what the view through `seen` about `focus` no longer needs,
    /// and choose the next ask: far patches over what it sees first, since
    /// they are cheap and fill the view at once, then the chunks about the
    /// focus solved in full.
    ///
    /// `None` where nothing is wanted, or the heap is short of room for what
    /// is. A scan finding nothing is not made again until the view or the
    /// ground held moves.
    pub fn next_ask(
        &mut self,
        params: RealmParams,
        seen: &Perspective,
        focus: WorldPoint,
    ) -> Option<Ask> {
        let eye = seen.eye();
        let stands = WorldPoint {
            x: mathf::round_i32(eye[0]),
            y: mathf::round_i32(eye[1]),
        };
        let at = (chunk_at(focus), chunk_at(stands));
        if self.released_at != Some(at) {
            self.released_at = Some(at);
            if self.ground.release_distant(focus, SOLVE_RADIUS, stands) {
                self.relieve();
            }
        }
        if self.settled.as_ref() == Some(seen) {
            return None;
        }
        if !self.short_far {
            let far = self.ground.next_far(params, seen, &self.refused);
            if !far.is_empty() {
                return Some(Ask::Far(far));
            }
        }
        if !self.short_solved {
            if let Some(coord) = self
                .ground
                .next_solve(params, focus, SOLVE_RADIUS, &self.refused)
            {
                return Some(Ask::Solve(coord));
            }
        }
        self.settled = Some(*seen);
        None
    }

    /// Hold what the worker `made`, answering what to say where it is news:
    /// a chunk refused for the first time, or the heap running short of room
    /// for a kind of ground since some was last given back.
    pub fn take(&mut self, made: Made) -> Option<&'static str> {
        self.settled = None;
        match made {
            Made::Solved(chunk) => {
                if self.ground.adopt(chunk).is_ok() {
                    return None;
                }
                Self::run_short(&mut self.short_solved, SHORT_SOLVED)
            }
            Made::Far {
                patches,
                refused,
                short,
            } => {
                let mut news = None;
                for coord in refused {
                    news = news.or(self.refuse(coord));
                }
                let held = patches
                    .into_iter()
                    .all(|patch| self.ground.adopt_far(patch).is_ok());
                if short || !held {
                    news = news.or(Self::run_short(&mut self.short_far, SHORT_FAR));
                }
                news
            }
            Made::Refused(coord) => self.refuse(coord),
            Made::Short => Self::run_short(&mut self.short_solved, SHORT_SOLVED),
        }
    }

    /// Ask again for what the heap had no room for: the pressure band moved,
    /// or ground was given back.
    pub fn relieve(&mut self) {
        self.short_far = false;
        self.short_solved = false;
        self.settled = None;
    }

    /// Mark a kind of ground short of room, answering `why` the first time.
    fn run_short(short: &mut bool, why: &'static str) -> Option<&'static str> {
        (!core::mem::replace(short, true)).then_some(why)
    }

    /// Never ask for `coord` again, answering what to say the first time.
    fn refuse(&mut self, coord: ChunkCoord) -> Option<&'static str> {
        if self.refused.contains(&coord) || self.refused.try_reserve(1).is_err() {
            return None;
        }
        self.refused.push(coord);
        Some("ground refused by the generator; it is not drawn")
    }
}

/// The chunk a world point lies in.
#[must_use]
pub const fn chunk_at(at: WorldPoint) -> ChunkCoord {
    ChunkCoord {
        x: at.x.div_euclid(CHUNK_SUB_UNITS),
        y: at.y.div_euclid(CHUNK_SUB_UNITS),
    }
}

/// How far, in world sub-units squared, `from` lies from the nearest point of
/// chunk `coord`'s ground.
#[must_use]
pub fn distance_squared(coord: ChunkCoord, from: WorldPoint) -> i64 {
    let side = i64::from(CHUNK_SUB_UNITS);
    let gap = |at: i32, chunk: i32| {
        let (low, at) = (i64::from(chunk) * side, i64::from(at));
        (low - at).max(at - (low + side)).max(0)
    };
    let (dx, dy) = (gap(from.x, coord.x), gap(from.y, coord.y));
    dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
}

/// The ground a client holds: solved chunks and far patches, each in
/// coordinate order.
#[derive(Debug, Default)]
pub struct HeldGround {
    solved: Vec<Chunk>,
    far: Vec<FarPatch>,
}

impl HeldGround {
    /// Nothing held.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            solved: Vec::new(),
            far: Vec::new(),
        }
    }

    /// The chunk at `coord`, where it is held solved.
    #[must_use]
    pub fn chunk(&self, coord: ChunkCoord) -> Option<&Chunk> {
        let at = self
            .solved
            .binary_search_by_key(&coord, Chunk::coord)
            .ok()?;
        self.solved.get(at)
    }

    /// The far patch over `coord`, where one is held.
    #[must_use]
    pub fn patch(&self, coord: ChunkCoord) -> Option<&FarPatch> {
        let at = self
            .far
            .binary_search_by_key(&coord, FarPatch::coord)
            .ok()?;
        self.far.get(at)
    }

    /// Whether the chunk at `coord` is held solved.
    #[must_use]
    pub fn holds(&self, coord: ChunkCoord) -> bool {
        self.chunk(coord).is_some()
    }

    /// Whether a far patch over `coord` is held.
    #[must_use]
    pub fn holds_far(&self, coord: ChunkCoord) -> bool {
        self.patch(coord).is_some()
    }

    /// Hold `chunk`; a chunk already held is kept as it is.
    ///
    /// # Errors
    ///
    /// [`ClientError::OutOfMemory`] when there is no room to hold it.
    pub fn adopt(&mut self, chunk: Chunk) -> Result<(), ClientError> {
        let Err(at) = self
            .solved
            .binary_search_by_key(&chunk.coord(), Chunk::coord)
        else {
            return Ok(());
        };
        self.solved
            .try_reserve(1)
            .map_err(|_| ClientError::OutOfMemory)?;
        self.solved.insert(at, chunk);
        Ok(())
    }

    /// Hold `patch`; a patch already held is kept as it is.
    ///
    /// # Errors
    ///
    /// [`ClientError::OutOfMemory`] when there is no room to hold it.
    pub fn adopt_far(&mut self, patch: FarPatch) -> Result<(), ClientError> {
        let Err(at) = self
            .far
            .binary_search_by_key(&patch.coord(), FarPatch::coord)
        else {
            return Ok(());
        };
        self.far
            .try_reserve(1)
            .map_err(|_| ClientError::OutOfMemory)?;
        self.far.insert(at, patch);
        Ok(())
    }

    /// Give back every solved chunk further than `radius` chunks from
    /// `focus`'s, keeping a chunk's breadth more so a player pacing over a
    /// boundary does not throw away ground it is about to ask for again, and
    /// every far patch beyond the view range of `eye`; answering whether any
    /// was given back.
    pub fn release_distant(&mut self, focus: WorldPoint, radius: i32, eye: WorldPoint) -> bool {
        let held = self.solved.len() + self.far.len();
        let at = chunk_at(focus);
        let keep = radius.saturating_add(1);
        self.solved.retain(|chunk| {
            let coord = chunk.coord();
            (coord.x - at.x).abs() <= keep && (coord.y - at.y).abs() <= keep
        });
        let range = i64::from(VIEW_RANGE) + i64::from(CHUNK_SUB_UNITS);
        self.far
            .retain(|patch| distance_squared(patch.coord(), eye) <= range * range);
        self.solved.len() + self.far.len() != held
    }

    /// The solved chunks, in coordinate order.
    #[must_use]
    pub fn solved(&self) -> &[Chunk] {
        &self.solved
    }

    /// The far patches, in coordinate order.
    #[must_use]
    pub fn far(&self) -> &[FarPatch] {
        &self.far
    }

    /// The nearest chunk within `radius` chunks of `focus`'s that the realm
    /// holds and the client has not solved, skipping any `refused` has given
    /// up on.
    #[must_use]
    pub fn next_solve(
        &self,
        params: RealmParams,
        focus: WorldPoint,
        radius: i32,
        refused: &[ChunkCoord],
    ) -> Option<ChunkCoord> {
        let at = chunk_at(focus);
        let mut nearest: Option<(i64, ChunkCoord)> = None;
        for y in at.y.saturating_sub(radius)..=at.y.saturating_add(radius) {
            for x in at.x.saturating_sub(radius)..=at.x.saturating_add(radius) {
                let coord = ChunkCoord { x, y };
                if !params.holds_chunk(x, y) || self.holds(coord) || refused.contains(&coord) {
                    continue;
                }
                let distance = distance_squared(coord, focus);
                if nearest.is_none_or(|(best, _)| distance < best) {
                    nearest = Some((distance, coord));
                }
            }
        }
        nearest.map(|(_, coord)| coord)
    }

    /// Up to [`FAR_BATCH`] of the nearest chunks `camera` can see within the
    /// view range of its eye that the realm holds and no far patch covers,
    /// skipping any `refused` has given up on. A solved chunk is asked for
    /// too, so its far reading is held when it is given back.
    #[must_use]
    pub fn next_far(
        &self,
        params: RealmParams,
        camera: &Perspective,
        refused: &[ChunkCoord],
    ) -> ArrayVec<ChunkCoord, FAR_BATCH> {
        let eye = camera.eye();
        let at = WorldPoint {
            x: mathf::round_i32(eye[0]),
            y: mathf::round_i32(eye[1]),
        };
        let centre = chunk_at(at);
        let reach = VIEW_RANGE / CHUNK_SUB_UNITS + 1;
        let mut nearest: ArrayVec<(i64, ChunkCoord), FAR_BATCH> = ArrayVec::new();
        for y in centre.y - reach..=centre.y + reach {
            for x in centre.x - reach..=centre.x + reach {
                let coord = ChunkCoord { x, y };
                let distance = distance_squared(coord, at);
                if distance > i64::from(VIEW_RANGE) * i64::from(VIEW_RANGE)
                    || !params.holds_chunk(x, y)
                    || self.holds_far(coord)
                    || refused.contains(&coord)
                {
                    continue;
                }
                let full = nearest.len() == FAR_BATCH;
                if full && nearest.last().is_some_and(|(worst, _)| distance >= *worst) {
                    continue;
                }
                let (low, high) = chunk_box(coord);
                if !camera.sees(low, high, f64::from(VIEW_RANGE)) {
                    continue;
                }
                if full {
                    nearest.pop();
                }
                let place = nearest
                    .iter()
                    .position(|(held, _)| distance < *held)
                    .unwrap_or(nearest.len());
                let _ = nearest.try_insert(place, (distance, coord));
            }
        }
        let mut batch = ArrayVec::new();
        for (_, coord) in nearest {
            // As many as were gathered, which is at most the batch.
            let _ = batch.try_push(coord);
        }
        batch
    }

    /// Make every piece of ground `camera` wants through `view` here, across
    /// `runner`: the chunks within `radius` of its focus solved from `land`
    /// over the worn ground `worn` keeps, and a far patch over every other
    /// chunk it sees; then give back what it no longer wants.
    ///
    /// For a caller that draws one picture and may wait for it; a live
    /// client asks its ground worker instead and draws what has arrived.
    ///
    /// # Errors
    ///
    /// [`ClientError::World`] for ground the generator refuses, and
    /// [`ClientError::OutOfMemory`] where it does not fit.
    pub fn hold(
        &mut self,
        land: &RealmLand,
        worn: &mut Keeping,
        camera: &Camera,
        view: &Viewport,
        radius: i32,
        runner: &dyn JobRunner,
    ) -> Result<(), ClientError> {
        let params = land.field().params();
        let focus = camera.focus();
        while let Some(coord) = self.next_solve(params, focus, radius, &[]) {
            let build = ChunkBuild::new(coord).map_err(|_| ClientError::World)?;
            let chunk = build
                .finish(land, worn, runner)
                .map_err(|_| ClientError::World)?;
            self.adopt(chunk)?;
        }
        // Where the eye stands follows the solved ground under it.
        let floor = self.height_at(focus).unwrap_or(0);
        let seen = camera.perspective(view, floor, |at| self.height_at(at))?;
        loop {
            let batch = self.next_far(params, &seen, &[]);
            if batch.is_empty() {
                break;
            }
            for coord in batch {
                self.adopt_far(land.far(coord).map_err(|_| ClientError::World)?)?;
            }
        }
        let eye = seen.eye();
        let stands = WorldPoint {
            x: mathf::round_i32(eye[0]),
            y: mathf::round_i32(eye[1]),
        };
        self.release_distant(focus, radius, stands);
        Ok(())
    }

    /// The surface's height under `at` as the held ground has it — a solved
    /// chunk's cells where one is held, else a far patch's vertices — or
    /// `None` where neither is.
    #[must_use]
    pub fn height_at(&self, at: WorldPoint) -> Option<i32> {
        let half = CELL_SUB_UNITS / 2;
        let lattice = WorldPoint {
            x: at.x.saturating_sub(half),
            y: at.y.saturating_sub(half),
        };
        let coord = chunk_at(lattice);
        let first = WorldPoint {
            x: coord.x.saturating_mul(CHUNK_SUB_UNITS).saturating_add(half),
            y: coord.y.saturating_mul(CHUNK_SUB_UNITS).saturating_add(half),
        };
        let (u, v) = (
            i64::from(at.x) - i64::from(first.x),
            i64::from(at.y) - i64::from(first.y),
        );
        if let Some(chunk) = self.chunk(coord) {
            let cell = i64::from(CELL_SUB_UNITS);
            let (ci, cj) = (u.div_euclid(cell), v.div_euclid(cell));
            let corner = |di: i64, dj: i64| {
                let dx = i32::try_from(ci + di).ok()?;
                let dy = i32::try_from(cj + dj).ok()?;
                chunk
                    .water_beside(dx, dy)
                    .map(|height| i64::from(height.0) * i64::from(WORLD_PER_ELEVATION))
            };
            return triangle_height(
                [corner(0, 0)?, corner(1, 0)?, corner(0, 1)?, corner(1, 1)?],
                (u - ci * cell, v - cj * cell),
                cell,
            );
        }
        let patch = self.patch(coord)?;
        let spacing = i64::from(patch.step()) * i64::from(CELL_SUB_UNITS);
        let (pi, pj) = (u.div_euclid(spacing), v.div_euclid(spacing));
        let corner = |di: i64, dj: i64| {
            patch
                .surface_beside(pi + di, pj + dj)
                .map(|height| i64::from(height.0) * i64::from(WORLD_PER_ELEVATION))
        };
        triangle_height(
            [corner(0, 0)?, corner(1, 0)?, corner(0, 1)?, corner(1, 1)?],
            (u - pi * spacing, v - pj * spacing),
            spacing,
        )
    }
}

/// The height at `(u, v)` within a quad `spacing` on a side whose corners
/// stand at `[north-west, north-east, south-west, south-east]`, cut from its
/// north-west corner to its south-east as the raster pass cuts it.
pub(crate) fn triangle_height(
    [nw, ne, sw, se]: [i64; 4],
    (u, v): (i64, i64),
    spacing: i64,
) -> Option<i32> {
    let rise = if u >= v {
        (ne - nw) * u + (se - ne) * v
    } else {
        (se - sw) * u + (sw - nw) * v
    };
    i32::try_from(nw + rise.div_euclid(spacing.max(1))).ok()
}

/// The stride a mesh `distance` world sub-units from the eye is drawn at,
/// with `focal` render pixels a unit one unit away: the coarsest power of two
/// whose quads span no more than [`QUAD_PIXELS`] on the screen there.
#[must_use]
pub fn stride_for(distance: i64, focal: u32) -> u32 {
    let cell = i64::from(CELL_SUB_UNITS) * i64::from(focal.max(1));
    let quads = (i64::from(QUAD_PIXELS).saturating_mul(distance.max(0)) / cell).max(1);
    let stride = 1u32 << quads.ilog2().min(CHUNK_CELLS.ilog2());
    stride.min(CHUNK_CELLS)
}

/// The widest a drawn quad may be on the screen, in pixels: wide enough that
/// a distant mesh is a handful of triangles, narrow enough that its relief
/// still reads as relief.
pub const QUAD_PIXELS: u32 = 16;

#[cfg(test)]
#[path = "terrain_tests.rs"]
mod tests;
