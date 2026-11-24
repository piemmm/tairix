//! Scan conversion as RISC OS 5's Draw module does it, each rule recorded
//! (`plans/RISCOS-EMULATOR.md` §8): coordinates in 256ths of a pixel, a
//! pixel inside where its centre's winding number says so, and on the
//! boundary where the path's thinnest line would draw it.
//!
//! - **Interior.** On each row's centre line, a segment rising or falling
//!   across it (its lower end below the centre, its upper at or above) adds
//!   its direction to every pixel whose centre lies strictly right of where
//!   it crosses.
//! - **Boundary.** A segment wider than it is tall marks, in each column
//!   whose centre lies past its left end and no further than its right, the
//!   row its height there is in, a height on a row's edge counting below it
//!   where the segment rises; any other segment marks, in each row whose
//!   centre lies past its lower end and no further than its upper, the
//!   column its exact crossing is in. Each end marks the pixel whose diamond
//!   of half a pixel holds it, its left corners and edges included.

use alloc::vec::Vec;

use tairix_drawing::riscos::units::DRAW_UNITS_PER_OS_UNIT;
use tairix_riscos_kernel::mouse::Bounds;
use tairix_util::fallible;

/// A pixel's side, in its units, which are draw units shifted down by the
/// eigen factors.
pub(crate) const UNIT: i64 = DRAW_UNITS_PER_OS_UNIT as i64;
const HALF: i64 = UNIT / 2;

/// A segment of a path, in 256ths of a pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Segment {
    pub(crate) from: (i64, i64),
    pub(crate) to: (i64, i64),
    /// It is drawn when the path is stroked thinnest: not a gap, nor the
    /// closing of an open subpath.
    pub(crate) boundary: bool,
    /// It counts toward the winding number.
    pub(crate) winds: bool,
}

/// The winding rule, as a fill style's low two bits name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rule {
    NonZero,
    Negative,
    EvenOdd,
    Positive,
}

/// The codes a fill style's low two bits name a winding rule by, three
/// being the positive rule's.
pub(crate) mod winding {
    /// The bits that hold the code.
    pub(crate) const BITS: u32 = 0b11;
    pub(crate) const NON_ZERO: u32 = 0;
    pub(crate) const NEGATIVE: u32 = 1;
    pub(crate) const EVEN_ODD: u32 = 2;
}

impl Rule {
    pub(crate) const fn of(style: u32) -> Self {
        match style & winding::BITS {
            winding::NON_ZERO => Self::NonZero,
            winding::NEGATIVE => Self::Negative,
            winding::EVEN_ODD => Self::EvenOdd,
            _ => Self::Positive,
        }
    }

    const fn inside(self, winding: i64) -> bool {
        match self {
            Self::NonZero => winding != 0,
            Self::Negative => winding < 0,
            Self::EvenOdd => winding & 1 != 0,
            Self::Positive => winding > 0,
        }
    }
}

/// The classes of pixel a fill style plots, by its bits 2 to 5.
pub(crate) mod class {
    pub(crate) const EXTERIOR: u32 = 1 << 2;
    pub(crate) const EXTERIOR_BOUNDARY: u32 = 1 << 3;
    pub(crate) const INTERIOR_BOUNDARY: u32 = 1 << 4;
    pub(crate) const INTERIOR: u32 = 1 << 5;
    pub(crate) const ALL: u32 = EXTERIOR | EXTERIOR_BOUNDARY | INTERIOR_BOUNDARY | INTERIOR;
}

/// Allocation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NoRoom;

fn floor_div(a: i128, b: i128) -> i128 {
    a.div_euclid(b)
}

/// `value`, already floored, as a pixel coordinate: far ones stay far, but
/// within a word.
#[expect(clippy::cast_possible_truncation, reason = "clamped into a word first")]
const fn pixel(value: i128) -> i64 {
    let held = if value > i32::MAX as i128 {
        i32::MAX as i128
    } else if value < i32::MIN as i128 {
        i32::MIN as i128
    } else {
        value
    };
    held as i64
}

/// The index of the first pixel whose centre lies past `value`, in a
/// pixel's units, and of the last no further than it.
fn centres_after(value: i64) -> i64 {
    pixel(floor_div(
        i128::from(value) + i128::from(HALF),
        i128::from(UNIT),
    ))
}

/// How a segment marks its boundary pixels.
#[derive(Clone, Copy, Debug)]
enum Mark {
    None,
    /// By column: `x0` its left end, `dx` positive.
    Columns {
        x0: i64,
        y0: i64,
        dx: i64,
        dy: i64,
        first: i64,
        last: i64,
    },
    /// By row: `y0` its lower end, `dy` positive.
    Rows {
        x0: i64,
        y0: i64,
        dx: i64,
        dy: i64,
    },
}

