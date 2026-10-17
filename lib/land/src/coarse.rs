//! The coarse land: a landform worn down by the water that drains it, its
//! rivers and lakes read from the last drainage, the road routed where the
//! ground lets it go, the countryside laid out over it and its ways laid in,
//! and, where snow lies, how sheltered from the wind each place stands.
//!
//! The work runs over the plan's coarse lattice, global and exact over its
//! fixed sample count: Priority-Flood drainage, implicit stream-power
//! incision along it and hillslope creep, pass after pass, cut the valleys
//! and the dendritic ridges between them. Every stage runs a bounded unit at
//! a time, so a caller answering frames spreads the land over as many as it
//! takes.

use alloc::vec::Vec;

use tairix_countryside::layout::{Countryside, Highway, Laying, Layout};
use tairix_countryside::network::Rank;
use tairix_countryside::route::Line;
use tairix_countryside::{self as countryside, Key as CountryKey, Point, Rect};
use tairix_noise::smoothstep;
use tairix_parallel::JobRunner;
use tairix_terrain::drainage::{accumulate, downstream, route, Flood, Network};
use tairix_terrain::hillslope::{self, Diffusion, Shed, Talus};
use tairix_terrain::incision::{incise_implicit, Implicit};
use tairix_terrain::route::{Routed, Router};
use tairix_terrain::{FlowDir, Grid as Square};
use tairix_util::fallible;
use tairix_util::mathf::{self, real, share, single};

use crate::channel::{self, Form, Section, Station};
use crate::course::{smoothed, Courses, Mark, Reach};
use crate::level::COARSE_DETAIL;
use crate::plan::{
    Character, Farming, Grain, Grains, Lattices, Plan, Rivers, Roadway, Surface, Wear,
};
use crate::relief::{Clearing, Terrain};
use crate::river::{
    bankfull_depth, delta, erodibility, meandering, river_reach, runs, Delta, BRIDGE_REACH,
    CHANNEL_HEAD, CLEARANCE, LAKE_DEPTH, MOST_RIVER_WIDTH,
};
use crate::ways::{held_to, lane_surface, Course, Drifts, Waying, Ways, RAMP_MARKS};
use crate::{run_cells, unit_end, Error, Season};

/// The water standing at a coarse sample no lake covers.
const DRY: f32 = f32::NEG_INFINITY;

/// Samples of the coarse lattice one core fills from the relief, or reads
/// the shelter of, in a unit of work, whole rows or not.
const UNIT_FILLED: usize = 4096;
/// Samples a flood reaches, a routing or accumulation or incision walks, in
/// a unit of work.
const UNIT_SAMPLES: usize = 40_000;
/// Samples an A\* search settles in a unit of work.
const UNIT_SETTLED: usize = 20_000;

/// What a sample filled from the relief costs, in nanoseconds on a desktop
/// core preparing across eight threads: measured, so the stages of a build's
/// progress, and the grids its consumers read from the relief, weigh against
/// each other.
pub const RELIEF_NS: f64 = 80.0;
/// What one item of each other kind of the coarse build's work costs, in the
/// same measure: a sample worn through one pass of drainage, incision, creep
/// and slumping, a sample's shelter from the wind read, and a holding of a
/// farmed land laid out — its share of the countryside's ways routed, its
/// fields cut and bounded. Only their proportions matter, against each other
/// and against the levels refining the land.
const WEAR_NS: f64 = 180.0;
const SHELTER_NS: f64 = 280.0;
const HOLDING_NS: f64 = 4_000_000.0;

/// The road through the land's lie as the coarse lattice samples it: how
/// far its steps cost against its length.
const ROAD_STEP: u32 = 64;
/// The steepest a road climbs.
const MOST_ROAD_GRADE: f64 = 0.09;

/// Where a way crosses a river on a bridge.
#[derive(Copy, Clone, Debug)]
pub struct Crossing {
    /// Its way's mark on one side of the water.
    pub from: Mark,
    /// Its way's mark on the other.
    pub to: Mark,
    /// The deck's level.
    pub deck: f64,
    /// The water's level beneath it.
    pub water: f64,
    /// The river's breadth.
    pub width: f64,
    /// What the way is surfaced with.
    pub surface: Surface,
}

/// The coarse land, as its build leaves it: everything the levels refining it
/// read.
#[derive(Debug)]
pub struct Coarse {
    pub(crate) lattices: Lattices,
    pub(crate) character: Character,
    pub(crate) grains: Grains,
    pub(crate) clearing: Option<Clearing>,
    /// The relief the land was raised from, which runs on past its square,
    /// where it was raised from one.
    pub(crate) relief: Option<Terrain>,
    pub(crate) form: Option<Form>,
    pub(crate) season: Option<Season>,
    /// The land's middle, and how far it reaches from it either way.
    pub(crate) extent: ((f64, f64), f64),
    pub(crate) square: Square,
    /// Where the coarse lattice's first vertex lies, and its step.
    pub(crate) origin: (f64, f64),
    pub(crate) step: f64,
    /// The coarse heights as worn, and what they were before any wear.
    pub(crate) height: Vec<f64>,
    pub(crate) original: Vec<f64>,
    /// How wet each coarse sample lies by what drains through it.
    pub(crate) wetness: Vec<f64>,
    /// The lakes: the water surface standing at or beside each coarse
    /// sample, where one does.
    pub(crate) lakes: Vec<f32>,
    /// How sheltered from the wind each coarse sample stands, if snow lies
    /// on the land.
    pub(crate) shelter: Vec<f64>,
    pub(crate) rivers: Courses,
    pub(crate) deltas: Vec<Delta>,
    /// The road through the land as graded, held until every way is laid.
    pub(crate) through: Vec<Mark>,
    pub(crate) ways: Ways,
    pub(crate) drifts: Drifts,
    pub(crate) layout: Option<Layout>,
    pub(crate) crossings: Vec<Crossing>,
    /// The courses a consumer of the land joins to it before its finer levels
    /// are refined: a footpath trodden from where one stands, and the level
    /// beds of the cuttings a work digs into a hillside.
    pub(crate) paths: Courses,
    pub(crate) cuttings: Courses,
}

