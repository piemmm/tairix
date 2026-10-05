//! Land: a landform worn down by the water that drains it, carved by its
//! rivers, graded for its roads, and traced as a grid out to the horizon and
//! a far finer one about the eye.
//!
//! The work runs in stages over a coarse grid first, where water's work is
//! done: Priority-Flood drainage, implicit stream-power incision along it and
//! hillslope creep, pass after pass, cut the valleys and the dendritic ridges
//! between them; a last drainage says where the rivers run and the lakes
//! stand, and the roads are routed where the ground lets them go. The far
//! grid is then filled from the worn land, its detail added, its river beds
//! and road beds cut, and droplets run over it for the rills and fans no
//! coarser pass can make. About the eye, once the scene says where that is, a
//! near grid refines the far one again, down to the hummocks underfoot, and
//! meets it along its border exactly. A land's vertices carry what it is like
//! there — wet, worn or built up, on a road or a path, how much grows — which
//! its shading and its planting read.
//!
//! Every stage runs a bounded unit at a time, so a caller answering frames
//! spreads the land over as many as it takes.

use alloc::vec::Vec;
use core::f64::consts::TAU;
use core::ops::Range;

use tairix_parallel::JobRunner;
use tairix_terrain::drainage::{accumulate, downstream, route, Flood, Network};
use tairix_terrain::droplet::{Droplets, Erosion};
use tairix_terrain::hillslope::{self, Diffusion, Shed, Talus};
use tairix_terrain::incision::{incise_implicit, Implicit};
use tairix_terrain::route::{Routed, Router};
use tairix_terrain::{FlowDir, Grid as Square};
use tairix_util::{fallible, mathf};

use crate::band;
use crate::channel::{self, Banked, Form, Section, Station, BROADEST};
use crate::course::{smoothed, Courses, Mark, Nearest, Reach};
use crate::heightfield::{apart, Heightfield, Sealing, ABSENT};
use crate::noise::{fbm2, noise2, ridged2, smoothstep};
use crate::terrain::Terrain;
use crate::vector::{byte, power, real, share, single, Vec3};

/// How water wears a land.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Wear {
    /// Passes of drainage, incision and creep.
    pub(crate) passes: u32,
    /// Incision per pass, as erodibility times a time step.
    pub(crate) incision: f64,
    /// The share of the gap to its neighbours' mean a sample creeps each pass.
    pub(crate) creep: f64,
    /// The steepest slope that stands, as rise over run; steeper slumps.
    pub(crate) repose: f64,
    /// The share of a basin's depth below the level it would spill at that
    /// sediment fills each pass, so pits silt up into flats and only the
    /// deepest hold lakes.
    pub(crate) infill: f64,
    /// Hard beds every `spacing` metres up, which wear `hardness` times as
    /// slowly and so stand as cliffs above benches: a canyon's.
    pub(crate) strata: Option<(f64, f64)>,
}

/// How a land's rivers run.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Rivers {
    /// The ground a stream must drain, in square metres, to run as a river.
    pub(crate) catchment: f64,
    /// A river's breadth where it begins, and how it grows with the square
    /// root of what it drains past that.
    pub(crate) width: f64,
    /// How far it wanders from side to side across level ground, in widths.
    pub(crate) meander: f64,
    /// The share of its channel's depth its water fills: one in spate, less
    /// in a dry season, the margins of its bed left bare.
    pub(crate) flowing: f64,
    /// The share of its reaches whose fall bedded rock gathers at a ledge,
    /// and of its banks rock outcrops in.
    pub(crate) ledges: f64,
    pub(crate) outcrops: f64,
}

impl Rivers {
    /// What shapes the channels of a land built under `seed`.
    pub(crate) fn form(&self, seed: u32) -> Form {
        Form {
            flowing: self.flowing,
            ledges: self.ledges,
            outcrops: self.outcrops,
            seed: seed ^ CHANNELS,
        }
    }
}

/// The key a land's channels are drawn under, against its own.
const CHANNELS: u32 = 0xc4a7;

/// A finer grid of the fresh water's surface laid about the eye, where a
/// scene shapes the surface itself: its half breadth, and its cells a side.
#[derive(Copy, Clone, Debug)]
pub(crate) struct NearWater {
    pub(crate) reach: f64,
    pub(crate) cells: usize,
}

/// What a road is made of.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Surface {
    /// Tarmac, lined.
    Tarmac,
    /// Loose stone.
    Gravel,
    /// A track of two ruts and the grass between them.
    Track,
}

/// A road across a land: what it is made of and how broad.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Roadway {
    pub(crate) surface: Surface,
    pub(crate) width: f64,
    /// Which way across the land it runs, in radians.
    pub(crate) heading: f64,
}

/// Everything a land is made from, chosen before any of it is built.
#[derive(Clone, Debug)]
pub(crate) struct Plan {
    /// The relief water wears.
    pub(crate) relief: Terrain,
    /// Half the breadth of the land built, about the relief's centre.
    pub(crate) reach: f64,
    /// The level below which the sea stands, if it does.
    pub(crate) sea: Option<f64>,
    pub(crate) wear: Wear,
    pub(crate) rivers: Option<Rivers>,
    pub(crate) road: Option<Roadway>,
    /// How rough the land is below the coarse grid's step, in metres.
    pub(crate) roughness: f64,
    /// How much of that roughness is sharp-crested ridges, as bare rock and
    /// gullied ground break, rather than the rounded hummocks of ground
    /// under a mantle of soil, sand or snow.
    pub(crate) ridges: f64,
    /// Droplets run over the far grid, per vertex.
    pub(crate) droplets: f64,
    /// Vertices along each side of the coarse and far grids, less one.
    pub(crate) cells: (usize, usize),
    /// The finer grids laid about the eye, coarsest first.
    pub(crate) nests: [Option<Nest>; NESTS],
    /// The fresh water's own finer grid about the eye, if the scene shapes
    /// its surface there.
    pub(crate) near_water: Option<NearWater>,
    /// The relief running on past the far grid out to the horizon, if it
    /// does rather than settling to a rim.
    pub(crate) horizon: Option<Horizon>,
    /// The height snow lies from, if any does.
    pub(crate) snow_line: Option<f64>,
    /// A hollow water keeps, sediment never filling it: its middle, and how
    /// far about it the infill spares.
    pub(crate) pond: Option<((f64, f64), f64)>,
    /// How much of the ground grows green where water and slope allow:
    /// all of it in a temperate land, a little in a desert, where only the
    /// wetter ground by its streams grows more.
    pub(crate) growth: f64,
    pub(crate) seed: u32,
}

/// A coarse grid of the relief out to the horizon, the far grid laid within
/// it: about how far it reaches either way, and its cells a side.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Horizon {
    pub(crate) reach: f64,
    pub(crate) cells: usize,
}

impl Plan {
    /// How far apart the far grid's vertices lie, and the fresh water's.
    pub(crate) fn far_step(&self) -> f64 {
        2.0 * self.reach / real(self.cells.1)
    }

    /// Where the horizon grid lies — its first vertex and its step — and its
    /// cells a side, its reach nudged so the far grid's border runs along its
    /// cell edges; `None` if the land has none, or one too small to leave a
    /// cell either side of the border.
    pub(crate) fn horizon_placing(&self) -> Option<((f64, f64), f64)> {
        let horizon = self.horizon.filter(|horizon| horizon.cells >= 4)?;
        let half = real(horizon.cells) / 2.0;
        // Whole cells between the far grid's edge and the horizon's.
        let between =
            mathf::round(half * (1.0 - self.reach / horizon.reach)).clamp(1.0, half - 1.0);
        let reach = self.reach / (1.0 - between / half);
        let centre = self.relief.centre;
        Some((
            (centre.0 - reach, centre.1 - reach),
            2.0 * reach / real(horizon.cells),
        ))
    }
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

/// How many finer grids a land lays about the eye, each within the last.
pub(crate) const NESTS: usize = 2;

/// A finer grid laid about the eye, within the grid before it.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Nest {
    /// Half its breadth.
    pub(crate) reach: f64,
    /// Cells along each side: a power of two.
    pub(crate) cells: usize,
    /// Droplets run over it, per vertex.
    pub(crate) droplets: f64,
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

/// Where a river runs out into standing water and fans the silt it carries
/// out before it: its mouth, the way it runs out, how far the fan spreads,
/// how broad the river is there, and the water's level.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Delta {
    pub(crate) apex: (f64, f64),
    pub(crate) toward: (f64, f64),
    pub(crate) length: f64,
    pub(crate) width: f64,
    pub(crate) level: f64,
}

/// Where the road crosses a river on a bridge: the road's marks either side
/// of the water, the deck's level, the water's, and the river's breadth.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Crossing {
    pub(crate) from: Mark,
    pub(crate) to: Mark,
    pub(crate) deck: f64,
    pub(crate) water: f64,
    pub(crate) width: f64,
}

/// The land once built, as a scene's grids hold it.
#[derive(Clone, Debug)]
pub(crate) struct Land {
    /// The scene's grids that hold it: the far land, the finer grids laid
    /// about the eye, coarsest first, the fresh water's surface, and the land
    /// beyond out to the horizon.
    pub(crate) far: u32,
    pub(crate) nests: [Option<Laid>; NESTS],
    pub(crate) water: Option<u32>,
    /// The fresh water's own finer grid about the eye, if it has one.
    pub(crate) near_water: Option<Laid>,
    pub(crate) horizon: Option<u32>,
    /// The rivers, and what shapes their channels.
    pub(crate) rivers: Courses,
    pub(crate) form: Option<Form>,
    pub(crate) roads: Courses,
    pub(crate) road: Option<Roadway>,
    /// Where the road bridges its rivers.
    pub(crate) crossings: Vec<Crossing>,
    pub(crate) sea: Option<f64>,
    /// The square the far land covers.
    pub(crate) centre: (f64, f64),
    pub(crate) reach: f64,
}

/// What a place on the land is like.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(crate) struct Lie {
    pub(crate) height: f64,
    /// How upright the ground stands: its normal's upward part.
    pub(crate) upright: f64,
    /// How wet, `0.0..=1.0`.
    pub(crate) wet: f64,
    /// What the water laid down there, or wore away where negative,
    /// `-1.0..=1.0`.
    pub(crate) sediment: f64,
    /// How much of a road, and of a path, the place lies on.
    pub(crate) road: f64,
    pub(crate) path: f64,
    /// How much can grow there, `0.0..=1.0`.
    pub(crate) green: f64,
}

impl Land {
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

    /// The fresh water's surface at `(x, z)`, if water stands there.
    pub(crate) fn water(&self, fields: &[Heightfield], x: f64, z: f64) -> Option<f64> {
        self.water_level(fields, x, z)
            .filter(|&level| level > self.height(fields, x, z))
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

    /// What one stands on at `(x, z)`: the ground, or the fresh water's
    /// surface where it lies above the ground.
    pub(crate) fn surface(&self, fields: &[Heightfield], x: f64, z: f64) -> f64 {
        self.water(fields, x, z)
            .unwrap_or(f64::NEG_INFINITY)
            .max(self.height(fields, x, z))
    }

    /// Whether `(x, z)` lies under water, fresh or salt.
    pub(crate) fn wet_at(&self, fields: &[Heightfield], x: f64, z: f64) -> bool {
        let ground = self.height(fields, x, z);
        self.sea.is_some_and(|sea| ground < sea + 0.2) || self.water(fields, x, z).is_some()
    }
}

/// What `grid` says the land is like at `(x, z)`.
fn lie_on(grid: &Heightfield, x: f64, z: f64) -> Lie {
    let (_, step) = grid.placing();
    let height = grid.height_at(x, z);
    let dx = (grid.height_at(x + step, z) - grid.height_at(x - step, z)) / (2.0 * step);
    let dz = (grid.height_at(x, z + step) - grid.height_at(x, z - step)) / (2.0 * step);
    let [wet, sediment, lane, green] = grid.attributes_at(x, z);
    let (road, path) = decode_lane(lane);
    Lie {
        height,
        upright: 1.0 / mathf::sqrt(1.0 + dx * dx + dz * dz),
        wet,
        sediment: 2.0 * sediment - 1.0,
        road,
        path,
        green,
    }
}

/// The far land as it stands before the near land is laid, which a scene
/// sites its eye by.
pub(crate) struct Survey<'a> {
    build: &'a Build,
    far: &'a Heightfield,
}

