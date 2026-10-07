//! Courses across the land — a river's, a road's, a path's — as polylines
//! carrying at each vertex the level they run at and how wide and deep they
//! are, and an index that finds the nearest one to any point in about as
//! many steps as there are courses near it.
//!
//! A land's grids are filled a vertex at a time, each asking what course
//! runs nearest and how far off, so the index buckets every segment into the
//! squares of a grid it can reach: a question reads one square.

use alloc::vec::Vec;

use tairix_countryside::plane::{self, Point};
use tairix_util::{fallible, mathf};

use crate::vector::real;

/// One vertex of a course.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(crate) struct Mark {
    pub(crate) x: f64,
    pub(crate) z: f64,
    /// The height the course runs at here: a river's brim, a road's bed.
    pub(crate) level: f64,
    /// Its breadth, bank to bank or verge to verge.
    pub(crate) width: f64,
    /// How far below its level its bed lies: a river's depth.
    pub(crate) depth: f64,
    /// What only a river's marks carry, nought along a road or a path: how
    /// far its water has run to come here from the divide above its head;
    /// how many units of pool and riffle it has run through since its head;
    /// how sharply it bends here, per metre, toward the side `Nearest`
    /// counts positive; and how steeply its brim falls.
    pub(crate) run: f64,
    pub(crate) phase: f64,
    pub(crate) turn: f64,
    pub(crate) fall: f64,
}

/// The nearest point of a course to a place.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Nearest {
    /// Which course, and how far along it from its first vertex.
    pub(crate) course: usize,
    pub(crate) along: f64,
    /// How far off the place lies, and on which side, positive to the left
    /// going downstream.
    pub(crate) distance: f64,
    pub(crate) side: f64,
    /// The course's level, breadth, depth, and its river's run, phase, turn
    /// and fall there, blended between its vertices.
    pub(crate) level: f64,
    pub(crate) width: f64,
    pub(crate) depth: f64,
    pub(crate) run: f64,
    pub(crate) phase: f64,
    pub(crate) turn: f64,
    pub(crate) fall: f64,
    /// Which way the course runs there, as a unit step in x and z:
    /// downstream along a river.
    pub(crate) toward: (f64, f64),
    /// How far past one of its ends the place lies, along its line there:
    /// nought where its nearest point falls within the course.
    pub(crate) past: f64,
}

/// A segment of a course as a place lies against it: the segment's first
/// mark, how far along it the place's nearest point lies as a share of its
/// length, short of its start or past its end where below nought or above
/// one, and how far off that nearest point is.
#[derive(Copy, Clone, Debug)]
struct Segment {
    mark: usize,
    unclamped: f64,
    distance: f64,
}

/// How far from a course a point may ask after it: `per_width` times the
/// course's breadth there, and `beyond` more.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Reach {
    pub(crate) per_width: f64,
    pub(crate) beyond: f64,
}

impl Reach {
    fn of(&self, width: f64) -> f64 {
        self.per_width * width + self.beyond
    }
}

/// A set of courses and the index over them.
#[derive(Clone, Debug, Default)]
pub(crate) struct Courses {
    marks: Vec<Mark>,
    /// Where each course's marks start in `marks`, and one past the last.
    starts: Vec<u32>,
    /// Distance along each mark's course, from its first.
    along: Vec<f64>,
    index: Index,
}

/// Segments bucketed by the grid squares they can reach.
#[derive(Clone, Debug, Default)]
struct Index {
    origin: (f64, f64),
    size: f64,
    columns: usize,
    rows: usize,
    /// Each square's first entry in `entries`, and one past its last.
    offsets: Vec<u32>,
    /// Segments, as the index of their first mark.
    entries: Vec<u32>,
}

impl Courses {
    /// No courses at all.
    pub(crate) fn none() -> Self {
        Self::default()
    }

    /// The courses of `courses`, indexed over the square from `origin`
    /// `span` across so a point may ask for any course within `reach` of it,
    /// a broad course reaching further than a narrow one, all at once; `None`
    /// when the heap will not hold them.
    pub(crate) fn new(
        courses: &[Vec<Mark>],
        square: ((f64, f64), f64),
        reach: Reach,
    ) -> Option<Self> {
        let mut indexing = Indexing::copied(courses, square, reach)?;
        while !indexing.step(usize::MAX)? {}
        indexing.finish()
    }

