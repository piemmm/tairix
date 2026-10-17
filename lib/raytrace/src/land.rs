//! The land as a scene traces it: the land generator's coarse land
//! (`tairix_land::coarse`) refined level by level (`tairix_land::level`) into
//! the scene's grids — level 0 over the whole land as its far grid, and a
//! square of each finer level about the eye — with the relief running on
//! beyond it to the horizon and the fresh water's surface over it. The
//! pieces are the land's own; only once every level is laid is each finer
//! grid blended into the grid about it across a band within its border, so
//! the two meet without a step and no level ever reads another's blend.
//!
//! Every stage runs a bounded unit at a time, so a caller answering frames
//! spreads the land over as many as it takes.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_countryside::layout::Layout;
use tairix_land::attributes::{sediment_of, Lie, GREEN, PATH, ROAD, SEDIMENT, SNOW, WET};
use tairix_land::channel::Form;
use tairix_land::coarse::{Build as Wearing, Coarse, Crossing, RELIEF_NS};
use tairix_land::course::{smoothed, Courses, Mark};
use tairix_land::level::{Keeps, Parent, Piece, Refining};
use tairix_land::plan::{Lattices, Plan as LandPlan, Roadway, LEVELS};
use tairix_land::snow;
use tairix_land::ways::Ways;
use tairix_noise::{noise2, smoothstep};
use tairix_parallel::JobRunner;
use tairix_terrain::droplet::{Placed, Unkept};
use tairix_util::{fallible, mathf};

use crate::heightfield::{apart, Heightfield, Sealing, ABSENT};
use crate::vector::{real, share, single, Ray, Vec3};

/// Everything a scene's land is made from, chosen before any of it is built:
/// the land itself, and how the scene traces it.
#[derive(Clone, Debug)]
pub(crate) struct Plan {
    pub(crate) land: LandPlan,
    /// How far either way of its middle each finer level's square about the
    /// eye reaches, coarsest first: one to each level after the first.
    pub(crate) nests: [Option<f64>; NESTS],
    /// The fresh water's own finer grid about the eye, if the scene shapes
    /// its surface there.
    pub(crate) near_water: Option<NearWater>,
    /// The relief running on past the far grid out to the horizon, if it
    /// does rather than settling to a rim.
    pub(crate) horizon: Option<Horizon>,
}

impl Plan {
    /// Where the horizon grid lies — its first vertex and its step — its
    /// reach nudged so the far grid's border runs along its cell edges;
    /// `None` if the land has none, or one too small to leave a cell either
    /// side of the border.
    pub(crate) fn horizon_placing(&self) -> Option<((f64, f64), f64)> {
        let horizon = self.horizon.filter(|horizon| horizon.cells >= 4)?;
        let (centre, reach) = far_square(&self.land)?;
        let half = real(horizon.cells) / 2.0;
        // Whole cells between the far grid's edge and the horizon's.
        let between = mathf::round(half * (1.0 - reach / horizon.reach)).clamp(1.0, half - 1.0);
        let reach = reach / (1.0 - between / half);
        Some((
            (centre.0 - reach, centre.1 - reach),
            2.0 * reach / real(horizon.cells),
        ))
    }
}

/// The middle of the far grid's square and half its breadth: the coarse
/// lattice's square, which level 0 covers.
fn far_square(plan: &LandPlan) -> Option<((f64, f64), f64)> {
    let lattice = plan.lattice().ok()?;
    let reach = 0.5 * f64::from(lattice.cells) * lattice.step;
    let corner = |index: i64| Some(f64::from(i32::try_from(index).ok()?) * lattice.step);
    Some((
        (
            corner(lattice.origin.0)? + reach,
            corner(lattice.origin.1)? + reach,
        ),
        reach,
    ))
}

/// How many finer levels a land lays about the eye, each within the last.
pub(crate) const NESTS: usize = LEVELS - 1;

/// A finer grid of the fresh water's surface laid about the eye, where a
/// scene shapes the surface itself: its half breadth, and its cells a side.
#[derive(Copy, Clone, Debug)]
pub(crate) struct NearWater {
    pub(crate) reach: f64,
    pub(crate) cells: usize,
}

/// A coarse grid of the relief out to the horizon, the far grid laid within
/// it: about how far it reaches either way, and its cells a side.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Horizon {
    pub(crate) reach: f64,
    pub(crate) cells: usize,
}

/// The scene's grids a land is built into.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Fields {
    pub(crate) far: u32,
    /// A grid for each finer grid the plan lays.
    pub(crate) nests: [Option<u32>; NESTS],
    pub(crate) water: Option<u32>,
    /// A grid for the fresh water's own finer grid, if the plan lays one.
    pub(crate) near_water: Option<u32>,
    /// A grid for the horizon, if the plan has one.
    pub(crate) horizon: Option<u32>,
}

/// Where a finer grid lies: the scene's grid that holds it, its middle, and
/// half its breadth.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Laid {
    pub(crate) field: u32,
    pub(crate) centre: (f64, f64),
    pub(crate) reach: f64,
}

impl Laid {
    /// Whether `(x, z)` lies well within the grid, clear of the border where
    /// it gives way to the grid about it.
    pub(crate) fn holds(&self, x: f64, z: f64) -> bool {
        let inner = 0.97 * self.reach;
        (x - self.centre.0).abs() < inner && (z - self.centre.1).abs() < inner
    }

    /// How far within its border `(x, z)` lies, negative outside it.
    fn inside(&self, (x, z): (f64, f64)) -> f64 {
        self.reach - (x - self.centre.0).abs().max((z - self.centre.1).abs())
    }
}

/// The land once built, as a scene's grids hold it.
#[derive(Clone, Debug)]
pub(crate) struct Land {
    pub(crate) grids: Grids,
    /// The rivers, and what shapes their channels.
    pub(crate) rivers: Courses,
    pub(crate) form: Option<Form>,
    pub(crate) ways: Ways,
    /// Where its ways bridge its rivers.
    pub(crate) crossings: Vec<Crossing>,
    /// The countryside laid out over it, where it is farmed.
    pub(crate) layout: Option<Layout>,
}

