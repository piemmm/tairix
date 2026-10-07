//! A way's line over the ground: the least-cost route across a lattice
//! about its two ends, priced for its rank, simplified and smoothed as its
//! rank is laid, with its level and its breadth along it.
//!
//! A route is a pure function of its ends, its rank, the ground, the greater
//! ways it may follow, the plots it keeps clear of and the key its wander is
//! drawn under, so whoever asks for a way gets the same line. Its search reads the ground only where it reaches,
//! bounded below by how far each point lies from a greater way.

use core::ops::RangeInclusive;
use core::sync::atomic::{AtomicU64, Ordering};

use alloc::vec::Vec;

use tairix_terrain::grid::Grid;
use tairix_terrain::route::{Routed, Router};
use tairix_util::mathf;

use crate::ground::Ground;
use crate::key::{Key, Stage};
use crate::network::Rank;
use crate::plane::{self, Buckets, Convex, Point, Rect};
use crate::Error;

/// How a rank of way is laid out on the ground.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Laying {
    /// The lattice it is routed over, in metres.
    pub step: f64,
    /// The grade it holds to where it can.
    pub grade: f64,
    /// The steepest it climbs.
    pub steepest: f64,
    /// How broad it runs.
    pub width: f64,
    /// How far its verges reach either side of it to the boundaries that
    /// line it.
    pub verge: f64,
    /// How many rounds of corner cutting smooth its line.
    pub rounds: u32,
    /// How far its line wanders either side of its route, in metres.
    pub wander: f64,
}

impl Rank {
    /// How this rank of way is laid out.
    #[must_use]
    pub const fn laying(self) -> Laying {
        match self {
            Self::Highway => Laying {
                step: 16.0,
                grade: 0.05,
                steepest: 0.09,
                width: 7.0,
                verge: 4.0,
                rounds: 5,
                wander: 0.0,
            },
            Self::Road => Laying {
                step: 12.0,
                grade: 0.07,
                steepest: 0.12,
                width: 5.5,
                verge: 2.5,
                rounds: 4,
                wander: 0.0,
            },
            Self::Lane => Laying {
                step: 8.0,
                grade: 0.1,
                steepest: 0.18,
                width: 3.6,
                verge: 1.2,
                rounds: 3,
                wander: 0.6,
            },
            Self::Track => Laying {
                step: 6.0,
                grade: 0.12,
                steepest: 0.25,
                width: 3.0,
                verge: 0.6,
                rounds: 2,
                wander: 1.2,
            },
            Self::Path => Laying {
                step: 4.0,
                grade: 0.2,
                steepest: 0.6,
                width: 0.9,
                verge: 0.0,
                rounds: 2,
                wander: 1.6,
            },
        }
    }
}

/// A point along a way.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Station {
    /// Where it lies.
    pub at: Point,
    /// The level its surface runs at there.
    pub level: f64,
    /// How broad it runs there.
    pub width: f64,
    /// The surface of the water it crosses there, where it does.
    pub water: Option<f64>,
}

/// A way's line, station by station from its first end to its second.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Line {
    /// Its stations.
    pub stations: Vec<Station>,
}

impl Line {
    /// Its stations' places.
    pub fn places(&self) -> impl Iterator<Item = Point> + '_ {
        self.stations.iter().map(|station| station.at)
    }

    /// A copy of it; `None` where the heap will not hold one.
    #[must_use]
    pub fn copied(&self) -> Option<Self> {
        Some(Self {
            stations: tairix_util::fallible::collected(
                self.stations.len(),
                self.stations.iter().copied(),
            )?,
        })
    }

    /// The rectangle it lies in, grown by `by`; `None` for no stations.
    #[must_use]
    pub fn bounds(&self, by: f64) -> Option<Rect> {
        Rect::of(self.places()).map(|rect| rect.grown(by))
    }

    /// The point of the line nearest `at`: how far from it, along it and to
    /// which side, and the station it lies beside; `None` for a line of no
    /// segment.
    #[must_use]
    pub fn nearest(&self, at: Point) -> Option<(plane::Nearest, Station)> {
        let mut best: Option<(f64, plane::Nearest)> = None;
        let mut walked = 0.0;
        for (segment, pair) in self.stations.windows(2).enumerate() {
            let (a, b) = (pair[0].at, pair[1].at);
            let (t, squared) = plane::onto_segment(at, a, b);
            let length = (b - a).length();
            if best.is_none_or(|(least, _)| squared < least) {
                best = Some((
                    squared,
                    plane::Nearest {
                        distance: mathf::sqrt(squared),
                        along: walked + t * length,
                        segment,
                        side: (b - a).cross(at - a),
                    },
                ));
            }
            walked += length;
        }
        let (_, nearest) = best?;
        Some((nearest, *self.stations.get(nearest.segment)?))
    }
}