    /// How many courses there are.
    pub(crate) fn len(&self) -> usize {
        self.starts.len().saturating_sub(1)
    }

    /// Course `index`'s marks.
    pub(crate) fn course(&self, index: usize) -> &[Mark] {
        let (Some(&start), Some(&end)) = (self.starts.get(index), self.starts.get(index + 1))
        else {
            return &[];
        };
        self.marks.get(start as usize..end as usize).unwrap_or(&[])
    }

    /// Course `index` as it runs `along` its length from its first mark,
    /// blended between the marks either side and held at its ends; `None`
    /// for a course with no marks.
    pub(crate) fn at(&self, index: usize, along: f64) -> Option<Mark> {
        let (start, end) = (
            *self.starts.get(index)? as usize,
            *self.starts.get(index + 1)? as usize,
        );
        blended(
            self.marks.get(start..end)?,
            self.along.get(start..end)?,
            along,
        )
    }

    /// The way course `index` runs at `along` its length — down it, and
    /// across it to its left — from its marks a metre either side, the place
    /// held that far within its ends so the way holds right to and past
    /// them; `None` for a course with no marks.
    pub(crate) fn way(&self, index: usize, along: f64) -> Option<((f64, f64), (f64, f64))> {
        let (start, end) = (
            *self.starts.get(index)? as usize,
            *self.starts.get(index + 1)? as usize,
        );
        let length = *self.along.get(start..end)?.last()?;
        let inside = 1.0f64.min(0.5 * length);
        let middle = mathf::clamp(along, inside, length - inside);
        let (before, after) = (self.at(index, middle - 1.0)?, self.at(index, middle + 1.0)?);
        let (dx, dz) = (after.x - before.x, after.z - before.z);
        let span = mathf::hypot(dx, dz).max(1e-6);
        Some(((dx / span, dz / span), (-dz / span, dx / span)))
    }

    /// The nearest point of any course to `(x, z)` within the reach the
    /// index was built for, if one lies that near.
    pub(crate) fn nearest(&self, x: f64, z: f64) -> Option<Nearest> {
        let mut best: Option<Segment> = None;
        for segment in self.segments(x, z)? {
            if best.is_none_or(|held| segment.distance < held.distance) {
                best = Some(segment);
            }
        }
        self.reading(best?, (x, z))
    }

    /// The nearest point to `(x, z)` of each course within the reach the
    /// index was built for, a course at a time.
    pub(crate) fn each_nearest(&self, x: f64, z: f64, mut each: impl FnMut(Nearest)) {
        let Some(segments) = self.segments(x, z) else {
            return;
        };
        // A square lists its segments in the order of their marks, so each
        // course's run together; `end` is where the course held ends.
        let mut held: Option<(usize, Segment)> = None;
        for segment in segments {
            match held {
                Some((end, best)) if segment.mark < end => {
                    if segment.distance < best.distance {
                        held = Some((end, segment));
                    }
                }
                _ => {
                    if let Some(near) = held.and_then(|(_, best)| self.reading(best, (x, z))) {
                        each(near);
                    }
                    let course = self
                        .starts
                        .partition_point(|&start| start as usize <= segment.mark);
                    let end = self
                        .starts
                        .get(course)
                        .map_or(self.marks.len(), |&next| next as usize);
                    held = Some((end, segment));
                }
            }
        }
        if let Some(near) = held.and_then(|(_, best)| self.reading(best, (x, z))) {
            each(near);
        }
    }

