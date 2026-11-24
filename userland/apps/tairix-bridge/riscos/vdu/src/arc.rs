//! Arcs, segments and sectors (`OS_Plot` &A0 to &B7): a circle's outline cut
//! where rays from its centre leave it, each rule pinned to what genuine
//! RISC OS 5 was recorded drawing.
//!
//! An arc runs anticlockwise from its start to where the ray toward its end
//! point leaves the circle, that pixel moved out along its column to the
//! outline when it lies inside. A sector adds both rays inside the circle
//! and, where the sweep closes on a ray at a corner of the circle, one pixel
//! beyond with the circle's edge beside it; one of more than half the circle
//! is the circle less the inside of the sector completing it. A segment's
//! chord runs to where the end point's direction meets the circle, cut
//! toward nought, and on along its own steps to the row nearest the true
//! meeting when the arc ends in the first or third quadrant. At some chord
//! ends RISC OS 5 differs from this by a pixel or a few, a known difference
//! (`plans/RISCOS-EMULATOR.md` RO21.2).
//!
//! Each shape is worked out as runs on the rows of what can be seen, cut to
//! its columns: a ray's exit is found by halving, since a pixel leaving a
//! circle along a ray from its centre never comes back, and a run on the
//! outline is tested for lying on the arc only where that can change.

use alloc::vec::Vec;

use crate::raster::{self, ray_pixel, room, Full, Point, Round, Rows, Span, View, Walk};

/// A circle to cut: its rows and the squared radius and pixel shape they
/// came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Circle {
    pub(crate) round: Round,
    radius2: u64,
    tall: u32,
    wide: u32,
}

/// The side of a sector a ray bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Start,
    End,
}

impl Side {
    /// Whether the sweep closes on this side's ray at `p`: the end's in the
    /// first and third quadrants, the start's in the second and fourth, as
    /// RISC OS generates each quadrant outward from the x axis.
    fn closes(self, (x, y): Point) -> bool {
        let product = i64::from(x) * i64::from(y);
        match self {
            Self::Start => product < 0,
            Self::End => product > 0,
        }
    }
}

/// A ray walked out of a circle.
struct Walked {
    toward: Point,
    /// The place of its last pixel inside the circle, and that pixel.
    last_place: u64,
    last: Point,
    /// The pixel beyond where it closes a sector's sweep at a corner, and
    /// the circle's edge beside that pixel.
    beyond: Option<Point>,
    edge: Option<Point>,
}

impl Walked {
    /// Its pixels inside the circle and the one beyond, as runs on the rows
    /// of `view`.
    fn runs(&self, view: View, out: &mut Vec<Span>) -> Result<(), Full> {
        let along = u64::from(
            self.toward
                .0
                .unsigned_abs()
                .max(self.toward.1.unsigned_abs()),
        );
        if along == 0 {
            return push_run(view, 0, (0, 0), out);
        }
        let laps = i64::try_from(self.last_place / along + 1).unwrap_or(i64::MAX);
        let ray = Walk::between(
            (0, 0),
            (
                laps.saturating_mul(i64::from(self.toward.0)),
                laps.saturating_mul(i64::from(self.toward.1)),
            ),
        );
        for y in self.last.1.min(0).max(view.bottom)..=self.last.1.max(0).min(view.top) {
            if let Some(run) = ray.row_within(y, (0, self.last_place)) {
                push_run(view, y, run, out)?;
            }
        }
        if let Some((x, y)) = self.beyond {
            push_run(view, y, (x, x), out)?;
        }
        Ok(())
    }
}

/// Run `run` on row `y` added to `out` where `view` holds the row, cut to
/// its columns.
fn push_run(view: View, y: i32, run: (i32, i32), out: &mut Vec<Span>) -> Result<(), Full> {
    if view.holds(y) {
        let (left, right) = view.cut(run);
        room(out, 1)?;
        out.push((y, left, right));
    }
    Ok(())
}

/// Each row's leftmost and rightmost pixel among `runs`.
fn rows_of<'a>(runs: impl IntoIterator<Item = &'a Span>) -> Result<Rows, Full> {
    let mut rows = Rows::default();
    for &(y, left, right) in runs {
        rows.add((left, y))?;
        rows.add((right, y))?;
    }
    Ok(rows)
}

impl Circle {
    /// The circle about the origin through `edge`, in pixels `tall` times as
    /// tall as wide, or `wide` times as wide as tall.
    pub(crate) fn through(edge: Point, tall: u32, wide: u32) -> Self {
        let radius2 = raster::soft_radius2(edge, tall, wide);
        Self {
            round: Round::circle(radius2, tall, wide),
            radius2,
            tall,
            wide,
        }
    }