/// The most points a side of a route's lattice holds: a bound on the work
/// one route costs, past which its lattice coarsens.
const MOST_SIDE: u32 = 384;

/// The cost of a straight step and a diagonal one, in the units the search
/// counts.
const STRAIGHT: f64 = 100.0;
const DIAGONAL: f64 = 141.421_356;

/// The least a half of a straight step costs off a greater way, and along
/// one: a straight step is two halves and a diagonal three, so these bound
/// every step from below and the rest of a route with them.
const OFF_HALF: u64 = 47;
const ON_HALF: u64 = 17;

/// What one point of a route's lattice is known to be.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
struct Sample {
    height: f32,
    wet: u8,
    flags: u8,
    /// How far it lies from the nearest point a greater way runs over, in
    /// half steps, saturating.
    reach: u16,
}

impl Sample {
    fn packed(self) -> u64 {
        u64::from(self.height.to_bits())
            | u64::from(self.wet) << 32
            | u64::from(self.flags) << 40
            | u64::from(self.reach) << 48
    }

    fn unpacked(word: u64) -> Self {
        let byte = |shift: u32| u8::try_from((word >> shift) & 0xff).unwrap_or(0);
        Self {
            height: f32::from_bits(u32::try_from(word & 0xffff_ffff).unwrap_or(0)),
            wet: byte(32),
            flags: byte(40),
            reach: u16::try_from(word >> 48).unwrap_or(u16::MAX),
        }
    }
}

/// A lattice point's `Sample`, read in the first time the search asks for
/// it: one word, so a route's lattice may be shared while only its search
/// writes it.
#[derive(Debug, Default)]
struct Slot(AtomicU64);

impl Slot {
    fn get(&self) -> Sample {
        Sample::unpacked(self.0.load(Ordering::Relaxed))
    }

    fn set(&self, sample: Sample) {
        self.0.store(sample.packed(), Ordering::Relaxed);
    }

    /// Change it through `change`, holding it alone.
    fn change(&mut self, change: impl FnOnce(&mut Sample)) {
        let word = self.0.get_mut();
        let mut sample = Sample::unpacked(*word);
        change(&mut sample);
        *word = sample.packed();
    }
}

impl Clone for Slot {
    fn clone(&self) -> Self {
        Self(AtomicU64::new(self.0.load(Ordering::Relaxed)))
    }
}

/// A sample's flags: whether its ground has been read, lies under water,
/// carries a greater way, is barred to the route, or lies near enough what
/// bars it that a step from it is tested against the shapes themselves.
const READ: u8 = 1;
const WATER: u8 = 2;
const TAKEN: u8 = 4;
const BARRED: u8 = 8;
const NEAR: u8 = 16;

/// The square lattice a route is searched over.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Square {
    origin: Point,
    step: f64,
    side: u32,
}

impl Square {
    /// The lattice a way of `rank` from `from` to `to` is routed over: the
    /// square their box spans, widened by the margin it may detour into.
    pub(crate) fn of(rank: Rank, (from, to): (Point, Point)) -> Result<Self, Error> {
        let laying = rank.laying();
        let span = (to - from).length();
        if !span.is_finite() {
            return Err(Error::Shape);
        }
        let margin = 0.3 * span + 4.0 * laying.step;
        let origin = Point::new(from.x.min(to.x) - margin, from.y.min(to.y) - margin);
        let reach = (from.x - to.x).abs().max((from.y - to.y).abs()) + 2.0 * margin;
        let wanted = mathf::ceil(reach / laying.step) + 1.0;
        let side = u32::try_from(mathf::round_i32(wanted.clamp(3.0, f64::from(MOST_SIDE))))
            .map_err(|_| Error::Shape)?;
        Ok(Self {
            origin,
            step: reach / f64::from(side - 1),
            side,
        })
    }

    /// The rectangle it covers.
    pub(crate) fn rect(&self) -> Rect {
        let reach = self.step * f64::from(self.side - 1);
        Rect {
            low: self.origin,
            high: self.origin + Point::new(reach, reach),
        }
    }

    fn columns(&self) -> usize {
        usize::try_from(self.side).unwrap_or(0)
    }

    fn area(&self) -> usize {
        self.columns() * self.columns()
    }

    /// Point `index`'s place.
    fn place(&self, index: usize) -> Point {
        let side = self.columns().max(1);
        self.origin
            + Point::new(
                real(index % side) * self.step,
                real(index / side) * self.step,
            )
    }

    /// The point nearest `at`.
    fn index_of(&self, at: Point) -> usize {
        let top = f64::from(self.side - 1);
        let whole = |value: f64| {
            usize::try_from(mathf::round_i32(mathf::round(value).clamp(0.0, top))).unwrap_or(0)
        };
        whole((at.y - self.origin.y) / self.step) * self.columns()
            + whole((at.x - self.origin.x) / self.step)
    }