/// The grids a land is traced on: all that reading it at a place needs,
/// small enough for the tracer to keep.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Grids {
    /// The scene's grids that hold it: the far land, the finer grids laid
    /// about the eye, coarsest first, the fresh water's surface, and the land
    /// beyond out to the horizon.
    pub(crate) far: u32,
    pub(crate) nests: [Option<Laid>; NESTS],
    pub(crate) water: Option<u32>,
    /// The fresh water's own finer grid about the eye, if it has one.
    pub(crate) near_water: Option<Laid>,
    pub(crate) horizon: Option<u32>,
    pub(crate) sea: Option<f64>,
    /// The square the far land covers.
    pub(crate) centre: (f64, f64),
    pub(crate) reach: f64,
}

impl Grids {
    /// The square the land's grids trace, out to the horizon where it runs
    /// on to one.
    pub(crate) fn traced(&self, fields: &[Heightfield]) -> ((f64, f64), f64) {
        self.horizon
            .and_then(|horizon| fields.get(horizon as usize)?.bounds())
            .map_or((self.centre, self.reach), |bounds| {
                let reach = 0.5 * (bounds.max.x - bounds.min.x);
                (
                    (
                        f64::midpoint(bounds.min.x, bounds.max.x),
                        f64::midpoint(bounds.min.z, bounds.max.z),
                    ),
                    reach,
                )
            })
    }

    /// The land's own grids among `fields`: the far land, its finer grids
    /// and the horizon's, without its water.
    fn ground<'a>(&self, fields: &'a [Heightfield]) -> impl Iterator<Item = &'a Heightfield> {
        let nests = self.nests.map(|laid| laid.map(|laid| laid.field));
        [Some(self.far), self.horizon]
            .into_iter()
            .chain(nests)
            .flatten()
            .filter_map(|field| fields.get(field as usize))
    }

    /// The lowest and the highest any of the land's grids reaching the square
    /// from `from` to `to` stands over it.
    pub(crate) fn extremes(
        &self,
        fields: &[Heightfield],
        (from, to): ((f64, f64), (f64, f64)),
    ) -> (f64, f64) {
        self.ground(fields)
            .filter(|grid| overlaps(grid, (from, to)))
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), grid| {
                (
                    low.min(grid.lowest_over(from, to)),
                    high.max(grid.highest_over(from, to)),
                )
            })
    }

    /// The deepest snow lies anywhere over the square from `from` to `to` on
    /// any of the land's grids reaching it.
    pub(crate) fn deepest_snow(
        &self,
        fields: &[Heightfield],
        (from, to): ((f64, f64), (f64, f64)),
    ) -> f64 {
        self.ground(fields)
            .filter(|grid| overlaps(grid, (from, to)))
            .map(|grid| snow::depth_of(grid.most_of(SNOW, from, to)))
            .fold(0.0, f64::max)
    }

    /// Where `ray` first comes within `lift` above any of the land's grids
    /// in `(from, to)`; `None` if it passes above them all.
    pub(crate) fn approach(
        &self,
        fields: &[Heightfield],
        ray: &Ray,
        lift: f64,
        span: (f64, f64),
    ) -> Option<f64> {
        self.ground(fields)
            .filter_map(|grid| grid.approach(ray, lift, span))
            .reduce(f64::min)
    }

    /// The grid that traces `(x, z)`: the finest that reaches it.
    fn grid<'a>(&self, fields: &'a [Heightfield], x: f64, z: f64) -> Option<&'a Heightfield> {
        let finest = self
            .nests
            .iter()
            .rev()
            .flatten()
            .find(|laid| laid.holds(x, z));
        let far = Laid {
            field: self.far,
            centre: self.centre,
            reach: self.reach,
        };
        let field = match (finest, self.horizon) {
            (Some(laid), _) => laid.field,
            (None, Some(horizon)) if !far.holds(x, z) => horizon,
            _ => self.far,
        };
        fields.get(field as usize)
    }

    /// The land's height at `(x, z)`, as a ray meets it there.
    pub(crate) fn height(&self, fields: &[Heightfield], x: f64, z: f64) -> f64 {
        self.grid(fields, x, z)
            .map_or(0.0, |grid| grid.height_at(x, z))
    }

    /// The ground's height at `(x, z)` beneath the snow lying on it: what a
    /// wall or a post is founded on.
    pub(crate) fn beneath_snow(&self, fields: &[Heightfield], x: f64, z: f64) -> f64 {
        self.grid(fields, x, z).map_or(0.0, |grid| {
            grid.height_at(x, z) - snow::depth_of(grid.quantity_at(x, z, SNOW))
        })
    }

    /// Which way the land faces at `(x, z)`.
    pub(crate) fn normal(&self, fields: &[Heightfield], x: f64, z: f64) -> Vec3 {
        let Some(grid) = self.grid(fields, x, z) else {
            return Vec3::UP;
        };
        let (_, step) = grid.placing();
        let dx = grid.height_at(x + step, z) - grid.height_at(x - step, z);
        let dz = grid.height_at(x, z + step) - grid.height_at(x, z - step);
        Vec3::new(-dx, 2.0 * step, -dz).normalized()
    }

    /// What the land is like at `(x, z)`.
    pub(crate) fn lie(&self, fields: &[Heightfield], x: f64, z: f64) -> Lie {
        self.grid(fields, x, z)
            .map_or_else(Lie::default, |grid| lie_on(grid, x, z))
    }

    /// What grows at `(x, z)` and how its rows run, as its nearest vertex
    /// keeps them; nothing farmed off the land.
    pub(crate) fn grows(&self, fields: &[Heightfield], x: f64, z: f64) -> (u8, u8) {
        self.grid(fields, x, z)
            .map_or((0, 0), |grid| grid.grows_at(x, z).nearest())
    }

    /// The fresh water's level about `(x, z)` wherever its grids hold one,
    /// over a bank beside the water as over the water itself: the finer
    /// grid's where it lies, as the flow has shaped it. The finer grid holds
    /// water only to just past its brim, so a bank beyond reads the far
    /// grid's.
    pub(crate) fn water_level(&self, fields: &[Heightfield], x: f64, z: f64) -> Option<f64> {
        let level_on = |grid: u32| {
            fields
                .get(grid as usize)
                .map(|field| field.height_at(x, z))
                .filter(|level| level.is_finite())
        };
        self.near_water
            .filter(|near| near.inside((x, z)) > 0.0)
            .and_then(|near| level_on(near.field))
            .or_else(|| self.water.and_then(level_on))
    }

    /// What one stands on at `(x, z)`: the ground, or the water's surface,
    /// fresh or salt, where it lies above the ground.
    pub(crate) fn surface(&self, fields: &[Heightfield], x: f64, z: f64) -> f64 {
        let ground = self.height(fields, x, z);
        self.water_level(fields, x, z)
            .into_iter()
            .chain(self.sea)
            .fold(ground, f64::max)
    }

    /// Whether `(x, z)` lies under water, fresh or salt.
    pub(crate) fn wet_at(&self, fields: &[Heightfield], x: f64, z: f64) -> bool {
        self.wet_over(fields, (x, z), self.height(fields, x, z))
    }

    /// Whether ground `ground` high at `(x, z)` lies under water, fresh or
    /// salt.
    pub(crate) fn wet_over(&self, fields: &[Heightfield], (x, z): (f64, f64), ground: f64) -> bool {
        drowned(self.sea, ground)
            || self
                .water_level(fields, x, z)
                .is_some_and(|level| level > ground)
    }
}

