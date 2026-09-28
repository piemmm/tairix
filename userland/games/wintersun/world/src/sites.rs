//! Settlements, the roads between them, and the landmarks scattered
//! around them.
//!
//! # Roads are routed, not drawn
//!
//! A road between two settlements is a least-cost path over a traversal
//! cost field that prefers level ground and valley floors, pays heavily to
//! cross water, and — crucially — **pays less to reuse a road already
//! routed**. That last term is what makes a road network look like one:
//! routes that share a corridor merge into it and separate again at the
//! end, so roads braid and converge the way they do on a real map, rather
//! than running as a bundle of parallel lines between every pair of towns.
//!
//! The search is A\* with integer costs. Integer because a priority queue
//! ordered by `f64` is a queue whose pop order depends on how the numbers
//! were reached; integer costs with an index tiebreak pop identically
//! everywhere. The heuristic is octile distance times the cheapest
//! possible step, so it never overestimates and the path it finds is
//! genuinely the cheapest.
//!
//! # What a landmark is, and is not
//!
//! A landmark is an *entrance*: a position and a kind. What lies behind it
//! is the realm's secret and is generated server-side under interest
//! management. A client holding the whole landmark list learns only what
//! it would see by walking past, which is the point — a secret the seed
//! could reveal would not be a secret.

use alloc::collections::BinaryHeap;
use alloc::vec::Vec;
use core::cmp::Reverse;

use tairix_util::mathf;

use crate::error::WorldError;
use crate::geom::{signed, CellCoord, CHUNK_CELLS};
use crate::hydrology::specific_catchment;
use crate::params::RealmParams;
use crate::realm::{try_filled, CoarseSample};
use crate::seed::{SeedKey, Stage};

/// Most settlements a realm places.
pub const MAX_SITES: usize = 48;

/// Most landmarks a realm places.
pub const MAX_LANDMARKS: usize = 96;

/// The steepest ground anyone settles, as a rise over one cell.
const STEEPEST_SETTLED: f64 = 0.343_75;

/// The rise over one cell that makes a site most defensible, and how far
/// either side of it the advantage lasts.
const DEFENSIBLE_RISE: f64 = 0.093_75;
const DEFENSIBLE_SPREAD: f64 = 0.25;

/// The specific catchment, in cells, of a river that gives a settlement all
/// the water it needs.
const AMPLE_WATER: f64 = 25_600.0;

/// The specific catchment, in cells, of the least stream a shrine stands
/// beside.
const SHRINE_WATER: f64 = 3840.0;

/// Coarse samples a route may stray outside the box its endpoints span.
///
/// A road detours around an obstacle; it does not cross the continent to
/// avoid a hill. Bounding the search this way is also what keeps routing
/// cost proportional to the distance between two towns rather than to the
/// grid.
const ROUTE_MARGIN: i32 = 24;

/// Cheapest a step can be, and so the per-step multiplier the heuristic
/// uses. Reaching this needs an existing road on level ground.
const MIN_STEP_COST: u32 = 4;

/// Base cost of one orthogonal step over dry, level ground.
const BASE_STEP_COST: u32 = 16;

/// Cost added per world unit of climb across one step.
const CLIMB_COST: u32 = 3;

/// Cost of stepping into standing water — a ford, a bridge, or a detour.
///
/// Large enough that a route crosses a river at its narrowest rather than
/// running along one bank, and not so large that an island settlement can
/// never be reached.
const WATER_COST: u32 = 900;

/// Cost of stepping into a cell an earlier route already used, as a
/// numerator over [`BASE_STEP_COST`]'s denominator.
const REUSE_COST: u32 = MIN_STEP_COST;

/// What a settlement is.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(u8)]
pub enum SiteKind {
    /// A handful of dwellings.
    Hamlet = 0,
    /// A village with a market.
    Village = 1,
    /// A walled town.
    Town = 2,
    /// A settlement on the coast, with a harbour.
    Port = 3,
}

/// A settlement.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Site {
    /// Where it stands.
    pub at: CellCoord,
    /// What it is.
    pub kind: SiteKind,
    /// How far its cleared, levelled ground reaches, in cells.
    pub radius_cells: u16,
}

/// What a landmark is.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(u8)]
pub enum LandmarkKind {
    /// The way into a dungeon. The interior is server-only.
    DungeonEntrance = 0,
    /// A shrine.
    Shrine = 1,
    /// A ruin.
    Ruin = 2,
    /// A scar where the world was torn.
    RiftScar = 3,
}