    /// A place a ray from the centre is sure to have left the circle by.
    fn reach(&self) -> u64 {
        let across = self.round.span(0).map_or(0, |(_, right)| right);
        u64::from(across.max(self.round.top()).unsigned_abs()) + 2
    }

    /// The ray toward `toward` out of the circle; for a sector's `side`, with
    /// the pixel beyond where it leaves on a new row at a corner of the
    /// circle and the sweep closes there.
    fn walk(&self, toward: Point, side: Option<Side>) -> Walked {
        let mut walked = Walked {
            toward,
            last_place: 0,
            last: (0, 0),
            beyond: None,
            edge: None,
        };
        if toward == (0, 0) {
            return walked;
        }
        let inside = |n: u64| self.round.contains(ray_pixel(toward, n));
        let (mut low, mut high) = (0, self.reach());
        while high - low > 1 {
            let middle = low + (high - low) / 2;
            if inside(middle) {
                low = middle;
            } else {
                high = middle;
            }
        }
        walked.last_place = low;
        walked.last = ray_pixel(toward, low);
        let p = ray_pixel(toward, low + 1);
        if side.is_some_and(|side| side.closes(p)) && p.1 != walked.last.1 && self.touches(p) {
            walked.beyond = Some(p);
            walked.edge = self
                .round
                .span(p.1)
                .map(|(left, right)| (if p.0 > 0 { right } else { left }, p.1));
        }
        walked
    }

    /// Whether `p`, just beyond the circle, touches it both across its row
    /// and up its column.
    fn touches(&self, (x, y): Point) -> bool {
        let across = self
            .round
            .span(y)
            .is_some_and(|(_, right)| x.saturating_abs() <= right.saturating_add(1));
        let up = self
            .round
            .column_top(x)
            .is_some_and(|top| y.saturating_abs() <= top.saturating_add(1));
        across && up
    }

    /// The arc's end, from the last pixel of its ray inside the circle: that
    /// pixel when on the outline, else the outline met moving away from the
    /// x axis along its column.
    fn end(&self, last: Point) -> Point {
        let step = match last {
            (_, y) if y > 0 => (0, 1),
            (_, y) if y < 0 => (0, -1),
            (x, _) if x < 0 => (-1, 0),
            _ => (1, 0),
        };
        let mut p = last;
        while self.round.contains(p) && !self.round.on_outline(p) {
            p = (p.0.saturating_add(step.0), p.1.saturating_add(step.1));
        }
        p
    }

    /// Where the direction of `toward` meets the circle, each coordinate cut
    /// toward nought, and the row nearest the true meeting.
    fn meeting(&self, toward: Point) -> (Point, i32) {
        let square = |v: i32, by: u32| u128::from(v.unsigned_abs()).pow(2) * u128::from(by).pow(2);
        let m2 = square(toward.0, self.wide) + square(toward.1, self.tall);
        if m2 == 0 {
            return ((0, 0), 0);
        }
        let r2 = u128::from(self.radius2);
        let signed = |v: i32, magnitude: u64| {
            let magnitude = raster::saturate(i64::try_from(magnitude).unwrap_or(i64::MAX));
            if v < 0 {
                -magnitude
            } else {
                magnitude
            }
        };
        let cut = |v: i32| {
            u64::try_from(u128::from(v.unsigned_abs()).pow(2) * r2 / m2)
                .unwrap_or(u64::MAX)
                .isqrt()
        };
        let (x, y) = (cut(toward.0), cut(toward.1));
        let rounds_up = u128::from(2 * y + 1).pow(2) * m2
            <= 4 * u128::from(toward.1.unsigned_abs()).pow(2) * r2;
        let nearest = if rounds_up { y + 1 } else { y };
        (
            (signed(toward.0, x), signed(toward.1, y)),
            signed(toward.1, nearest),
        )
    }

    /// The outline's runs on the rows of `view` from `start` anticlockwise to
    /// `end`, cut to its columns.
    fn arc_runs(
        &self,
        start: Point,
        end: Point,
        view: View,
        out: &mut Vec<Span>,
    ) -> Result<(), Full> {
        for (k, from, to) in self.round.outline(view) {
            on_arc_parts(k, (from, to), start, end, |part| {
                push_run(view, k, part, out)
            })?;
        }
        Ok(())
    }
}