/// How far above the sea's level ground still lies wet: its shore's lapping
/// edge.
const SHORE: f64 = 0.2;

/// Whether ground `ground` high lies under the sea `sea`, if the land has
/// one.
fn drowned(sea: Option<f64>, ground: f64) -> bool {
    sea.is_some_and(|sea| ground < sea + SHORE)
}

/// Whether `grid` reaches over any of the square from `from` to `to`.
fn overlaps(grid: &Heightfield, (from, to): ((f64, f64), (f64, f64))) -> bool {
    grid.bounds().is_some_and(|bounds| {
        bounds.min.x < to.0 && bounds.max.x > from.0 && bounds.min.z < to.1 && bounds.max.z > from.1
    })
}

/// What `grid` says the land is like at `(x, z)`.
fn lie_on(grid: &Heightfield, x: f64, z: f64) -> Lie {
    let (_, step) = grid.placing();
    let height = grid.height_at(x, z);
    let dx = (grid.height_at(x + step, z) - grid.height_at(x - step, z)) / (2.0 * step);
    let dz = (grid.height_at(x, z + step) - grid.height_at(x, z - step)) / (2.0 * step);
    let quantities = grid.attributes_at(x, z);
    let (grown, rows) = grid.grows_at(x, z).nearest();
    Lie {
        height,
        upright: 1.0 / mathf::sqrt(1.0 + dx * dx + dz * dz),
        wet: quantities[WET],
        sediment: sediment_of(quantities[SEDIMENT]),
        road: quantities[ROAD],
        path: quantities[PATH],
        green: quantities[GREEN],
        snow: snow::depth_of(quantities[SNOW]),
        grown,
        rows,
    }
}

/// A course for a footpath from `start` wandering toward `heading` over
/// `reach`, bending round what `rough` says is hard going; `None` when the
/// heap will not hold it.
pub(crate) fn footpath(
    start: (f64, f64),
    heading: f64,
    reach: f64,
    width: f64,
    rough: &dyn Fn(f64, f64) -> f64,
    seed: u32,
) -> Option<Vec<Mark>> {
    let steps = 48u32;
    let stride = reach / f64::from(steps);
    let mut marks = Vec::new();
    if !fallible::reserve(&mut marks, usize::try_from(steps + 1).ok()?) {
        return None;
    }
    let (mut x, mut z) = start;
    let mut going = heading;
    for step in 0..=steps {
        marks.push(Mark {
            x,
            z,
            width,
            ..Mark::default()
        });
        // Of three ways ahead, the easiest, and a wander of its own.
        let mut best = (f64::INFINITY, going);
        for turn in [-0.35, 0.0, 0.35] {
            let way = going + turn;
            let (nx, nz) = (x + mathf::sin(way) * stride, z + mathf::cos(way) * stride);
            let cost = rough(nx, nz) + 0.4 * (way - heading).abs();
            if cost < best.0 {
                best = (cost, way);
            }
        }
        going = best.1 + 0.25 * noise2(f64::from(step) * 0.21, 0.5, seed);
        x += mathf::sin(going) * stride;
        z += mathf::cos(going) * stride;
    }
    smoothed(&marks, 3)
}

/// How broad the band within the fresh water's finer grid's border is over
/// which its surface gives way to the far water grid's.
const SEAM: f64 = 1.5;

/// How much of the fresh water's finer grid `near`'s own surface shows at
/// `(x, z)`, the rest the far water grid's: none at its border, where the
/// two meet, and all of it a `SEAM` within.
pub(crate) fn seam(near: Laid, at: (f64, f64)) -> f64 {
    border_blend(near, at, (1.0, (0.0, SEAM)))
}

#[cfg(test)]
impl Land {
    /// A land traced on the one grid `far` over the square `(centre, reach)`:
    /// no finer grids, water, horizon, courses or road.
    pub(crate) fn plain(far: u32, (centre, reach): ((f64, f64), f64)) -> Self {
        Self {
            grids: Grids {
                far,
                nests: [None; NESTS],
                water: None,
                near_water: None,
                horizon: None,
                sea: None,
                centre,
                reach,
            },
            rivers: Courses::none(),
            form: None,
            ways: Ways::none(),
            crossings: Vec::new(),
            layout: None,
        }
    }
}

/// The far land as it stands before the finer levels are laid, which a scene
/// sites its eye by.
pub(crate) struct Survey<'a> {
    coarse: &'a Coarse,
    far: &'a Heightfield,
    plan: &'a Plan,
}