    /// The columns and the rows of its points within `rect`; `None` for none.
    fn covering(&self, rect: Rect) -> Option<(RangeInclusive<usize>, RangeInclusive<usize>)> {
        let top = f64::from(self.side - 1);
        let first = |low: f64, origin: f64| mathf::ceil((low - origin) / self.step).max(0.0);
        let last = |high: f64, origin: f64| mathf::floor((high - origin) / self.step).min(top);
        let (x0, x1) = (
            first(rect.low.x, self.origin.x),
            last(rect.high.x, self.origin.x),
        );
        let (y0, y1) = (
            first(rect.low.y, self.origin.y),
            last(rect.high.y, self.origin.y),
        );
        if !(x0 <= x1 && y0 <= y1) {
            return None;
        }
        let whole = |value: f64| usize::try_from(mathf::round_i32(value)).unwrap_or(0);
        Some((whole(x0)..=whole(x1), whole(y0)..=whole(y1)))
    }
}

/// A stretch of a greater way a route may follow, and how far either side of
/// its line the route runs along it.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Followed {
    from: Station,
    to: Station,
    reach: f64,
}

/// A route being found: its lattice marked, then searched, its ground read
/// only where the search reaches.
#[derive(Debug)]
pub(crate) struct Routing {
    rank: Rank,
    ends: (Point, Point),
    square: Square,
    samples: Vec<Slot>,
    /// How far the goal lies from the nearest greater way, in half steps.
    goal_reach: u16,
    router: Option<Router>,
    /// The greater ways' stretches over its lattice, filed by where they run.
    followed: Vec<Followed>,
    filed: Buckets,
    /// What its breadth keeps clear of, each with the rectangle it lies in.
    barred: Vec<(Rect, Convex)>,
}

/// How many lattice points a unit of a route's search settles: measured, a
/// unit of a long path's search, the dearest, keeps within about 8 ms.
const SETTLED_A_UNIT: usize = 10_000;

impl Routing {
    /// A route of `rank` from `from` to `to`.
    pub(crate) fn new(rank: Rank, ends: (Point, Point)) -> Result<Self, Error> {
        Ok(Self {
            rank,
            ends,
            square: Square::of(rank, ends)?,
            samples: Vec::new(),
            goal_reach: u16::MAX,
            router: None,
            followed: Vec::new(),
            filed: Buckets::default(),
            barred: Vec::new(),
        })
    }

    /// Whether its lattice is marked and its search begun.
    pub(crate) const fn prepared(&self) -> bool {
        self.router.is_some()
    }

    /// The rectangle its lattice covers.
    pub(crate) fn rect(&self) -> Rect {
        self.square.rect()
    }

    /// The next unit of the route's search across `ground`: its line once it
    /// is found, `None` while it is not; `Err` where none is, the heap will
    /// not hold one, or its lattice is not yet marked.
    pub(crate) fn step(&mut self, key: Key, ground: &dyn Ground) -> Result<Option<Line>, Error> {
        let view = View {
            rank: self.rank,
            square: self.square,
            samples: &self.samples,
            ground,
            barred: &self.barred,
        };
        let goal = self.square.index_of(self.ends.1);
        let goal_reach = self.goal_reach;
        let routed = self
            .router
            .as_mut()
            .ok_or(Error::Shape)?
            .advance_guided(
                SETTLED_A_UNIT,
                &|from, to, diagonal| view.price(from, to, diagonal),
                &|index| view.guide(index, (goal, goal_reach)),
            )
            .map_err(|_| Error::OutOfMemory)?;
        match routed {
            Routed::Pending => Ok(None),
            Routed::Unreachable => Err(Error::Unreachable),
            Routed::Found(path) => self.laid(key, ground, &path).map(Some),
        }
    }

    /// Mark the lattice — where the greater ways `greater` run, where
    /// `barred` keeps the route from, and how far every point lies from a
    /// greater way — and begin the search.
    pub(crate) fn prepare(
        &mut self,
        greater: &[(Rank, &Line)],
        barred: &[&Convex],
    ) -> Result<(), Error> {
        let area = self.square.area();
        self.samples =
            tairix_util::fallible::filled(area, Slot::default()).ok_or(Error::OutOfMemory)?;
        self.follow(greater)?;
        self.bar(barred)?;
        self.reach();
        let goal = self.square.index_of(self.ends.1);
        self.goal_reach = self
            .samples
            .get(goal)
            .map_or(u16::MAX, |sample| sample.get().reach);
        let grid = Grid::new(self.square.side);
        let mut router = Router::new(grid.area()).map_err(|_| Error::OutOfMemory)?;
        router
            .begin(
                grid,
                (self.square.index_of(self.ends.0), goal),
                self.square.side,
                u32::try_from(2 * ON_HALF).map_err(|_| Error::Shape)?,
            )
            .map_err(|_| Error::Shape)?;
        self.router = Some(router);
        Ok(())
    }

