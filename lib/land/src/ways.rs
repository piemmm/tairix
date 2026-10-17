//! A land's ways: the road routed through it and the countryside's roads,
//! lanes, tracks and paths laid into it, the ditches dug along its
//! boundaries, the gateways stock churn, and what snow drifts against; and
//! the beds each of them leaves in the ground.

use alloc::vec::Vec;

use tairix_countryside::boundary::{Kind as Bound, Side, Through};
use tairix_countryside::layout::Layout;
use tairix_countryside::usage::Use;
use tairix_countryside::{self as countryside, Point};
use tairix_noise::smoothstep;
use tairix_parallel::JobRunner;
use tairix_util::fallible;
use tairix_util::mathf::{self, real};

use crate::coarse::{Coarse, Crossing};
use crate::course::{traced, Courses, Indexing, Mark, Nearest, Reach, INDEX_UNIT};
use crate::plan::{Plan, Surface};
use crate::snow::{self, Barrier};
use crate::Error;

/// A land's ways, each surface's apart, and the paths worn across it; and
/// what its countryside cuts and treads into its fields: the ditches dug
/// along their boundaries, and the gateways where stock churn the ground.
#[derive(Clone, Debug)]
pub struct Ways {
    /// Each surface's ways: tarmac, gravel and tracks, in that order.
    pub surfaced: [Courses; 3],
    /// The paths trodden where no way is surfaced.
    pub trodden: Courses,
    /// The ditches dug along the fields' boundaries.
    pub ditches: Courses,
    /// The gateways stock churn to mud.
    pub churned: Courses,
}

/// What snow drifts against while a land is laid: its boundaries standing
/// between their gaps, each a course, and each course's barrier.
#[derive(Clone, Debug, Default)]
pub(crate) struct Drifts {
    pub(crate) courses: Courses,
    pub(crate) barriers: Vec<Barrier>,
}

impl Ways {
    /// No ways at all.
    #[must_use]
    pub fn none() -> Self {
        Self {
            surfaced: [Courses::none(), Courses::none(), Courses::none()],
            trodden: Courses::none(),
            ditches: Courses::none(),
            churned: Courses::none(),
        }
    }

    /// The ways surfaced with `surface`.
    #[must_use]
    pub fn of(&self, surface: Surface) -> &Courses {
        &self.surfaced[surface as usize]
    }
}

/// How many marks either side of a bridge's deck its approach is held to its
/// way's grade over, at the most: a ramp, not a regrading of the whole way.
pub(crate) const RAMP_MARKS: usize = 40;

/// How far a track lies worn below the ground it runs over.
const WORN: f64 = 0.12;

/// Hold `course`'s levels to `grade`, rise over run, walking it each way, the
/// `pinned` marks held where they stand: from every pinned mark at most
/// `reach` marks on, or along the whole course where every mark is graded.
pub(crate) fn held_to(course: &mut [Mark], pinned: &[bool], (grade, reach): (f64, usize)) {
    let count = course.len();
    for pass in 0..2 {
        let mut since = usize::MAX;
        for step in 0..count {
            let index = if pass == 0 { step } else { count - 1 - step };
            if pinned.get(index).copied().unwrap_or(false) {
                since = 0;
                continue;
            }
            since = since.saturating_add(1);
            let other = if pass == 0 {
                index.checked_sub(1)
            } else {
                Some(index + 1).filter(|&next| next < count)
            };
            let Some(other) = other.filter(|_| reach == usize::MAX || since <= reach) else {
                continue;
            };
            let run = mathf::hypot(
                course[index].x - course[other].x,
                course[index].z - course[other].z,
            );
            let limit = grade * run;
            let (from, to) = (course[other].level, course[index].level);
            course[index].level = mathf::clamp(to, from - limit, from + limit);
        }
    }
}

/// What the lane keyed `key` is surfaced with, of the shares `(tarmac,
/// gravel)` a land's lanes are.
pub(crate) fn lane_surface(key: u64, (tarmac, gravel): (f64, f64)) -> Surface {
    let drawn = real(usize::try_from(key >> 40).unwrap_or(0)) / real(1 << 24);
    if drawn < tarmac {
        Surface::Tarmac
    } else if drawn < tarmac + (1.0 - tarmac) * gravel {
        Surface::Gravel
    } else {
        Surface::Track
    }
}

/// How much of a place a way wears: bare of anything growing, and trodden
/// short, both `0.0..=1.0`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Worn {
    pub(crate) bare: f64,
    pub(crate) trodden: f64,
}

/// How trodden a track's grass grows between and beside its ruts.
const TRODDEN: f64 = 0.4;