impl Survey<'_> {
    pub(crate) fn height(&self, x: f64, z: f64) -> f64 {
        self.far.height_at(x, z)
    }

    /// What one stands on at `(x, z)`: the ground, or the surface of the
    /// sea, a lake or a river where it lies above the ground.
    pub(crate) fn surface(&self, x: f64, z: f64) -> f64 {
        let ground = self.height(x, z);
        self.coarse.water_above((x, z), ground).unwrap_or(ground)
    }

    pub(crate) fn lie(&self, x: f64, z: f64) -> Lie {
        lie_on(self.far, x, z)
    }

    /// The surface of the sea, a lake or a river at `(x, z)`, where one
    /// stands above the ground there.
    pub(crate) fn water(&self, x: f64, z: f64) -> Option<f64> {
        self.coarse.water_above((x, z), self.height(x, z))
    }

    /// Whether `(x, z)` lies under water, fresh or salt.
    pub(crate) fn wet_at(&self, x: f64, z: f64) -> bool {
        let ground = self.height(x, z);
        drowned(self.plan.land.sea, ground) || self.coarse.water_above((x, z), ground).is_some()
    }

    /// The land's ways, the road through it the first of its surface's.
    pub(crate) fn ways(&self) -> &Ways {
        self.coarse.ways()
    }

    /// The road through the land, if it has one.
    pub(crate) fn road(&self) -> Option<Roadway> {
        self.plan.land.road
    }

    /// The countryside laid out over the land, where it is farmed.
    pub(crate) fn layout(&self) -> Option<&Layout> {
        self.coarse.layout()
    }

    pub(crate) fn rivers(&self) -> &Courses {
        self.coarse.rivers()
    }

    /// What shapes the land's rivers' channels, if it has any.
    pub(crate) fn form(&self) -> Option<Form> {
        self.coarse.form()
    }

    /// Where the land's road bridges its rivers.
    pub(crate) fn crossings(&self) -> &[Crossing] {
        self.coarse.crossings()
    }

    /// The land's middle, and how far it reaches from it either way.
    pub(crate) fn extent(&self) -> ((f64, f64), f64) {
        self.coarse.extent()
    }

    /// How far the finest grid laid about the eye will reach either way of
    /// its middle; nought if none is.
    pub(crate) fn finest_reach(&self) -> f64 {
        self.plan
            .nests
            .iter()
            .rev()
            .flatten()
            .next()
            .copied()
            .unwrap_or(0.0)
    }
}

/// Where a land's build stands.
#[allow(
    clippy::large_enum_variant,
    reason = "a build holds one step, and a box could not fail gracefully"
)]
#[derive(Debug)]
enum Step {
    /// Wearing the coarse land.
    Coarse(Wearing),
    /// Filling the horizon grid from the relief, from its row.
    Horizon(usize),
    /// Refining level 0 over the whole land: the far grid.
    Far(Refining),
    /// Waiting for the scene to say where the finer grids are to lie.
    Sited,
    /// Refining finer level `level` about the eye.
    Nest {
        level: usize,
        refining: Refining,
    },
    /// Blending grid `grid` — the far grid into the horizon's, then each
    /// finer grid into the one about it — from its row `row`.
    Blend {
        grid: usize,
        row: usize,
    },
    Water(usize),
    /// Filling the fresh water's finer grid from its row.
    NearWater(usize),
    /// Sealing the land's grids, the `next` of them on.
    Seal {
        next: usize,
        sealing: Sealing,
    },
    Done,
    Gone,
}

impl Step {
    /// The sealing of the land's grids, from the first.
    const SEALING: Self = Self::Seal {
        next: 0,
        sealing: Sealing::BEGUN,
    };
}

/// A land being built.
#[derive(Debug)]
pub(crate) struct Build {
    plan: Plan,
    lattices: Lattices,
    /// The coarse land, once worn.
    coarse: Option<Coarse>,
    far: u32,
    /// Each level's square of its lattice, and the scene's grid each finer
    /// one is laid into, once the scene sites them.
    squares: [Option<Placed>; LEVELS],
    nests: [Option<Laid>; NESTS],
    nest_fields: [Option<u32>; NESTS],
    water: Option<u32>,
    /// The fresh water's own finer grid, and where it is placed with the
    /// others.
    near_field: Option<u32>,
    near_water: Option<Laid>,
    horizon: Option<u32>,
    /// What each stage costs, measured, so the build's progress is a share of
    /// work: the coarse land, the horizon, each level, and the water.
    costs: Costs,
    stage: Step,
}

/// What each stage of a build costs, as the land generator weighs its own.
#[derive(Copy, Clone, Debug, Default)]
struct Costs {
    coarse: f64,
    horizon: f64,
    levels: [f64; LEVELS],
    blend: f64,
    water: f64,
}

/// What filling a vertex of a water grid, and blending a finer grid's
/// vertex, cost in the land generator's measure, a horizon's vertex costing
/// what a vertex of its relief read does (`RELIEF_NS`): a vertex of the
/// water's surface read, and one blended.
const WATER_NS: f64 = 30.0;
const BLEND_NS: f64 = 20.0;

/// Rows of a grid one core fills or blends in a unit of work, and of a water
/// grid at most twice as many: each of its vertices asks the rivers' index
/// and their channel.
const UNIT_ROWS: usize = 4;

impl Build {
    /// A land of `plan`, built into the scene's grids `fields`; `None` when
    /// the plan and the grids disagree, or the heap will not hold its working
    /// grids.
    pub(crate) fn new(plan: Plan, fields: Fields) -> Option<Self> {
        if plan.horizon.is_some() != fields.horizon.is_some() {
            return None;
        }
        let lattices = plan.land.lattices().ok()?;
        let depth = lattices.depth();
        if depth == 0 {
            return None;
        }
        for (index, (reach, field)) in plan.nests.iter().zip(fields.nests).enumerate() {
            let refined = index + 1 < depth;
            if reach.is_some() != field.is_some() || reach.is_some() != refined {
                return None;
            }
        }
        if plan.near_water.is_some() != fields.near_water.is_some()
            || (plan.near_water.is_some() && (plan.land.rivers.is_none() || fields.water.is_none()))
        {
            return None;
        }
        let wearing = Wearing::new(plan.land.clone()).ok()?;
        let costs = Costs::of(&plan, &lattices, (wearing.cost(), fields.water.is_some()))?;
        Some(Self {
            plan,
            lattices,
            coarse: None,
            far: fields.far,
            squares: [None; LEVELS],
            nests: [None; NESTS],
            nest_fields: fields.nests,
            water: fields.water,
            near_field: fields.near_water,
            near_water: None,
            horizon: fields.horizon,
            costs,
            stage: Step::Coarse(wearing),
        })
    }

    /// How far the build has come, as a share of its work: each stage
    /// weighed by what its work costs.
    pub(crate) fn done(&self) -> f64 {
        let costs = &self.costs;
        let levels = |before: usize| costs.levels.iter().take(before).sum::<f64>();
        let worn = costs.coarse + costs.horizon;
        let laid = worn + levels(LEVELS);
        let spent = match &self.stage {
            Step::Coarse(wearing) => costs.coarse * wearing.done(),
            Step::Horizon(row) => {
                let side = self.plan.horizon.map_or(1, |horizon| horizon.cells + 1);
                costs.coarse + costs.horizon * share(*row, side)
            }
            Step::Far(refining) => worn + costs.levels[0] * refining.done(),
            Step::Sited => worn + levels(1),
            Step::Nest { level, refining } => {
                let cost = costs.levels.get(*level).copied().unwrap_or(0.0);
                worn + levels(*level) + cost * refining.done()
            }
            Step::Blend { grid, .. } => laid + costs.blend * share(*grid, LEVELS),
            Step::Water(_) | Step::NearWater(_) => laid + costs.blend,
            Step::Seal { .. } | Step::Done => return 1.0,
            Step::Gone => 0.0,
        };
        (spent / costs.total().max(1.0)).min(1.0)
    }