impl LandmarkKind {
    /// Every kind, in discriminant order.
    pub const ALL: [Self; 4] = [
        Self::DungeonEntrance,
        Self::Shrine,
        Self::Ruin,
        Self::RiftScar,
    ];
}

/// A landmark's entrance.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Landmark {
    /// Where it is.
    pub at: CellCoord,
    /// What it is.
    pub kind: LandmarkKind,
}

/// A routed road.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Road {
    /// Index into the site list of the settlement it starts at.
    pub from: u16,
    /// Index into the site list of the settlement it ends at.
    pub to: u16,
    /// The cells it runs through, start to end.
    pub path: Vec<CellCoord>,
}

/// Everything placed on the realm.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Places {
    /// The settlements.
    pub sites: Vec<Site>,
    /// The roads between them.
    pub roads: Vec<Road>,
    /// The landmark entrances.
    pub landmarks: Vec<Landmark>,
}

/// Place settlements, route roads, and scatter landmarks.
///
/// # Errors
///
/// [`WorldError::OutOfMemory`] if the working vectors do not fit.
pub fn solve(
    params: RealmParams,
    key: SeedKey,
    samples: &[CoarseSample],
) -> Result<Places, WorldError> {
    let sites = place_sites(params, key, samples)?;
    let roads = route_roads(params, samples, &sites)?;
    let landmarks = place_landmarks(params, key, samples, &sites)?;
    Ok(Places {
        sites,
        roads,
        landmarks,
    })
}

/// The world cell the coarse sample at row-major `index` stands at.
fn sample_cell(params: RealmParams, index: usize, side: u32) -> CellCoord {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "the index is below the grid's area, so each component is \
                  below MAX_COARSE_SAMPLES"
    )]
    let (sx, sy) = (
        (index % (side as usize)) as i32,
        (index / (side as usize)) as i32,
    );
    params.sample_cell(sx, sy)
}

/// Score every land sample and take the best, separated.
fn place_sites(
    params: RealmParams,
    key: SeedKey,
    samples: &[CoarseSample],
) -> Result<Vec<Site>, WorldError> {
    let side = params.coarse_samples();
    // Separation follows the grid rather than the world, so a realm's
    // settlements are spread over it at the same density whatever its
    // extent.
    let separation = i64::from(side / 12).max(3);

    let mut ranked = try_filled(samples.len(), (0_i64, 0_u32))?;
    for (index, slot) in ranked.iter_mut().enumerate() {
        let score = site_score(key, params, samples, index);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the area is at most MAX_COARSE_SAMPLES squared"
        )]
        {
            // Descending score, ascending index: a total order with no
            // dependence on how the grid was walked.
            *slot = (-score, index as u32);
        }
    }
    ranked.sort_unstable();

    let mut sites: Vec<Site> = Vec::new();
    sites
        .try_reserve_exact(MAX_SITES)
        .map_err(|_| WorldError::OutOfMemory)?;

    for (negated, raw) in ranked {
        if sites.len() == MAX_SITES || -negated <= 0 {
            break;
        }
        let index = raw as usize;
        let cell = sample_cell(params, index, side);
        let step = i64::from(params.cells_per_coarse());
        if sites.iter().any(|site| {
            let dx = i64::from(site.at.x - cell.x).abs();
            let dy = i64::from(site.at.y - cell.y).abs();
            dx.max(dy) < separation * step
        }) {
            continue;
        }
        sites.push(Site {
            at: cell,
            kind: site_kind(samples, index, side, -negated),
            radius_cells: site_radius(-negated),
        });
    }
    Ok(sites)
}

