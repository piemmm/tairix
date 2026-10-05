//! Courses across the land — a river's, a road's, a path's — as polylines
//! carrying at each vertex the level they run at and how wide and deep they
//! are, and an index that finds the nearest one to any point in about as
//! many steps as there are courses near it.
//!
//! A land's grids are filled a vertex at a time, each asking what course
//! runs nearest and how far off, so the index buckets every segment into the
//! squares of a grid it can reach: a question reads one square.

use alloc::vec::Vec;

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
    /// a broad course reaching further than a narrow one; `None` when the
    /// heap will not hold them.
    pub(crate) fn new(
        courses: &[Vec<Mark>],
        (origin, span): ((f64, f64), f64),
        reach: Reach,
    ) -> Option<Self> {
        let total: usize = courses.iter().map(Vec::len).sum();
        let mut marks = Vec::new();
        let mut along = Vec::new();
        let mut starts = Vec::new();
        if !(fallible::reserve(&mut marks, total)
            && fallible::reserve(&mut along, total)
            && fallible::reserve(&mut starts, courses.len() + 1))
        {
            return None;
        }
        for course in courses.iter().filter(|course| course.len() >= 2) {
            starts.push(u32::try_from(marks.len()).ok()?);
            marks.extend_from_slice(course);
            along.extend(travelled(course));
        }
        starts.push(u32::try_from(marks.len()).ok()?);
        let mut courses = Self {
            marks,
            starts,
            along,
            index: Index::default(),
        };
        courses.index = courses.bucket(origin, span, reach)?;
        Some(courses)
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

    /// The first mark of every segment: each mark but its course's last.
    fn segments(&self) -> impl Iterator<Item = usize> + '_ {
        self.starts.windows(2).flat_map(|pair| {
            let (start, end) = (pair[0] as usize, pair[1] as usize);
            start..end.saturating_sub(1)
        })
    }

    fn bucket(&self, origin: (f64, f64), span: f64, reach: Reach) -> Option<Index> {
        // Squares a typical segment's reach across: a segment lies in the few
        // within its reach, and a point's own square holds everything within
        // reach of the point and little more, so a question tests few
        // segments.
        let count = self.marks.len().max(1);
        let mean = self.marks.iter().map(|mark| mark.width).sum::<f64>() / real(count);
        let size = reach.of(mean).max(span / 1024.0).max(1e-3);
        let across = |length: f64| {
            usize::try_from(mathf::round_i32(mathf::ceil(length / size)))
                .unwrap_or(1)
                .max(1)
        };
        let (columns, rows) = (across(span), across(span));
        let squares = columns.checked_mul(rows)?;
        let cell = |value: f64, from: f64, count: usize| {
            let at = mathf::floor((value - from) / size);
            usize::try_from(mathf::round_i32(at.max(0.0)))
                .unwrap_or(0)
                .min(count - 1)
        };
        let mut counts = fallible::filled(squares + 1, 0u32)?;
        let visit = |mark: usize, visit_square: &mut dyn FnMut(usize)| {
            let (Some(a), Some(b)) = (self.marks.get(mark), self.marks.get(mark + 1)) else {
                return;
            };
            let widest = a.width.max(b.width);
            let pad = reach.of(widest) + 0.5 * widest;
            let (x0, x1) = (a.x.min(b.x) - pad, a.x.max(b.x) + pad);
            let (z0, z1) = (a.z.min(b.z) - pad, a.z.max(b.z) + pad);
            for row in cell(z0, origin.1, rows)..=cell(z1, origin.1, rows) {
                for column in cell(x0, origin.0, columns)..=cell(x1, origin.0, columns) {
                    visit_square(row * columns + column);
                }
            }
        };
        for mark in self.segments() {
            visit(mark, &mut |square| {
                if let Some(count) = counts.get_mut(square + 1) {
                    *count = count.saturating_add(1);
                }
            });
        }
        for square in 0..squares {
            counts[square + 1] = counts[square + 1].saturating_add(counts[square]);
        }
        let total = counts.last().copied().unwrap_or(0) as usize;
        let mut entries = fallible::filled(total, 0u32)?;
        let mut fill = fallible::collected(counts.len(), counts.iter().copied())?;
        for mark in self.segments() {
            let id = u32::try_from(mark).unwrap_or(u32::MAX);
            visit(mark, &mut |square| {
                if let Some(slot) = fill.get_mut(square) {
                    if let Some(entry) = entries.get_mut(*slot as usize) {
                        *entry = id;
                    }
                    *slot = slot.saturating_add(1);
                }
            });
        }
        Some(Index {
            origin,
            size,
            columns,
            rows,
            offsets: counts,
            entries,
        })
    }

    /// The nearest point of any course to `(x, z)` within the reach the
    /// index was built for, if one lies that near.
    pub(crate) fn nearest(&self, x: f64, z: f64) -> Option<Nearest> {
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
        let mut best: Option<Nearest> = None;
        for &entry in index.entries.get(start as usize..end as usize)? {
            let mark = entry as usize;
            let (Some(here), Some(next)) = (self.marks.get(mark), self.marks.get(mark + 1)) else {
                continue;
            };
            let (dx, dz) = (next.x - here.x, next.z - here.z);
            let length2 = dx * dx + dz * dz;
            let share = if length2 > 0.0 {
                (((x - here.x) * dx + (z - here.z) * dz) / length2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let (px, pz) = (here.x + dx * share, here.z + dz * share);
            let distance = mathf::hypot(x - px, z - pz);
            if best.is_some_and(|held| held.distance <= distance) {
                continue;
            }
            let side = dx * (z - here.z) - dz * (x - here.x);
            let Some(course) = self
                .starts
                .partition_point(|&start| start as usize <= mark)
                .checked_sub(1)
            else {
                continue;
            };
            let length = mathf::sqrt(length2);
            let along = self.along.get(mark).copied().unwrap_or(0.0) + share * length;
            let blend = |from: f64, to: f64| from + (to - from) * share;
            best = Some(Nearest {
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
            });
        }
        best
    }
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