impl Survey<'_> {
    pub(crate) fn height(&self, x: f64, z: f64) -> f64 {
        self.far.height_at(x, z)
    }

    /// What one stands on at `(x, z)`: the ground, or a lake's or a river's
    /// surface where it lies above the ground.
    pub(crate) fn surface(&self, x: f64, z: f64) -> f64 {
        self.water(x, z)
            .unwrap_or(f64::NEG_INFINITY)
            .max(self.height(x, z))
    }

    pub(crate) fn lie(&self, x: f64, z: f64) -> Lie {
        lie_on(self.far, x, z)
    }

    /// The surface of a lake or a river at `(x, z)`, where one stands above
    /// the ground there.
    pub(crate) fn water(&self, x: f64, z: f64) -> Option<f64> {
        let build = self.build;
        let ground = self.height(x, z);
        let lake = build
            .lakes
            .get(build.sample_at(x, z))
            .map(|&level| f64::from(level));
        let river = build.form().and_then(|form| {
            let near = build.rivers.nearest(x, z)?;
            let section = Section::new(&Station::of(&near), &form);
            (near.distance < section.half).then_some(section.water)
        });
        lake.into_iter()
            .chain(river)
            .filter(|level| level.is_finite() && *level > ground)
            .reduce(f64::max)
    }

    /// Whether `(x, z)` lies under water, fresh or salt.
    pub(crate) fn wet_at(&self, x: f64, z: f64) -> bool {
        let ground = self.height(x, z);
        self.build.plan.sea.is_some_and(|sea| ground < sea + 0.2) || self.water(x, z).is_some()
    }

    pub(crate) fn roads(&self) -> &Courses {
        &self.build.roads
    }

    pub(crate) fn rivers(&self) -> &Courses {
        &self.build.rivers
    }

    /// What shapes the land's rivers' channels, if it has any.
    pub(crate) fn form(&self) -> Option<Form> {
        self.build.form()
    }

    /// Where the land's road bridges its rivers.
    pub(crate) fn crossings(&self) -> &[Crossing] {
        &self.build.crossings
    }

    /// The land's middle, and how far it reaches from it either way.
    pub(crate) fn extent(&self) -> ((f64, f64), f64) {
        (self.build.plan.relief.centre, self.build.plan.reach)
    }

    /// How far the finest grid laid about the eye will reach either way of
    /// its middle; nought if none is.
    pub(crate) fn finest_reach(&self) -> f64 {
        self.build
            .nests
            .iter()
            .rev()
            .flatten()
            .next()
            .map_or(0.0, |laid| laid.reach)
    }
}

/// A lane attribute's share of road and of path.
pub(crate) fn decode_lane(lane: f64) -> (f64, f64) {
    let byte = lane * 255.0;
    if byte >= 127.5 {
        (((byte - 128.0) / 127.0).clamp(0.0, 1.0), 0.0)
    } else {
        (0.0, (byte / 127.0).clamp(0.0, 1.0))
    }
}

/// A lane attribute of `road` and `path`, the road winning where both lie.
fn encode_lane(road: f64, path: f64) -> u8 {
    if road > 0.01 {
        byte((128.0 + 127.0 * road.clamp(0.0, 1.0)) / 255.0)
    } else {
        byte(127.0 / 255.0 * path.clamp(0.0, 1.0))
    }
}

/// Where a land's build stands.
#[derive(Debug)]
enum Step {
    /// Filling the coarse grid from the relief.
    Relief(usize),
    /// Filling the horizon grid from the relief.
    Horizon(usize),
    /// Solving the drainage of pass `pass`; the last pass's is the final
    /// drainage the rivers are read from.
    Flood {
        pass: u32,
        flood: Flood,
    },
    Route {
        pass: u32,
        row: u32,
        flood: Flood,
        flow: Vec<FlowDir>,
    },
    Accumulate {
        pass: u32,
        end: usize,
        network: Network,
    },
    Incise {
        pass: u32,
        start: usize,
        network: Network,
    },
    Creep {
        pass: u32,
        row: u32,
        before: Vec<f64>,
    },
    Slump {
        pass: u32,
        row: u32,
        before: Vec<f64>,
        sheds: Vec<Shed>,
        settle: bool,
    },
    /// Reading the rivers and lakes from the final drainage.
    Waters {
        network: Network,
    },
    /// Routing the road.
    Road {
        network: Network,
        router: Router,
    },
    /// Filling the far grid.
    Far {
        network: Network,
        row: usize,
    },
    /// Running droplets over the far grid.
    FarDroplets {
        erosion: Erosion,
        before: Vec<f32>,
        flux: Vec<f32>,
    },
    /// The far grid's sediment and wetness, from what the droplets did.
    FarSettle {
        row: usize,
        before: Vec<f32>,
        flux: Vec<f32>,
    },
    /// Waiting for the scene to say where the finer grids are to lie.
    Sited,
    /// Filling finer grid `level` from its row `row`.
    Nest {
        level: usize,
        row: usize,
    },
    /// Running droplets over finer grid `level`, its heights as they stood
    /// before them kept.
    NestDroplets {
        level: usize,
        erosion: Erosion,
        before: Vec<f32>,
    },
    /// Holding finer grid `level`'s channels to what they stood at before the
    /// droplets, its border to the grid about it, and its clearing level, from
    /// row `row`.
    NestSettle {
        level: usize,
        row: usize,
        before: Vec<f32>,
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
    square: Square,
    /// The coarse grid's heights, and what they were before any wear.
    height: Vec<f64>,
    original: Vec<f64>,
    /// Where the coarse grid lies: its first vertex, and its step.
    origin: (f64, f64),
    step: f64,
    rivers: Courses,
    roads: Courses,
    paths: Courses,
    deltas: Vec<Delta>,
    crossings: Vec<Crossing>,
    /// The lakes: the water surface standing at or beside each coarse
    /// sample, where one does.
    lakes: Vec<f32>,
    far: u32,
    /// The finer grids, placed once the scene sites them.
    nests: [Option<Laid>; NESTS],
    water: Option<u32>,
    /// The fresh water's own finer grid, placed with the others.
    near_water: Option<Laid>,
    horizon: Option<u32>,
    stage: Step,
}

/// Rows of a grid one core fills in a unit of work, and of a water grid at
/// most twice as many: each of its vertices asks the rivers' index and their
/// channel.
const UNIT_ROWS: usize = 4;
/// Samples a flood reaches, a routing or accumulation or incision walks, in
/// a unit of work.
const UNIT_SAMPLES: usize = 40_000;
/// Samples an A\* search settles in a unit of work.
const UNIT_SETTLED: usize = 20_000;

/// What one item of each kind of a build's work costs, in nanoseconds on a
/// desktop core preparing across eight threads: a vertex filled from the
/// relief, a coarse sample worn through one pass of drainage, incision, creep
/// and slumping, a far or finer vertex filled, a droplet run, a vertex settled
/// after the droplets, and a fresh-water vertex. Measured; only their
/// proportions matter, to weigh the stages of a build's progress.
const RELIEF_NS: f64 = 80.0;
const WEAR_NS: f64 = 180.0;
const FILL_NS: f64 = 60.0;
const DROPLET_NS: f64 = 1_040.0;
const SETTLE_NS: f64 = 50.0;
const WATER_NS: f64 = 30.0;

impl Build {
    /// A land of `plan`, built into the scene's grids `fields`; `None` when
    /// the plan and the grids disagree, or the heap will not hold its working
    /// grids.
    pub(crate) fn new(plan: Plan, fields: Fields) -> Option<Self> {
        if plan.horizon.is_some() != fields.horizon.is_some() {
            return None;
        }
        let side = plan.cells.0 + 1;
        let square = Square::new(u32::try_from(side).ok()?);
        let step = 2.0 * plan.reach / real(plan.cells.0);
        let origin = (
            plan.relief.centre.0 - plan.reach,
            plan.relief.centre.1 - plan.reach,
        );
        // Each finer grid spans whole cells of the grid about it, so its
        // border runs along that grid's cell edges.
        let mut parent = plan.far_step();
        let mut nests = [None; NESTS];
        for ((slot, nest), field) in nests.iter_mut().zip(&plan.nests).zip(fields.nests) {
            match (nest, field) {
                (Some(nest), Some(field)) => {
                    let reach = (mathf::round(nest.reach / parent) * parent).max(8.0 * parent);
                    *slot = Some(Laid {
                        field,
                        centre: plan.relief.centre,
                        reach,
                    });
                    parent = 2.0 * reach / real(nest.cells);
                }
                (None, None) => {}
                _ => return None,
            }
        }
        // Whole cells of the water grid, which is the far grid's, so its
        // border runs along that grid's cell edges.
        let far_cell = plan.far_step();
        let near_water = match (plan.near_water, fields.near_water) {
            (Some(near), Some(field)) => Some(Laid {
                field,
                centre: plan.relief.centre,
                reach: (mathf::round(near.reach / far_cell) * far_cell).max(2.0 * far_cell),
            }),
            (None, None) => None,
            _ => return None,
        };
        if near_water.is_some() && (plan.rivers.is_none() || fields.water.is_none()) {
            return None;
        }
        Some(Self {
            square,
            height: fallible::filled(side * side, 0.0)?,
            original: Vec::new(),
            origin,
            step,
            rivers: Courses::none(),
            roads: Courses::none(),
            paths: Courses::none(),
            deltas: Vec::new(),
            crossings: Vec::new(),
            lakes: Vec::new(),
            far: fields.far,
            nests,
            water: fields.water,
            near_water,
            horizon: fields.horizon,
            stage: Step::Relief(0),
            plan,
        })
    }

    /// How far the build has come, as a share of its work: each stage weighed
    /// by what its work costs, its items times the measured cost of one.
    pub(crate) fn done(&self) -> f64 {
        let vertices = |cells: usize| real((cells + 1) * (cells + 1));
        let run = |rate: f64, area: f64| rate.max(0.0) * area * DROPLET_NS;
        let coarse_side = self.square.side() as usize;
        let relief = real(self.square.area()) * RELIEF_NS;
        let horizon_side = self.plan.horizon.map_or(1, |horizon| horizon.cells + 1);
        let horizon = self
            .plan
            .horizon
            .map_or(0.0, |horizon| vertices(horizon.cells) * RELIEF_NS);
        let pass = real(self.square.area()) * WEAR_NS;
        let worn = relief + horizon + pass * (f64::from(self.plan.wear.passes) + 1.0);
        let far_side = self.plan.cells.1 + 1;
        let far = vertices(self.plan.cells.1);
        let (far_fill, far_drops, far_settle) =
            (far * FILL_NS, run(self.plan.droplets, far), far * SETTLE_NS);
        let nest = |level: usize| {
            self.plan
                .nests
                .get(level)
                .copied()
                .flatten()
                .map_or((1, 0.0, 0.0, 0.0), |nest| {
                    let area = vertices(nest.cells);
                    let drops = run(nest.droplets, area);
                    (nest.cells + 1, area * FILL_NS, drops, area * SETTLE_NS)
                })
        };
        let nests_before = |level: usize| {
            (0..level)
                .map(|below| {
                    let (_, fill, drops, settle) = nest(below);
                    fill + drops + settle
                })
                .sum::<f64>()
        };
        let sited = worn + far_fill + far_drops + far_settle;
        let laid = sited + nests_before(NESTS);
        let water = if self.water.is_some() {
            far * WATER_NS
        } else {
            0.0
        };
        let (near_side, near_water) = self.plan.near_water.map_or((1, 0.0), |near| {
            (near.cells + 1, vertices(near.cells) * WATER_NS)
        });
        let ran = |erosion: &Erosion| {
            1.0 - f64::from(erosion.left()) / f64::from(erosion.total()).max(1.0)
        };
        let wearing = |pass_index: u32, part: f64, within: f64| {
            relief + horizon + pass * (f64::from(pass_index) + (part + within.min(1.0)) / 6.0)
        };
        let coarse = |row: u32| share(row as usize, coarse_side);
        let spent = match &self.stage {
            Step::Relief(row) => relief * share(*row, coarse_side),
            Step::Horizon(row) => relief + horizon * share(*row, horizon_side),
            Step::Flood { pass, .. } => wearing(*pass, 0.0, 0.0),
            Step::Route { pass, row, .. } => wearing(*pass, 1.0, coarse(*row)),
            Step::Accumulate { pass, end, network } => {
                wearing(*pass, 2.0, 1.0 - share(*end, network.order.len()))
            }
            Step::Incise {
                pass,
                start,
                network,
            } => wearing(*pass, 3.0, share(*start, network.order.len())),
            Step::Creep { pass, row, .. } => wearing(*pass, 4.0, coarse(*row)),
            Step::Slump {
                pass, row, settle, ..
            } => wearing(
                *pass,
                5.0,
                f64::from(u8::from(*settle)).midpoint(coarse(*row)),
            ),
            Step::Waters { .. } | Step::Road { .. } => worn,
            Step::Far { row, .. } => worn + far_fill * share(*row, far_side),
            Step::FarDroplets { erosion, .. } => worn + far_fill + far_drops * ran(erosion),
            Step::FarSettle { row, .. } => {
                worn + far_fill + far_drops + far_settle * share(*row, far_side)
            }
            Step::Sited => sited,
            Step::Nest { level, row } => {
                let (side, fill, ..) = nest(*level);
                sited + nests_before(*level) + fill * share(*row, side)
            }
            Step::NestDroplets { level, erosion, .. } => {
                let (_, fill, drops, _) = nest(*level);
                sited + nests_before(*level) + fill + drops * ran(erosion)
            }
            Step::NestSettle { level, row, .. } => {
                let (side, fill, drops, settle) = nest(*level);
                sited + nests_before(*level) + fill + drops + settle * share(*row, side)
            }
            Step::Water(row) => laid + water * share(*row, far_side),
            Step::NearWater(row) => laid + water + near_water * share(*row, near_side),
            Step::Seal { .. } | Step::Done => return 1.0,
            Step::Gone => 0.0,
        };
        (spent / (laid + water + near_water).max(1.0)).min(1.0)
    }