/// The parts of row `k`'s run from `from` to `to` on the arc from `start` to
/// `end`, each handed to `each`. Whether a pixel of the row is on the arc
/// changes only where its octant does or where it passes the start's or the
/// end's column, so each stretch between those is tested once.
fn on_arc_parts(
    k: i32,
    (from, to): (i32, i32),
    start: Point,
    end: Point,
    mut each: impl FnMut((i32, i32)) -> Result<(), Full>,
) -> Result<(), Full> {
    let mut marks = [
        k,
        k.saturating_add(1),
        k.saturating_neg(),
        k.saturating_neg().saturating_add(1),
        0,
        1,
        start.0,
        start.0.saturating_add(1),
        end.0,
        end.0.saturating_add(1),
    ];
    marks.sort_unstable();
    let stretches =
        core::iter::once(from).chain(marks.into_iter().filter(|&mark| mark > from && mark <= to));
    let mut current: Option<(i32, i32)> = None;
    let mut ends = stretches
        .clone()
        .skip(1)
        .map(|next| next - 1)
        .chain(core::iter::once(to));
    for first in stretches {
        let Some(last) = ends.next() else {
            break;
        };
        if first > last {
            continue;
        }
        if on_arc((first, k), start, end) {
            current = Some(current.map_or((first, last), |(left, _)| (left, last)));
        } else if let Some(part) = current.take() {
            each(part)?;
        }
    }
    current.map_or(Ok(()), each)
}

/// An arc about the origin from `start` toward `toward`: its runs on the
/// rows of `view`, cut to its columns.
pub(crate) fn arc(
    circle: &Circle,
    start: Point,
    toward: Point,
    view: View,
    out: &mut Vec<Span>,
) -> Result<(), Full> {
    let end = circle.end(circle.walk(toward, None).last);
    circle.arc_runs(start, end, view, out)
}

/// A segment about the origin from `start` toward `toward`: its spans on the
/// rows of `view`, each row between the arc and the chord and no further
/// than the circle.
pub(crate) fn segment(
    circle: &Circle,
    start: Point,
    toward: Point,
    view: View,
) -> Result<Vec<Span>, Full> {
    let end = circle.end(circle.walk(toward, None).last);
    let mut runs = Vec::new();
    circle.arc_runs(start, end, view, &mut runs)?;
    let (cut, nearest) = circle.meeting(toward);
    let chord = (
        i64::from(cut.0) - i64::from(start.0),
        i64::from(cut.1) - i64::from(start.1),
    );
    let past = |y: i32, mark: i32| if chord.1 > 0 { y > mark } else { y < mark };
    let carries_on = Side::End.closes(end) && chord.1 != 0 && past(nearest, cut.1);
    let chord_rows =
        |from: i32, to: i32| from.min(to).max(view.bottom)..=from.max(to).min(view.top);
    let first = Walk::new(start, cut);
    for y in chord_rows(start.1, cut.1) {
        if let Some(run) = first.row(y) {
            push_run(view, y, run, &mut runs)?;
        }
    }
    if carries_on {
        let cut64 = (i64::from(cut.0), i64::from(cut.1));
        let on = Walk::between(cut64, (cut64.0 + chord.0, cut64.1 + chord.1));
        for y in chord_rows(cut.1, nearest).filter(|&y| past(y, cut.1)) {
            if let Some(run) = on.row(y) {
                push_run(view, y, run, &mut runs)?;
            }
        }
    }
    let mut spans = Vec::new();
    for (y, left, right) in rows_of(&runs)?.spans() {
        let Some((inside_left, inside_right)) = circle.round.span(y) else {
            continue;
        };
        let (left, right) = (left.max(inside_left), right.min(inside_right));
        if left <= right {
            room(&mut spans, 1)?;
            spans.push((y, left, right));
        }
    }
    Ok(spans)
}