/// A land being built.
#[allow(
    clippy::large_enum_variant,
    reason = "a build holds one step, and a box could not fail gracefully"
)]
#[derive(Debug)]
enum Step {
    /// Filling the coarse lattice from the relief, from the sample numbered,
    /// row by row.
    Relief(usize),
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
    /// Laying out the countryside over the land.
    Countryside {
        laying: Laying,
    },
    /// Laying the land's ways into it.
    Ways {
        waying: Waying,
    },
    /// Reading how sheltered from the wind each coarse sample stands, from
    /// the sample numbered `first`, row by row, where snow lies.
    Shelter {
        first: usize,
    },
    Done,
    Gone,
}

/// A coarse land brought from outside, worn as far as its maker wore it, for
/// the levels to refine as they refine one the crate wore itself.
#[derive(Clone, Debug)]
pub struct Field {
    /// The lattices it lies on and is refined through.
    pub lattices: Lattices,
    /// Its height at each coarse sample, row by row.
    pub heights: Vec<f64>,
    /// The level the water draining each sample fills it to: above its height
    /// where a lake stands.
    pub filled: Vec<f64>,
    /// How wet each sample lies, `0.0..=1.0`, as [`wetness`] has it of what
    /// drains through it.
    pub wetness: Vec<f64>,
    /// The grain of its relief below the coarse step.
    pub grains: Grains,
    /// What its levels' vertices are read through.
    pub character: Character,
}

/// The coarse land being built.
#[derive(Debug)]
pub struct Build {
    plan: Plan,
    coarse: Coarse,
    stage: Step,
}

impl Build {
    /// The coarse land of `plan`, begun: `Shape` for a plan whose lattice or
    /// levels do not fit or any of whose numbers is not one a land is built
    /// from, `OutOfMemory` when the heap will not hold its working grids.
    pub fn new(plan: Plan) -> Result<Self, Error> {
        let lattices = plan.lattices()?;
        let lattice = lattices.coarse;
        let character = Character {
            longest: COARSE_DETAIL * lattice.step,
            growth: plan.growth,
            snow_line: plan.snow_line,
            sea: plan.sea,
            snowpack: plan.snowpack,
            seed: plan.seed,
        };
        let grain = Grain {
            roughness: plan.roughness,
            ridges: plan.ridges,
            dunes: 0.0,
        };
        if !(plan.fits() && character.fits() && grain.fits()) {
            return Err(Error::Shape);
        }
        let side = lattice.cells.checked_add(1).ok_or(Error::Shape)?;
        let square = Square::new(side);
        let origin = (
            place(lattice.origin.0, lattice.step)?,
            place(lattice.origin.1, lattice.step)?,
        );
        Ok(Self {
            coarse: Coarse {
                square,
                height: fallible::filled(square.area(), 0.0).ok_or(Error::OutOfMemory)?,
                original: Vec::new(),
                wetness: Vec::new(),
                origin,
                step: lattice.step,
                lattices,
                character,
                grains: Grains::Even(grain),
                clearing: plan.relief.clearing(),
                relief: Some(plan.relief.clone()),
                form: plan.rivers.map(|rivers| rivers.form(plan.seed)),
                season: plan.farming.map(|farming| farming.season),
                extent: (plan.relief.centre, plan.reach),
                rivers: Courses::none(),
                deltas: Vec::new(),
                through: Vec::new(),
                ways: Ways::none(),
                drifts: Drifts::default(),
                layout: None,
                crossings: Vec::new(),
                lakes: Vec::new(),
                shelter: Vec::new(),
                paths: Courses::none(),
                cuttings: Courses::none(),
            },
            plan,
            stage: Step::Relief(0),
        })
    }

    /// Do the next unit of the build across `runner`: whether the coarse land
    /// is built. A refused unit leaves the build where it was refused, and
    /// every later call refuses too.
    pub fn step(&mut self, runner: &dyn JobRunner) -> Result<bool, Error> {
        let (plan, coarse) = (&self.plan, &mut self.coarse);
        let stage = core::mem::replace(&mut self.stage, Step::Gone);
        self.stage = match stage {
            Step::Relief(first) => coarse.relief(&plan.relief, first, runner)?,
            Step::Flood { pass, flood } => coarse.flooding(pass, flood)?,
            Step::Route {
                pass,
                row,
                flood,
                flow,
            } => coarse.routing(pass, row, flood, flow)?,
            Step::Accumulate { pass, end, network } => {
                coarse.accumulating(plan.wear.passes, (pass, end), network)?
            }
            Step::Incise {
                pass,
                start,
                network,
            } => coarse.incising(plan, (pass, start), network)?,
            Step::Creep { pass, row, before } => coarse.creeping(plan.wear, (pass, row), before)?,
            Step::Slump {
                pass,
                row,
                before,
                sheds,
                settle,
            } => coarse.slumping(plan.wear, (pass, row), (before, sheds), settle)?,
            Step::Waters { network } => coarse.reading_waters(plan, network)?,
            Step::Road { network, router } => coarse.routing_road(plan, network, router)?,
            Step::Countryside { laying } => coarse.laying_countryside(plan, laying, runner)?,
            Step::Ways { waying } => coarse.laying_ways(waying, runner)?,
            Step::Shelter { first } => coarse.sheltering(first, runner)?,
            done @ Step::Done => done,
            Step::Gone => return Err(Error::Shape),
        };
        Ok(matches!(self.stage, Step::Done))
    }

    /// What the whole build costs, its items times the measured cost of one:
    /// the scale `done` is a share of, to weigh it against other work.
    #[must_use]
    pub fn cost(&self) -> f64 {
        let (read, countryside, sheltered) = self.costs();
        read + countryside + sheltered
    }