    /// Each segment the index lists about `(x, z)`, in the order of its
    /// marks, and where the place lies against it; `None` off the index.
    fn segments(&self, x: f64, z: f64) -> Option<impl Iterator<Item = Segment> + '_> {
        let index = &self.index;
        if index.offsets.is_empty() {
            return None;
        }
        let column = mathf::floor((x - index.origin.0) / index.size);
        let row = mathf::floor((z - index.origin.1) / index.size);
        if column < 0.0 || row < 0.0 || column >= real(index.columns) || row >= real(index.rows) {
            return None;
        }
        let square = usize::try_from(mathf::round_i32(row)).ok()? * index.columns
            + usize::try_from(mathf::round_i32(column)).ok()?;
        let (start, end) = (*index.offsets.get(square)?, *index.offsets.get(square + 1)?);
        let entries = index.entries.get(start as usize..end as usize)?;
        Some(entries.iter().filter_map(move |&entry| {
            let mark = entry as usize;
            let (here, next) = (self.marks.get(mark)?, self.marks.get(mark + 1)?);
            let (dx, dz) = (next.x - here.x, next.z - here.z);
            let length2 = dx * dx + dz * dz;
            let unclamped = if length2 > 0.0 {
                ((x - here.x) * dx + (z - here.z) * dz) / length2
            } else {
                0.0
            };
            let share = unclamped.clamp(0.0, 1.0);
            let distance = mathf::hypot(x - (here.x + dx * share), z - (here.z + dz * share));
            Some(Segment {
                mark,
                unclamped,
                distance,
            })
        }))
    }

    /// The nearest point of `segment`'s course to `(x, z)`, read on it.
    fn reading(&self, segment: Segment, (x, z): (f64, f64)) -> Option<Nearest> {
        let Segment {
            mark,
            unclamped,
            distance,
        } = segment;
        let (here, next) = (self.marks.get(mark)?, self.marks.get(mark + 1)?);
        let (dx, dz) = (next.x - here.x, next.z - here.z);
        let share = unclamped.clamp(0.0, 1.0);
        let side = dx * (z - here.z) - dz * (x - here.x);
        let course = self
            .starts
            .partition_point(|&start| start as usize <= mark)
            .checked_sub(1)?;
        let length = mathf::hypot(dx, dz);
        let along = self.along.get(mark).copied().unwrap_or(0.0) + share * length;
        let first = self
            .starts
            .get(course)
            .is_some_and(|&start| start as usize == mark);
        let last = self
            .starts
            .get(course + 1)
            .map_or(self.marks.len(), |&next| next as usize)
            == mark + 2;
        let past = if unclamped > 1.0 && last {
            (unclamped - 1.0) * length
        } else if unclamped < 0.0 && first {
            -unclamped * length
        } else {
            0.0
        };
        let blend = |from: f64, to: f64| from + (to - from) * share;
        Some(Nearest {
            course,
            along,
            distance,
            side: if side >= 0.0 { 1.0 } else { -1.0 },
            level: blend(here.level, next.level),
            width: blend(here.width, next.width),
            depth: blend(here.depth, next.depth),
            run: blend(here.run, next.run),
            phase: blend(here.phase, next.phase),
            turn: blend(here.turn, next.turn),
            fall: blend(here.fall, next.fall),
            toward: if length > 0.0 {
                (dx / length, dz / length)
            } else {
                (0.0, 1.0)
            },
            past,
        })
    }
}

/// The marks of `line` from `from` to `to` along it, `width` broad; `None`
/// when the heap will not hold them or the line has no length.
pub(crate) fn traced(line: &[Point], (from, to): (f64, f64), width: f64) -> Option<Vec<Mark>> {
    let mark = |at: Point| Mark {
        x: at.x,
        z: at.y,
        width,
        ..Mark::default()
    };
    let mut marks = Vec::new();
    marks.try_reserve(line.len() + 2).ok()?;
    marks.push(mark(plane::at(line, from)?.0));
    let mut walked = 0.0;
    for pair in line.windows(2) {
        walked += (pair[1] - pair[0]).length();
        if walked > from && walked < to {
            marks.push(mark(pair[1]));
        }
    }
    marks.push(mark(plane::at(line, to)?.0));
    Some(marks)
}

/// `marks` smoothed by Chaikin's corner cutting, `rounds` times over, its two
/// ends kept where they are; `None` when the heap will not hold it.
pub(crate) fn smoothed(marks: &[Mark], rounds: u32) -> Option<Vec<Mark>> {
    let mut current = fallible::collected(marks.len(), marks.iter().copied())?;
    for _ in 0..rounds {
        if current.len() < 3 {
            break;
        }
        let mut next = Vec::new();
        if !fallible::reserve(&mut next, 2 * current.len()) {
            return None;
        }
        next.push(current[0]);
        for pair in current.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            next.push(between(a, b, 0.25));
            next.push(between(a, b, 0.75));
        }
        if let Some(&last) = current.last() {
            next.push(last);
        }
        current = next;
    }
    Some(current)
}