/// How good a settlement site a sample is. Zero or below means "nowhere".
fn site_score(key: SeedKey, params: RealmParams, samples: &[CoarseSample], index: usize) -> i64 {
    let side = params.coarse_samples();
    let sample = samples[index];
    if sample.is_water() || sample.elevation.is_submerged() {
        return 0;
    }
    // Nobody settles where the summer never thaws the ground, or in a
    // furnace.
    let celsius = sample.temperature.celsius();
    let summer = celsius + sample.range.celsius() / 2.0;
    if summer < 2.0 || celsius > 34.0 {
        return 0;
    }

    let slope = slope_per_cell(params, samples, index);
    if slope > STEEPEST_SETTLED {
        return 0;
    }

    // Level ground, fresh water, workable climate, and a coast — the four
    // things that decide where people actually build.
    let flat = 1.0 - mathf::clamp(slope / STEEPEST_SETTLED, 0.0, 1.0);
    let catchment = specific_catchment(params, f64::from(sample.discharge));
    let water = mathf::clamp(catchment / AMPLE_WATER, 0.0, 1.0);
    // Enough rain to farm; past this, more does not make a better site.
    let damp = mathf::clamp(sample.precipitation.millimetres() / 1200.0, 0.0, 1.0);
    let coastal = if touches_sea(samples, index, side) {
        1.0
    } else {
        0.0
    };
    // Defensible: a little local relief is an advantage, a lot is a
    // mountainside.
    let defensible = 1.0 - mathf::fabs(slope - DEFENSIBLE_RISE) / DEFENSIBLE_SPREAD;

    let jitter = key.unit(Stage::Settlement, index_i32(index), 0) * 0.15;
    let score = flat * 0.30
        + water * 0.26
        + damp * 0.12
        + coastal * 0.18
        + mathf::clamp(defensible, 0.0, 1.0) * 0.14
        + jitter;

    mathf::round_i32(score * 1_000_000.0).into()
}

/// What kind of settlement a site becomes.
fn site_kind(samples: &[CoarseSample], index: usize, side: u32, score: i64) -> SiteKind {
    if touches_sea(samples, index, side) {
        return SiteKind::Port;
    }
    if score > 700_000 {
        SiteKind::Town
    } else if score > 520_000 {
        SiteKind::Village
    } else {
        SiteKind::Hamlet
    }
}

/// How far a site's levelled ground reaches.
fn site_radius(score: i64) -> u16 {
    /// Smallest cleared radius, in cells.
    const SMALLEST: i64 = 6;
    /// Largest cleared radius, in cells.
    const LARGEST: i64 = 26;
    let scaled = SMALLEST + (score.clamp(0, 1_000_000) * (LARGEST - SMALLEST)) / 1_000_000;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the result is clamped between SMALLEST and LARGEST"
    )]
    {
        scaled.clamp(SMALLEST, LARGEST) as u16
    }
}

/// The steepest drop to a four-neighbour, as a rise over one cell, so a
/// threshold means the same ground underfoot in a realm of any extent.
fn slope_per_cell(params: RealmParams, samples: &[CoarseSample], index: usize) -> f64 {
    local_slope(samples, index, params.coarse_samples()) / f64::from(params.cells_per_coarse())
}

/// The steepest drop to a four-neighbour, in world units.
fn local_slope(samples: &[CoarseSample], index: usize, side: u32) -> f64 {
    let here = samples[index].elevation.units();
    let mut steepest = 0.0_f64;
    for next in four_neighbours(index, side).into_iter().flatten() {
        steepest = mathf::fmax(
            steepest,
            mathf::fabs(here - samples[next].elevation.units()),
        );
    }
    steepest
}

/// Whether a sample touches open sea — not a lake.
fn touches_sea(samples: &[CoarseSample], index: usize, side: u32) -> bool {
    four_neighbours(index, side)
        .into_iter()
        .flatten()
        .any(|next| samples[next].elevation.is_submerged())
}

/// The four orthogonal neighbours of a sample, `None` off the grid.
fn four_neighbours(index: usize, side: u32) -> [Option<usize>; 4] {
    let width = side as usize;
    let (sx, sy) = (index % width, index / width);
    [
        (sx + 1 < width).then(|| index + 1),
        (sy + 1 < width).then(|| index + width),
        (sx > 0).then(|| index - 1),
        (sy > 0).then(|| index - width),
    ]
}

/// Narrow a grid index for use as a hash coordinate.
fn index_i32(index: usize) -> i32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "the grid's area is at most MAX_COARSE_SAMPLES squared, \
                  which is 2^18"
    )]
    {
        index as i32
    }
}