/// How far either side of its line a track's wheels wear its ruts bare.
pub const RUT: f64 = 0.22;

/// `natural` ground `near` a way of `surface`: its bed across its breadth —
/// an engineered way's cambered at its graded level, a track's worn into two
/// ruts a little below the ground it runs over — cut or filled back to the
/// land across its verges; and how it wears the place: an engineered way bare
/// across its breadth, a track bare only in its ruts and trodden between and
/// beside them, as a green lane is.
pub(crate) fn road_bed(
    near: &Nearest,
    natural: f64,
    (surface, step): (Surface, f64),
) -> (f64, Worn) {
    let half = 0.5 * near.width;
    let across = near.distance;
    let wheel = (across - TRACK_GAUGE).abs();
    let (level, shape) = match surface {
        Surface::Tarmac | Surface::Gravel => (near.level, -0.03 * mathf::fmin(across, half)),
        Surface::Track => (natural - WORN, -0.09 * (1.0 - smoothstep(0.1, 0.32, wheel))),
    };
    let bed = level + shape;
    // Verges and cuttings as broad as the road, softened across a step.
    let verge = smoothstep(half, half + near.width + step, across);
    let height = bed + (natural - bed) * verge;
    let footprint = 1.0 - smoothstep(half - 0.3, half + 0.3 + 0.5 * step, across);
    let worn = match surface {
        Surface::Tarmac | Surface::Gravel => Worn {
            bare: footprint,
            trodden: 0.0,
        },
        Surface::Track => Worn {
            bare: footprint * (1.0 - smoothstep(RUT - 0.05, RUT + 0.05, wheel)),
            trodden: TRODDEN * footprint,
        },
    };
    (height, worn)
}

/// Courses' marks, course by course.
type Marked = Vec<Vec<Mark>>;

/// The ditches dug along the boundaries of `layout` its kind says are
/// ditched, and the gateways stock churn: a mark across each gateway into a
/// field they graze, reaching a little into the ground either side.
fn dug(layout: &Layout) -> Option<(Marked, Marked)> {
    let (mut ditches, mut churned) = (Vec::new(), Vec::new());
    let grazed = |side: Side| match side {
        Side::Field(id) => layout
            .parcels()
            .binary_search_by(|parcel| parcel.field.id.cmp(&id))
            .ok()
            .and_then(|index| layout.parcels().get(index))
            .is_some_and(|parcel| matches!(parcel.usage.used, Use::Pasture | Use::Meadow)),
        _ => false,
    };
    for boundary in layout.boundaries() {
        if boundary.kind == Bound::Ditch {
            let course = fallible::collected(
                boundary.line.len(),
                boundary.line.iter().map(|at| Mark {
                    x: at.x,
                    z: at.y,
                    width: DITCH.0,
                    depth: DITCH.1,
                    ..Mark::default()
                }),
            )?;
            ditches.try_reserve(1).ok()?;
            ditches.push(course);
        }
        if !(grazed(boundary.left) || grazed(boundary.right)) {
            continue;
        }
        for gap in boundary
            .gaps
            .iter()
            .filter(|gap| gap.through == Through::Gateway)
        {
            let Some((at, way)) = countryside::plane::at(&boundary.line, gap.along) else {
                continue;
            };
            let across = way.left() * CHURNED;
            let mark = |at: Point| Mark {
                x: at.x,
                z: at.y,
                width: gap.width + 1.5,
                ..Mark::default()
            };
            churned.try_reserve(1).ok()?;
            churned.push(fallible::collected(
                2,
                [mark(at - across), mark(at + across)].into_iter(),
            )?);
        }
    }
    Some((ditches, churned))
}

/// How broad a ditch is cut and how deep.
const DITCH: (f64, f64) = (1.4, 0.65);

/// The boundaries of `layout` snow drifts against, each stretch standing
/// between its gaps a course of its own, and each course's barrier; `None`
/// when the heap will not hold them.
fn barriers(layout: &Layout) -> Option<(Marked, Vec<Barrier>)> {
    let (mut courses, mut standing) = (Vec::new(), Vec::new());
    for boundary in layout.boundaries() {
        let porosity = boundary.kind.porosity();
        if boundary.height <= 0.0 || porosity >= 1.0 {
            continue;
        }
        let barrier = Barrier {
            height: boundary.height,
            porosity,
        };
        let length = countryside::plane::length(&boundary.line);
        for span in countryside::boundary::standing(&boundary.gaps, (0.0, length), 0.5).ok()? {
            courses.try_reserve(1).ok()?;
            courses.push(traced(&boundary.line, span, 0.0)?);
            standing.try_reserve(1).ok()?;
            standing.push(barrier);
        }
    }
    Some((courses, standing))
}