/// The squares an index buckets segments into: where they start, how broad
/// each is, and how many across and down; and how far a segment reaches.
#[derive(Copy, Clone, Debug)]
struct Squares {
    origin: (f64, f64),
    size: f64,
    columns: usize,
    rows: usize,
    reach: Reach,
}

impl Squares {
    /// Squares a typical one of `marks`' segments reaches across, over the
    /// square from `origin` `span` across: a segment lies in the few within
    /// its reach, and a point's own square holds everything within reach of
    /// the point and little more, so a question tests few segments.
    fn over(marks: &[Mark], (origin, span): ((f64, f64), f64), reach: Reach) -> Option<Self> {
        let mean = marks.iter().map(|mark| mark.width).sum::<f64>() / real(marks.len().max(1));
        let size = reach.of(mean).max(span / 1024.0).max(1e-3);
        let across = usize::try_from(mathf::round_i32(mathf::ceil(span / size)))
            .unwrap_or(1)
            .max(1);
        across.checked_mul(across)?;
        Some(Self {
            origin,
            size,
            columns: across,
            rows: across,
            reach,
        })
    }

    fn count(&self) -> usize {
        self.columns * self.rows
    }

    /// The square `value` falls in along an axis of `count` squares from
    /// `from`.
    fn cell(&self, value: f64, from: f64, count: usize) -> usize {
        let at = mathf::floor((value - from) / self.size);
        usize::try_from(mathf::round_i32(at.max(0.0)))
            .unwrap_or(0)
            .min(count - 1)
    }

    /// Hand `visit` every square the segment from `a` to `b` can reach.
    fn visit(&self, (a, b): (&Mark, &Mark), visit: &mut dyn FnMut(usize)) {
        let widest = a.width.max(b.width);
        let pad = self.reach.of(widest) + 0.5 * widest;
        let (x0, x1) = (a.x.min(b.x) - pad, a.x.max(b.x) + pad);
        let (z0, z1) = (a.z.min(b.z) - pad, a.z.max(b.z) + pad);
        for row in self.cell(z0, self.origin.1, self.rows)..=self.cell(z1, self.origin.1, self.rows)
        {
            for column in self.cell(x0, self.origin.0, self.columns)
                ..=self.cell(x1, self.origin.0, self.columns)
            {
                visit(row * self.columns + column);
            }
        }
    }
}

/// How many marks, segments or squares a unit of an indexing takes.
pub(crate) const INDEX_UNIT: usize = 1 << 16;

/// Courses being indexed, a bounded share of the work a step: their marks
/// gathered, then the squares each segment reaches counted, the counts summed
/// into each square's first entry, and every segment filed in its squares.
#[derive(Debug)]
pub(crate) struct Indexing {
    sources: Vec<Vec<Mark>>,
    courses: Courses,
    square: ((f64, f64), f64),
    reach: Reach,
    stage: Indexed,
}

/// Where an indexing stands: gathering from the next source; counting,
/// summing or filling from a mark or square; or done.
#[derive(Debug)]
enum Indexed {
    Gathering(usize),
    Counting {
        squares: Squares,
        mark: usize,
        counts: Vec<u32>,
    },
    Summing {
        squares: Squares,
        square: usize,
        counts: Vec<u32>,
    },
    Filling {
        squares: Squares,
        mark: usize,
        slots: Vec<u32>,
    },
    Done,
}

impl Indexing {
    /// `courses` to be indexed over the square from `origin` `span` across,
    /// each to be asked after within `reach` of it, gathered a unit at a time;
    /// `None` when the heap will not hold them.
    pub(crate) fn new(
        courses: Vec<Vec<Mark>>,
        square: ((f64, f64), f64),
        reach: Reach,
    ) -> Option<Self> {
        let total: usize = courses.iter().map(Vec::len).sum();
        let mut gathered = Courses::none();
        if !(fallible::reserve(&mut gathered.marks, total)
            && fallible::reserve(&mut gathered.along, total)
            && fallible::reserve(&mut gathered.starts, courses.len() + 1))
        {
            return None;
        }
        Some(Self {
            sources: courses,
            courses: gathered,
            square,
            reach,
            stage: Indexed::Gathering(0),
        })
    }