/// Connect the settlements with a minimum spanning tree, then route each
/// of its edges.
fn route_roads(
    params: RealmParams,
    samples: &[CoarseSample],
    sites: &[Site],
) -> Result<Vec<Road>, WorldError> {
    let side = params.coarse_samples();
    let mut roads = Vec::new();
    if sites.len() < 2 {
        return Ok(roads);
    }
    roads
        .try_reserve_exact(sites.len() - 1)
        .map_err(|_| WorldError::OutOfMemory)?;

    // Cells an already-routed road runs through, which the next route
    // pays less to reuse. This is what makes the network braid.
    let mut used = try_filled(samples.len(), false)?;
    let mut router = Router::new(samples.len())?;

    for (from, to) in spanning_edges(sites)? {
        let start = nearest_sample(params, sites[from].at, side);
        let goal = nearest_sample(params, sites[to].at, side);
        let Some(path) = router.route(params, samples, &used, start, goal)? else {
            // No admissible route inside the search box — an island, or a
            // settlement behind an unfordable reach. Recording no road is
            // the honest answer; a road that does not connect its ends
            // would be worse than none.
            continue;
        };
        for &cell in &path {
            used[cell] = true;
        }
        let mut cells = Vec::new();
        cells
            .try_reserve_exact(path.len())
            .map_err(|_| WorldError::OutOfMemory)?;
        cells.extend(path.iter().map(|&index| sample_cell(params, index, side)));
        #[allow(
            clippy::cast_possible_truncation,
            reason = "both indices are below MAX_SITES"
        )]
        roads.push(Road {
            from: from as u16,
            to: to as u16,
            path: cells,
        });
    }
    Ok(roads)
}

/// A minimum spanning tree over the settlements, by straight-line
/// distance.
///
/// Prim's, which visits the sites in a fixed order and breaks a tie on the
/// lower index, so the tree is a pure function of the site list.
fn spanning_edges(sites: &[Site]) -> Result<Vec<(usize, usize)>, WorldError> {
    let mut edges = Vec::new();
    edges
        .try_reserve_exact(sites.len().saturating_sub(1))
        .map_err(|_| WorldError::OutOfMemory)?;

    let mut joined = try_filled(sites.len(), false)?;
    joined[0] = true;

    for _ in 1..sites.len() {
        let mut best: Option<(i64, usize, usize)> = None;
        for (a, site_a) in sites.iter().enumerate() {
            if !joined[a] {
                continue;
            }
            for (b, site_b) in sites.iter().enumerate() {
                if joined[b] {
                    continue;
                }
                let dx = i64::from(site_a.at.x - site_b.at.x);
                let dy = i64::from(site_a.at.y - site_b.at.y);
                let candidate = (dx * dx + dy * dy, a, b);
                if best.is_none_or(|current| candidate < current) {
                    best = Some(candidate);
                }
            }
        }
        let Some((_, a, b)) = best else { break };
        joined[b] = true;
        edges.push((a, b));
    }
    Ok(edges)
}

/// The coarse sample nearest a world cell.
fn nearest_sample(params: RealmParams, cell: CellCoord, side: u32) -> usize {
    let origin = params.min_chunk() * signed(CHUNK_CELLS);
    let step = signed(params.cells_per_coarse());
    crate::realm::clamped_index((cell.x - origin) / step, (cell.y - origin) / step, side)
}

/// The A\* scratch, allocated once and reset per route.
///
/// A grid-sized working set per road, freed and reallocated between
/// roads, would be the same zeroing done under an allocator round trip.
struct Router {
    cost: Vec<u32>,
    came: Vec<u32>,
    settled: Vec<bool>,
    open: BinaryHeap<Reverse<(u32, u32)>>,
}

impl Router {
    /// Scratch for a grid of `area` samples.
    fn new(area: usize) -> Result<Self, WorldError> {
        Ok(Self {
            cost: try_filled(area, u32::MAX)?,
            came: try_filled(area, u32::MAX)?,
            settled: try_filled(area, false)?,
            open: BinaryHeap::new(),
        })
    }