/// A segment ready for the rows.
#[derive(Clone, Copy, Debug)]
struct Prepared {
    /// The rows any part of it reaches.
    first_row: i64,
    last_row: i64,
    /// The rows it winds on, if it winds.
    winds: Option<(i64, i64)>,
    /// The rows it marks the boundary on.
    marks: (i64, i64),
    mark: Mark,
    from: (i64, i64),
    to: (i64, i64),
}

/// The row a column-marking segment marks in column `i`.
fn column_row(x0: i64, y0: i64, dx: i64, dy: i64, i: i64) -> i64 {
    let centre = i128::from(i) * i128::from(UNIT) + i128::from(HALF);
    let n = i128::from(y0) * i128::from(dx) + (centre - i128::from(x0)) * i128::from(dy);
    let d = i128::from(dx) * i128::from(UNIT);
    let row = floor_div(n, d);
    let on_edge = n.rem_euclid(d) == 0;
    pixel(if on_edge && dy > 0 { row - 1 } else { row })
}

impl Prepared {
    fn of(segment: &Segment) -> Self {
        let (ax, ay) = segment.from;
        let (bx, by) = segment.to;
        let (dx, dy) = (bx - ax, by - ay);
        let row_span = |lo: i64, hi: i64| (centres_after(lo), centres_after(hi) - 1);
        let winds = (segment.winds && dy != 0).then(|| row_span(ay.min(by), ay.max(by)));
        let (mark, marks) = if !segment.boundary || (dx == 0 && dy == 0) {
            (Mark::None, (1, 0))
        } else if dx.abs() > dy.abs() {
            let (x0, y0, x1, y1) = if ax <= bx {
                (ax, ay, bx, by)
            } else {
                (bx, by, ax, ay)
            };
            let (first, last) = row_span(x0, x1);
            let mark = Mark::Columns {
                x0,
                y0,
                dx: x1 - x0,
                dy: y1 - y0,
                first,
                last,
            };
            let rows = if first <= last {
                let (r0, r1) = (
                    column_row(x0, y0, x1 - x0, y1 - y0, first),
                    column_row(x0, y0, x1 - x0, y1 - y0, last),
                );
                (r0.min(r1), r0.max(r1))
            } else {
                (1, 0)
            };
            (mark, rows)
        } else {
            let (x0, y0, x1, y1) = if ay <= by {
                (ax, ay, bx, by)
            } else {
                (bx, by, ax, ay)
            };
            (
                Mark::Rows {
                    x0,
                    y0,
                    dx: x1 - x0,
                    dy: y1 - y0,
                },
                row_span(y0, y1),
            )
        };
        let mut first_row = i64::MAX;
        let mut last_row = i64::MIN;
        for (lo, hi) in [winds.unwrap_or((1, 0)), marks] {
            if lo <= hi {
                first_row = first_row.min(lo);
                last_row = last_row.max(hi);
            }
        }
        Self {
            first_row,
            last_row,
            winds,
            marks,
            mark,
            from: segment.from,
            to: segment.to,
        }
    }

    /// Where it crosses row `row`'s centre line, as the first pixel it lies
    /// left of, and its direction; `None` where it does not wind there.
    fn crossing(&self, row: i64) -> Option<(i64, i64)> {
        let (lo, hi) = self.winds?;
        if row < lo || row > hi {
            return None;
        }
        let (x0, y0) = (i128::from(self.from.0), i128::from(self.from.1));
        let (x1, y1) = (i128::from(self.to.0), i128::from(self.to.1));
        let centre = i128::from(row) * i128::from(UNIT) + i128::from(HALF);
        let mut num = x0 * (y1 - y0) + (centre - y0) * (x1 - x0);
        let mut den = y1 - y0;
        if den < 0 {
            num = -num;
            den = -den;
        }
        let first = floor_div(num - i128::from(HALF) * den, i128::from(UNIT) * den) + 1;
        Some((pixel(first), if y1 < y0 { 1 } else { -1 }))
    }