    /// How far the build has come, as a share of its work: each stage weighed
    /// by what its work costs, rising unit by unit; nought once a unit is
    /// refused, the build then going no further.
    #[must_use]
    pub fn done(&self) -> f64 {
        let coarse = &self.coarse;
        let side = coarse.square.side() as usize;
        let relief = real(coarse.square.area()) * RELIEF_NS;
        let pass = real(coarse.square.area()) * WEAR_NS;
        let (read, countryside, sheltered) = self.costs();
        let wearing = |pass_index: u32, part: f64, within: f64| {
            relief + pass * (f64::from(pass_index) + (part + mathf::fmin(within, 1.0)) / 6.0)
        };
        let rows = |row: u32| share(row as usize, side);
        let spent = match &self.stage {
            Step::Relief(first) => relief * share(*first, coarse.height.len()),
            Step::Flood { pass, .. } => wearing(*pass, 0.0, 0.0),
            Step::Route { pass, row, .. } => wearing(*pass, 1.0, rows(*row)),
            Step::Accumulate { pass, end, network } => {
                wearing(*pass, 2.0, 1.0 - share(*end, network.order.len()))
            }
            Step::Incise {
                pass,
                start,
                network,
            } => wearing(*pass, 3.0, share(*start, network.order.len())),
            Step::Creep { pass, row, .. } => wearing(*pass, 4.0, rows(*row)),
            Step::Slump {
                pass, row, settle, ..
            } => wearing(
                *pass,
                5.0,
                f64::from(u8::from(*settle)).midpoint(rows(*row)),
            ),
            Step::Waters { .. } | Step::Road { .. } => read,
            Step::Countryside { laying } => read + countryside * laying.done(),
            Step::Ways { .. } => read + countryside,
            Step::Shelter { first } => {
                read + countryside + sheltered * share(*first, coarse.height.len())
            }
            Step::Done => return 1.0,
            Step::Gone => 0.0,
        };
        mathf::fmin(spent / mathf::fmax(self.cost(), 1.0), 1.0)
    }

    /// What reading the land's wear, laying the countryside out over it and
    /// reading its shelter from the wind cost, each its items times the
    /// measured cost of one.
    fn costs(&self) -> (f64, f64, f64) {
        let plan = &self.plan;
        let area = real(self.coarse.square.area());
        let read = area * RELIEF_NS + area * WEAR_NS * (f64::from(plan.wear.passes) + 1.0);
        let countryside = plan.farming.map_or(0.0, |farming| {
            let breadth = 2.0 * mathf::fmin(farming.reach, plan.reach) / farming.spacing;
            breadth * breadth * HOLDING_NS
        });
        let sheltered = f64::from(u8::from(plan.snowpack.is_some())) * area * SHELTER_NS;
        (read, countryside, sheltered)
    }

    /// The coarse land, once built; `Shape` before it is.
    pub fn finish(self) -> Result<Coarse, Error> {
        if matches!(self.stage, Step::Done) {
            Ok(self.coarse)
        } else {
            Err(Error::Shape)
        }
    }
}

impl Coarse {
    /// The coarse land `field` brings, with how sheltered from the wind each
    /// sample stands read across `runner` where snow lies: `Shape` for a
    /// field whose samples do not fill its lattice, or a height, a share or a
    /// character that is not a finite number in its range, `OutOfMemory` when
    /// the heap will not hold it.
    pub fn of_field(field: Field, runner: &dyn JobRunner) -> Result<Self, Error> {
        let Field {
            lattices,
            heights,
            filled,
            wetness,
            grains,
            character,
        } = field;
        let lattice = lattices.coarse;
        let square = Square::new(lattice.cells.checked_add(1).ok_or(Error::Shape)?);
        let area = square.area();
        let sampled = match &grains {
            Grains::Even(grain) => grain.fits().then_some(area),
            Grains::Sampled(grains) => grains.iter().all(Grain::fits).then_some(grains.len()),
        };
        let finite = |values: &[f64]| values.len() == area && values.iter().all(|v| v.is_finite());
        if sampled != Some(area)
            || !finite(&heights)
            || !finite(&filled)
            || !finite(&wetness)
            || wetness.iter().any(|wet| !(0.0..=1.0).contains(wet))
            || !character.fits()
        {
            return Err(Error::Shape);
        }
        let origin = (
            place(lattice.origin.0, lattice.step)?,
            place(lattice.origin.1, lattice.step)?,
        );
        let half = 0.5 * f64::from(lattice.cells) * lattice.step;
        let mut coarse = Self {
            lattices,
            character,
            grains,
            clearing: None,
            relief: None,
            form: None,
            season: None,
            extent: ((origin.0 + half, origin.1 + half), half),
            square,
            origin,
            step: lattice.step,
            original: hillslope::snapshot(&heights)?,
            height: heights,
            wetness,
            lakes: Vec::new(),
            shelter: Vec::new(),
            rivers: Courses::none(),
            deltas: Vec::new(),
            through: Vec::new(),
            ways: Ways::none(),
            drifts: Drifts::default(),
            layout: None,
            crossings: Vec::new(),
            paths: Courses::none(),
            cuttings: Courses::none(),
        };
        coarse.lakes = coarse.standing(&filled)?;
        let mut first = 0;
        while let Step::Shelter { first: next } = coarse.sheltering(first, runner)? {
            first = next;
        }
        Ok(coarse)
    }

    /// Fill a unit of the coarse lattice's samples from the one numbered
    /// `first` from `relief`.
    fn relief(
        &mut self,
        relief: &Terrain,
        first: usize,
        runner: &dyn JobRunner,
    ) -> Result<Step, Error> {
        let side = self.square.side() as usize;
        let area = self.height.len();
        let end = unit_end(first, UNIT_FILLED, runner).min(area);
        let (origin, step) = (self.origin, self.step);
        let samples = self.height.get_mut(first..end).ok_or(Error::Shape)?;
        let bands = (first / UNIT_FILLED, UNIT_FILLED);
        tairix_parallel::for_each_band(runner, samples, bands, &|band, run| {
            let cells = run_cells(side, (band * UNIT_FILLED, run.len()));
            for (slot, (column, row)) in run.iter_mut().zip(cells) {
                *slot = relief.height(origin.0 + real(column) * step, origin.1 + real(row) * step);
            }
        });
        if end < area {
            return Ok(Step::Relief(end));
        }
        self.original = hillslope::snapshot(&self.height)?;
        let flood = Flood::new(&self.height, self.square, |i| self.outlet(i))?;
        Ok(Step::Flood { pass: 0, flood })
    }

    /// Rows of the coarse lattice a unit of routing, creep or slump covers.
    fn coarse_rows(&self) -> u32 {
        u32::try_from((UNIT_SAMPLES / self.square.side().max(1) as usize).max(1)).unwrap_or(1)
    }