    /// Whether the build waits for the scene to say where the near grid lies.
    pub(crate) fn waiting(&self) -> bool {
        matches!(self.stage, Step::Sited)
    }

    /// The far land as it stands, before the near land is laid: what a
    /// scene sites its eye by.
    pub(crate) fn survey<'a>(&'a self, fields: &'a [Heightfield]) -> Option<Survey<'a>> {
        Some(Survey {
            build: self,
            far: fields.get(self.far as usize)?,
        })
    }

    /// Lay each finer grid about `focus`, moved on by `lead` times its own
    /// half breadth, wear the path `path` into them, and go on.
    pub(crate) fn site(
        &mut self,
        focus: (f64, f64),
        lead: (f64, f64),
        path: Option<&[Mark]>,
    ) -> Option<()> {
        if !matches!(self.stage, Step::Sited) {
            return None;
        }
        let (mut around, mut around_reach) = (self.plan.relief.centre, self.plan.reach);
        let mut parent = self.far_placing();
        for (laid, nest) in self.nests.iter_mut().zip(&self.plan.nests) {
            let (Some(laid), Some(nest)) = (laid.as_mut(), nest) else {
                continue;
            };
            // Snapped to a vertex of the grid about it, so its border runs
            // along that grid's cell edges and the two meet there exactly,
            // and kept well within it.
            let ((origin_x, origin_z), step) = parent;
            let room = (around_reach - laid.reach - 8.0 * step).max(0.0);
            laid.centre = (
                snapped(
                    focus.0 + lead.0 * laid.reach,
                    (around.0, room),
                    (origin_x, step),
                ),
                snapped(
                    focus.1 + lead.1 * laid.reach,
                    (around.1, room),
                    (origin_z, step),
                ),
            );
            (around, around_reach) = (laid.centre, laid.reach);
            let own = 2.0 * laid.reach / real(nest.cells);
            parent = (
                (laid.centre.0 - laid.reach, laid.centre.1 - laid.reach),
                own,
            );
        }
        let ((origin_x, origin_z), step) = self.far_placing();
        let (centre, reach) = (self.plan.relief.centre, self.plan.reach);
        if let Some(near) = self.near_water.as_mut() {
            // On a vertex of the far water grid it leaves its square out of,
            // and well within it.
            let room = (reach - near.reach - 8.0 * step).max(0.0);
            near.centre = (
                snapped(
                    focus.0 + lead.0 * near.reach,
                    (centre.0, room),
                    (origin_x, step),
                ),
                snapped(
                    focus.1 + lead.1 * near.reach,
                    (centre.1, room),
                    (origin_z, step),
                ),
            );
        }
        let outermost = self.nests.iter().flatten().next().copied();
        if let (Some(path), Some(outer)) = (path, outermost) {
            let bounds = (
                (outer.centre.0 - outer.reach, outer.centre.1 - outer.reach),
                2.0 * outer.reach,
            );
            let course = fallible::collected(path.len(), path.iter().copied())?;
            let reach = Reach {
                per_width: 1.0,
                beyond: 3.0,
            };
            self.paths = Courses::new(&[course], bounds, reach)?;
        }
        self.stage = self.next_nest(0);
        Some(())
    }

    /// The stage that fills finer grid `level`, or what follows the last.
    fn next_nest(&self, level: usize) -> Step {
        if self.nests.get(level).is_some_and(Option::is_some) {
            Step::Nest { level, row: 0 }
        } else {
            Step::Water(0)
        }
    }

    /// The land, once built.
    pub(crate) fn finish(self) -> Option<Land> {
        matches!(self.stage, Step::Done).then(|| Land {
            far: self.far,
            nests: self.nests,
            water: self.water,
            near_water: self.near_water,
            horizon: self.horizon,
            form: self.form(),
            rivers: self.rivers,
            roads: self.roads,
            road: self.plan.road,
            crossings: self.crossings,
            sea: self.plan.sea,
            centre: self.plan.relief.centre,
            reach: self.plan.reach,
        })
    }

    /// What shapes the land's rivers' channels, if it has any.
    fn form(&self) -> Option<Form> {
        self.plan.rivers.map(|rivers| rivers.form(self.plan.seed))
    }

    fn far_placing(&self) -> ((f64, f64), f64) {
        (self.origin, self.plan.far_step())
    }

    /// The scene's grid finer grid `level` is laid within.
    fn parent_of(&self, level: usize) -> Option<u32> {
        match level.checked_sub(1) {
            None => Some(self.far),
            Some(outer) => self
                .nests
                .get(outer)
                .copied()
                .flatten()
                .map(|laid| laid.field),
        }
    }

    /// The level of the water standing over coarse sample `index`: a lake's
    /// surface, or the sea.
    fn water_level(&self, index: usize) -> f64 {
        let lake = self
            .lakes
            .get(index)
            .map_or(f64::NEG_INFINITY, |&level| f64::from(level));
        lake.max(self.plan.sea.unwrap_or(f64::NEG_INFINITY))
    }

    /// Whether water stands over the coarse sample `index`: a lake's surface
    /// above its ground, or the sea.
    fn flooded(&self, index: usize) -> bool {
        let Some(&ground) = self.height.get(index) else {
            return false;
        };
        let lake = self
            .lakes
            .get(index)
            .is_some_and(|&level| f64::from(level) > ground);
        lake || self.plan.sea.is_some_and(|sea| ground <= sea + 0.5)
    }

    /// The dry coarse sample nearest world `(x, z)`, looking no further than
    /// a sixteenth of the land either way; `None` if all of that is flooded.
    fn dry_near(&self, (x, z): (f64, f64)) -> Option<usize> {
        let centre = self.sample_at(x, z);
        if !self.flooded(centre) {
            return Some(centre);
        }
        let (cx, cy) = self.square.position(centre);
        let most = i32::try_from(self.square.side() / 16).ok()?;
        for ring in 1..=most {
            let around =
                (-ring..=ring).flat_map(|d| [(d, -ring), (d, ring), (-ring, d), (ring, d)]);
            let dry = around
                .filter_map(|(dx, dy)| self.square.neighbour(cx, cy, dx, dy))
                .find(|&index| !self.flooded(index));
            if dry.is_some() {
                return dry;
            }
        }
        None
    }

    /// Whether the coarse sample `index` drains out of the land: its rim, and
    /// the sea.
    fn outlet(&self, index: usize) -> bool {
        let (x, y) = self.square.position(index);
        self.square.is_rim(x, y)
            || self
                .plan
                .sea
                .is_some_and(|sea| self.height.get(index).is_some_and(|&h| h <= sea))
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
            Step::Relief(row) => self.relief(row, runner)?,
            Step::Horizon(row) => self.filling_horizon(fields, row, runner)?,
            Step::Flood { pass, flood } => self.flooding(pass, flood)?,
            Step::Route {
                pass,
                row,
                flood,
                flow,
            } => self.routing(pass, row, flood, flow)?,
            Step::Accumulate { pass, end, network } => self.accumulating(pass, end, network)?,
            Step::Incise {
                pass,
                start,
                network,
            } => self.incising(pass, start, network)?,
            Step::Creep { pass, row, before } => self.creeping(pass, row, before)?,
            Step::Slump {
                pass,
                row,
                before,
                sheds,
                settle,
            } => self.slumping(pass, row, (before, sheds), settle)?,
            Step::Waters { network } => self.reading_waters(network)?,
            Step::Road { network, router } => self.routing_road(network, router)?,
            Step::Far { network, row } => self.filling_far(fields, network, row, runner)?,
            Step::FarDroplets {
                erosion,
                before,
                flux,
            } => self.eroding_far(fields, (erosion, runner), (before, flux))?,
            Step::FarSettle { row, before, flux } => {
                self.settling_far(fields, (row, runner), (before, flux))?
            }
            Step::Sited => Step::Sited,
            Step::Nest { level, row } => self.filling_nest(fields, level, row, runner)?,
            Step::NestDroplets {
                level,
                erosion,
                before,
            } => self.eroding_nest(fields, (level, erosion, before), runner)?,
            Step::NestSettle { level, row, before } => {
                self.settling_nest(fields, (level, row, before), runner)?
            }
            Step::Water(row) => self.filling_water(fields, row, runner)?,
            Step::NearWater(row) => self.filling_near_water(fields, row, runner)?,
            Step::Seal { next, sealing } => self.sealing(fields, (next, sealing), runner)?,
            done @ Step::Done => done,
            Step::Gone => return None,
        };
        Some(matches!(self.stage, Step::Done | Step::Sited))
    }

    /// A unit of the horizon grid's rows from `row`.
    fn filling_horizon(
        &self,
        fields: &mut [Heightfield],
        row: usize,
        runner: &dyn JobRunner,
    ) -> Option<Step> {
        let field = fields.get_mut(self.horizon? as usize)?;
        let end = (row + UNIT_ROWS * runner.width().max(1)).min(field.side());
        self.fill_horizon(field, row..end, runner);
        if end < field.side() {
            Some(Step::Horizon(end))
        } else {
            self.first_flood()
        }
    }

    /// A unit of pass `pass`'s drainage solved.
    fn flooding(&self, pass: u32, mut flood: Flood) -> Option<Step> {
        Some(if flood.advance(&self.height, UNIT_SAMPLES).ok()? {
            Step::Route {
                pass,
                row: 0,
                flood,
                flow: fallible::filled(self.square.area(), FlowDir::Sink)?,
            }
        } else {
            Step::Flood { pass, flood }
        })
    }

    /// A unit of pass `pass`'s flow routed downhill, from row `row`.
    fn routing(&self, pass: u32, row: u32, flood: Flood, mut flow: Vec<FlowDir>) -> Option<Step> {
        let rows =
            u32::try_from((UNIT_SAMPLES / self.square.side().max(1) as usize).max(1)).ok()?;
        let end = (row + rows).min(self.square.side());
        route(
            (flood.filled(), flood.rank()),
            self.square,
            |i| self.outlet(i),
            &mut flow,
            row..end,
        )
        .ok()?;
        if end < self.square.side() {
            return Some(Step::Route {
                pass,
                row: end,
                flood,
                flow,
            });
        }
        let (filled, order, _) = flood.into_parts();
        let network = Network {
            filled,
            flow,
            discharge: fallible::filled(self.square.area(), 1)?,
            order,
        };
        Some(Step::Accumulate {
            pass,
            end: network.order.len(),
            network,
        })
    }