    /// The run of columns it marks on row `row`, if any.
    fn marked(&self, row: i64) -> Option<(i64, i64)> {
        if row < self.marks.0 || row > self.marks.1 {
            return None;
        }
        match self.mark {
            Mark::None => None,
            Mark::Rows { x0, y0, dx, dy } => {
                let centre = i128::from(row) * i128::from(UNIT) + i128::from(HALF);
                let n =
                    i128::from(x0) * i128::from(dy) + (centre - i128::from(y0)) * i128::from(dx);
                let column = pixel(floor_div(n, i128::from(dy) * i128::from(UNIT)));
                Some((column, column))
            }
            Mark::Columns {
                x0,
                y0,
                dx,
                dy,
                first,
                last,
            } => {
                let row_of = |i| column_row(x0, y0, dx, dy, i);
                // The rows run one way along the columns, so the columns on
                // one row are a run, found by halving.
                let rising = dy >= 0;
                let lo = first_where(first, last, |i| {
                    if rising {
                        row_of(i) >= row
                    } else {
                        row_of(i) <= row
                    }
                });
                let hi = first_where(first, last, |i| {
                    if rising {
                        row_of(i) > row
                    } else {
                        row_of(i) < row
                    }
                }) - 1;
                (lo <= hi).then_some((lo, hi))
            }
        }
    }
}