    /// Mark the points the greater ways `greater` run over, so the route
    /// follows them where they lead its way — each stretch's points within
    /// its way's breadth and half its verges — and keep the stretches, so
    /// the route's line runs on theirs where it follows them.
    fn follow(&mut self, greater: &[(Rank, &Line)]) -> Result<(), Error> {
        let columns = self.square.columns();
        let lattice = self.square.rect();
        self.filed = Buckets::over(lattice, FILED);
        for (rank, line) in greater {
            for pair in line.stations.windows(2) {
                let (from, to) = (pair[0], pair[1]);
                let reach = 0.5 * from.width.max(to.width) + 0.5 * rank.laying().verge;
                let Some(bounds) = Rect::of([from.at, to.at]).map(|rect| rect.grown(reach)) else {
                    continue;
                };
                let Some((xs, ys)) = self.square.covering(bounds) else {
                    continue;
                };
                for row in ys {
                    for column in xs.clone() {
                        let index = row * columns + column;
                        if plane::onto_segment(self.square.place(index), from.at, to.at).1
                            < reach * reach
                        {
                            if let Some(sample) = self.samples.get_mut(index) {
                                sample.change(|sample| sample.flags |= TAKEN);
                            }
                        }
                    }
                }
                let item = u32::try_from(self.followed.len()).map_err(|_| Error::Shape)?;
                self.followed
                    .try_reserve(1)
                    .map_err(|_| Error::OutOfMemory)?;
                self.followed.push(Followed { from, to, reach });
                self.filed.file(bounds.grown(self.square.step), item)?;
            }
        }
        self.filed.sort();
        Ok(())
    }

    /// Bar the points within reach of `barred` that the way's breadth would
    /// overlap, but for the points its ends lie at — where a way ends is its
    /// planner's to keep clear — and keep the shapes, marking the points near
    /// enough them that a step from one could cross a corner between points.
    fn bar(&mut self, barred: &[&Convex]) -> Result<(), Error> {
        let columns = self.square.columns();
        let laying = self.rank.laying();
        let clearance = 0.5 * laying.width + laying.verge + 0.5;
        // A diagonal step's nearest approach to anything lies within its
        // length of one of its ends.
        let near = 0.5 * laying.width + DIAGONAL / STRAIGHT * self.square.step;
        let ends = (
            self.square.index_of(self.ends.0),
            self.square.index_of(self.ends.1),
        );
        self.barred
            .try_reserve_exact(barred.len())
            .map_err(|_| Error::OutOfMemory)?;
        for shape in barred {
            let Some(bounds) = shape.bounds() else {
                continue;
            };
            self.barred
                .push((bounds, shape.copied().ok_or(Error::OutOfMemory)?));
            let Some((xs, ys)) = self.square.covering(bounds.grown(clearance.max(near))) else {
                continue;
            };
            for row in ys {
                for column in xs.clone() {
                    let index = row * columns + column;
                    let apart = shape.distance(self.square.place(index));
                    let flags = if apart < clearance && index != ends.0 && index != ends.1 {
                        BARRED | NEAR
                    } else if apart < near {
                        NEAR
                    } else {
                        0
                    };
                    if let Some(sample) = self.samples.get_mut(index) {
                        sample.change(|sample| sample.flags |= flags);
                    }
                }
            }
        }
        Ok(())
    }

    /// The point of the greater way it follows that `at` runs on, and that
    /// way's level there; `None` where it follows none at `at`.
    fn onto_followed(&self, at: Point) -> Option<(Point, f64)> {
        let mut nearest: Option<(f64, Point, f64)> = None;
        for item in self.filed.at(at) {
            let Some(stretch) = self.followed.get(item as usize) else {
                continue;
            };
            // Where the route follows a way it runs within a lattice step of
            // the points that way takes, wherever its line lies between them.
            let reach = stretch.reach + 0.75 * self.square.step;
            let (share, squared) = plane::onto_segment(at, stretch.from.at, stretch.to.at);
            if squared < reach * reach && nearest.is_none_or(|(least, ..)| squared < least) {
                let on = stretch.from.at.lerp(stretch.to.at, share);
                let level = stretch.from.level + (stretch.to.level - stretch.from.level) * share;
                nearest = Some((squared, on, level));
            }
        }
        nearest.map(|(_, on, level)| (on, level))
    }