    /// A unit of pass `pass`'s drainage solved.
    fn flooding(&self, pass: u32, mut flood: Flood) -> Result<Step, Error> {
        Ok(if flood.advance(&self.height, UNIT_SAMPLES)? {
            Step::Route {
                pass,
                row: 0,
                flood,
                flow: fallible::filled(self.square.area(), FlowDir::Sink)
                    .ok_or(Error::OutOfMemory)?,
            }
        } else {
            Step::Flood { pass, flood }
        })
    }

    /// A unit of pass `pass`'s flow routed downhill, from row `row`.
    fn routing(
        &self,
        pass: u32,
        row: u32,
        flood: Flood,
        mut flow: Vec<FlowDir>,
    ) -> Result<Step, Error> {
        let end = (row + self.coarse_rows()).min(self.square.side());
        route(
            (flood.filled(), flood.rank()),
            self.square,
            |i| self.outlet(i),
            &mut flow,
            row..end,
        )?;
        if end < self.square.side() {
            return Ok(Step::Route {
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
            discharge: fallible::filled(self.square.area(), 1).ok_or(Error::OutOfMemory)?,
            order,
        };
        Ok(Step::Accumulate {
            pass,
            end: network.order.len(),
            network,
        })
    }

    /// A unit of pass `pass`'s discharge gathered downstream, those of the
    /// samples in drainage order before `end`, of `passes` of wear.
    fn accumulating(
        &self,
        passes: u32,
        (pass, end): (u32, usize),
        mut network: Network,
    ) -> Result<Step, Error> {
        let start = end.saturating_sub(UNIT_SAMPLES);
        let Network {
            order,
            flow,
            discharge,
            ..
        } = &mut network;
        accumulate((order, flow), self.square, discharge, start..end)?;
        Ok(if start > 0 {
            Step::Accumulate {
                pass,
                end: start,
                network,
            }
        } else if pass < passes {
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
    fn incising(
        &mut self,
        plan: &Plan,
        (pass, start): (u32, usize),
        network: Network,
    ) -> Result<Step, Error> {
        let end = (start + UNIT_SAMPLES).min(network.order.len());
        let law = Implicit {
            k_dt: plan.wear.incision,
            m: 0.5,
            cell_area: self.step * self.step,
            spacing: self.step,
            head: CHANNEL_HEAD * self.step * self.step,
        };
        let strata = plan.wear.strata;
        let erodibility = |_: usize, height: f64| erodibility(strata, height);
        incise_implicit(
            &mut self.height,
            &network,
            self.square,
            law,
            (start..end, &erodibility),
        )?;
        if end < network.order.len() {
            return Ok(Step::Incise {
                pass,
                start: end,
                network,
            });
        }
        let Wear { infill, .. } = plan.wear;
        let pond = plan.pond;
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
        Ok(Step::Creep {
            pass,
            row: 0,
            before: hillslope::snapshot(&self.height)?,
        })
    }

    /// A unit of pass `pass`'s soil crept downhill, from row `row`.
    fn creeping(
        &mut self,
        wear: Wear,
        (pass, row): (u32, u32),
        before: Vec<f64>,
    ) -> Result<Step, Error> {
        let end = (row + self.coarse_rows()).min(self.square.side());
        let law = Diffusion {
            rate: wear.creep,
            floor: self.character.sea.unwrap_or(f64::NEG_INFINITY),
        };
        hillslope::diffuse_rows(&before, &mut self.height, self.square, law, row..end)?;
        if end < self.square.side() {
            return Ok(Step::Creep {
                pass,
                row: end,
                before,
            });
        }
        Ok(Step::Slump {
            pass,
            row: 0,
            before: hillslope::snapshot(&self.height)?,
            sheds: fallible::filled(self.square.area(), Shed::default())
                .ok_or(Error::OutOfMemory)?,
            settle: false,
        })
    }

    /// A unit of pass `pass`'s slopes steeper than they stand measured, or
    /// once all are, slumped, from row `row`; the next pass's drainage begun
    /// after the last.
    fn slumping(
        &mut self,
        wear: Wear,
        (pass, row): (u32, u32),
        (before, mut sheds): (Vec<f64>, Vec<Shed>),
        settle: bool,
    ) -> Result<Step, Error> {
        let end = (row + self.coarse_rows()).min(self.square.side());
        let law = Talus {
            drop: wear.repose * self.step,
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
            )?;
        } else {
            hillslope::slump_measure(&before, &mut sheds, self.square, law, row..end)?;
        }
        Ok(if end < self.square.side() {
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
                flood: Flood::new(&self.height, self.square, |i| self.outlet(i))?,
            }
        })
    }

    /// The rivers and lakes read from the final drainage, how wet each coarse
    /// sample lies by what drains through it, and the road's route begun if
    /// the land has one.
    fn reading_waters(&mut self, plan: &Plan, network: Network) -> Result<Step, Error> {
        self.waters(plan.rivers, &network)?;
        let cell_area = self.step * self.step;
        self.wetness = fallible::collected(
            network.discharge.len(),
            network
                .discharge
                .iter()
                .map(|&count| wetness(f64::from(count) * cell_area)),
        )
        .ok_or(Error::OutOfMemory)?;
        let Some(ends) = plan.road.and_then(|roadway| self.road_ends(plan, roadway)) else {
            return self.toward_countryside(plan);
        };
        let mut router = Router::new(self.square.area())?;
        router.begin(self.square, ends, self.square.side() / 3, ROAD_STEP)?;
        Ok(Step::Road { network, router })
    }

    /// A unit of the road's route found, and the road graded along it once
    /// it is.
    fn routing_road(
        &mut self,
        plan: &Plan,
        network: Network,
        mut router: Router,
    ) -> Result<Step, Error> {
        let advanced = {
            let price = |from: usize, to: usize, diagonal: bool| {
                self.road_cost(plan.rivers, &network, (from, to), diagonal)
            };
            router.advance(UNIT_SETTLED, &price)?
        };
        match advanced {
            Routed::Pending => Ok(Step::Road { network, router }),
            Routed::Found(path) => {
                if let Some(roadway) = plan.road {
                    self.grade_road(roadway, &path)?;
                }
                self.toward_countryside(plan)
            }
            Routed::Unreachable => self.toward_countryside(plan),
        }
    }

    /// The countryside's laying begun where the land is farmed, about its
    /// road if it has one; else its ways laid.
    fn toward_countryside(&mut self, plan: &Plan) -> Result<Step, Error> {
        let Some(farming) = plan.farming else {
            let waying = Waying::new(self, plan, None)?;
            return Ok(Step::Ways { waying });
        };
        let mut highways = Vec::new();
        if !self.through.is_empty() {
            highways
                .try_reserve_exact(1)
                .map_err(|_| Error::OutOfMemory)?;
            highways.push(Highway {
                number: 0,
                line: Line {
                    stations: fallible::collected(
                        self.through.len(),
                        self.through.iter().map(|mark| countryside::route::Station {
                            at: Point::new(mark.x, mark.z),
                            level: mark.level,
                            width: mark.width,
                            water: None,
                        }),
                    )
                    .ok_or(Error::OutOfMemory)?,
                },
            });
        }
        let countryside = Countryside {
            key: CountryKey::new(u64::from(plan.seed) ^ 0x0c0a_57ee),
            spacing: farming.spacing,
            villages: farming.villages,
            style: farming.style,
            mix: farming.mix,
            // The noon sun stands toward the land's south, its far side along -z.
            warm: Point::new(0.0, -1.0),
            highways,
        };
        let (cx, cz) = plan.relief.centre;
        let region = Rect::around(Point::new(cx, cz), mathf::fmin(farming.reach, plan.reach));
        let laying = Laying::new(countryside, region)?;
        Ok(Step::Countryside { laying })
    }

    /// A unit of the countryside's laying across `runner`, and its ways laid
    /// into the land once it is laid.
    fn laying_countryside(
        &mut self,
        plan: &Plan,
        mut laying: Laying,
        runner: &dyn JobRunner,
    ) -> Result<Step, Error> {
        let farming = plan.farming.ok_or(Error::Shape)?;
        let country = Country {
            coarse: self,
            farming,
            outcrops: plan.rivers.map_or(0.0, |rivers| rivers.outcrops),
        };
        let Some(layout) = laying.step(&country, runner)? else {
            return Ok(Step::Countryside { laying });
        };
        let waying = Waying::new(self, plan, Some(layout))?;
        Ok(Step::Ways { waying })
    }

    /// A unit of laying the land's ways into it across `runner`, and how
    /// sheltered it stands from the wind read once they are laid.
    fn laying_ways(&mut self, mut waying: Waying, runner: &dyn JobRunner) -> Result<Step, Error> {
        if !waying.step(self, runner)? {
            return Ok(Step::Ways { waying });
        }
        self.crossings
            .try_reserve(waying.crossings.len())
            .map_err(|_| Error::OutOfMemory)?;
        self.crossings.extend(waying.crossings);
        self.ways = waying.ways;
        self.drifts = waying.drifts;
        self.layout = waying.layout;
        Ok(if self.character.snowpack.is_some() {
            Step::Shelter { first: 0 }
        } else {
            Step::Done
        })
    }

    /// A unit of the coarse lattice's shelter from the wind read from the
    /// sample numbered `first`.
    fn sheltering(&mut self, first: usize, runner: &dyn JobRunner) -> Result<Step, Error> {
        let Some(pack) = self.character.snowpack else {
            return Ok(Step::Done);
        };
        let side = self.square.side() as usize;
        let mut shelter = core::mem::take(&mut self.shelter);
        if shelter.is_empty() {
            shelter = fallible::filled(self.square.area(), 0.0).ok_or(Error::OutOfMemory)?;
        }
        let area = shelter.len();
        let end = unit_end(first, UNIT_FILLED, runner).min(area);
        let (origin, step) = (self.origin, self.step);
        let ground = |x: f64, z: f64| self.worn(x, z);
        let samples = shelter.get_mut(first..end).ok_or(Error::Shape)?;
        let bands = (first / UNIT_FILLED, UNIT_FILLED);
        tairix_parallel::for_each_band(runner, samples, bands, &|band, run| {
            let cells = run_cells(side, (band * UNIT_FILLED, run.len()));
            for (slot, (column, row)) in run.iter_mut().zip(cells) {
                let place = (origin.0 + real(column) * step, origin.1 + real(row) * step);
                *slot = pack.shelter(&ground, place);
            }
        });
        self.shelter = shelter;
        Ok(if end < area {
            Step::Shelter { first: end }
        } else {
            Step::Done
        })
    }

    /// Read the rivers and lakes, where `rivers` says it has them, from the
    /// final drainage.
    fn waters(&mut self, rivers: Option<Rivers>, network: &Network) -> Result<(), Error> {
        self.lakes = self.standing(&network.filled)?;
        if let Some(rivers) = rivers {
            self.rivers = self.trace_rivers(network, rivers)?;
        }
        Ok(())
    }

    /// The water standing at or beside each coarse sample, where `filled`
    /// fills it deeper than a lake stands, and not with the sea.
    fn standing(&self, filled: &[f64]) -> Result<Vec<f32>, Error> {
        let area = self.square.area();
        if filled.len() != area || self.height.len() != area {
            return Err(Error::Shape);
        }
        let mut standing = fallible::filled(area, DRY).ok_or(Error::OutOfMemory)?;
        for ((slot, &ground), &surface) in standing.iter_mut().zip(&self.height).zip(filled) {
            if surface - ground > LAKE_DEPTH && self.character.sea.is_none_or(|sea| ground > sea) {
                *slot = single(surface);
            }
        }
        // Each sample takes the surface of any lake beside it too, so a lake
        // reaches its shore however the finer levels round the shore off.
        let mut lakes =
            fallible::collected(area, standing.iter().copied()).ok_or(Error::OutOfMemory)?;
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
                    .filter(|&&lake| lake > *slot)
                {
                    *slot = lake;
                }
            }
        }
        Ok(lakes)
    }

    /// The river network: every stream draining at least a river's catchment,
    /// traced from its head down to its mouth or the river it joins.
    fn trace_rivers(&mut self, network: &Network, rivers: Rivers) -> Result<Courses, Error> {
        let area = self.square.area();
        let cell_area = self.step * self.step;
        let drained = |index: usize| {
            network
                .discharge
                .get(index)
                .map_or(0.0, |&count| f64::from(count))
                * cell_area
        };
        let river = |index: usize| drained(index) >= rivers.catchment;
        // A head is a river sample no river sample drains into.
        let mut fed = fallible::filled(area, false).ok_or(Error::OutOfMemory)?;
        for index in 0..area {
            if river(index) {
                if let Some(slot) =
                    downstream(self.square, &network.flow, index).and_then(|next| fed.get_mut(next))
                {
                    *slot = true;
                }
            }
        }
        let mut traced = fallible::filled(area, false).ok_or(Error::OutOfMemory)?;
        let runs = runs(network, self.square, self.step).ok_or(Error::OutOfMemory)?;
        let mut courses: Vec<Vec<Mark>> = Vec::new();
        for head in (0..area).filter(|&index| river(index) && !fed[index]) {
            let mut marks = Vec::new();
            let mut here = head;
            loop {
                let (x, z) = self.place_of(here);
                let width = mathf::fmin(
                    rivers.width * mathf::sqrt(drained(here) / rivers.catchment),
                    MOST_RIVER_WIDTH,
                );
                marks.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                marks.push(Mark {
                    x,
                    z,
                    level: network.filled.get(here).copied().ok_or(Error::Shape)?,
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
                        self.deltas.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                        self.deltas.push(delta);
                    }
                }
                courses.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                courses.push(shaped);
            }
        }
        // As far from a river as its banks and the wet ground beside them
        // reach on the coarsest level its bed is cut into.
        let reach = Reach {
            per_width: 2.5,
            beyond: 4.0 * self.finest_cut(),
        };
        Courses::new(&courses, self.bounds(), reach).ok_or(Error::OutOfMemory)
    }

    /// A river's course smoothed from the sample steps it was traced along,
    /// set wandering across its level stretches, its brim falling steadily
    /// downstream, and its pools and riffles counted along it.
    fn shape_river(&self, traced: &[Mark], rivers: Rivers) -> Result<Vec<Mark>, Error> {
        let smooth = smoothed(traced, 3).ok_or(Error::OutOfMemory)?;
        let mut course =
            meandering(&smooth, rivers, self.character.seed).ok_or(Error::OutOfMemory)?;
        // The brim falls, never rises, on its way down, below the floodplain
        // the river has cut its channel into.
        let mut surface = f64::INFINITY;
        for mark in &mut course {
            surface = mathf::fmin(surface, mark.level);
            mark.level = surface - (0.3 + 0.6 * mark.depth);
        }
        channel::survey(&mut course, rivers.form(self.character.seed).seed)
            .ok_or(Error::OutOfMemory)?;
        Ok(course)
    }

    /// Where the road's two ends lie: on dry ground across the land along
    /// its heading, either side of the middle; `None` where either end finds
    /// none.
    fn road_ends(&self, plan: &Plan, roadway: Roadway) -> Option<(usize, usize)> {
        let (cx, cz) = plan.relief.centre;
        let reach = 0.8 * plan.reach;
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
        rivers: Option<Rivers>,
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
        let grade = (self.height.get(to)? - self.height.get(from)?).abs() / run;
        let climb = grade * grade * 400.0
            + if grade > 0.12 {
                60.0 * (grade - 0.12) * 100.0
            } else {
                0.0
            };
        let drained = f64::from(*network.discharge.get(to)?) * self.step * self.step;
        let river = rivers.map_or(0.0, |rivers| {
            if drained >= rivers.catchment {
                30.0
            } else {
                0.0
            }
        });
        let straight = f64::from(ROAD_STEP) * (1.0 + climb + river);
        let cost = if diagonal { straight * 1.5 } else { straight };
        u32::try_from(mathf::round_i32(mathf::fmin(cost, 1e9))).ok()
    }

    /// `roadway` along `path`, smoothed, its level graded so it climbs no
    /// steeper than a road may.
    fn grade_road(&mut self, roadway: Roadway, path: &[usize]) -> Result<(), Error> {
        let mut marks = Vec::new();
        if !fallible::reserve(&mut marks, path.len()) {
            return Err(Error::OutOfMemory);
        }
        for &index in path {
            let (x, z) = self.place_of(index);
            marks.push(Mark {
                x,
                z,
                level: self.height.get(index).copied().ok_or(Error::Shape)?,
                width: roadway.width,
                ..Mark::default()
            });
        }
        let mut course = smoothed(&marks, 4).ok_or(Error::OutOfMemory)?;
        for mark in &mut course {
            mark.level = self.worn(mark.x, mark.z);
        }
        // Graded: the level smoothed along the road.
        let mut levels = fallible::filled(course.len(), 0.0).ok_or(Error::OutOfMemory)?;
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
        // nothing grades the deck back down; then it is held to its steepest
        // grade along its length, so it ramps up to each deck and down from it.
        let mut pinned = fallible::filled(course.len(), false).ok_or(Error::OutOfMemory)?;
        let crossings = self
            .bridge(&mut course, &mut pinned, roadway.surface)
            .ok_or(Error::OutOfMemory)?;
        self.crossings
            .try_reserve(crossings.len())
            .map_err(|_| Error::OutOfMemory)?;
        self.crossings.extend(crossings);
        held_to(&mut course, &pinned, (MOST_ROAD_GRADE, usize::MAX));
        self.through = course;
        Ok(())
    }

    /// The course `way` of a countryside lays into the land, its lanes
    /// surfaced in the shares `lanes` has them, and the crossings its bridges
    /// make over the land's rivers.
    pub(crate) fn course_of(&self, way: &countryside::layout::Way, lanes: (f64, f64)) -> Course {
        let surface = match way.id.rank {
            // The road through the land is laid already.
            Rank::Highway => return Course::Already,
            Rank::Road => Some(Surface::Tarmac),
            Rank::Lane => Some(lane_surface(way.key, lanes)),
            Rank::Track => Some(Surface::Track),
            Rank::Path => None,
        };
        let Some(mut marks) = fallible::collected(
            way.line.stations.len(),
            way.line.stations.iter().map(|station| Mark {
                x: station.at.x,
                z: station.at.y,
                level: station.level,
                width: station.width,
                ..Mark::default()
            }),
        ) else {
            return Course::Refused;
        };
        let mut crossings = Vec::new();
        if let Some(surface @ (Surface::Tarmac | Surface::Gravel)) = surface {
            let Some(mut pinned) = fallible::filled(marks.len(), false) else {
                return Course::Refused;
            };
            let Some(bridged) = self.bridge(&mut marks, &mut pinned, surface) else {
                return Course::Refused;
            };
            crossings = bridged;
            held_to(
                &mut marks,
                &pinned,
                (way.id.rank.laying().steepest, RAMP_MARKS),
            );
        }
        Course::Laid {
            surface,
            marks,
            crossings,
        }
    }

    /// The spans of `course`, a way of `surface`, over a river, each raised to
    /// a deck clear of the water and `pinned` there; `None` when the heap will
    /// not hold them.
    fn bridge(
        &self,
        course: &mut [Mark],
        pinned: &mut [bool],
        surface: Surface,
    ) -> Option<Vec<Crossing>> {
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
                water = mathf::fmax(water, more.level);
                width = mathf::fmax(width, more.width);
                index += 1;
            }
            let (first, last) = (start.saturating_sub(1), index.min(course.len() - 1));
            let deck = course[first..=last]
                .iter()
                .map(|mark| mark.level)
                .fold(water + CLEARANCE + 0.08 * width, mathf::fmax);
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
                surface,
            });
        }
        Some(crossings)
    }
}