/// What one of a countryside's ways lays into a land.
#[derive(Debug)]
pub(crate) enum Course {
    /// Nothing: the road through the land is laid already.
    Already,
    /// A way surfaced with `surface`, or a path trodden where none: its
    /// marks, and the crossings its bridges make.
    Laid {
        surface: Option<Surface>,
        marks: Vec<Mark>,
        crossings: Vec<Crossing>,
    },
    /// The heap would not hold it.
    Refused,
}

/// How many of a countryside's ways each core lays into a land a unit.
const WAYS_EACH: usize = 8;

/// A land's ways being laid into it, a part a unit: the countryside's ways
/// laid out over it, a few to each core, then each surface's courses, the
/// paths trodden, the ditches and the churned gateways each indexed in a unit
/// of its own.
#[derive(Debug)]
pub(crate) struct Waying {
    pub(crate) layout: Option<Layout>,
    /// The next of the layout's ways to lay, how many it holds, and whether
    /// its lanes are tarmacked, gravelled or left green.
    next: usize,
    count: usize,
    lanes: (f64, f64),
    /// Each surface's courses, the paths', and the crossings their bridges
    /// make, in their ways' order.
    surfaced: [Vec<Vec<Mark>>; 3],
    trodden: Vec<Vec<Mark>>,
    pub(crate) crossings: Vec<Crossing>,
    /// The churned gateways' marks, found with the ditches'.
    churned: Vec<Vec<Mark>>,
    /// The ways' courses, as many of their indexes built as `built` counts,
    /// and the next being built; and, where snow lies, what it drifts
    /// against.
    pub(crate) ways: Ways,
    pub(crate) drifts: Drifts,
    built: usize,
    indexing: Option<Indexing>,
}