    /// Every point's distance from the nearest greater way, in half steps: a
    /// chamfer transform weighing a straight step two and a diagonal three,
    /// which is the octile distance exactly.
    fn reach(&mut self) {
        let columns = self.square.columns().max(1);
        let rows = self.samples.len() / columns;
        let samples = &mut self.samples;
        for sample in samples.iter_mut() {
            sample.change(|sample| {
                sample.reach = if sample.flags & TAKEN == 0 {
                    u16::MAX
                } else {
                    0
                }
            });
        }
        for row in 0..rows {
            for column in 0..columns {
                let index = row * columns + column;
                let (west, north) = (column > 0, row > 0);
                relax(samples, index, west.then(|| index - 1), 2);
                relax(samples, index, north.then(|| index - columns), 2);
                relax(
                    samples,
                    index,
                    (north && west).then(|| index - columns - 1),
                    3,
                );
                relax(
                    samples,
                    index,
                    (north && column + 1 < columns).then(|| index - columns + 1),
                    3,
                );
            }
        }
        for row in (0..rows).rev() {
            for column in (0..columns).rev() {
                let index = row * columns + column;
                let (east, south) = (column + 1 < columns, row + 1 < rows);
                relax(samples, index, east.then(|| index + 1), 2);
                relax(samples, index, south.then(|| index + columns), 2);
                relax(
                    samples,
                    index,
                    (south && east).then(|| index + columns + 1),
                    3,
                );
                relax(
                    samples,
                    index,
                    (south && column > 0).then(|| index + columns - 1),
                    3,
                );
            }
        }
    }

    /// The line of the route along lattice points `path`: pulled taut and
    /// smoothed wherever that costs no more than the lattice's path, run on
    /// the greater ways it follows, wandering as its rank does elsewhere,
    /// levelled and broadened.
    fn laid(&self, key: Key, ground: &dyn Ground, path: &[usize]) -> Result<Line, Error> {
        let laying = self.rank.laying();
        let view = View {
            rank: self.rank,
            square: self.square,
            samples: &self.samples,
            ground,
            barred: &self.barred,
        };
        let taut = pull(&view, path)?;
        let mut smooth = Vec::new();
        smooth
            .try_reserve_exact(taut.len() + 2)
            .map_err(|_| Error::OutOfMemory)?;
        smooth.extend(taut.iter().map(|&index| self.square.place(index)));
        attach(&mut smooth, self.ends, &view);
        // A corner is cut only where the cut costs no more than the corner,
        // and keeps the way clear of what bars it.
        let cuts = |before: Point, corner: Point, after: Point| {
            let index = |at: Point| self.square.index_of(at);
            let kept = view
                .chord(index(before), index(corner))
                .zip(view.chord(index(corner), index(after)))
                .map(|(a, b)| a + b);
            view.chord(index(before), index(after))
                .is_some_and(|cut| kept.is_none_or(|kept| cut <= kept))
                && view.clear((before, after))
        };
        for _ in 0..laying.rounds {
            smooth = chaikin(&smooth, &cuts)?;
        }
        let spacing = (0.5 * laying.step).clamp(1.0, 4.0);
        let stations = resampled(&smooth, spacing)?;
        let length = plane::length(&stations);
        let seed = key.word(Stage::Route, way_place(self.ends), 0);
        let mut line = Line::default();
        line.stations
            .try_reserve_exact(stations.len())
            .map_err(|_| Error::OutOfMemory)?;
        let mut walked = 0.0;
        for (index, &at) in stations.iter().enumerate() {
            if index > 0 {
                walked += (at - stations[index - 1]).length();
            }
            let width = laying.width;
            if let Some((on, level)) = self.onto_followed(at) {
                line.stations.push(Station {
                    at: on,
                    level,
                    width,
                    water: None,
                });
                continue;
            }
            let way = way_at(&stations, index);
            // The wander dies away at either end, where the way meets what it
            // joins.
            let taper = (walked / 30.0)
                .min((length - walked) / 30.0)
                .clamp(0.0, 1.0);
            let wander = laying.wander * taper * swing(seed, walked / 70.0);
            let wandered = at + way.left() * wander;
            // It never wanders into what it keeps clear of.
            let at = if view.clear((wandered, wandered)) {
                wandered
            } else {
                at
            };
            let height = ground.height(at);
            let water = ground.water(at).filter(|&level| level > height);
            let wet = ground.lie(at).wet;
            line.stations.push(Station {
                at,
                level: height,
                width: width
                    * (1.0 + SWING * swing(seed ^ 0x9e37, walked / 45.0) + WET_SPREAD * wet),
                water,
            });
        }
        if matches!(self.rank, Rank::Highway | Rank::Road) {
            graded(&mut line.stations, laying.grade, spacing);
        }
        Ok(line)
    }
}