    /// A unit of pass `pass`'s discharge gathered downstream, those of the
    /// samples in drainage order before `end`.
    fn accumulating(&self, pass: u32, end: usize, mut network: Network) -> Option<Step> {
        let start = end.saturating_sub(UNIT_SAMPLES);
        let Network {
            order,
            flow,
            discharge,
            ..
        } = &mut network;
        accumulate((order, flow), self.square, discharge, start..end).ok()?;
        Some(if start > 0 {
            Step::Accumulate {
                pass,
                end: start,
                network,
            }
        } else if pass < self.plan.wear.passes {
            Step::Incise {
                pass,
                start: 0,
                network,
            }
        } else {
            Step::Waters { network }
        })
    }

    /// A unit of pass `pass`'s channels cut, from the sample `start` in
    /// drainage order; the basins then partly infilled once all are.
    fn incising(&mut self, pass: u32, start: usize, network: Network) -> Option<Step> {
        let end = (start + UNIT_SAMPLES).min(network.order.len());
        let law = Implicit {
            k_dt: self.plan.wear.incision,
            m: 0.5,
            cell_area: self.step * self.step,
            spacing: self.step,
            head: CHANNEL_HEAD * self.step * self.step,
        };
        let strata = self.plan.wear.strata;
        let erodibility = |_: usize, height: f64| erodibility(strata, height);
        incise_implicit(
            &mut self.height,
            &network,
            self.square,
            law,
            (start..end, &erodibility),
        )
        .ok()?;
        if end < network.order.len() {
            return Some(Step::Incise {
                pass,
                start: end,
                network,
            });
        }
        let infill = self.plan.wear.infill;
        let pond = self.plan.pond;
        let (origin, step, square) = (self.origin, self.step, self.square);
        for (index, (ground, &spill)) in self.height.iter_mut().zip(&network.filled).enumerate() {
            let (x, y) = square.position(index);
            let (px, pz) = (
                origin.0 + f64::from(x) * step,
                origin.1 + f64::from(y) * step,
            );
            let kept = pond.is_some_and(|((cx, cz), reach)| mathf::hypot(px - cx, pz - cz) < reach);
            if spill > *ground && !kept {
                *ground += infill * (spill - *ground);
            }
        }
        Some(Step::Creep {
            pass,
            row: 0,
            before: hillslope::snapshot(&self.height).ok()?,
        })
    }

    /// A unit of pass `pass`'s soil crept downhill, from row `row`.
    fn creeping(&mut self, pass: u32, row: u32, before: Vec<f64>) -> Option<Step> {
        let end = (row + self.coarse_rows()).min(self.square.side());
        let law = Diffusion {
            rate: self.plan.wear.creep,
            floor: self.plan.sea.unwrap_or(f64::NEG_INFINITY),
        };
        hillslope::diffuse_rows(&before, &mut self.height, self.square, law, row..end).ok()?;
        if end < self.square.side() {
            return Some(Step::Creep {
                pass,
                row: end,
                before,
            });
        }
        Some(Step::Slump {
            pass,
            row: 0,
            before: hillslope::snapshot(&self.height).ok()?,
            sheds: fallible::filled(self.square.area(), Shed::default())?,
            settle: false,
        })
    }

    /// A unit of pass `pass`'s slopes steeper than they stand measured, or
    /// once all are, slumped, from row `row`; the next pass's drainage begun
    /// after the last.
    fn slumping(
        &mut self,
        pass: u32,
        row: u32,
        (before, mut sheds): (Vec<f64>, Vec<Shed>),
        settle: bool,
    ) -> Option<Step> {
        let end = (row + self.coarse_rows()).min(self.square.side());
        let law = Talus {
            drop: self.plan.wear.repose * self.step,
            rate: 0.3,
        };
        if settle {
            hillslope::slump_settle(
                &before,
                &sheds,
                &mut self.height,
                self.square,
                law,
                row..end,
            )
            .ok()?;
        } else {
            hillslope::slump_measure(&before, &mut sheds, self.square, law, row..end).ok()?;
        }
        Some(if end < self.square.side() {
            Step::Slump {
                pass,
                row: end,
                before,
                sheds,
                settle,
            }
        } else if !settle {
            Step::Slump {
                pass,
                row: 0,
                before,
                sheds,
                settle: true,
            }
        } else {
            Step::Flood {
                pass: pass + 1,
                flood: Flood::new(&self.height, self.square, |i| self.outlet(i)).ok()?,
            }
        })
    }

    /// The rivers and lakes read from the final drainage, and the road's
    /// route begun if the land has one.
    fn reading_waters(&mut self, network: Network) -> Option<Step> {
        self.waters(&network)?;
        let Some(ends) = self.plan.road.and_then(|roadway| self.road_ends(roadway)) else {
            return Some(Step::Far { network, row: 0 });
        };
        let mut router = Router::new(self.square.area()).ok()?;
        router
            .begin(self.square, ends, self.square.side() / 3, ROAD_STEP)
            .ok()?;
        Some(Step::Road { network, router })
    }

    /// A unit of the road's route found, and the road graded along it once
    /// it is.
    fn routing_road(&mut self, network: Network, mut router: Router) -> Option<Step> {
        let price =
            |from: usize, to: usize, diagonal: bool| self.road_cost(&network, (from, to), diagonal);
        match router.advance(UNIT_SETTLED, &price).ok()? {
            Routed::Pending => Some(Step::Road { network, router }),
            Routed::Found(path) => {
                self.grade_road(&path)?;
                Some(Step::Far { network, row: 0 })
            }
            Routed::Unreachable => Some(Step::Far { network, row: 0 }),
        }
    }

    /// A unit of the far grid's rows from `row`, and its droplets readied
    /// once all are.
    fn filling_far(
        &self,
        fields: &mut [Heightfield],
        network: Network,
        row: usize,
        runner: &dyn JobRunner,
    ) -> Option<Step> {
        let (horizon, field) = match self.horizon {
            Some(horizon) => {
                let (field, horizon) = apart(fields, self.far as usize, horizon as usize)?;
                (Some(horizon), field)
            }
            None => (None, fields.get_mut(self.far as usize)?),
        };
        let end = (row + UNIT_ROWS * runner.width().max(1)).min(field.side());
        self.fill_far((horizon, field), &network, row..end, runner)?;
        if end < field.side() {
            return Some(Step::Far { network, row: end });
        }
        let seed = u64::from(self.plan.seed) ^ 0xd50b;
        let square = Square::new(u32::try_from(field.side()).ok()?);
        let law = Self::droplets(self.far_placing().1);
        Some(Step::FarDroplets {
            erosion: Erosion::new(square, law, self.plan.droplets, seed).ok()?,
            before: fallible::collected(field.heights().len(), field.heights().iter().copied())?,
            flux: fallible::filled(field.heights().len(), 0.0)?,
        })
    }

    /// A turn of the droplets running over the far grid across `runner`.
    fn eroding_far(
        &self,
        fields: &mut [Heightfield],
        (mut erosion, runner): (Erosion, &dyn JobRunner),
        (before, mut flux): (Vec<f32>, Vec<f32>),
    ) -> Option<Step> {
        let field = fields.get_mut(self.far as usize)?;
        let ran = erosion
            .run(field.heights_mut(), Some(&mut flux), runner)
            .ok()?;
        Some(if ran {
            Step::FarSettle {
                row: 0,
                before,
                flux,
            }
        } else {
            Step::FarDroplets {
                erosion,
                before,
                flux,
            }
        })
    }

    /// A unit of the far grid's rows from `row` across `runner`: settled
    /// after its droplets, held to its clearing, and blended into the
    /// horizon's along its border.
    fn settling_far(
        &self,
        fields: &mut [Heightfield],
        (row, runner): (usize, &dyn JobRunner),
        (before, flux): (Vec<f32>, Vec<f32>),
    ) -> Option<Step> {
        let far = self.far_laid();
        let (field, horizon) = match self.horizon {
            Some(beyond) => {
                let (field, horizon) = apart(fields, self.far as usize, beyond as usize)?;
                (field, Some(horizon))
            }
            None => (fields.get_mut(self.far as usize)?, None),
        };
        let side = field.side();
        let end = (row + UNIT_ROWS * runner.width().max(1)).min(side);
        let placing = field.placing();
        let roughness = self.plan.roughness;
        field.each_row(row..end, runner, &|(row, heights, attributes)| {
            let first = *row * side;
            let (before, flux) = (
                before.get(first..first + side).unwrap_or_default(),
                flux.get(first..first + side).unwrap_or_default(),
            );
            self.keep_channel((*row, heights), before, placing);
            settle_row((heights, attributes), (before, flux), roughness);
            self.hold_row((*row, heights), placing);
            if let Some(horizon) = horizon {
                let band = (horizon.placing().1, FAR_BAND);
                blend_row(horizon, (*row, heights), placing, &|at| {
                    border_blend(far, at, band)
                });
            }
        });
        if end < side {
            return Some(Step::FarSettle {
                row: end,
                before,
                flux,
            });
        }
        if let Some(beyond) = self.horizon {
            leave_out(far, fields.get_mut(beyond as usize)?, 1);
        }
        Some(Step::Sited)
    }

    /// A unit of finer grid `level`'s rows from `row`, and its droplets
    /// readied once all are.
    fn filling_nest(
        &self,
        fields: &mut [Heightfield],
        level: usize,
        row: usize,
        runner: &dyn JobRunner,
    ) -> Option<Step> {
        let laid = self.nests.get(level).copied().flatten()?;
        let nest = self.plan.nests.get(level).copied().flatten()?;
        if row == 0 {
            let origin = (laid.centre.0 - laid.reach, laid.centre.1 - laid.reach);
            let step = 2.0 * laid.reach / real(nest.cells);
            *fields.get_mut(laid.field as usize)? =
                Heightfield::new(nest.cells, origin, step, false)?;
        }
        let (grid, parent) = apart(fields, laid.field as usize, self.parent_of(level)? as usize)?;
        let end = (row + UNIT_ROWS * runner.width().max(1)).min(grid.side());
        self.fill_nest((level, laid), (parent, grid), row..end, runner)?;
        if end < grid.side() {
            return Some(Step::Nest { level, row: end });
        }
        let (_, step) = grid.placing();
        let square = Square::new(u32::try_from(grid.side()).ok()?);
        let seed = u64::from(self.plan.seed) ^ 0x2ea7 ^ u64::try_from(level).ok()?;
        Some(Step::NestDroplets {
            level,
            erosion: Erosion::new(square, Self::droplets(step), nest.droplets, seed).ok()?,
            before: fallible::collected(grid.heights().len(), grid.heights().iter().copied())?,
        })
    }

    /// A turn of the droplets running over finer grid `level` across
    /// `runner`.
    fn eroding_nest(
        &self,
        fields: &mut [Heightfield],
        (level, mut erosion, before): (usize, Erosion, Vec<f32>),
        runner: &dyn JobRunner,
    ) -> Option<Step> {
        let laid = self.nests.get(level).copied().flatten()?;
        let grid = fields.get_mut(laid.field as usize)?;
        Some(if erosion.run(grid.heights_mut(), None, runner).ok()? {
            Step::NestSettle {
                level,
                row: 0,
                before,
            }
        } else {
            Step::NestDroplets {
                level,
                erosion,
                before,
            }
        })
    }