/// The first `i` in `lo..=hi` that `test` holds for, all after it holding
/// too; `hi + 1` if none.
fn first_where(mut lo: i64, hi: i64, test: impl Fn(i64) -> bool) -> i64 {
    let mut end = hi + 1;
    while lo < end {
        let mid = lo + (end - lo) / 2;
        if test(mid) {
            end = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

/// The pixels the end `(x, y)` of a stroked segment marks.
fn end_pixels(x: i64, y: i64, mut mark: impl FnMut(i64, i64)) {
    let (column, row) = (x.div_euclid(UNIT), y.div_euclid(UNIT));
    for i in column - 1..=column + 1 {
        for j in row - 1..=row + 1 {
            let (cx, cy) = (i * UNIT + HALF, j * UNIT + HALF);
            let reach = (x - cx).abs() + (y - cy).abs();
            if reach < HALF || (reach == HALF && x < cx) {
                mark(i, j);
            }
        }
    }
}

/// A scan's working buffers, kept from one scan to the next so a fill made
/// a subpath at a time allocates them once.
#[derive(Default)]
pub(crate) struct Scratch {
    prepared: Vec<Prepared>,
    ends: Vec<(i64, i64)>,
    active: Vec<usize>,
    crossings: Vec<(i64, i64)>,
    runs: Vec<(i64, i64)>,
    inside: Vec<(i64, i64)>,
}

/// Scan converts `segments` within `window` in `scratch`, the pixels of the
/// classes `classes` selects handed to `span` as a row and the first and
/// last columns of a run, `step` called as each row is scanned. A row no
/// segment reaches holds only exterior pixels, so it is scanned only where
/// those are asked for.
///
/// # Errors
///
/// [`NoRoom`] where the segments cannot be held, or what `step` gives.
pub(crate) fn scan<E: From<NoRoom>>(
    segments: &[Segment],
    rule: Rule,
    classes: u32,
    window: Bounds,
    scratch: &mut Scratch,
    mut step: impl FnMut() -> Result<(), E>,
    mut span: impl FnMut(i32, i32, i32),
) -> Result<(), E> {
    if classes & class::ALL == 0 || window.left > window.right || window.bottom > window.top {
        return Ok(());
    }
    let Scratch {
        prepared,
        ends,
        active,
        crossings,
        runs,
        inside,
    } = scratch;
    prepared.clear();
    ends.clear();
    active.clear();
    if !fallible::reserve(prepared, segments.len()) {
        return Err(NoRoom.into());
    }
    let (bottom, top) = (i64::from(window.bottom), i64::from(window.top));
    for segment in segments {
        let ready = Prepared::of(segment);
        if ready.first_row <= ready.last_row && ready.last_row >= bottom && ready.first_row <= top {
            prepared.push(ready);
        }
        if segment.boundary {
            let mut failed = false;
            for (x, y) in [segment.from, segment.to] {
                end_pixels(x, y, |i, j| {
                    if (bottom..=top).contains(&j) && fallible::try_push(ends, (j, i)).is_err() {
                        failed = true;
                    }
                });
            }
            if failed {
                return Err(NoRoom.into());
            }
        }
    }
    prepared.sort_unstable_by_key(|p| p.first_row);
    ends.sort_unstable();
    let rows = if classes & class::EXTERIOR == 0 {
        let low = prepared
            .first()
            .map(|p| p.first_row)
            .into_iter()
            .chain(ends.first().map(|e| e.0))
            .min();
        let high = prepared
            .iter()
            .map(|p| p.last_row)
            .chain(ends.last().map(|e| e.0))
            .max();
        match low.zip(high) {
            Some((low, high)) => low.max(bottom)..=high.min(top),
            None => return Ok(()),
        }
    } else {
        bottom..=top
    };
    let (mut next, mut next_end) = (0, 0);
    let (left, right) = (i64::from(window.left), i64::from(window.right));
    for row in rows {
        step()?;
        while next < prepared.len() && prepared[next].first_row <= row {
            fallible::try_push(active, next).map_err(|_| NoRoom)?;
            next += 1;
        }
        active.retain(|&k| prepared[k].last_row >= row);
        crossings.clear();
        runs.clear();
        for &k in active.iter() {
            let edge = &prepared[k];
            if let Some(crossing) = edge.crossing(row) {
                fallible::try_push(crossings, crossing).map_err(|_| NoRoom)?;
            }
            if let Some(run) = edge.marked(row) {
                fallible::try_push(runs, run).map_err(|_| NoRoom)?;
            }
        }
        while next_end < ends.len() && ends[next_end].0 < row {
            next_end += 1;
        }
        while next_end < ends.len() && ends[next_end].0 == row {
            let column = ends[next_end].1;
            fallible::try_push(runs, (column, column)).map_err(|_| NoRoom)?;
            next_end += 1;
        }
        interior(crossings, rule, left, right, inside)?;
        coalesce(runs, |(lo, hi), (from, to)| {
            (from <= hi + 1).then_some((lo, hi.max(to)))
        });
        classify(inside, runs, classes, left, right, |from, to| {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "every run lies within the window, which is in a word"
            )]
            span(row as i32, from as i32, to as i32);
        });
    }
    Ok(())
}

/// The runs of the row from `left` to `right` that are inside by `rule`,
/// from the crossings and their directions.
fn interior(
    crossings: &mut [(i64, i64)],
    rule: Rule,
    left: i64,
    right: i64,
    inside: &mut Vec<(i64, i64)>,
) -> Result<(), NoRoom> {
    inside.clear();
    crossings.sort_unstable_by_key(|c| c.0);
    let mut winding = 0;
    let mut at = left;
    for &(first, direction) in crossings.iter() {
        if first > at {
            if rule.inside(winding) {
                fallible::try_push(inside, (at, (first - 1).min(right))).map_err(|_| NoRoom)?;
            }
            at = first;
            if at > right {
                return Ok(());
            }
        }
        winding += direction;
    }
    if at <= right && rule.inside(winding) {
        fallible::try_push(inside, (at, right)).map_err(|_| NoRoom)?;
    }
    Ok(())
}

/// `items` sorted, each merged into the one kept before it where `joined`
/// makes the two one: runs that touch or overlap made one.
pub(crate) fn coalesce<T: Copy + Ord>(items: &mut Vec<T>, joined: impl Fn(T, T) -> Option<T>) {
    items.sort_unstable();
    let mut kept: usize = 0;
    for i in 0..items.len() {
        let item = items[i];
        if let Some(both) = kept
            .checked_sub(1)
            .and_then(|last| joined(items[last], item))
        {
            items[kept - 1] = both;
        } else {
            items[kept] = item;
            kept += 1;
        }
    }
    items.truncate(kept);
}

/// The parts of the row from `left` to `right` in each class `classes`
/// selects, from its interior and boundary runs.
fn classify(
    inside: &[(i64, i64)],
    boundary: &[(i64, i64)],
    classes: u32,
    left: i64,
    right: i64,
    mut span: impl FnMut(i64, i64),
) {
    let (mut a, mut b) = (0, 0);
    let mut at = left;
    let mut open: Option<i64> = None;
    while at <= right {
        while a < inside.len() && inside[a].1 < at {
            a += 1;
        }
        while b < boundary.len() && boundary[b].1 < at {
            b += 1;
        }
        let in_inside = a < inside.len() && inside[a].0 <= at;
        let in_boundary = b < boundary.len() && boundary[b].0 <= at;
        let until = [
            if in_inside {
                Some(inside[a].1)
            } else {
                inside.get(a).map(|r| r.0 - 1)
            },
            if in_boundary {
                Some(boundary[b].1)
            } else {
                boundary.get(b).map(|r| r.0 - 1)
            },
        ]
        .into_iter()
        .flatten()
        .fold(right, i64::min);
        let class = match (in_inside, in_boundary) {
            (false, false) => class::EXTERIOR,
            (false, true) => class::EXTERIOR_BOUNDARY,
            (true, true) => class::INTERIOR_BOUNDARY,
            (true, false) => class::INTERIOR,
        };
        if classes & class != 0 {
            open.get_or_insert(at);
        } else if let Some(start) = open.take() {
            span(start, at - 1);
        }
        at = until + 1;
    }
    if let Some(start) = open {
        span(start, right);
    }
}

#[cfg(test)]
mod tests;