    /// Whether a lake or a river stands anywhere on the land, once its coarse
    /// land is worn.
    pub(crate) fn watered(&self) -> bool {
        self.coarse.as_ref().is_some_and(Coarse::watered)
    }

    /// Whether the build waits for the scene to say where the finer grids lie.
    pub(crate) fn waiting(&self) -> bool {
        matches!(self.stage, Step::Sited)
    }

    /// The far land as it stands, before the finer levels are laid: what a
    /// scene sites its eye by.
    pub(crate) fn survey<'a>(&'a self, fields: &'a [Heightfield]) -> Option<Survey<'a>> {
        Some(Survey {
            coarse: self.coarse.as_ref()?,
            far: fields.get(self.far as usize)?,
            plan: &self.plan,
        })
    }

    /// Lay each finer level's square about `focus`, moved on by `lead` times
    /// its own half breadth, join the scene's own courses to the land — a
    /// footpath along `path`, the level beds of `cuttings` — and go on.
    pub(crate) fn site(
        &mut self,
        focus: (f64, f64),
        lead: (f64, f64),
        (path, cuttings): (Option<&[Mark]>, &[Vec<Mark>]),
    ) -> Option<()> {
        if !matches!(self.stage, Step::Sited) {
            return None;
        }
        let lattices = &self.lattices;
        for level in 1..LEVELS {
            let (Some(reach), Some(field)) = (
                self.plan.nests.get(level - 1).copied().flatten(),
                self.nest_fields.get(level - 1).copied().flatten(),
            ) else {
                continue;
            };
            let parent = self.squares.get(level - 1).copied().flatten()?;
            let step = lattices.level_step(level).ok()?;
            let square = sited(lattices, level, parent, (focus, lead, reach))?;
            let (centre, half) = middle(square, step)?;
            *self.squares.get_mut(level)? = Some(square);
            *self.nests.get_mut(level - 1)? = Some(Laid {
                field,
                centre,
                reach: half,
            });
        }
        let far = self.far_laid()?;
        let far_step = lattices.level_step(0).ok()?;
        if let (Some(near), Some(field)) = (self.plan.near_water, self.near_field) {
            // On a vertex of the far water grid it leaves its square out of,
            // and well within it.
            let (origin, _) = self.far_placing()?;
            let reach = (mathf::round(near.reach / far_step) * far_step).max(2.0 * far_step);
            let room = (far.reach - reach - 8.0 * far_step).max(0.0);
            let place = |at: f64, lead: f64, middle: f64, origin: f64| {
                snapped(at + lead * reach, (middle, room), (origin, far_step))
            };
            self.near_water = Some(Laid {
                field,
                centre: (
                    place(focus.0, lead.0, far.centre.0, origin.0),
                    place(focus.1, lead.1, far.centre.1, origin.1),
                ),
                reach,
            });
        }
        if let Some(outer) = self.nests.iter().flatten().next().copied() {
            let bounds = (
                (outer.centre.0 - outer.reach, outer.centre.1 - outer.reach),
                2.0 * outer.reach,
            );
            self.coarse.as_mut()?.join(path, cuttings, bounds).ok()?;
        }
        self.stage = self.next_nest(1)?;
        Some(())
    }

    /// The stage that refines finer level `level`, or what follows the last.
    fn next_nest(&self, level: usize) -> Option<Step> {
        match self.squares.get(level).copied().flatten() {
            Some(square) => Some(Step::Nest {
                level,
                refining: Refining::new(
                    self.coarse.as_ref()?,
                    (level, square),
                    Keeps::Everything,
                    &mut Unkept,
                )
                .ok()?,
            }),
            None => Some(Step::Blend { grid: 0, row: 0 }),
        }
    }

    /// The land, once built.
    pub(crate) fn finish(self) -> Option<Land> {
        if !matches!(self.stage, Step::Done) {
            return None;
        }
        let (centre, reach) = far_square(&self.plan.land)?;
        let coarse = self.coarse?;
        let form = coarse.form();
        let (rivers, ways, crossings, layout) = coarse.into_parts();
        Some(Land {
            grids: Grids {
                far: self.far,
                nests: self.nests,
                water: self.water,
                near_water: self.near_water,
                horizon: self.horizon,
                sea: self.plan.land.sea,
                centre,
                reach,
            },
            form,
            rivers,
            ways,
            crossings,
            layout,
        })
    }

    /// The far grid as a finer grid's parent sees it: its grid, its middle
    /// and half its breadth.
    fn far_laid(&self) -> Option<Laid> {
        let (centre, reach) = far_square(&self.plan.land)?;
        Some(Laid {
            field: self.far,
            centre,
            reach,
        })
    }

    /// Where the far grid's first vertex lies, and its step.
    fn far_placing(&self) -> Option<((f64, f64), f64)> {
        let square = self.squares.first().copied().flatten()?;
        placing(square, self.lattices.level_step(0).ok()?)
    }

    /// The scene's grid level `level` is laid into.
    fn field_of(&self, level: usize) -> Option<u32> {
        match level.checked_sub(1) {
            None => Some(self.far),
            Some(finer) => self
                .nests
                .get(finer)
                .copied()
                .flatten()
                .map(|laid| laid.field),
        }
    }