    /// `courses` gathered at once, to be indexed as [`Self::new`]'s are.
    fn copied(courses: &[Vec<Mark>], square: ((f64, f64), f64), reach: Reach) -> Option<Self> {
        let mut indexing = Self::new(Vec::new(), square, reach)?;
        let total: usize = courses.iter().map(Vec::len).sum();
        let gathered = &mut indexing.courses;
        if !(fallible::reserve(&mut gathered.marks, total)
            && fallible::reserve(&mut gathered.along, total)
            && fallible::reserve(&mut gathered.starts, courses.len() + 1))
        {
            return None;
        }
        for course in courses {
            gathered.gather(course)?;
        }
        Some(indexing)
    }

    /// The next unit of the indexing, at most `budget` marks, segments or
    /// squares; whether it is whole, or `None` when the heap will not hold
    /// it.
    pub(crate) fn step(&mut self, budget: usize) -> Option<bool> {
        let budget = budget.max(1);
        let courses = &mut self.courses;
        self.stage = match core::mem::replace(&mut self.stage, Indexed::Done) {
            Indexed::Gathering(next) => {
                let mut taken = 0;
                let mut at = next;
                while let Some(course) = self.sources.get(at).filter(|_| taken < budget) {
                    courses.gather(course)?;
                    taken += course.len().max(1);
                    at += 1;
                }
                if at < self.sources.len() {
                    Indexed::Gathering(at)
                } else {
                    self.sources = Vec::new();
                    courses
                        .starts
                        .push(u32::try_from(courses.marks.len()).ok()?);
                    let squares = Squares::over(&courses.marks, self.square, self.reach)?;
                    let counts = fallible::filled(squares.count() + 1, 0u32)?;
                    Indexed::Counting {
                        squares,
                        mark: 0,
                        counts,
                    }
                }
            }
            Indexed::Counting {
                squares,
                mark,
                mut counts,
            } => {
                let end = courses.walk_segments((mark, budget), &mut |a, b, _| {
                    squares.visit((a, b), &mut |square| {
                        if let Some(count) = counts.get_mut(square + 1) {
                            *count = count.saturating_add(1);
                        }
                    });
                });
                if end < courses.marks.len() {
                    Indexed::Counting {
                        squares,
                        mark: end,
                        counts,
                    }
                } else {
                    Indexed::Summing {
                        squares,
                        square: 0,
                        counts,
                    }
                }
            }
            Indexed::Summing {
                squares,
                square,
                mut counts,
            } => {
                let end = square.saturating_add(budget).min(squares.count());
                for at in square..end {
                    counts[at + 1] = counts[at + 1].saturating_add(counts[at]);
                }
                if end < squares.count() {
                    Indexed::Summing {
                        squares,
                        square: end,
                        counts,
                    }
                } else {
                    let slots = courses.lay_index(&squares, counts)?;
                    Indexed::Filling {
                        squares,
                        mark: 0,
                        slots,
                    }
                }
            }
            Indexed::Filling {
                squares,
                mark,
                mut slots,
            } => {
                let end = courses.fill(&squares, (mark, budget), &mut slots);
                if end < courses.marks.len() {
                    Indexed::Filling {
                        squares,
                        mark: end,
                        slots,
                    }
                } else {
                    Indexed::Done
                }
            }
            Indexed::Done => Indexed::Done,
        };
        Some(matches!(self.stage, Indexed::Done))
    }

    /// The courses, once indexed; `None` before.
    pub(crate) fn finish(self) -> Option<Courses> {
        matches!(self.stage, Indexed::Done).then_some(self.courses)
    }
}

impl Courses {
    /// Gather `course`'s marks and how far along it each lies, where it has
    /// a segment at all; `None` when it would number past what a start holds.
    fn gather(&mut self, course: &[Mark]) -> Option<()> {
        if course.len() >= 2 {
            self.starts.push(u32::try_from(self.marks.len()).ok()?);
            self.marks.extend_from_slice(course);
            self.along.extend(travelled(course));
        }
        Some(())
    }

    /// Hand `visit` each segment from the one starting at mark `from`, as
    /// many as `budget`, with its first mark; where the walk ends.
    fn walk_segments(
        &self,
        (from, budget): (usize, usize),
        visit: &mut dyn FnMut(&Mark, &Mark, usize),
    ) -> usize {
        Self::walk_marks(&self.marks, &self.starts, (from, budget), visit)
    }