impl Coarse {
    /// The land's lattices: the coarse lattice it was worn on, and the levels
    /// refining it.
    #[must_use]
    pub const fn lattices(&self) -> &Lattices {
        &self.lattices
    }

    /// The land's middle, and how far it reaches from it either way.
    #[must_use]
    pub const fn extent(&self) -> ((f64, f64), f64) {
        self.extent
    }

    /// The square the coarse land covers: its first corner and its breadth.
    #[must_use]
    pub fn bounds(&self) -> ((f64, f64), f64) {
        (self.origin, 2.0 * self.extent.1)
    }

    /// The rivers.
    #[must_use]
    pub fn rivers(&self) -> &Courses {
        &self.rivers
    }

    /// What shapes the land's rivers' channels, if it has any.
    #[must_use]
    pub const fn form(&self) -> Option<Form> {
        self.form
    }

    /// The land's ways.
    #[must_use]
    pub fn ways(&self) -> &Ways {
        &self.ways
    }

    /// Where its ways bridge its rivers.
    #[must_use]
    pub fn crossings(&self) -> &[Crossing] {
        &self.crossings
    }

    /// The countryside laid out over it, where it is farmed.
    #[must_use]
    pub fn layout(&self) -> Option<&Layout> {
        self.layout.as_ref()
    }