    /// A\* from `start` to `goal` over the traversal cost field, bounded
    /// to the box the two span plus a margin.
    fn route(
        &mut self,
        params: RealmParams,
        samples: &[CoarseSample],
        used: &[bool],
        start: usize,
        goal: usize,
    ) -> Result<Option<Vec<usize>>, WorldError> {
        let side = params.coarse_samples();
        let width = side as usize;
        let (sx, sy) = (start % width, start / width);
        let (gx, gy) = (goal % width, goal / width);

        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_possible_wrap,
            reason = "a grid coordinate is below MAX_COARSE_SAMPLES"
        )]
        let box_lo = (
            (sx.min(gx) as i32 - ROUTE_MARGIN).max(0),
            (sy.min(gy) as i32 - ROUTE_MARGIN).max(0),
        );
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_possible_wrap,
            reason = "a grid coordinate is below MAX_COARSE_SAMPLES"
        )]
        let box_hi = (
            (sx.max(gx) as i32 + ROUTE_MARGIN).min(side as i32 - 1),
            (sy.max(gy) as i32 + ROUTE_MARGIN).min(side as i32 - 1),
        );

        self.cost.fill(u32::MAX);
        self.came.fill(u32::MAX);
        self.settled.fill(false);
        self.open.clear();

        self.cost[start] = 0;
        self.open
            .push(Reverse((heuristic(start, goal, width), index_u32(start))));

        while let Some(Reverse((_, raw))) = self.open.pop() {
            let index = raw as usize;
            if self.settled[index] {
                continue;
            }
            self.settled[index] = true;
            if index == goal {
                return Ok(Some(unwind(&self.came, start, goal)?));
            }

            let (cx, cy) = (index % width, index / width);
            for (dx, dy, diagonal) in STEPS {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_possible_wrap,
                    reason = "a grid coordinate is below MAX_COARSE_SAMPLES"
                )]
                let (nx, ny) = (cx as i32 + dx, cy as i32 + dy);
                if nx < box_lo.0 || ny < box_lo.1 || nx > box_hi.0 || ny > box_hi.1 {
                    continue;
                }
                #[allow(
                    clippy::cast_sign_loss,
                    reason = "both components were just bounds-checked into \
                              the search box, which is itself inside the grid"
                )]
                let next = (ny as usize) * width + (nx as usize);
                let step = step_cost(samples, used, index, next, diagonal);
                let Some(total) = self.cost[index].checked_add(step) else {
                    continue;
                };
                if total >= self.cost[next] {
                    continue;
                }
                self.cost[next] = total;
                self.came[next] = index_u32(index);
                let Some(priority) = total.checked_add(heuristic(next, goal, width)) else {
                    continue;
                };
                self.open.push(Reverse((priority, index_u32(next))));
            }
        }
        Ok(None)
    }
}

/// The eight steps a route may take, and whether each is diagonal.
const STEPS: [(i32, i32, bool); 8] = [
    (1, 0, false),
    (1, 1, true),
    (0, 1, false),
    (-1, 1, true),
    (-1, 0, false),
    (-1, -1, true),
    (0, -1, false),
    (1, -1, true),
];

/// What one step costs.
fn step_cost(
    samples: &[CoarseSample],
    used: &[bool],
    from: usize,
    to: usize,
    diagonal: bool,
) -> u32 {
    let base = if used[to] { REUSE_COST } else { BASE_STEP_COST };
    let climb = mathf::fabs(samples[to].elevation.units() - samples[from].elevation.units());
    let water = if samples[to].is_water() {
        WATER_COST
    } else {
        0
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the climb is clamped to the elevation field's own range, \
                  so the product stays far inside u32"
    )]
    let climb_cost = (mathf::clamp(climb, 0.0, 8192.0) * f64::from(CLIMB_COST)) as u32;
    let straight = base + climb_cost + water;
    if diagonal {
        // The octile approximation of √2, in the same integer units the
        // heuristic uses, so the two cannot disagree about a diagonal.
        straight + straight / 2
    } else {
        straight
    }
}

/// Octile distance times the cheapest possible step: admissible, so A\*
/// returns a genuinely least-cost path.
fn heuristic(from: usize, goal: usize, width: usize) -> u32 {
    let (fx, fy) = (from % width, from / width);
    let (gx, gy) = (goal % width, goal / width);
    let dx = fx.abs_diff(gx);
    let dy = fy.abs_diff(gy);
    let (long, short) = if dx > dy { (dx, dy) } else { (dy, dx) };
    let straight = long - short;
    // Each diagonal step costs one and a half of the cheapest step in the
    // integer scheme above, and there are `short` of them.
    let scaled = (straight as u64) * u64::from(MIN_STEP_COST)
        + (short as u64) * u64::from(MIN_STEP_COST) * 3 / 2;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the grid's diagonal is at most MAX_COARSE_SAMPLES, so the \
                  product is far inside u32"
    )]
    {
        scaled.min(u64::from(u32::MAX)) as u32
    }
}

/// Walk the predecessor chain back from the goal.
fn unwind(came: &[u32], start: usize, goal: usize) -> Result<Vec<usize>, WorldError> {
    let mut reversed = Vec::new();
    reversed
        .try_reserve(came.len())
        .map_err(|_| WorldError::OutOfMemory)?;
    let mut here = goal;
    reversed.push(here);
    while here != start {
        let previous = came[here];
        if previous == u32::MAX {
            break;
        }
        here = previous as usize;
        reversed.push(here);
    }
    reversed.reverse();
    Ok(reversed)
}