    /// A unit of finer grid `level`'s rows from `row` across `runner`, its
    /// border held to the grid about it and then its clearing, and the next
    /// finer grid begun once all of it is.
    fn settling_nest(
        &self,
        fields: &mut [Heightfield],
        (level, row, before): (usize, usize, Vec<f32>),
        runner: &dyn JobRunner,
    ) -> Option<Step> {
        let laid = self.nests.get(level).copied().flatten()?;
        let parent = self.parent_of(level)? as usize;
        let (grid, around) = apart(fields, laid.field as usize, parent)?;
        let side = grid.side();
        let end = (row + UNIT_ROWS * runner.width().max(1)).min(side);
        let placing = grid.placing();
        grid.each_row(row..end, runner, &|(row, heights, _)| {
            let before = before
                .get(*row * side..(*row + 1) * side)
                .unwrap_or_default();
            self.keep_channel((*row, heights), before, placing);
            let band = (around.placing().1, NEST_BAND);
            blend_row(around, (*row, heights), placing, &|at| {
                border_blend(laid, at, band)
            });
            self.hold_row((*row, heights), placing);
        });
        if end < side {
            return Some(Step::NestSettle {
                level,
                row: end,
                before,
            });
        }
        leave_out(laid, fields.get_mut(parent)?, 1);
        Some(self.next_nest(level + 1))
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
        if row == 0 && !self.watered() {
            // A dry land's water keeps its place as a grid of one empty cell.
            for kept in [Some(index), self.near_water.map(|near| near.field)]
                .into_iter()
                .flatten()
            {
                fields.get_mut(kept as usize)?.heights_mut().fill(ABSENT);
            }
            return Some(Step::SEALING);
        }
        let field = fields.get_mut(index as usize)?;
        if row == 0 {
            let (origin, step) = self.far_placing();
            *field = Heightfield::new(self.plan.cells.1, origin, step, false)?;
        }
        let end = (row + water_rows(field.side()) * runner.width().max(1)).min(field.side());
        self.fill_water(field, row..end, runner);
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
        let (field, far) = apart(fields, near.field as usize, self.water? as usize)?;
        if row == 0 {
            let origin = (near.centre.0 - near.reach, near.centre.1 - near.reach);
            *field = Heightfield::new(cells, origin, 2.0 * near.reach / real(cells), false)?;
            if !field.carry_attributes() {
                return None;
            }
        }
        let end = (row + water_rows(field.side()) * runner.width().max(1)).min(field.side());
        self.fill_water(field, row..end, runner);
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

    /// Whether a lake or a river stands anywhere on the land.
    fn watered(&self) -> bool {
        self.lakes.iter().any(|level| level.is_finite()) || self.rivers.len() > 0
    }

    /// Rows of the coarse grid a unit of creep or slump covers.
    fn coarse_rows(&self) -> u32 {
        u32::try_from((UNIT_SAMPLES / self.square.side().max(1) as usize).max(1)).unwrap_or(1)
    }

    /// Fill a unit of the coarse grid's rows from `row`.
    fn relief(&mut self, row: usize, runner: &dyn JobRunner) -> Option<Step> {
        let side = self.square.side() as usize;
        let end = (row + UNIT_ROWS * 2 * runner.width().max(1)).min(side);
        let (relief, origin, step) = (&self.plan.relief, self.origin, self.step);
        let rows = self.height.get_mut(row * side..end * side)?;
        band::for_each(runner, rows, (row, side), &|row, band| {
            let z = origin.1 + real(row) * step;
            for (column, slot) in band.iter_mut().enumerate() {
                *slot = relief.height(origin.0 + real(column) * step, z);
            }
        });
        if end < side {
            return Some(Step::Relief(end));
        }
        self.original = hillslope::snapshot(&self.height).ok()?;
        if self.horizon.is_some() {
            return Some(Step::Horizon(0));
        }
        self.first_flood()
    }

    /// The first pass's drainage, begun.
    fn first_flood(&self) -> Option<Step> {
        let flood = Flood::new(&self.height, self.square, |i| self.outlet(i)).ok()?;
        Some(Step::Flood { pass: 0, flood })
    }

    /// The far grid's square, as a finer grid laid within the horizon.
    fn far_laid(&self) -> Laid {
        Laid {
            field: self.far,
            centre: self.plan.relief.centre,
            reach: self.plan.reach,
        }
    }

    /// Fill rows `rows` of the horizon grid across `runner`, from the relief
    /// and the coarsest of the far land's detail: water has not worn the land
    /// that far off, and the haze would hide it if it had.
    fn fill_horizon(&self, field: &mut Heightfield, rows: Range<usize>, runner: &dyn JobRunner) {
        let side = field.side();
        let ((origin_x, origin_z), step) = field.placing();
        let (heights, _) = field.rows_mut(rows.clone());
        let relief = &self.plan.relief;
        let longest = 8.0 * step;
        let octaves = octaves_between(longest, step);
        band::for_each(runner, heights, (rows.start, side), &|row, band| {
            let z = origin_z + real(row) * step;
            for (column, slot) in band.iter_mut().enumerate() {
                let x = origin_x + real(column) * step;
                let rise = |dx: f64, dz: f64| relief.height(x + dx, z + dz);
                let slope = mathf::hypot(
                    rise(step, 0.0) - rise(-step, 0.0),
                    rise(0.0, step) - rise(0.0, -step),
                ) / (2.0 * step);
                *slot = single(rise(0.0, 0.0) + self.detail((x, z), slope, (longest, octaves)));
            }
        });
    }

    /// `height` shelving away beneath the sea, if the land has one, so the
    /// ground a hand's breadth under the water lies well under it rather
    /// than meeting the surface all across a flat.
    fn shelve(&self, height: f64) -> f64 {
        match self.plan.sea {
            Some(sea) if height < sea + SHELF => height - 3.0 * (sea + SHELF - height),
            _ => height,
        }
    }

    /// The longest wavelength of the detail the far grid adds to the coarse.
    fn far_detail(&self) -> f64 {
        3.0 * self.step
    }

    /// The wavelengths of detail grid `level` adds — `0` the far grid, then
    /// each finer grid in turn — as its longest and how many octaves: each
    /// grid carries on down the spectrum where the grid about it left off,
    /// to as short a wave as it holds well.
    fn band(&self, level: usize) -> (f64, u32) {
        let mut longest = self.far_detail();
        let mut octaves = octaves_between(longest, self.far_placing().1);
        for (laid, nest) in self.nests.iter().zip(&self.plan.nests).take(level) {
            let (Some(laid), Some(nest)) = (laid, nest) else {
                break;
            };
            longest /= mathf::exp(f64::from(octaves) * core::f64::consts::LN_2);
            octaves = octaves_between(longest, 2.0 * laid.reach / real(nest.cells));
        }
        (longest, octaves)
    }

    /// Droplets for a grid whose samples are `step` apart.
    fn droplets(step: f64) -> Droplets {
        Droplets {
            lifetime: 90,
            inertia: 0.08,
            capacity: 4.0,
            min_slope: 0.01 * step,
            erosion: 0.25,
            deposition: 0.25,
            evaporation: 0.018,
            gravity: 4.0 / step.max(0.1),
            friction: 0.06,
            radius: 3,
        }
    }

    /// What `value` gives at the coarse samples about world `(x, z)`, blended
    /// between them.
    fn between(&self, (x, z): (f64, f64), value: &dyn Fn(usize) -> f64) -> f64 {
        let last = real(self.square.side().saturating_sub(1) as usize);
        let (u, v) = (
            ((x - self.origin.0) / self.step).clamp(0.0, last),
            ((z - self.origin.1) / self.step).clamp(0.0, last),
        );
        let (column, row) = (
            mathf::floor(u).min(last - 1.0).max(0.0),
            mathf::floor(v).min(last - 1.0).max(0.0),
        );
        let (across, down) = (u - column, v - row);
        let whole = |value: f64| u32::try_from(mathf::round_i32(value)).unwrap_or(0);
        let (column, row) = (whole(column), whole(row));
        let at = |dx: u32, dy: u32| value(self.square.index(column + dx, row + dy));
        crate::heightfield::bilinear([at(0, 0), at(1, 0), at(0, 1), at(1, 1)], (across, down))
    }

    /// The coarse sample nearest world `(x, z)`.
    fn sample_at(&self, x: f64, z: f64) -> usize {
        let side = self.square.side();
        let index = |value: f64, origin: f64| {
            let at = mathf::round((value - origin) / self.step).clamp(0.0, real(side as usize - 1));
            u32::try_from(mathf::round_i32(at)).unwrap_or(0)
        };
        self.square
            .index(index(x, self.origin.0), index(z, self.origin.1))
    }

    /// The world place of coarse sample `index`.
    fn place_of(&self, index: usize) -> (f64, f64) {
        let (x, y) = self.square.position(index);
        (
            self.origin.0 + f64::from(x) * self.step,
            self.origin.1 + f64::from(y) * self.step,
        )
    }

    /// The worn land's height at `(x, z)`, the coarse grid read through a
    /// Catmull–Rom patch so no crease of its cells shows.
    fn worn(&self, x: f64, z: f64) -> f64 {
        self.worn_sloped(x, z).0
    }

    /// The worn land's height at `(x, z)`, and its slope along x and z.
    fn worn_sloped(&self, x: f64, z: f64) -> (f64, f64, f64) {
        let side = self.square.side() as usize;
        let at = |column: i64, row: i64| {
            let clamp = |value: i64| {
                usize::try_from(value.clamp(0, i64::try_from(side).unwrap_or(1) - 1)).unwrap_or(0)
            };
            self.height
                .get(clamp(row) * side + clamp(column))
                .copied()
                .unwrap_or(0.0)
        };
        let (height, du, dv) = catmull_rom_2d(
            &at,
            (
                (x - self.origin.0) / self.step,
                (z - self.origin.1) / self.step,
            ),
        );
        (height, du / self.step, dv / self.step)
    }

    /// Hold the rows `rows` of `field` level in the relief's clearing, if it
    /// has one, whatever the droplets did to it.
    fn hold_row(&self, (row, heights): (usize, &mut [f32]), placing: ((f64, f64), f64)) {
        let relief = &self.plan.relief;
        if relief.clearing.is_none() {
            return;
        }
        let ((origin_x, origin_z), step) = placing;
        let z = origin_z + real(row) * step;
        for (column, slot) in heights.iter_mut().enumerate() {
            if slot.is_finite() {
                let x = origin_x + real(column) * step;
                *slot = single(relief.pin(x, z, f64::from(*slot)));
            }
        }
    }

    /// Read the rivers and lakes from the final drainage.
    fn waters(&mut self, network: &Network) -> Option<()> {
        let area = self.square.area();
        let mut standing = fallible::filled(area, ABSENT)?;
        for ((slot, &ground), &surface) in
            standing.iter_mut().zip(&self.height).zip(&network.filled)
        {
            if surface - ground > LAKE_DEPTH && self.plan.sea.is_none_or(|sea| ground > sea) {
                *slot = single(surface);
            }
        }
        // Each sample takes the surface of any lake beside it too, so a lake
        // reaches its shore however the finer grids round the shore off.
        let mut lakes = fallible::collected(area, standing.iter().copied())?;
        for (index, slot) in lakes.iter_mut().enumerate() {
            let (x, y) = self.square.position(index);
            for (dx, dy) in [
                (1, 0),
                (-1, 0),
                (0, 1),
                (0, -1),
                (1, 1),
                (-1, -1),
                (1, -1),
                (-1, 1),
            ] {
                if let Some(&lake) = self
                    .square
                    .neighbour(x, y, dx, dy)
                    .and_then(|beside| standing.get(beside))
                {
                    *slot = slot.max(lake);
                }
            }
        }
        self.lakes = lakes;
        if let Some(rivers) = self.plan.rivers {
            self.rivers = self.trace_rivers(network, rivers)?;
        }
        Some(())
    }

    /// The river network: every stream draining at least a river's catchment,
    /// traced from its head down to its mouth or the river it joins.
    fn trace_rivers(&mut self, network: &Network, rivers: Rivers) -> Option<Courses> {
        let area = self.square.area();
        let cell_area = self.step * self.step;
        let river =
            |index: usize| real(network.discharge[index] as usize) * cell_area >= rivers.catchment;
        // A head is a river sample no river sample drains into.
        let mut fed = fallible::filled(area, false)?;
        for index in 0..area {
            if river(index) {
                if let Some(next) = downstream(self.square, &network.flow, index) {
                    fed[next] = true;
                }
            }
        }
        let mut traced = fallible::filled(area, false)?;
        let runs = runs(network, self.square, self.step)?;
        let mut courses: Vec<Vec<Mark>> = Vec::new();
        for head in (0..area).filter(|&index| river(index) && !fed[index]) {
            let mut marks = Vec::new();
            let mut here = head;
            loop {
                let (x, z) = self.place_of(here);
                let drained = real(network.discharge[here] as usize) * cell_area;
                let width =
                    (rivers.width * mathf::sqrt(drained / rivers.catchment)).min(MOST_RIVER_WIDTH);
                marks.try_reserve(1).ok()?;
                marks.push(Mark {
                    x,
                    z,
                    level: network.filled[here],
                    width,
                    depth: bankfull_depth(width),
                    run: runs.get(here).copied().unwrap_or(0.0),
                    ..Mark::default()
                });
                let joined = traced[here];
                traced[here] = true;
                // A river ends where it meets the sea, a lake, or the river
                // it joins; the joining sample kept so the two meet.
                let lake = self.flooded(here);
                match downstream(self.square, &network.flow, here) {
                    Some(next) if !joined && !lake && !self.outlet(here) => here = next,
                    _ => break,
                }
            }
            if marks.len() >= 3 {
                let shaped = self.shape_river(&marks, rivers)?;
                // A river that ends in standing water rather than another
                // river fans its silt out into it.
                if self.flooded(here) {
                    if let Some(delta) = delta(&shaped, self.water_level(here)) {
                        self.deltas.try_reserve(1).ok()?;
                        self.deltas.push(delta);
                    }
                }
                courses.try_reserve(1).ok()?;
                courses.push(shaped);
            }
        }
        // As far from a river as its banks and the wet ground beside them
        // reach on the coarsest grid its bed is cut into.
        let reach = Reach {
            per_width: 2.5,
            beyond: 4.0 * self.far_placing().1,
        };
        let bounds = ((self.origin.0, self.origin.1), 2.0 * self.plan.reach);
        Courses::new(&courses, bounds, reach)
    }

    /// A river's course smoothed from the sample steps it was traced along,
    /// set wandering across its level stretches, its brim falling steadily
    /// downstream, and its pools and riffles counted along it.
    fn shape_river(&self, traced: &[Mark], rivers: Rivers) -> Option<Vec<Mark>> {
        let mut course = meandering(&smoothed(traced, 3)?, rivers, self.plan.seed)?;
        // The brim falls, never rises, on its way down, below the floodplain
        // the river has cut its channel into.
        let mut surface = f64::INFINITY;
        for mark in &mut course {
            surface = surface.min(mark.level);
            mark.level = surface - (0.3 + 0.6 * mark.depth);
        }
        channel::survey(&mut course, rivers.form(self.plan.seed).seed)?;
        Some(course)
    }

    /// Where the road's two ends lie: on dry ground across the land along
    /// its heading, either side of the middle; `None` where either end finds
    /// none.
    fn road_ends(&self, roadway: Roadway) -> Option<(usize, usize)> {
        let (cx, cz) = self.plan.relief.centre;
        let reach = 0.8 * self.plan.reach;
        let (sin, cos) = (mathf::sin(roadway.heading), mathf::cos(roadway.heading));
        Some((
            self.dry_near((cx - sin * reach, cz - cos * reach))?,
            self.dry_near((cx + sin * reach, cz + cos * reach))?,
        ))
    }

    /// What a step of the road costs: its length, dearer the steeper it
    /// climbs, and far dearer across a river, which it must bridge; none
    /// through a lake or the sea.
    fn road_cost(
        &self,
        network: &Network,
        (from, to): (usize, usize),
        diagonal: bool,
    ) -> Option<u32> {
        if self.flooded(to) {
            return None;
        }
        let run = if diagonal {
            core::f64::consts::SQRT_2
        } else {
            1.0
        } * self.step;
        let grade = (self.height[to] - self.height[from]).abs() / run;
        let climb = grade * grade * 400.0
            + if grade > 0.12 {
                60.0 * (grade - 0.12) * 100.0
            } else {
                0.0
            };
        let drained = real(network.discharge[to] as usize) * self.step * self.step;
        let river = self.plan.rivers.map_or(0.0, |rivers| {
            if drained >= rivers.catchment {
                30.0
            } else {
                0.0
            }
        });
        let straight = f64::from(ROAD_STEP) * (1.0 + climb + river);
        let cost = if diagonal { straight * 1.5 } else { straight };
        u32::try_from(mathf::round_i32(cost.min(1e9))).ok()
    }

    /// The road along `path`, smoothed, its level graded so it climbs no
    /// steeper than a road may.
    fn grade_road(&mut self, path: &[usize]) -> Option<()> {
        let Some(roadway) = self.plan.road else {
            return Some(());
        };
        let mut marks = Vec::new();
        if !fallible::reserve(&mut marks, path.len()) {
            return None;
        }
        for &index in path {
            let (x, z) = self.place_of(index);
            marks.push(Mark {
                x,
                z,
                level: self.height[index],
                width: roadway.width,
                ..Mark::default()
            });
        }
        let mut course = smoothed(&marks, 4)?;
        for mark in &mut course {
            mark.level = self.worn(mark.x, mark.z);
        }
        // Graded: the level smoothed along the road.
        let mut levels = fallible::filled(course.len(), 0.0)?;
        for _ in 0..6 {
            for (level, mark) in levels.iter_mut().zip(&course) {
                *level = mark.level;
            }
            for index in 1..course.len().saturating_sub(1) {
                course[index].level =
                    0.25 * levels[index - 1] + 0.5 * levels[index] + 0.25 * levels[index + 1];
            }
        }
        // Where it crosses a river it rises to a deck clear of the water, and
        // nothing grades the deck back down.
        let mut pinned = fallible::filled(course.len(), false)?;
        self.crossings = self.bridge(&mut course, &mut pinned)?;
        // Then held to its steepest grade walking each way, so it ramps up to
        // each deck and down from it.
        for pass in 0..2 {
            for step in 1..course.len() {
                let (index, other) = if pass == 0 {
                    (step, step - 1)
                } else {
                    (course.len() - 1 - step, course.len() - step)
                };
                if pinned[index] {
                    continue;
                }
                let run = mathf::hypot(
                    course[index].x - course[other].x,
                    course[index].z - course[other].z,
                );
                let limit = MOST_ROAD_GRADE * run;
                let (from, to) = (course[other].level, course[index].level);
                course[index].level = mathf::clamp(to, from - limit, from + limit);
            }
        }
        let bounds = ((self.origin.0, self.origin.1), 2.0 * self.plan.reach);
        let reach = Reach {
            per_width: 3.0,
            beyond: 4.0 * self.far_placing().1,
        };
        self.roads = Courses::new(&[course], bounds, reach)?;
        Some(())
    }

    /// The spans of `course` over a river, each raised to a deck clear of the
    /// water and `pinned` there; `None` when the heap will not hold them.
    fn bridge(&self, course: &mut [Mark], pinned: &mut [bool]) -> Option<Vec<Crossing>> {
        let mut crossings = Vec::new();
        let mut index = 0;
        while index < course.len() {
            let over = |mark: &Mark| {
                self.rivers
                    .nearest(mark.x, mark.z)
                    .filter(|river| river.distance < 0.5 * river.width + BRIDGE_REACH)
            };
            let Some(river) = course.get(index).and_then(over) else {
                index += 1;
                continue;
            };
            let start = index;
            let (mut water, mut width) = (river.level, river.width);
            while let Some(more) = course.get(index).and_then(over) {
                water = water.max(more.level);
                width = width.max(more.width);
                index += 1;
            }
            let (first, last) = (start.saturating_sub(1), index.min(course.len() - 1));
            let deck = course[first..=last]
                .iter()
                .map(|mark| mark.level)
                .fold(water + CLEARANCE + 0.08 * width, f64::max);
            for (mark, pin) in course[first..=last]
                .iter_mut()
                .zip(&mut pinned[first..=last])
            {
                mark.level = deck;
                *pin = true;
            }
            crossings.try_reserve(1).ok()?;
            crossings.push(Crossing {
                from: course[first],
                to: course[last],
                deck,
                water,
                width,
            });
        }
        Some(crossings)
    }

    /// Fill the far grid's rows `rows`: the worn land, its fine detail, its
    /// river and road beds cut, and what it is like at each vertex.
    fn fill_far(
        &self,
        (horizon, field): (Option<&Heightfield>, &mut Heightfield),
        network: &Network,
        rows: Range<usize>,
        runner: &dyn JobRunner,
    ) -> Option<()> {
        if !field.carry_attributes() {
            return None;
        }
        let side = field.side();
        let ((origin_x, origin_z), step) = field.placing();
        let mut values = fallible::filled(rows.len() * side, (0.0f32, [0u8; 4]))?;
        let far = self.far_laid();
        band::for_each(runner, &mut values, (rows.start, side), &|row, band| {
            let z = origin_z + real(row) * step;
            for (column, slot) in band.iter_mut().enumerate() {
                let x = origin_x + real(column) * step;
                let (height, attributes) = self.far_vertex(network, (x, z), step);
                // Toward its border the far land gives way to the horizon's.
                let height = match horizon {
                    Some(horizon) => {
                        let (flat, beyond) = (horizon.height_at(x, z), horizon.placing().1);
                        flat + (f64::from(height) - flat)
                            * border_blend(far, (x, z), (beyond, FAR_BAND))
                    }
                    None => f64::from(height),
                };
                *slot = (single(height), attributes);
            }
        });
        store(field, rows.start, &values);
        Some(())
    }

    /// The far grid's vertex at `(x, z)`, its neighbours `step` away.
    fn far_vertex(&self, network: &Network, (x, z): (f64, f64), step: f64) -> (f32, [u8; 4]) {
        let (worn, dx, dz) = self.worn_sloped(x, z);
        let slope = mathf::hypot(dx, dz);
        // The relief holds its clearing already; what this grid adds to it
        // fades out there, so the clearing stays as level as the relief left it.
        let keep = self.plan.relief.keep(x, z);
        let natural = worn + keep * self.detail((x, z), slope, self.band(0));
        let (carved, lie, channel) = self.carve((x, z), natural, step);
        let height = natural + keep * (self.shelve(carved) - natural);
        let height = self.plan.relief.pin(x, z, height);
        // Read between the coarse samples, so neither steps from one to the
        // next along the coarse grid's lines.
        let cell_area = self.step * self.step;
        let wetness = |index: usize| {
            let drained = network
                .discharge
                .get(index)
                .map_or(0.0, |&count| f64::from(count))
                * cell_area;
            (mathf::ln(1.0 + drained / 5_000.0) / 9.0).clamp(0.0, 1.0)
        };
        let laid = |index: usize| match (self.height.get(index), self.original.get(index)) {
            (Some(now), Some(was)) => ((now - was) / self.plan.roughness.max(1.0)).clamp(-1.0, 1.0),
            _ => 0.0,
        };
        let wet = self.between((x, z), &wetness).max(lie.wet);
        let sediment = self.between((x, z), &laid).max(lie.sediment);
        let lie = Lie {
            wet,
            sediment: sediment + (channel.0 - sediment) * channel.1,
            upright: 1.0 / mathf::sqrt(1.0 + slope * slope),
            ..lie
        };
        (single(height), self.attributes(&lie, height))
    }

    /// The fine relief a coarser grid is too coarse to hold, `longest` its
    /// longest wavelength and `octaves` its count: roughest on steep ground,
    /// and fainter the shorter its waves, as natural relief is.
    fn detail(&self, (x, z): (f64, f64), slope: f64, (longest, octaves): (f64, u32)) -> f64 {
        let spectrum = mathf::exp(0.8 * mathf::ln(longest / self.far_detail()));
        let rough = spectrum * self.plan.roughness * (0.25 + 0.75 * smoothstep(0.05, 0.6, slope));
        let hummocks = fbm2(
            x / longest,
            z / longest,
            self.plan.seed ^ 0x4a11,
            (octaves, 0.5, 2.0),
        );
        let share = self.plan.ridges;
        let ridges = if share > 0.0 {
            ridged2(x / longest, z / longest, self.plan.seed ^ 0xde7a, octaves) - 0.5
        } else {
            0.0
        };
        rough * (share * ridges + (1.0 - share) * hummocks)
    }

    /// `natural` ground at `(x, z)` with river beds and the road's bed cut
    /// into it, and what the cutting leaves the place like; `step` the
    /// grid's, which the cut is softened across. And what a river's channel
    /// laid down or wore away there, with how much of the say it has over
    /// what was laid, the rest the land's own.
    fn carve(&self, (x, z): (f64, f64), natural: f64, step: f64) -> (f64, Lie, (f64, f64)) {
        let mut height = natural;
        let mut lie = Lie {
            green: 1.0,
            ..Lie::default()
        };
        for delta in &self.deltas {
            let (fan, laid) = delta_bed(delta, (x, z), height);
            height = fan;
            lie.sediment = lie.sediment.max(laid);
            lie.wet = lie.wet.max(0.8 * laid);
        }
        let mut channel = (0.0, 0.0);
        let river = self.rivers.nearest(x, z);
        if let (Some(near), Some(form)) = (&river, self.form()) {
            let banked = Banked::new(&Station::of(near), &form);
            let wander = noise2(x / SCOUR_WANDER, z / SCOUR_WANDER, self.plan.seed ^ 0x5c0f);
            let bedding = river_bed(&banked, near, natural.min(height), (step, wander));
            height = bedding.height;
            lie.wet = lie.wet.max(bedding.wet);
            lie.green *= 1.0 - bedding.scoured;
            channel = (bedding.laid, bedding.say);
        }
        if let (Some(near), Some(roadway)) = (self.roads.nearest(x, z), self.plan.road) {
            // A road bridges a river rather than filling it, and its bridge
            // carries it: the ground beneath is the river's.
            let bridging =
                river.is_some_and(|river| river.distance < 0.5 * river.width + BRIDGE_REACH);
            if !bridging {
                let (bed, weight) = road_bed(&near, height, (roadway.surface, step));
                height = bed;
                lie.road = weight;
            }
        }
        if let Some(near) = self.paths.nearest(x, z) {
            let half = 0.5 * near.width;
            let worn = 1.0 - smoothstep(half * 0.6, half + step, near.distance);
            height -= 0.08 * worn;
            lie.path = worn;
        }
        (height, lie, channel)
    }

    /// Encode what a vertex at `height` is like as its four bytes.
    fn attributes(&self, lie: &Lie, height: f64) -> [u8; 4] {
        let steep = smoothstep(0.62, 0.45, lie.upright);
        let snowed = self
            .plan
            .snow_line
            .map_or(0.0, |line| smoothstep(line - 40.0, line + 40.0, height));
        let shore = self
            .plan
            .sea
            .map_or(0.0, |sea| smoothstep(sea + 2.5, sea + 0.3, height));
        let growth = self.plan.growth;
        let watered = growth + (1.0 - growth) * smoothstep(0.3, 0.8, lie.wet);
        let green = watered
            * lie.green
            * (1.0 - steep)
            * (1.0 - lie.road)
            * (1.0 - 0.8 * lie.path)
            * (1.0 - shore)
            * (1.0 - snowed)
            * (1.0 - smoothstep(0.85, 1.0, lie.wet));
        [
            byte(lie.wet),
            byte(0.5 + 0.5 * lie.sediment),
            encode_lane(lie.road, lie.path),
            byte(green),
        ]
    }

    /// Fill rows `rows` of the finer grid `laid` describes across `runner`:
    /// the grid about it, `parent`, refined — its detail finer, its beds cut
    /// sharp — and blended back to the parent's own surface along its border,
    /// so the two meet exactly.
    fn fill_nest(
        &self,
        (level, laid): (usize, Laid),
        (parent, grid): (&Heightfield, &mut Heightfield),
        rows: Range<usize>,
        runner: &dyn JobRunner,
    ) -> Option<()> {
        if !grid.carry_attributes() {
            return None;
        }
        let side = grid.side();
        let ((origin_x, origin_z), step) = grid.placing();
        let detail = self.band(level + 1);
        let mut values = fallible::filled(rows.len() * side, (0.0f32, [0u8; 4]))?;
        band::for_each(runner, &mut values, (rows.start, side), &|row, band| {
            let z = origin_z + real(row) * step;
            for (column, slot) in band.iter_mut().enumerate() {
                let x = origin_x + real(column) * step;
                let (height, attributes) = self.nest_vertex(laid, parent, (x, z), (step, detail));
                *slot = (single(height), attributes);
            }
        });
        store(grid, rows.start, &values);
        Some(())
    }

    /// A finer grid's vertex at `(x, z)`, laid as `laid` describes within
    /// `parent` on a grid `step` apart and holding the `detail` its band of
    /// octaves does: its height, and what it is like.
    fn nest_vertex(
        &self,
        laid: Laid,
        parent: &Heightfield,
        (x, z): (f64, f64),
        (step, detail): (f64, (f64, u32)),
    ) -> (f64, [u8; 4]) {
        let (_, parent_step) = parent.placing();
        let flat = parent.height_at(x, z);
        let (smooth, dx, dz) = cubic_on(parent, (x, z));
        let slope = mathf::hypot(dx, dz);
        let keep = self.plan.relief.keep(x, z);
        let own = smooth + keep * self.detail((x, z), slope, detail);
        let (carved, lie, channel) = self.carve((x, z), own, step);
        let carved = self
            .plan
            .relief
            .pin(x, z, own + keep * (self.shelve(carved) - own));
        let height = flat + (carved - flat) * border_blend(laid, (x, z), (parent_step, NEST_BAND));
        let [wet, sediment, _, _] = parent.attributes_at(x, z);
        let sediment = (2.0 * sediment - 1.0).max(lie.sediment);
        let lie = Lie {
            wet: wet.max(lie.wet),
            sediment: sediment + (channel.0 - sediment) * channel.1,
            upright: 1.0 / mathf::sqrt(1.0 + slope * slope),
            ..lie
        };
        (height, self.attributes(&lie, height))
    }

    /// Row `row` of a grid placed at `placing` put back toward what it stood
    /// at `before` the droplets ran, as far as a river's channel has its say
    /// there: its floods, not the droplets, shape a channel and its banks'
    /// faces. A height absent either side is left as it is.
    fn keep_channel(
        &self,
        (row, heights): (usize, &mut [f32]),
        before: &[f32],
        ((origin_x, origin_z), step): ((f64, f64), f64),
    ) {
        let z = origin_z + real(row) * step;
        for (column, (slot, &was)) in heights.iter_mut().zip(before).enumerate() {
            if slot.is_finite() && was.is_finite() {
                let say = self.channel_say((origin_x + real(column) * step, z), step);
                *slot += single(say) * (was - *slot);
            }
        }
    }

    /// How much of the say a river's channel has over the ground at
    /// `(x, z)` on a grid `step` apart: what it carved there stands against
    /// the droplets run over the land after.
    fn channel_say(&self, (x, z): (f64, f64), step: f64) -> f64 {
        match (self.rivers.nearest(x, z), self.form()) {
            (Some(near), Some(form)) if near.distance < channel::farthest_say(near.width, step) => {
                say(&Banked::new(&Station::of(&near), &form), &near, step)
            }
            _ => 0.0,
        }
    }

    /// Fill the fresh-water grid's rows `rows` across `runner`: a lake's
    /// surface over its basin and a little beyond, a river's along its
    /// channel, and nothing elsewhere.
    fn fill_water(&self, field: &mut Heightfield, rows: Range<usize>, runner: &dyn JobRunner) {
        let side = field.side();
        let ((origin_x, origin_z), step) = field.placing();
        let (heights, _) = field.rows_mut(rows.clone());
        band::for_each(runner, heights, (rows.start, side), &|row, band| {
            let z = origin_z + real(row) * step;
            for (column, slot) in band.iter_mut().enumerate() {
                let level = self.water_surface(origin_x + real(column) * step, z, step);
                *slot = if level.is_finite() {
                    single(level)
                } else {
                    ABSENT
                };
            }
        });
    }

    /// The fresh water's surface at `(x, z)` on a grid `step` apart, or
    /// negative infinity where none stands: a lake's over its basin and a
    /// little beyond, a river's along its channel as high as it flows.
    fn water_surface(&self, x: f64, z: f64, step: f64) -> f64 {
        let lake = self
            .lakes
            .get(self.sample_at(x, z))
            .map_or(f64::NEG_INFINITY, |&lake| f64::from(lake));
        let river = match self.form() {
            Some(form) => self
                .rivers
                .nearest(x, z)
                .filter(|near| near.distance < river_reach(near.width, step))
                .map_or(f64::NEG_INFINITY, |near| {
                    channel::water(&Station::of(&near), &form)
                }),
            None => f64::NEG_INFINITY,
        };
        lake.max(river)
    }
}

/// Rows of a water grid `side` vertices a side a core fills or shapes in a
/// unit.
pub(crate) fn water_rows(side: usize) -> usize {
    crate::band::unit_rows(side).min(2 * UNIT_ROWS)
}

/// How far from its middle a river `width` wide wets a grid `step` apart's
/// vertices: past its banks, as broad as its channel ever stands, by a
/// cell's diagonal, so every cell its course crosses holds water at all four
/// corners, however narrow the river and however it slants across the grid.
/// Water drawn past the banks lies under the ground there, unseen.
fn river_reach(width: f64, step: f64) -> f64 {
    0.5 * BROADEST * width + core::f64::consts::SQRT_2 * step
}

/// How far the water draining each coarse sample of `network`, `step`
/// apart over `square`, has run to reach it: the longest way down the
/// routing from a divide, where nothing drains into a sample, to it.
fn runs(network: &Network, square: Square, step: f64) -> Option<Vec<f64>> {
    let mut run = fallible::filled(network.flow.len(), 0.0)?;
    // Upstream first: every sample before the one it drains to.
    for &raw in network.order.iter().rev() {
        let index = raw as usize;
        let Some(next) = downstream(square, &network.flow, index) else {
            continue;
        };
        let reached = *run.get(index)? + step * network.flow.get(index)?.length();
        let slot = run.get_mut(next)?;
        *slot = slot.max(reached);
    }
    Some(run)
}

/// How deep a river `width` across runs at bankfull: as hydraulic geometry
/// has a gravel-bed river, its breadth five times its depth for each cube
/// root of a metre of it, so a stream two to nine metres broad is six to ten
/// times as broad as it is deep.
fn bankfull_depth(width: f64) -> f64 {
    0.2 * power(width.max(0.5), 2.0 / 3.0)
}

/// How readily ground at `height` wears under `strata`, against ordinary
/// ground's one: the top tenth of each bed is its hard cap, which wears its
/// hardness times as slowly.
fn erodibility(strata: Option<(f64, f64)>, height: f64) -> f64 {
    let Some((spacing, hardness)) = strata else {
        return 1.0;
    };
    let (_, within) = crate::noise::cell(height / spacing);
    if within > 0.9 {
        1.0 / hardness
    } else {
        1.0
    }
}

/// A lake stands where the ground lies this far below the surface its basin
/// fills to.
const LAKE_DEPTH: f64 = 0.8;
/// The broadest a river grows.
const MOST_RIVER_WIDTH: f64 = 70.0;
/// The least a road's straight coarse step costs.
const ROAD_STEP: u32 = 64;
/// The steepest a road climbs.
const MOST_ROAD_GRADE: f64 = 0.09;
/// How far either rut of a track lies from its middle.
pub(crate) const TRACK_GAUGE: f64 = 0.72;

/// What a river's channel makes of the ground about it.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Bedding {
    height: f64,
    /// How wet the place is, and how much of it the river's floods scour.
    wet: f64,
    scoured: f64,
    /// What the river laid down or wore away there, and how much of the say
    /// it has over that, the rest the land's own.
    laid: f64,
    say: f64,
}