/// Run `points`, a route's lattice points from end to end, from and to its
/// exact `ends`: each end in place of its lattice point where the straight way
/// on from it keeps clear, and before it where it would not.
fn attach(points: &mut Vec<Point>, (from, to): (Point, Point), view: &View<'_>) {
    let (Some(&second), Some(&before_last)) = (
        points.get(1),
        points.len().checked_sub(2).and_then(|at| points.get(at)),
    ) else {
        if let Some(first) = points.first_mut() {
            *first = from;
        }
        if let Some(last) = points.last_mut() {
            *last = to;
        }
        return;
    };
    if view.clear((before_last, to)) {
        if let Some(last) = points.last_mut() {
            *last = to;
        }
    } else {
        points.push(to);
    }
    if view.clear((from, second)) {
        points[0] = from;
    } else {
        points.insert(0, from);
    }
}

/// How far a way's breadth swings either way along it, and how much broader
/// it runs over the wettest ground, as shares of its rank's.
const SWING: f64 = 0.1;
const WET_SPREAD: f64 = 0.25;

/// How far apart the squares a route files the greater ways' stretches in
/// lie.
const FILED: f64 = 16.0;

/// The lattice points `path` is pulled taut through: from each, the farthest
/// point ahead a straight chord reaches at no more cost than the path there.
fn pull(view: &View<'_>, path: &[usize]) -> Result<Vec<usize>, Error> {
    let mut taut = Vec::new();
    let Some(&first) = path.first() else {
        return Ok(taut);
    };
    let columns = view.square.columns().max(1);
    let mut spent: Vec<u64> = Vec::new();
    spent
        .try_reserve_exact(path.len())
        .map_err(|_| Error::OutOfMemory)?;
    spent.push(0);
    for pair in path.windows(2) {
        let diagonal =
            pair[0] % columns != pair[1] % columns && pair[0] / columns != pair[1] / columns;
        let step = view
            .price(pair[0], pair[1], diagonal)
            .ok_or(Error::Unreachable)?;
        spent.push(spent.last().copied().unwrap_or(0) + u64::from(step));
    }
    let fits = |from: usize, to: usize| {
        view.chord(path[from], path[to])
            .is_some_and(|cost| cost <= spent[to] - spent[from])
            && view.clear((view.square.place(path[from]), view.square.place(path[to])))
    };
    taut.try_reserve(16).map_err(|_| Error::OutOfMemory)?;
    taut.push(first);
    let last = path.len() - 1;
    let mut from = 0;
    while from < last {
        // Gallop out while the chord fits, then halve back to its farthest.
        let (mut good, mut bad) = (from + 1, last + 1);
        let mut stride = 2;
        while good < last {
            let probe = (from + stride).min(last);
            if fits(from, probe) {
                good = probe;
                stride *= 2;
            } else {
                bad = probe;
                break;
            }
        }
        while bad - good > 1 {
            let middle = good + (bad - good) / 2;
            if fits(from, middle) {
                good = middle;
            } else {
                bad = middle;
            }
        }
        taut.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
        taut.push(path[good]);
        from = good;
    }
    Ok(taut)
}

/// Sample `index`'s reach narrowed to what reaching it from sample `from`,
/// `weight` half steps away, gives.
fn relax(samples: &mut [Slot], index: usize, from: Option<usize>, weight: u16) {
    let Some(through) = from
        .and_then(|from| samples.get(from))
        .map(|from| from.get().reach.saturating_add(weight))
    else {
        return;
    };
    if let Some(sample) = samples.get_mut(index) {
        sample.change(|sample| sample.reach = sample.reach.min(through));
    }
}

/// What a route's search reads its lattice through.
struct View<'a> {
    rank: Rank,
    square: Square,
    samples: &'a [Slot],
    ground: &'a dyn Ground,
    barred: &'a [(Rect, Convex)],
}