    /// Do the next unit of the build across `runner`, `fields` the scene's
    /// grids; whether the build is done or waits to be sited, or `None` when
    /// the heap refused it.
    pub(crate) fn step(
        &mut self,
        fields: &mut [Heightfield],
        runner: &dyn JobRunner,
    ) -> Option<bool> {
        let stage = core::mem::replace(&mut self.stage, Step::Gone);
        self.stage = match stage {
            Step::Coarse(mut wearing) => {
                if wearing.step(runner).ok()? {
                    self.coarse = Some(wearing.finish().ok()?);
                    if self.horizon.is_some() {
                        Step::Horizon(0)
                    } else {
                        self.toward_far()?
                    }
                } else {
                    Step::Coarse(wearing)
                }
            }
            Step::Horizon(row) => self.filling_horizon(fields, row, runner)?,
            Step::Far(mut refining) => {
                let coarse = self.coarse.as_ref()?;
                if refining.step(coarse, None, &mut Unkept, runner).ok()? {
                    self.lay(fields, 0, refining.finish().ok()?)?;
                    Step::Sited
                } else {
                    Step::Far(refining)
                }
            }
            Step::Sited => Step::Sited,
            Step::Nest {
                level,
                mut refining,
            } => {
                let coarse = self.coarse.as_ref()?;
                let parent = fields.get(self.field_of(level - 1)? as usize)?;
                let held = Parent {
                    square: self.squares.get(level - 1).copied().flatten()?,
                    heights: parent.heights(),
                    attributes: parent.attributes(),
                };
                if refining
                    .step(coarse, Some(held), &mut Unkept, runner)
                    .ok()?
                {
                    self.lay(fields, level, refining.finish().ok()?)?;
                    self.next_nest(level + 1)?
                } else {
                    Step::Nest { level, refining }
                }
            }
            Step::Blend { grid, row } => self.blending(fields, (grid, row), runner)?,
            Step::Water(row) => self.filling_water(fields, row, runner)?,
            Step::NearWater(row) => self.filling_near_water(fields, row, runner)?,
            Step::Seal { next, sealing } => self.sealing(fields, (next, sealing), runner)?,
            done @ Step::Done => done,
            Step::Gone => return None,
        };
        Some(matches!(self.stage, Step::Done | Step::Sited))
    }

    /// Level 0's refinement over the whole land begun.
    fn toward_far(&mut self) -> Option<Step> {
        let coarse = self.coarse.as_ref()?;
        let square = self.lattices.domain(0).ok()?;
        *self.squares.first_mut()? = Some(square);
        Some(Step::Far(
            Refining::new(coarse, (0, square), Keeps::Everything, &mut Unkept).ok()?,
        ))
    }

    /// Lay `piece`, refined, into level `level`'s grid among `fields`.
    fn lay(&self, fields: &mut [Heightfield], level: usize, piece: Piece) -> Option<()> {
        let step = self.lattices.level_step(level).ok()?;
        let side = usize::try_from(piece.square.size.0).ok()?;
        let (origin, step) = placing(piece.square, step)?;
        let grid =
            Heightfield::of_vertices((side, origin, step), (piece.heights, piece.attributes))?;
        *fields.get_mut(self.field_of(level)? as usize)? = grid;
        Some(())
    }

    /// A unit of the horizon grid's rows from `row`, from the relief and the
    /// coarsest of the land's detail.
    fn filling_horizon(
        &mut self,
        fields: &mut [Heightfield],
        row: usize,
        runner: &dyn JobRunner,
    ) -> Option<Step> {
        let field = fields.get_mut(self.horizon? as usize)?;
        if row == 0 && !field.carry_attributes() {
            return None;
        }
        let end = (row + UNIT_ROWS * runner.width().max(1)).min(field.side());
        let ((origin_x, origin_z), step) = field.placing();
        let beyond = self.coarse.as_ref()?.beyond(step)?;
        field.each_row(row..end, runner, &|(row, heights, kept)| {
            let z = origin_z + real(*row) * step;
            for (column, slot) in heights.iter_mut().enumerate() {
                let (height, attributes) = beyond.unworn((origin_x + real(column) * step, z));
                *slot = single(height);
                if let Some(carried) = kept.get_mut(column) {
                    *carried = attributes;
                }
            }
        });
        if end < field.side() {
            Some(Step::Horizon(end))
        } else {
            self.toward_far()
        }
    }

    /// A unit of grid `grid`'s rows from `row` blended into the grid about it
    /// across `runner` — the far grid into the horizon's, each finer grid
    /// into its parent — and the grid about it left out beneath it once all
    /// of it is.
    fn blending(
        &self,
        fields: &mut [Heightfield],
        (grid, row): (usize, usize),
        runner: &dyn JobRunner,
    ) -> Option<Step> {
        let next = || {
            if grid + 1 < LEVELS {
                Some(Step::Blend {
                    grid: grid + 1,
                    row: 0,
                })
            } else {
                Some(Step::Water(0))
            }
        };
        let (laid, around, band) = match grid.checked_sub(1) {
            None => match self.horizon {
                Some(horizon) => (self.far_laid()?, horizon, FAR_BAND),
                None => return next(),
            },
            Some(finer) => match self.nests.get(finer).copied().flatten() {
                Some(laid) => (laid, self.field_of(grid - 1)?, NEST_BAND),
                None => return next(),
            },
        };
        let (field, parent) = apart(fields, laid.field as usize, around as usize)?;
        let side = field.side();
        let end = (row + UNIT_ROWS * runner.width().max(1)).min(side);
        let placing = field.placing();
        let parent_step = parent.placing().1;
        field.each_row(row..end, runner, &|(row, heights, _)| {
            blend_row(parent, (*row, heights), placing, &|at| {
                border_blend(laid, at, (parent_step, band))
            });
        });
        if end < side {
            return Some(Step::Blend { grid, row: end });
        }
        leave_out(laid, fields.get_mut(around as usize)?, 1);
        next()
    }

    /// A unit of the fresh-water grid's rows from `row`.
    fn filling_water(
        &self,
        fields: &mut [Heightfield],
        row: usize,
        runner: &dyn JobRunner,
    ) -> Option<Step> {
        let Some(index) = self.water else {
            return Some(Step::SEALING);
        };
        let coarse = self.coarse.as_ref()?;
        if row == 0 && !self.watered() {
            // A dry land's water keeps its place as a grid of one empty cell.
            for kept in [Some(index), self.near_field].into_iter().flatten() {
                fields.get_mut(kept as usize)?.heights_mut().fill(ABSENT);
            }
            return Some(Step::SEALING);
        }
        let field = fields.get_mut(index as usize)?;
        if row == 0 {
            let (origin, step) = self.far_placing()?;
            let cells = usize::try_from(self.squares.first().copied().flatten()?.size.0)
                .ok()?
                .checked_sub(1)?;
            *field = Heightfield::new(cells, origin, step, false)?;
        }
        let end = (row + water_rows(field.side()) * runner.width().max(1)).min(field.side());
        fill_water(coarse, field, row..end, runner);
        if end < field.side() {
            return Some(Step::Water(end));
        }
        Some(match self.near_water {
            Some(near) => {
                // The finer grid meets it at its border rather than sharing a
                // ring with it: a ray must cross one water's surface, not two.
                leave_out(near, field, 0);
                Step::NearWater(0)
            }
            None => Step::SEALING,
        })
    }