/// `natural` ground `near` a river whose channel and banks are `banked`
/// there: its bed within its brims, and its banks rising beyond to the land, never lower
/// beside the river than its levees raise them, their faces softened across
/// a grid `step`; how wet the place is, the margin of its bed its low water
/// leaves bare soaked but not under; how much of it its floods scour, all
/// of its bed and a fringe up its banks where plants thin toward it, the
/// fringe's edge wandering by `wander` either way; and what the river laid
/// down or wore away, as far as its banks' faces reach.
fn river_bed(banked: &Banked, near: &Nearest, natural: f64, (step, wander): (f64, f64)) -> Bedding {
    let section = &banked.section;
    let half = section.half;
    let width = 2.0 * half;
    let distance = near.distance;
    let across = near.side * distance;
    let beside = 1.0 - smoothstep(half + width, half + 2.5 * width + 2.0 * step, distance);
    let top = natural.max(natural + (section.brim + FREEBOARD - natural) * beside);
    let height = banked.ground(across, top, step);
    // Soaked within the brim where the low water bares the bed, damp up the
    // banks above it.
    let margin = 1.0 - (1.0 - SOAKED) * smoothstep(0.0, BARE, height - section.water);
    let bank = DAMP_BANK * (1.0 - smoothstep(half, half + 2.0 * width + 3.0 * step, distance));
    let wet = margin + (bank - margin) * smoothstep(half, half + (2.0 * step).max(0.3), distance);
    let fringe = (FRINGE * width).clamp(0.4, 2.0).max(step);
    // Pioneer plants take the tops of its bars, high over its low water, in
    // the patches its last flood spared.
    let pioneers =
        PIONEERS * smoothstep(0.12, 0.45, height - section.water) * smoothstep(-0.2, 0.5, wander);
    let scoured =
        (1.0 - smoothstep(
            half - 0.25 * fringe,
            half + fringe,
            distance + 0.6 * fringe * wander,
        )) * (1.0 - pioneers);
    Bedding {
        height,
        wet,
        scoured,
        laid: banked.laid(across),
        say: say(banked, near, step),
    }
}