impl View<'_> {
    /// Point `index`, its ground read the first time it is asked for.
    fn sample(&self, index: usize) -> Option<Sample> {
        let cell = self.samples.get(index)?;
        let mut sample = cell.get();
        if sample.flags & READ == 0 {
            let at = self.square.place(index);
            let height = self.ground.height(at);
            sample.height = single(height);
            sample.wet = byte(self.ground.lie(at).wet);
            if self.ground.water(at).is_some_and(|level| level > height) {
                sample.flags |= WATER;
            }
            sample.flags |= READ;
            cell.set(sample);
        }
        Some(sample)
    }

    /// What the step from point `from` to its neighbour `to` costs a way of
    /// this rank; `None` where it may not go.
    fn price(&self, from: usize, to: usize, diagonal: bool) -> Option<u32> {
        let next = self.sample(to)?;
        if next.flags & BARRED != 0 {
            return None;
        }
        let here = self.sample(from)?;
        if (next.flags | here.flags) & NEAR != 0
            && !self.clear((self.square.place(from), self.square.place(to)))
        {
            return None;
        }
        let halves = if diagonal { 3 } else { 2 };
        if next.flags & TAKEN != 0 && next.flags & WATER == 0 {
            return u32::try_from(ON_HALF * halves).ok();
        }
        let laying = self.rank.laying();
        let run = if diagonal { DIAGONAL } else { STRAIGHT };
        let rise = f64::from(next.height - here.height).abs();
        let grade = rise / (self.square.step * run / STRAIGHT);
        let climb = (grade / laying.grade) * (grade / laying.grade);
        let steep = 40.0 * ((grade - laying.steepest) / laying.steepest).max(0.0);
        let wet = 3.0 * f64::from(next.wet) / 255.0;
        // Water is crossed where it is narrowest: a bridge costs a highway, a
        // road or a lane dear, a ford a track less, stepping stones a path
        // most of all.
        let water = if next.flags & WATER == 0 {
            0.0
        } else {
            match self.rank {
                Rank::Highway | Rank::Road | Rank::Lane => 30.0,
                Rank::Track => 12.0,
                Rank::Path => 60.0,
            }
        };
        let cost = run * (1.0 + climb + steep + wet + water);
        let counted = u64::try_from(mathf::round_i32(cost.min(2.0e9))).unwrap_or(u64::MAX);
        u32::try_from(counted.max(OFF_HALF * halves)).ok()
    }

    /// Whether a way of this rank laid straight from `a` to `b` keeps its
    /// breadth, as broad as it is ever laid, clear of everything that bars
    /// it.
    fn clear(&self, (a, b): (Point, Point)) -> bool {
        let half = 0.5 * self.rank.laying().width * (1.0 + SWING + WET_SPREAD);
        let Some(span) = Rect::of([a, b]).map(|rect| rect.grown(half)) else {
            return true;
        };
        self.barred
            .iter()
            .all(|(bounds, shape)| !bounds.overlaps(span) || shape.segment_distance((a, b)) >= half)
    }

    /// What the straight chord from point `from` to point `to` costs, along
    /// the lattice's digital line between them (Bresenham, 1965); `None`
    /// where it crosses a barred point.
    fn chord(&self, from: usize, to: usize) -> Option<u64> {
        let columns = self.square.columns().max(1);
        let position = |index: usize| {
            (
                i64::try_from(index % columns).unwrap_or(0),
                i64::try_from(index / columns).unwrap_or(0),
            )
        };
        let ((mut x, mut y), (x1, y1)) = (position(from), position(to));
        let (dx, dy) = ((x1 - x).abs(), -(y1 - y).abs());
        let (sx, sy) = ((x1 - x).signum(), (y1 - y).signum());
        let mut error = dx + dy;
        let (mut here, mut cost) = (from, 0u64);
        while (x, y) != (x1, y1) {
            let doubled = 2 * error;
            let (before_x, before_y) = (x, y);
            if doubled >= dy {
                error += dy;
                x += sx;
            }
            if doubled <= dx {
                error += dx;
                y += sy;
            }
            let next = usize::try_from(y).ok()? * columns + usize::try_from(x).ok()?;
            cost += u64::from(self.price(here, next, x != before_x && y != before_y)?);
            here = next;
        }
        Some(cost)
    }

    /// The least the rest of the way from point `index` to the goal costs:
    /// all of it off greater ways, or as much along them as their nearest
    /// to either end allows.
    fn guide(&self, index: usize, (goal, goal_reach): (usize, u16)) -> u32 {
        let columns = self.square.columns().max(1);
        let (dx, dy) = (
            (index % columns).abs_diff(goal % columns),
            (index / columns).abs_diff(goal / columns),
        );
        let (long, short) = (dx.max(dy), dx.min(dy));
        let halves = u64::try_from(2 * (long - short) + 3 * short).unwrap_or(u64::MAX);
        let reach = self
            .samples
            .get(index)
            .map_or(u16::MAX, |sample| sample.get().reach);
        let off = OFF_HALF.saturating_mul(halves);
        let along = ON_HALF.saturating_mul(halves).saturating_add(
            (OFF_HALF - ON_HALF) * (u64::from(reach.saturating_sub(3)) + u64::from(goal_reach)),
        );
        u32::try_from(off.min(along)).unwrap_or(u32::MAX)
    }
}