    /// A unit of the fresh water's finer grid's rows from `row`, its surface
    /// as the far water grid has it, to be shaped by the scene; the grid
    /// carries what the scene's shaping leaves on the water.
    fn filling_near_water(
        &self,
        fields: &mut [Heightfield],
        row: usize,
        runner: &dyn JobRunner,
    ) -> Option<Step> {
        let (Some(near), Some(cells)) =
            (self.near_water, self.plan.near_water.map(|near| near.cells))
        else {
            return Some(Step::SEALING);
        };
        let coarse = self.coarse.as_ref()?;
        let (field, far) = apart(fields, near.field as usize, self.water? as usize)?;
        if row == 0 {
            let origin = (near.centre.0 - near.reach, near.centre.1 - near.reach);
            *field = Heightfield::new(cells, origin, 2.0 * near.reach / real(cells), false)?;
            if !field.carry_attributes() {
                return None;
            }
        }
        let end = (row + water_rows(field.side()) * runner.width().max(1)).min(field.side());
        fill_water(coarse, field, row..end, runner);
        seam_rows(near, far, field, row..end, runner);
        Some(if end >= field.side() {
            Step::SEALING
        } else {
            Step::NearWater(end)
        })
    }

    /// A unit of the land's grids sealed, from the `next` of them on.
    fn sealing(
        &self,
        fields: &mut [Heightfield],
        (next, mut sealing): (usize, Sealing),
        runner: &dyn JobRunner,
    ) -> Option<Step> {
        let nests = self.nests.iter().flatten().map(|laid| laid.field);
        let laid = [self.far]
            .into_iter()
            .chain(nests)
            .chain(self.water)
            .chain(self.near_water.map(|near| near.field))
            .chain(self.horizon);
        let Some(index) = laid.into_iter().nth(next) else {
            return Some(Step::Done);
        };
        Some(if sealing.step(fields.get_mut(index as usize)?, runner) {
            Step::Seal {
                next: next + 1,
                sealing: Sealing::BEGUN,
            }
        } else {
            Step::Seal { next, sealing }
        })
    }
}

impl Costs {
    /// What each stage of a build of `plan` on `lattices` costs, the coarse
    /// land's `coarse`, a water grid laid if `watered`; `None` for a plan
    /// whose levels do not fit.
    fn of(plan: &Plan, lattices: &Lattices, (coarse, watered): (f64, bool)) -> Option<Self> {
        let domain = lattices.domain(0).ok()?;
        let vertices = |side: u32| {
            let side = real(usize::try_from(side).unwrap_or(usize::MAX));
            side * side
        };
        let horizon = plan.horizon.map_or(0.0, |horizon| {
            real((horizon.cells + 1) * (horizon.cells + 1)) * RELIEF_NS
        });
        let mut levels = [0.0; LEVELS];
        let mut blend = vertices(domain.size.0) * BLEND_NS;
        if let Some(cost) = levels.first_mut() {
            *cost = lattices.cost(0, domain).ok()?;
        }
        for (level, reach) in plan.nests.iter().enumerate() {
            let (Some(reach), Some(cost)) = (reach, levels.get_mut(level + 1)) else {
                continue;
            };
            // Where the scene will site it is not known yet; its cost hangs
            // only on its size, which its reach and its level's step fix, so
            // it is weighed as if laid in the land's middle.
            let side = nest_cells(lattices, level + 1, *reach)?.checked_add(1)?;
            let within = lattices.domain(level + 1).ok()?;
            let middle = Placed::square(
                (
                    within.origin.0 + i64::from(within.size.0.saturating_sub(side) / 2),
                    within.origin.1 + i64::from(within.size.1.saturating_sub(side) / 2),
                ),
                side.min(within.size.0),
            );
            *cost = lattices.cost(level + 1, middle).ok()?;
            blend += vertices(side) * BLEND_NS;
        }
        let water = if watered {
            vertices(domain.size.0) * WATER_NS
                + plan.near_water.map_or(0.0, |near| {
                    real((near.cells + 1) * (near.cells + 1)) * WATER_NS
                })
        } else {
            0.0
        };
        Some(Self {
            coarse,
            horizon,
            levels,
            blend,
            water,
        })
    }

    /// What the whole build costs.
    fn total(&self) -> f64 {
        self.coarse + self.horizon + self.levels.iter().sum::<f64>() + self.blend + self.water
    }
}

/// How many of its own cells a finer level's square reaching `reach` either
/// way spans: whole cells of its parent, its step about a 512th of `reach`.
fn nest_cells(lattices: &Lattices, level: usize, reach: f64) -> Option<u32> {
    let step = lattices.level_step(level).ok()?;
    let ratio = f64::from(lattices.ratio(level).ok()?);
    let cells = (mathf::round(2.0 * reach / step / ratio) * ratio).max(8.0 * ratio);
    u32::try_from(mathf::round_i32(cells)).ok()
}

/// The square of finer level `level` a scene sites about `focus`, moved on by
/// `lead` times its half breadth `reach`: on its parent's vertices, so its
/// border runs along the parent's cell edges and the two meet there exactly,
/// and kept within its parent's square `parent` with all a piece reads of it,
/// and a band to blend across besides.
fn sited(
    lattices: &Lattices,
    level: usize,
    parent: Placed,
    (focus, lead, reach): ((f64, f64), (f64, f64), f64),
) -> Option<Placed> {
    let step = lattices.level_step(level).ok()?;
    let ratio = i64::from(lattices.ratio(level).ok()?);
    let cells = i64::from(nest_cells(lattices, level, reach)?);
    // The parent's vertices a piece reads beyond its own, and the band of
    // the parent's cells the blend runs across.
    let margin = if level == 1 {
        BLEND_CELLS * ratio
    } else {
        i64::from(lattices.halo(level).ok()?) + (BLEND_CELLS + 3) * ratio
    };
    // On the parent's vertices: whole ratios of the child's.
    let within = |origin: i64, size: u32| {
        let low = (origin * ratio + margin + ratio - 1).div_euclid(ratio) * ratio;
        let high =
            ((origin + i64::from(size) - 1) * ratio - margin - cells).div_euclid(ratio) * ratio;
        (low <= high).then_some((low, high))
    };
    let along = |at: f64, lead: f64, (low, high): (i64, i64)| {
        let first = (at + lead * reach) / step - 0.5 * f64::from(i32::try_from(cells).ok()?);
        let snapped = i64::from(mathf::round_i32(mathf::round(
            first / f64::from(i32::try_from(ratio).ok()?),
        ))) * ratio;
        Some(snapped.clamp(low, high))
    };
    let origin = (
        along(focus.0, lead.0, within(parent.origin.0, parent.size.0)?)?,
        along(focus.1, lead.1, within(parent.origin.1, parent.size.1)?)?,
    );
    let square = Placed::square(origin, u32::try_from(cells).ok()?.checked_add(1)?);
    // Whatever the snapping did, a piece that reads beyond its parent is
    // refused rather than read short.
    if level > 1 && !parent.holds(lattices.needs(level, square).ok()?) {
        return None;
    }
    Some(square)
}