/// How much of the say a river's channel and banks, `banked`, have over the
/// ground `near` them, on a grid `step` apart: all of it within their faces,
/// none beyond.
fn say(banked: &Banked, near: &Nearest, step: f64) -> f64 {
    let faces = banked.section.half + banked.bank_reach(near.side * near.distance, step);
    1.0 - smoothstep(faces, faces + 2.0 * step, near.distance)
}

/// How high above a river's water its bare bed has drained, and how wet it
/// stays there: soaked, though no water stands on it; and how damp its
/// banks are above its brim, too dry to be its bed.
const BARE: f64 = 0.05;
pub(crate) const SOAKED: f64 = 0.78;
pub(crate) const DAMP_BANK: f64 = 0.55;

/// How much of its bars' tops pioneer plants take, at most.
const PIONEERS: f64 = 0.3;

/// How far up a river's banks its floods thin the plants, as a share of its
/// breadth, and how far apart the turns of that fringe's edge lie, in
/// metres.
const FRINGE: f64 = 0.15;
const SCOUR_WANDER: f64 = 2.5;

/// How many coarse samples' ground a stream drains where its channel
/// begins: a few hundred metres of slope, as hillslopes run before their
/// water gathers enough to cut.
const CHANNEL_HEAD: f64 = 12.0;