/// A way's ends as a key's place, the same however the two are given.
fn way_place((from, to): (Point, Point)) -> (i64, i64) {
    let centimetres = |point: Point| {
        (
            i64::from(mathf::round_i32((point.x * 100.0).clamp(-2.0e9, 2.0e9))),
            i64::from(mathf::round_i32((point.y * 100.0).clamp(-2.0e9, 2.0e9))),
        )
    };
    let (a, b) = (centimetres(from), centimetres(to));
    let (low, high) = if a <= b { (a, b) } else { (b, a) };
    (
        low.0 ^ high.1.rotate_left(17),
        low.1 ^ high.0.rotate_left(29),
    )
}

/// A smooth swing in `-1.0..=1.0` along `along`, drawn from `seed`: a sum
/// of two waves, its phases its seed's.
fn swing(seed: u64, along: f64) -> f64 {
    let phase =
        |shift: u32| tairix_rng::rand::unit_from(seed.rotate_left(shift)) * core::f64::consts::TAU;
    0.65 * mathf::sin(along * core::f64::consts::TAU + phase(7))
        + 0.35 * mathf::sin(along * 2.71 * core::f64::consts::TAU + phase(31))
}

/// The way the line runs at its `index`th point.
fn way_at(points: &[Point], index: usize) -> Point {
    let before = points[index.saturating_sub(1)];
    let after = points[(index + 1).min(points.len() - 1)];
    (after - before).normalized()
}

/// `points` with every corner but its ends cut a quarter of the way along
/// each edge (Chaikin, 1974), where `cuts` — told the cut's ends and the
/// corner between — allows it; a corner it refuses is kept.
fn chaikin(
    points: &[Point],
    cuts: &dyn Fn(Point, Point, Point) -> bool,
) -> Result<Vec<Point>, Error> {
    let mut cut = Vec::new();
    if points.len() < 3 {
        cut.try_reserve_exact(points.len())
            .map_err(|_| Error::OutOfMemory)?;
        cut.extend_from_slice(points);
        return Ok(cut);
    }
    cut.try_reserve_exact(3 * points.len())
        .map_err(|_| Error::OutOfMemory)?;
    cut.push(points[0]);
    for (index, pair) in points.windows(2).enumerate() {
        let (a, b) = (pair[0], pair[1]);
        let three = a.lerp(b, 0.75);
        cut.push(a.lerp(b, 0.25));
        cut.push(three);
        if let Some(&next) = points.get(index + 2) {
            if !cuts(three, b, b.lerp(next, 0.25)) {
                cut.push(b);
            }
        }
    }
    cut.push(points[points.len() - 1]);
    Ok(cut)
}

/// `points` laid again at stations `spacing` apart along it, its ends kept.
fn resampled(points: &[Point], spacing: f64) -> Result<Vec<Point>, Error> {
    let length = plane::length(points);
    let count = usize::try_from(mathf::round_i32(mathf::ceil(length / spacing).max(1.0)))
        .map_err(|_| Error::Shape)?;
    let mut stations = Vec::new();
    stations
        .try_reserve_exact(count + 1)
        .map_err(|_| Error::OutOfMemory)?;
    let mut walk = plane::Walk::new(points);
    for index in 0..=count {
        let along = length * real(index) / real(count);
        if let Some((at, _)) = walk.at(along) {
            stations.push(at);
        }
    }
    Ok(stations)
}

/// `stations`' levels, eased as an engineered way's are and held to its
/// `grade`, its cuttings and banks graded to the ground beside them.
fn graded(stations: &mut [Station], grade: f64, spacing: f64) {
    let count = stations.len();
    if count < 3 {
        return;
    }
    for _ in 0..6 {
        // The level before each station as it stood before this pass.
        let mut before = stations[0].level;
        for index in 1..count - 1 {
            let here = stations[index].level;
            stations[index].level = 0.25 * before + 0.5 * here + 0.25 * stations[index + 1].level;
            before = here;
        }
    }
    let most = grade * spacing;
    for index in 1..count {
        let held = stations[index - 1].level;
        stations[index].level = stations[index].level.clamp(held - most, held + most);
    }
    for index in (0..count - 1).rev() {
        let held = stations[index + 1].level;
        stations[index].level = stations[index].level.clamp(held - most, held + most);
    }
}

/// A share in `0.0..=1.0` as a byte.
fn byte(share: f64) -> u8 {
    u8::try_from(mathf::round_i32((share.clamp(0.0, 1.0)) * 255.0)).unwrap_or(u8::MAX)
}

/// `value` as `f32`, rounded as every target rounds it.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a height is held to a float's precision, as every grid of a land holds it"
)]
fn single(value: f64) -> f32 {
    value as f32
}

/// A count as `f64`.
#[allow(
    clippy::cast_precision_loss,
    reason = "a lattice's points and a line's stations are counted far within an f64's whole numbers"
)]
fn real(count: usize) -> f64 {
    count as f64
}

#[cfg(test)]
#[path = "route_tests.rs"]
mod tests;