    /// Lay its index over `squares`, `counts` holding where each square's
    /// entries begin and one past the last: the slots each square's entries
    /// are filled in from, its first's to begin with.
    fn lay_index(&mut self, squares: &Squares, counts: Vec<u32>) -> Option<Vec<u32>> {
        let total = counts.last().copied().unwrap_or(0) as usize;
        self.index = Index {
            origin: squares.origin,
            size: squares.size,
            columns: squares.columns,
            rows: squares.rows,
            entries: fallible::filled(total, 0u32)?,
            offsets: Vec::new(),
        };
        let slots = fallible::collected(counts.len(), counts.iter().copied())?;
        self.index.offsets = counts;
        Some(slots)
    }

    /// Fill in the index's entries for the segments from mark `from`, as many
    /// as `budget`, each square's next entry the one its slot in `slots`
    /// holds; where the walk ends.
    fn fill(
        &mut self,
        squares: &Squares,
        (from, budget): (usize, usize),
        slots: &mut [u32],
    ) -> usize {
        let entries = &mut self.index.entries;
        Self::walk_marks(
            &self.marks,
            &self.starts,
            (from, budget),
            &mut |a, b, first| {
                let id = u32::try_from(first).unwrap_or(u32::MAX);
                squares.visit((a, b), &mut |square| {
                    if let Some(slot) = slots.get_mut(square) {
                        if let Some(entry) = entries.get_mut(*slot as usize) {
                            *entry = id;
                        }
                        *slot = slot.saturating_add(1);
                    }
                });
            },
        )
    }

    /// [`Self::walk_segments`] over `marks` gathered at `starts`.
    fn walk_marks(
        marks: &[Mark],
        starts: &[u32],
        (from, budget): (usize, usize),
        visit: &mut dyn FnMut(&Mark, &Mark, usize),
    ) -> usize {
        let end = from.saturating_add(budget).min(marks.len());
        // The course `from` lies in, and where the next one starts.
        let mut course = starts.partition_point(|&start| start as usize <= from);
        for first in from..end {
            while starts
                .get(course)
                .is_some_and(|&start| start as usize <= first)
            {
                course += 1;
            }
            let next_start = starts
                .get(course)
                .map_or(marks.len(), |&start| start as usize);
            if first + 1 < next_start {
                if let (Some(a), Some(b)) = (marks.get(first), marks.get(first + 1)) {
                    visit(a, b, first);
                }
            }
        }
        end
    }
}

/// How far along `course` each of its marks lies from its first.
pub(crate) fn travelled(course: &[Mark]) -> impl Iterator<Item = f64> + '_ {
    let mut total = 0.0;
    let mut last: Option<&Mark> = None;
    course.iter().map(move |mark| {
        if let Some(last) = last {
            total += mathf::hypot(mark.x - last.x, mark.z - last.z);
        }
        last = Some(mark);
        total
    })
}

/// `marks`, lying `along` their course, as they run `at` that far along it:
/// blended between the marks either side and held at its ends; `None` for
/// no marks.
pub(crate) fn blended(marks: &[Mark], along: &[f64], at: f64) -> Option<Mark> {
    let last = along.len().min(marks.len()).checked_sub(1)?;
    let next = along
        .partition_point(|&mark| mark <= at)
        .clamp(1, last.max(1));
    let (a, b) = (*marks.get(next - 1)?, *marks.get(next.min(last))?);
    let (from, to) = (*along.get(next - 1)?, *along.get(next.min(last))?);
    let t = if to > from {
        ((at - from) / (to - from)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some(between(a, b, t))
}

/// The mark `t` of the way from `a` to `b`.
pub(crate) fn between(a: Mark, b: Mark, t: f64) -> Mark {
    let blend = |from: f64, to: f64| from + (to - from) * t;
    Mark {
        x: blend(a.x, b.x),
        z: blend(a.z, b.z),
        level: blend(a.level, b.level),
        width: blend(a.width, b.width),
        depth: blend(a.depth, b.depth),
        run: blend(a.run, b.run),
        phase: blend(a.phase, b.phase),
        turn: blend(a.turn, b.turn),
        fall: blend(a.fall, b.fall),
    }
}

#[cfg(test)]
#[path = "course_tests.rs"]
mod tests;