/// A sector about the origin from `start` toward `toward`: its spans on the
/// rows of `view`.
pub(crate) fn sector(
    circle: &Circle,
    start: Point,
    toward: Point,
    view: View,
) -> Result<Vec<Span>, Full> {
    let from = circle.walk(start, Some(Side::Start));
    let to = circle.walk(toward, Some(Side::End));
    let end = circle.end(to.last);
    let mut rays = Vec::new();
    from.runs(view, &mut rays)?;
    to.runs(view, &mut rays)?;
    let mut edges = Vec::new();
    for (x, y) in [from.edge, to.edge].into_iter().flatten() {
        push_run(view, y, (x, x), &mut edges)?;
    }
    let mut arc = Vec::new();
    circle.arc_runs(start, end, view, &mut arc)?;
    let mut spans = Vec::new();
    if !reflex(start, to.last) {
        let boundary = rows_of(arc.iter().chain(&rays).chain(&edges))?;
        room(&mut spans, boundary.spans().count())?;
        spans.extend(boundary.spans());
        return Ok(spans);
    }
    let mut rest = Vec::new();
    circle.arc_runs(end, start, view, &mut rest)?;
    let completing = rows_of(rest.iter().chain(&rays))?;
    let mut singles = Vec::new();
    room(&mut singles, rays.len() + edges.len() + arc.len())?;
    singles.extend(rays.iter().chain(&edges).chain(&arc).copied());
    singles.sort_unstable();
    let mut pieces = Vec::new();
    room(&mut pieces, singles.len() + 2)?;
    let mut next = 0;
    for (y, left, right) in circle.round.fill(view) {
        pieces.clear();
        match completing.get(y) {
            Some((first, last)) => {
                pieces.push((left, first.saturating_sub(1).min(right)));
                pieces.push((last.saturating_add(1).max(left), right));
            }
            None => pieces.push((left, right)),
        }
        while singles.get(next).is_some_and(|&(row, ..)| row < y) {
            next += 1;
        }
        while let Some(&(_, first, last)) = singles.get(next).filter(|&&(row, ..)| row == y) {
            pieces.push((first, last));
            next += 1;
        }
        merge(y, &mut pieces, &mut spans)?;
    }
    Ok(spans)
}

/// Row `y`'s `pieces`, each an inclusive run, merged where they meet or
/// overlap and added to `out` left to right.
fn merge(y: i32, pieces: &mut [(i32, i32)], out: &mut Vec<Span>) -> Result<(), Full> {
    pieces.sort_unstable();
    let mut current: Option<(i32, i32)> = None;
    for &(from, to) in pieces.iter() {
        if from > to {
            continue;
        }
        current = match current {
            Some((left, right)) if from <= right.saturating_add(1) => Some((left, right.max(to))),
            Some((left, right)) => {
                room(out, 1)?;
                out.push((y, left, right));
                Some((from, to))
            }
            None => Some((from, to)),
        };
    }
    if let Some((left, right)) = current {
        room(out, 1)?;
        out.push((y, left, right));
    }
    Ok(())
}

/// The octant `p` lies in, anticlockwise from just above the positive x
/// axis.
const fn octant(p: Point) -> u8 {
    let (x, y) = p;
    if x > 0 && y >= 0 && y <= x {
        0
    } else if y > 0 && x > 0 {
        1
    } else if y > 0 && x <= 0 && -x <= y {
        2
    } else if x < 0 && y > 0 {
        3
    } else if x < 0 && y <= 0 && -y <= -x {
        4
    } else if y < 0 && x < 0 {
        5
    } else if y < 0 && x >= 0 && x <= -y {
        6
    } else {
        7
    }
}

/// The coordinate that grows as a point moves anticlockwise round a circle
/// within octant `o`.
const fn onward(o: u8, p: Point) -> i32 {
    match o {
        0 | 7 => p.1,
        1 | 2 => -p.0,
        3 | 4 => -p.1,
        _ => p.0,
    }
}

/// Whether outline pixel `p` lies on the arc anticlockwise from `start` to
/// the end `end`: every octant between theirs, the start's from the start
/// on, the end's up to where it has not passed the end, the octant both are
/// in split where the start lies.
fn on_arc(p: Point, start: Point, end: Point) -> bool {
    let (os, oe, op) = (octant(start), octant(end), octant(p));
    let after_start = |q: Point| onward(os, q) >= onward(os, start);
    let up_to_end = |q: Point| onward(oe, q) <= onward(oe, end);
    if os == oe {
        if op != os {
            return onward(oe, end) < onward(os, start);
        }
        return if onward(oe, end) >= onward(os, start) {
            after_start(p) && up_to_end(p)
        } else {
            after_start(p) || up_to_end(p)
        };
    }
    if op == os {
        return after_start(p);
    }
    if op == oe {
        return up_to_end(p);
    }
    (op + 8 - os) % 8 < (oe + 8 - os) % 8
}

/// Whether the arc from `start` to `end` is more than half the circle.
fn reflex(start: Point, end: Point) -> bool {
    let cross = i128::from(start.0) * i128::from(end.1) - i128::from(start.1) * i128::from(end.0);
    let dot = i128::from(start.0) * i128::from(end.0) + i128::from(start.1) * i128::from(end.1);
    cross < 0 || (cross == 0 && dot < 0)
}

#[cfg(test)]
mod tests;