/// Narrow a grid index to the `u32` the search arrays hold.
fn index_u32(index: usize) -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the grid's area is at most MAX_COARSE_SAMPLES squared"
    )]
    {
        index as u32
    }
}

/// Scatter landmark entrances over the terrain that suits each kind.
///
/// Each kind has its own share of the budget and its candidates are ranked
/// by their own draw, and the kinds take turns: one ranking over every kind
/// would give every slot to whichever kind the most ground suits, and a
/// realm of nothing but dungeon doors.
fn place_landmarks(
    params: RealmParams,
    key: SeedKey,
    samples: &[CoarseSample],
    sites: &[Site],
) -> Result<Vec<Landmark>, WorldError> {
    let side = params.coarse_samples();
    let mut ranked: [Vec<(u64, u32)>; LandmarkKind::ALL.len()] = Default::default();
    for index in 0..samples.len() {
        let Some(kind) = landmark_kind(params, samples, index) else {
            continue;
        };
        let candidates = &mut ranked[kind as usize];
        candidates
            .try_reserve(1)
            .map_err(|_| WorldError::OutOfMemory)?;
        // Descending draw, ascending index: a total order with no
        // dependence on how the grid was walked.
        candidates.push((
            u64::MAX - key.lattice(Stage::Landmark, index_i32(index), 1),
            index_u32(index),
        ));
    }
    for candidates in &mut ranked {
        candidates.sort_unstable();
    }

    let mut landmarks = Vec::new();
    landmarks
        .try_reserve_exact(MAX_LANDMARKS)
        .map_err(|_| WorldError::OutOfMemory)?;

    let step = i64::from(params.cells_per_coarse());
    let separation = i64::from(side / 24).max(2) * step;
    let share = MAX_LANDMARKS / LandmarkKind::ALL.len();
    let mut placed = [0_usize; LandmarkKind::ALL.len()];
    let mut next = [0_usize; LandmarkKind::ALL.len()];

    loop {
        let mut progressed = false;
        for kind in LandmarkKind::ALL {
            let k = kind as usize;
            // Take this kind's best remaining candidate that stands clear of
            // everything placed so far; a kind whose share is met, or whose
            // candidates are spent, sits the rest of the turns out.
            while placed[k] < share && next[k] < ranked[k].len() {
                let index = ranked[k][next[k]].1 as usize;
                next[k] += 1;
                let cell = sample_cell(params, index, side);
                let crowded = |at: CellCoord, keep_out: i64| {
                    i64::from(at.x - cell.x)
                        .abs()
                        .max(i64::from(at.y - cell.y).abs())
                        < keep_out
                };
                // Not on top of a settlement, and not on top of each other.
                if sites
                    .iter()
                    .any(|site| crowded(site.at, i64::from(site.radius_cells) + step))
                    || landmarks
                        .iter()
                        .any(|other: &Landmark| crowded(other.at, separation))
                {
                    continue;
                }
                landmarks.push(Landmark { at: cell, kind });
                placed[k] += 1;
                progressed = true;
                break;
            }
        }
        if !progressed {
            break;
        }
    }
    Ok(landmarks)
}

/// What kind of landmark a sample suits, if any.
fn landmark_kind(
    params: RealmParams,
    samples: &[CoarseSample],
    index: usize,
) -> Option<LandmarkKind> {
    let sample = samples[index];
    if sample.is_water() {
        return None;
    }
    let belt = f64::from(sample.belt) / f64::from(u8::MAX);
    let rift = f64::from(sample.rift) / f64::from(u8::MAX);
    let slope = slope_per_cell(params, samples, index);
    let height = sample.elevation.units();

    // A rift scar needs ground the plates pulled apart and left low; a
    // dungeon needs rock, which means a belt or a steep face; a shrine
    // wants a quiet place with water in reach; a ruin wants ground that
    // was once worth settling.
    if rift > 0.02 && height < 120.0 {
        return Some(LandmarkKind::RiftScar);
    }
    if belt > 0.4 || slope > 1.5 {
        return Some(LandmarkKind::DungeonEntrance);
    }
    if specific_catchment(params, f64::from(sample.discharge)) >= SHRINE_WATER && slope < 1.0 {
        return Some(LandmarkKind::Shrine);
    }
    if slope < 1.25 && height > 8.0 {
        return Some(LandmarkKind::Ruin);
    }
    None
}

#[cfg(test)]
mod tests;