impl Waying {
    /// The laying of `coarse`'s ways as `plan` has them: the road through it,
    /// taken from it, and the ways of the countryside `layout` laid out over
    /// it.
    pub(crate) fn new(
        coarse: &mut Coarse,
        plan: &Plan,
        layout: Option<Layout>,
    ) -> Result<Self, Error> {
        let mut surfaced: [Vec<Vec<Mark>>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        if let Some(roadway) = plan.road.filter(|_| !coarse.through.is_empty()) {
            let into = &mut surfaced[roadway.surface as usize];
            into.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
            into.push(core::mem::take(&mut coarse.through));
        }
        Ok(Self {
            count: layout.as_ref().map_or(0, |layout| layout.ways().count()),
            layout,
            next: 0,
            lanes: plan.farming.map_or((1.0, 1.0), |farming| farming.lanes),
            surfaced,
            trodden: Vec::new(),
            crossings: Vec::new(),
            churned: Vec::new(),
            ways: Ways::none(),
            drifts: Drifts::default(),
            built: 0,
            indexing: None,
        })
    }

    /// The next unit of the laying over `coarse` across `runner`: whether
    /// every way is laid and indexed.
    pub(crate) fn step(&mut self, coarse: &Coarse, runner: &dyn JobRunner) -> Result<bool, Error> {
        if self.next < self.count {
            self.lay(coarse, runner)?;
            return Ok(false);
        }
        let bounds = coarse.bounds();
        let reach = Reach {
            per_width: 3.0,
            beyond: 4.0 * coarse.finest_cut(),
        };
        let narrow = Reach {
            per_width: 1.0,
            beyond: 3.0,
        };
        let Some(indexing) = self.indexing.as_mut() else {
            let (courses, reach) = match self.built {
                surface @ 0..=2 => (core::mem::take(&mut self.surfaced[surface]), reach),
                3 => (core::mem::take(&mut self.trodden), narrow),
                4 => {
                    let (ditches, churned) = self
                        .layout
                        .as_ref()
                        .map_or_else(|| Some((Vec::new(), Vec::new())), dug)
                        .ok_or(Error::OutOfMemory)?;
                    self.churned = churned;
                    (ditches, narrow)
                }
                5 => (core::mem::take(&mut self.churned), narrow),
                6 if coarse.character.snowpack.is_some() => {
                    let (courses, standing) = self
                        .layout
                        .as_ref()
                        .map_or_else(|| Some((Vec::new(), Vec::new())), barriers)
                        .ok_or(Error::OutOfMemory)?;
                    let tallest = standing
                        .iter()
                        .map(|barrier| barrier.height)
                        .fold(0.0, mathf::fmax);
                    self.drifts.barriers = standing;
                    let drifting = Reach {
                        per_width: 0.0,
                        beyond: snow::reach(tallest),
                    };
                    (courses, drifting)
                }
                _ => return Ok(true),
            };
            self.indexing = Some(Indexing::new(courses, bounds, reach).ok_or(Error::OutOfMemory)?);
            return Ok(false);
        };
        if indexing.step(INDEX_UNIT).ok_or(Error::OutOfMemory)? {
            let courses = self
                .indexing
                .take()
                .and_then(Indexing::finish)
                .ok_or(Error::OutOfMemory)?;
            match self.built {
                surface @ 0..=2 => self.ways.surfaced[surface] = courses,
                3 => self.ways.trodden = courses,
                4 => self.ways.ditches = courses,
                5 => self.ways.churned = courses,
                _ => self.drifts.courses = courses,
            }
            self.built += 1;
        }
        Ok(false)
    }

    /// The next of the layout's ways laid over `coarse` across `runner`, a
    /// few to each core, each kept in its order.
    fn lay(&mut self, coarse: &Coarse, runner: &dyn JobRunner) -> Result<(), Error> {
        let layout = self.layout.as_ref().ok_or(Error::Shape)?;
        let take = runner.width().max(1).saturating_mul(WAYS_EACH);
        let mut jobs: Vec<(&countryside::layout::Way, Option<Course>)> = Vec::new();
        jobs.try_reserve_exact(take)
            .map_err(|_| Error::OutOfMemory)?;
        jobs.extend(
            layout
                .ways()
                .skip(self.next)
                .take(take)
                .map(|way| (way, None)),
        );
        self.next += jobs.len();
        let lanes = self.lanes;
        tairix_parallel::for_each(runner, &mut jobs, &|(way, course)| {
            *course = Some(coarse.course_of(way, lanes));
        });
        for (_, course) in jobs {
            match course.ok_or(Error::Shape)? {
                Course::Already => {}
                Course::Laid {
                    surface,
                    marks,
                    crossings,
                } => {
                    let into = match surface {
                        Some(surface) => &mut self.surfaced[surface as usize],
                        None => &mut self.trodden,
                    };
                    into.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                    into.push(marks);
                    self.crossings
                        .try_reserve(crossings.len())
                        .map_err(|_| Error::OutOfMemory)?;
                    self.crossings.extend(crossings);
                }
                Course::Refused => return Err(Error::OutOfMemory),
            }
        }
        Ok(())
    }
}

/// How far into the ground either side a gateway's churned mud reaches.
const CHURNED: f64 = 5.0;

/// The ground a ditch `near` leaves at `natural` ground: its bed falling to
/// its depth across its breadth, rounded as a ditch silts and slumps; and how
/// wet it lies, its foot holding water.
pub(crate) fn ditch_bed(near: &Nearest, natural: f64) -> (f64, f64) {
    let half = 0.5 * near.width;
    let across = mathf::fmin(near.distance / half, 1.0);
    let deep = near.depth * (1.0 - across * across);
    (natural - deep, smoothstep(0.65, 0.15, across))
}

/// How far the ground `near` a path is worn by it: bare along its middle,
/// fading out across a step past its edges.
pub(crate) fn path_worn(near: &Nearest, step: f64) -> f64 {
    let half = 0.5 * near.width;
    1.0 - smoothstep(half * 0.6, half + step, near.distance)
}

/// The ground a cutting `near` leaves: its level floor across its breadth,
/// and beyond it sides climbing as steeply as cut earth stands, back to the
/// land; and past its ends nothing, so a cutting ends square at the face a
/// structure there retains. A cutting only ever takes ground away.
pub(crate) fn cutting_bed(near: &Nearest) -> f64 {
    if near.past > 0.0 {
        return f64::INFINITY;
    }
    near.level + mathf::fmax(near.distance - 0.5 * near.width, 0.0) * CUT_SLOPE
}

/// How steeply a cutting's sides climb, as rise over run.
pub const CUT_SLOPE: f64 = 1.1;
/// The deepest a cutting is dug: a work carried on deeper into a hill
/// tunnels instead.
pub const DEEPEST_CUT: f64 = 4.5;

/// How far beyond its breadth a cutting reaches: as far as its sides climb
/// from twice the deepest cut, which no work's sampling overshoots.
pub(crate) const CUT_REACH: f64 = 2.0 * DEEPEST_CUT / CUT_SLOPE;

/// How far either rut of a track lies from its middle.
pub const TRACK_GAUGE: f64 = 0.72;