    /// The land's rivers, ways, crossings and layout, given up by a consumer
    /// done refining it.
    #[must_use]
    pub fn into_parts(self) -> (Courses, Ways, Vec<Crossing>, Option<Layout>) {
        (self.rivers, self.ways, self.crossings, self.layout)
    }

    /// Join a consumer's own courses to the land before its finer levels are
    /// refined: a footpath trodden along `path`, and the level beds of
    /// `cuttings`, each indexed over the square `bounds`, its first corner
    /// and its breadth.
    pub fn join(
        &mut self,
        path: Option<&[Mark]>,
        cuttings: &[Vec<Mark>],
        bounds: ((f64, f64), f64),
    ) -> Result<(), Error> {
        if let Some(path) = path {
            let course =
                fallible::collected(path.len(), path.iter().copied()).ok_or(Error::OutOfMemory)?;
            let reach = Reach {
                per_width: 1.0,
                beyond: 3.0,
            };
            self.paths = Courses::new(&[course], bounds, reach).ok_or(Error::OutOfMemory)?;
        }
        if !cuttings.is_empty() {
            let reach = Reach {
                per_width: 1.0,
                beyond: crate::ways::CUT_REACH,
            };
            self.cuttings = Courses::new(cuttings, bounds, reach).ok_or(Error::OutOfMemory)?;
        }
        Ok(())
    }