/// How far beyond a river's banks a road's bridge reaches before it meets
/// the land again.
const BRIDGE_REACH: f64 = 2.5;
/// How far above the water a bridge's deck stands, at the least.
const CLEARANCE: f64 = 2.5;

/// How far above the sea the ground begins to shelve away beneath it.
const SHELF: f64 = 0.3;

/// The least a river's banks stand above its water.
const FREEBOARD: f64 = 0.35;

/// `natural` ground `near` a road of `surface`: its bed level across its
/// breadth — cambered, or for a track worn into two ruts — cut or filled back
/// to the land across its verges; and how much of the place is road.
fn road_bed(near: &Nearest, natural: f64, (surface, step): (Surface, f64)) -> (f64, f64) {
    let half = 0.5 * near.width;
    let across = near.distance;
    let shape = match surface {
        Surface::Tarmac | Surface::Gravel => -0.03 * across.min(half),
        Surface::Track => -0.09 * (1.0 - smoothstep(0.1, 0.32, (across - TRACK_GAUGE).abs())),
    };
    let bed = near.level + shape;
    // Verges and cuttings as broad as the road, softened across a step.
    let verge = smoothstep(half, half + near.width + step, across);
    let height = bed + (natural - bed) * verge;
    let weight = 1.0 - smoothstep(half - 0.3, half + 0.3 + 0.5 * step, across);
    (height, weight)
}

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

/// How broad the band within the fresh water's finer grid's border is over
/// which its surface gives way to the far water grid's.
const SEAM: f64 = 1.5;

/// How much of the fresh water's finer grid `near`'s own surface shows at
/// `(x, z)`, the rest the far water grid's: none at its border, where the
/// two meet, and all of it a `SEAM` within.
pub(crate) fn seam(near: Laid, at: (f64, f64)) -> f64 {
    border_blend(near, at, (1.0, (0.0, SEAM)))
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
    band::for_each(runner, heights, (rows.start, side), &|row, band| {
        blend_row(far, (row, band), placing, &|at| seam(near, at));
    });
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

/// How many octaves of detail span wavelengths from `longest` down to four
/// of a grid's `step`s: a shorter wave the grid holds too few samples of, and
/// the finer grids refining it would draw it out as a ripple running along
/// the grid's own cells.
fn octaves_between(longest: f64, step: f64) -> u32 {
    let span = 1.0 + mathf::ln((longest / (4.0 * step)).max(1.0)) / core::f64::consts::LN_2;
    u32::try_from(mathf::round_i32(mathf::floor(span)))
        .unwrap_or(1)
        .clamp(1, 6)
}

/// `course` set wandering from side to side across its level stretches under
/// `seed`: each point moved across the untouched course, by how far that
/// course has run to it, so no move bends the next; `None` when the heap will
/// not hold it.
fn meandering(course: &[Mark], rivers: Rivers, seed: u32) -> Option<Vec<Mark>> {
    let mut wandering = fallible::collected(course.len(), course.iter().copied())?;
    let mut travelled = 0.0;
    for (moved, run) in wandering.iter_mut().skip(1).zip(course.windows(3)) {
        let [last, here, next] = [run[0], run[1], run[2]];
        travelled += mathf::hypot(here.x - last.x, here.z - last.z);
        let (dx, dz) = (next.x - last.x, next.z - last.z);
        let length = mathf::hypot(dx, dz).max(1e-9);
        let fall = (last.level - next.level).max(0.0) / length;
        // Wandering most where the valley floor is flattest.
        let level = 1.0 - smoothstep(0.002, 0.03, fall);
        let wavelength = 11.0 * here.width.max(2.0);
        let sway = rivers.meander
            * here.width
            * level
            * mathf::sin(
                TAU * travelled / wavelength + noise2(travelled / (4.0 * wavelength), 0.3, seed),
            );
        moved.x += -dz / length * sway;
        moved.z += dx / length * sway;
    }
    Some(wandering)
}

/// The fan a river whose course is `course` lays where it runs out into
/// water standing at `level`, if its mouth has a heading to fan along.
fn delta(course: &[Mark], level: f64) -> Option<Delta> {
    let (mouth, before) = (course.last()?, course.get(course.len().checked_sub(4)?)?);
    let (dx, dz) = (mouth.x - before.x, mouth.z - before.z);
    let length = mathf::hypot(dx, dz);
    if length < 1e-6 || !level.is_finite() {
        return None;
    }
    Some(Delta {
        apex: (mouth.x, mouth.z),
        toward: (dx / length, dz / length),
        length: 40.0 + 7.0 * mouth.width,
        width: mouth.width,
        level,
    })
}

/// The ground at `(x, z)` with `delta`'s fan laid over `height`: built up a
/// little above the water near the mouth, shelving under it toward its
/// fringe, and cut by the distributaries it spreads through; and how much
/// of the place is the river's fresh silt.
fn delta_bed(delta: &Delta, (x, z): (f64, f64), height: f64) -> (f64, f64) {
    let (px, pz) = (x - delta.apex.0, z - delta.apex.1);
    let along = px * delta.toward.0 + pz * delta.toward.1;
    let across = px * delta.toward.1 - pz * delta.toward.0;
    // Back up the river a little too, where its mouth widens.
    if along < -delta.width || along > delta.length {
        return (height, 0.0);
    }
    let reach = (along / delta.length).clamp(0.0, 1.0);
    let spread = delta.width + along.max(0.0) * FAN_SPREAD;
    if across.abs() > spread {
        return (height, 0.0);
    }
    let edge = smoothstep(spread, 0.6 * spread, across.abs())
        * smoothstep(delta.length, 0.8 * delta.length, along);
    let surface = delta.level + 0.45 - 1.4 * reach;
    // The distributaries fan out from the mouth, each a channel a third as
    // broad as the river, angled evenly across the fan: the place's angle
    // off the fan's middle, as a share of the fan, in channels.
    let out = along.max(delta.width);
    let half = mathf::atan(FAN_SPREAD);
    let share = f64::midpoint(mathf::atan2(across, out) / half, 1.0) * DISTRIBUTARIES;
    let (_, within) = crate::noise::cell(share);
    let off = (within - 0.5).abs() * 2.0 * half / DISTRIBUTARIES * out;
    let channel = 1.0 - smoothstep(0.1 * delta.width, 0.2 * delta.width, off);
    let fan = surface - 0.8 * channel;
    (height + (fan - height) * edge, edge * (1.0 - reach))
}

/// How far a delta's fan spreads either way for every unit it runs out.
const FAN_SPREAD: f64 = 0.7;
/// How many channels a delta's river divides into across its fan.
const DISTRIBUTARIES: f64 = 4.0;

/// A grid read through a Catmull–Rom patch at `(x, z)`: its height, and its
/// slope along x and z.
fn cubic_on(grid: &Heightfield, (x, z): (f64, f64)) -> (f64, f64, f64) {
    let ((origin_x, origin_z), step) = grid.placing();
    let side = grid.side();
    let heights = grid.heights();
    let at = |column: i64, row: i64| {
        let clamp = |value: i64| {
            usize::try_from(value.clamp(0, i64::try_from(side).unwrap_or(1) - 1)).unwrap_or(0)
        };
        f64::from(
            heights
                .get(clamp(row) * side + clamp(column))
                .copied()
                .unwrap_or(0.0),
        )
    };
    let (height, du, dv) = catmull_rom_2d(&at, ((x - origin_x) / step, (z - origin_z) / step));
    (height, du / step, dv / step)
}

/// Set `field`'s vertices from row `start` on to `values`, each a height and
/// the attributes there.
fn store(field: &mut Heightfield, start: usize, values: &[(f32, [u8; 4])]) {
    let rows = values.len().div_ceil(field.side().max(1));
    let (heights, attributes) = field.rows_mut(start..start + rows);
    for (index, &(height, kept)) in values.iter().enumerate() {
        if let Some(slot) = heights.get_mut(index) {
            *slot = height;
        }
        if let Some(slot) = attributes.get_mut(index) {
            *slot = kept;
        }
    }
}

/// The Catmull–Rom patch of the grid `at` reads, `across` and `down` it in
/// samples: its value, and its rate of change each way.
fn catmull_rom_2d(at: &dyn Fn(i64, i64) -> f64, (across, down): (f64, f64)) -> (f64, f64, f64) {
    let (west, south) = (mathf::floor(across), mathf::floor(down));
    let (right, lower) = (across - west, down - south);
    let (column, row) = (
        i64::from(mathf::round_i32(west)),
        i64::from(mathf::round_i32(south)),
    );
    // The spline through four samples, and its derivative, at `t`.
    let spline = |[p0, p1, p2, p3]: [f64; 4], t: f64| {
        let (a, b, c) = (
            p2 - p0,
            2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3,
            3.0 * p1 - p0 - 3.0 * p2 + p3,
        );
        (
            f64::midpoint(2.0 * p1, t * (a + t * (b + t * c))),
            f64::midpoint(a, t * (2.0 * b + 3.0 * t * c)),
        )
    };
    let mut rows = [0.0; 4];
    let mut slopes = [0.0; 4];
    for (index, near) in (row - 1..=row + 2).enumerate() {
        (rows[index], slopes[index]) = spline(
            [
                at(column - 1, near),
                at(column, near),
                at(column + 1, near),
                at(column + 2, near),
            ],
            right,
        );
    }
    let (value, down_rate) = spline(rows, lower);
    let (across_rate, _) = spline(slopes, lower);
    (value, across_rate, down_rate)
}

/// A row of the far grid, its heights and attributes, settled after its
/// droplets as the row's heights `before` them and the water `flux` they
/// carried across it say: what they wore away or laid down, and where their
/// water ran.
fn settle_row(
    (heights, attributes): (&[f32], &mut [[u8; 4]]),
    (before, flux): (&[f32], &[f32]),
    roughness: f64,
) {
    let settled = heights.iter().zip(attributes.iter_mut());
    for ((&now, attributes), (&was, &water)) in settled.zip(before.iter().zip(flux)) {
        let change = f64::from(now - was) / (0.2 * roughness.max(0.5));
        let runs = (f64::from(water) / 12.0).clamp(0.0, 1.0);
        let sediment = f64::from(attributes[1]) / 255.0 * 2.0 - 1.0;
        attributes[1] = byte(0.5 + 0.5 * (sediment + change).clamp(-1.0, 1.0));
        let wet = f64::from(attributes[0]) / 255.0;
        attributes[0] = byte(wet.max(0.8 * runs));
        // Fresh wash and gullies grow less for a while.
        let green = f64::from(attributes[3]) / 255.0;
        attributes[3] = byte(green * (1.0 - 0.5 * smoothstep(0.3, 1.0, change.abs())));
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

#[cfg(test)]
#[path = "land_tests.rs"]
mod tests;