/// The middle of `square` on a lattice `step` apart, and half its breadth.
fn middle(square: Placed, step: f64) -> Option<((f64, f64), f64)> {
    let ((x, z), _) = placing(square, step)?;
    let half = 0.5 * f64::from(square.size.0.checked_sub(1)?) * step;
    Some(((x + half, z + half), half))
}

/// Where `square`'s first vertex lies on a lattice `step` apart, and the step.
fn placing(square: Placed, step: f64) -> Option<((f64, f64), f64)> {
    let place = |index: i64| Some(f64::from(i32::try_from(index).ok()?) * step);
    Some(((place(square.origin.0)?, place(square.origin.1)?), step))
}

/// How many of a parent's cells inside a finer grid's border the band runs
/// where the finer grid gives way to it.
const BLEND_CELLS: i64 = 8;

/// How far inside a finer grid's border, in the cells of the grid about it,
/// its own surface begins to show, and from where it shows alone.
const NEST_BAND: (f64, f64) = (1.0, 8.0);
/// The same for the far grid within the horizon's far larger cells.
const FAR_BAND: (f64, f64) = (0.5, 3.0);

/// How much of a finer grid's own surface shows at `(x, z)`, the rest the
/// surface of the grid about it, whose cells are `parent_step` across: all
/// of it within, none at its border, over the `band` between.
fn border_blend(laid: Laid, (x, z): (f64, f64), (parent_step, band): (f64, (f64, f64))) -> f64 {
    smoothstep(band.0, band.1, laid.inside((x, z)) / parent_step)
}

/// `value` held within `room` of `middle`, on the nearest vertex of a grid
/// `step` apart from `from`.
fn snapped(value: f64, (middle, room): (f64, f64), (from, step): (f64, f64)) -> f64 {
    let kept = value.clamp(middle - room, middle + room);
    from + mathf::round((kept - from) / step) * step
}

/// Hold row `row` of a finer grid, lying as `placing` says, to the surface
/// of the grid about it, `parent`, by how much of its own surface `shows` at
/// each place; a height either grid leaves out is left as it is.
fn blend_row(
    parent: &Heightfield,
    (row, heights): (usize, &mut [f32]),
    placing: ((f64, f64), f64),
    shows: &dyn Fn((f64, f64)) -> f64,
) {
    let ((origin_x, origin_z), step) = placing;
    let z = origin_z + real(row) * step;
    for (column, slot) in heights.iter_mut().enumerate() {
        let x = origin_x + real(column) * step;
        let own = shows((x, z));
        if own < 1.0 {
            let flat = single(parent.height_at(x, z));
            if slot.is_finite() && flat.is_finite() {
                *slot = flat + (*slot - flat) * single(own);
            }
        }
    }
}

/// Leave out of `parent` the cells the finer grid `laid` describes covers,
/// but for the `kept` rings of them along its border where both hold the
/// same surface.
fn leave_out(laid: Laid, parent: &mut Heightfield, kept: usize) {
    let ((origin_x, origin_z), step) = parent.placing();
    let cell = |value: f64, origin: f64| {
        usize::try_from(mathf::round_i32(mathf::round((value - origin) / step))).unwrap_or(0)
    };
    let (x0, x1) = (
        cell(laid.centre.0 - laid.reach, origin_x) + kept,
        cell(laid.centre.0 + laid.reach, origin_x).saturating_sub(kept),
    );
    let (z0, z1) = (
        cell(laid.centre.1 - laid.reach, origin_z) + kept,
        cell(laid.centre.1 + laid.reach, origin_z).saturating_sub(kept),
    );
    if x0 < x1 && z0 < z1 {
        parent.leave_out(x0..x1, z0..z1);
    }
}

/// Fill the fresh-water grid `field`'s rows `rows` across `runner`: a lake's
/// surface over its basin and a little beyond, a river's along its channel,
/// and nothing elsewhere.
fn fill_water(
    coarse: &Coarse,
    field: &mut Heightfield,
    rows: Range<usize>,
    runner: &dyn JobRunner,
) {
    let side = field.side();
    let ((origin_x, origin_z), step) = field.placing();
    let (heights, _) = field.rows_mut(rows.clone());
    tairix_parallel::for_each_band(runner, heights, (rows.start, side), &|row, band| {
        let z = origin_z + real(row) * step;
        for (column, slot) in band.iter_mut().enumerate() {
            let level = coarse.water_surface(origin_x + real(column) * step, z, step);
            *slot = if level.is_finite() {
                single(level)
            } else {
                ABSENT
            };
        }
    });
}

/// Rows of a water grid `side` vertices a side a core fills or shapes in a
/// unit.
pub(crate) fn water_rows(side: usize) -> usize {
    crate::band::unit_rows(side).min(2 * UNIT_ROWS)
}

/// Hold rows `rows` of the fresh water's finer grid `near`, `field`, to the
/// far water grid `far` across its seam, wherever both hold water; across
/// `runner`.
fn seam_rows(
    near: Laid,
    far: &Heightfield,
    field: &mut Heightfield,
    rows: Range<usize>,
    runner: &dyn JobRunner,
) {
    let side = field.side();
    let placing = field.placing();
    let (heights, _) = field.rows_mut(rows.clone());
    tairix_parallel::for_each_band(runner, heights, (rows.start, side), &|row, band| {
        blend_row(far, (row, band), placing, &|at| seam(near, at));
    });
}

#[cfg(test)]
#[path = "land_tests.rs"]
mod tests;