    /// Whether a lake or a river stands anywhere on the land.
    #[must_use]
    pub fn watered(&self) -> bool {
        self.lakes.iter().any(|level| level.is_finite()) || !self.rivers.is_empty()
    }

    /// The worn land's height at `(x, z)`, the coarse lattice read through a
    /// Catmull–Rom patch so no crease of its cells shows.
    #[must_use]
    pub fn worn(&self, x: f64, z: f64) -> f64 {
        self.worn_sloped(x, z).0
    }

    /// The worn land's height at `(x, z)`, and its slope along x and z.
    pub(crate) fn worn_sloped(&self, x: f64, z: f64) -> (f64, f64, f64) {
        self.coarse_spline(&self.height, (x, z))
    }

    /// What `values`, one to each coarse sample, read at `(x, z)` through a
    /// Catmull–Rom patch so no crease of the coarse cells shows, and its rate
    /// of change along x and z; nought where it holds no value.
    pub(crate) fn coarse_spline(&self, values: &[f64], (x, z): (f64, f64)) -> (f64, f64, f64) {
        let side = self.square.side() as usize;
        let at = |column: i64, row: i64| {
            let clamp = |value: i64| {
                usize::try_from(value.clamp(0, i64::try_from(side).unwrap_or(1) - 1)).unwrap_or(0)
            };
            values
                .get(clamp(row) * side + clamp(column))
                .copied()
                .unwrap_or(0.0)
        };
        let (value, du, dv) = crate::level::catmull_rom_2d(
            at,
            (
                (x - self.origin.0) / self.step,
                (z - self.origin.1) / self.step,
            ),
        );
        (value, du / self.step, dv / self.step)
    }

    /// What `value` gives at the coarse samples about world `(x, z)`, blended
    /// between them.
    pub(crate) fn between(&self, at: (f64, f64), value: &dyn Fn(usize) -> f64) -> f64 {
        let (corners, within) = self.corners(at);
        mathf::bilinear(corners.map(value), within)
    }

    /// The coarse samples at the corners of the cell about world `(x, z)` —
    /// north-west, north-east, south-west, south-east — and where it lies
    /// across and down the cell, held to the lattice at its edges.
    fn corners(&self, (x, z): (f64, f64)) -> ([usize; 4], (f64, f64)) {
        let last = real(self.square.side().saturating_sub(1) as usize);
        let (u, v) = (
            ((x - self.origin.0) / self.step).clamp(0.0, last),
            ((z - self.origin.1) / self.step).clamp(0.0, last),
        );
        let (column, row) = (
            mathf::fmax(mathf::fmin(mathf::floor(u), last - 1.0), 0.0),
            mathf::fmax(mathf::fmin(mathf::floor(v), last - 1.0), 0.0),
        );
        let (across, down) = (u - column, v - row);
        let whole = |value: f64| u32::try_from(mathf::round_i32(value)).unwrap_or(0);
        let (column, row) = (whole(column), whole(row));
        let at = |dx: u32, dy: u32| self.square.index(column + dx, row + dy);
        ([at(0, 0), at(1, 0), at(0, 1), at(1, 1)], (across, down))
    }

    /// The grain of the land's relief at `at`, read between the coarse
    /// samples where it varies.
    pub(crate) fn grain(&self, at: (f64, f64)) -> Grain {
        match &self.grains {
            Grains::Even(grain) => *grain,
            Grains::Sampled(grains) => {
                let (corners, within) = self.corners(at);
                let read = |of: fn(&Grain) -> f64| {
                    mathf::bilinear(
                        corners.map(|index| grains.get(index).map_or(0.0, of)),
                        within,
                    )
                };
                Grain {
                    roughness: read(|grain| grain.roughness),
                    ridges: read(|grain| grain.ridges),
                    dunes: read(|grain| grain.dunes),
                }
            }
        }
    }

    /// How wet the ground at `(x, z)` lies by what drains through it: read
    /// between the coarse samples, so it never steps from one to the next.
    pub(crate) fn drained(&self, at: (f64, f64)) -> f64 {
        self.between(at, &|index| self.wetness.get(index).copied().unwrap_or(0.0))
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

    /// The level of the water standing over coarse sample `index`: a lake's
    /// surface, or the sea.
    fn water_level(&self, index: usize) -> f64 {
        let lake = self
            .lakes
            .get(index)
            .map_or(f64::NEG_INFINITY, |&level| f64::from(level));
        mathf::fmax(lake, self.character.sea.unwrap_or(f64::NEG_INFINITY))
    }

    /// The surface of the sea, a lake or a river at `(x, z)`, where one
    /// stands above ground `ground` high there.
    #[must_use]
    pub fn water_above(&self, (x, z): (f64, f64), ground: f64) -> Option<f64> {
        let lake = self
            .lakes
            .get(self.sample_at(x, z))
            .map(|&level| f64::from(level));
        let river = self.form().and_then(|form| {
            let near = self.rivers.nearest(x, z)?;
            let section = Section::new(&Station::of(&near), &form);
            (near.distance < section.half).then_some(section.water)
        });
        lake.into_iter()
            .chain(river)
            .chain(self.character.sea)
            .filter(|level| level.is_finite() && *level > ground)
            .reduce(mathf::fmax)
    }

    /// The fresh water's surface at `(x, z)` on a grid `step` apart, or
    /// negative infinity where none stands: a lake's over its basin and a
    /// little beyond, a river's along its channel as high as it flows.
    #[must_use]
    pub fn water_surface(&self, x: f64, z: f64, step: f64) -> f64 {
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
        mathf::fmax(lake, river)
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
        lake || self.character.sea.is_some_and(|sea| ground <= sea + 0.5)
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
                .character
                .sea
                .is_some_and(|sea| self.height.get(index).is_some_and(|&h| h <= sea))
    }

    /// The step of the coarsest level the land's beds are cut into, or the
    /// coarse lattice's own where it is refined by none.
    pub(crate) fn finest_cut(&self) -> f64 {
        self.lattices.level_step(0).unwrap_or(self.step)
    }
}

/// Where vertex `index` of a lattice `step` apart lies along its axis:
/// `Shape` for one an `i32` does not hold, so every place converts exactly.
fn place(index: i64, step: f64) -> Result<f64, Error> {
    i32::try_from(index)
        .map(|index| f64::from(index) * step)
        .map_err(|_| Error::Shape)
}

/// How wet ground lies, `0.0..=1.0`, that `drained` square metres drain
/// through: the more, the wetter, by the logarithm of it, so a river's floor
/// lies wet and a spur dry.
#[must_use]
pub fn wetness(drained: f64) -> f64 {
    (mathf::ln(1.0 + drained / 5_000.0) / 9.0).clamp(0.0, 1.0)
}

/// A land being built, as its countryside reads it: the ground as worn, the
/// water standing on it, and how it lies, as wooded as its `farming` says its
/// country is and with at least as much stone at hand.
struct Country<'a> {
    coarse: &'a Coarse,
    farming: Farming,
    /// The share of the land's river banks rock outcrops in.
    outcrops: f64,
}

/// The land as its countryside was laid over it: the ground as worn, and the
/// water standing on it.
impl countryside::Waters for Coarse {
    fn height(&self, at: Point) -> f64 {
        self.worn(at.x, at.y)
    }

    fn water(&self, at: Point) -> Option<f64> {
        self.water_above((at.x, at.y), self.worn(at.x, at.y))
    }
}

impl countryside::Waters for Country<'_> {
    fn height(&self, at: Point) -> f64 {
        countryside::Waters::height(self.coarse, at)
    }

    fn water(&self, at: Point) -> Option<f64> {
        countryside::Waters::water(self.coarse, at)
    }
}

impl countryside::Ground for Country<'_> {
    fn lie(&self, at: Point) -> countryside::Lie {
        let character = &self.coarse.character;
        let grain = self.coarse.grain((at.x, at.y));
        let (_, dx, dz) = self.coarse.worn_sloped(at.x, at.y);
        let slope = mathf::hypot(dx, dz);
        let wet = self.coarse.drained((at.x, at.y));
        // Stone lies at hand where the ground is steep and its rock breaks
        // through, the more so where the rock stands in beds or ridges.
        let breaking = mathf::fmax(grain.ridges, self.outcrops);
        let stony = mathf::fmax(
            smoothstep(0.08, 0.45, slope) * (0.55 + 0.45 * breaking),
            self.farming.stone,
        );
        countryside::Lie {
            wet,
            stony,
            wooded: self.farming.wooded,
            fertile: (character.growth * (1.0 - smoothstep(0.12, 0.4, slope)) * (1.0 - 0.5 * wet))
                .clamp(0.0, 1.0),
        }
    }
}

#[cfg(test)]
#[path = "coarse_tests.rs"]
mod tests;
