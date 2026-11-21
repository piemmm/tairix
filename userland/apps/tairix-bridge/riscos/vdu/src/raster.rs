//! The shapes `OS_Plot` draws, as spans of pixels, each rule pinned to what
//! genuine RISC OS 5 was recorded drawing: lines walked from their leftmost
//! or lowest end, polygons as the rows their edges' lines reach, circles
//! from a radius softened by its own root, ellipses in RISC OS's fixed
//! point. Arcs, segments and sectors cut circles in [`crate::arc`].
//!
//! Everything here is pure: coordinates are pixels from the screen's bottom
//! left, and a shape is handed over a span at a time.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::changed::Rect;

/// A point in pixels.
pub(crate) type Point = (i32, i32);

/// A row and the leftmost and rightmost pixel drawn on it.
pub(crate) type Span = (i32, i32, i32);

/// The host could not hold a shape this large.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Full;

/// Room in `v` for `more` further items.
pub(crate) fn room<T>(v: &mut Vec<T>, more: usize) -> Result<(), Full> {
    v.try_reserve(more).map_err(|_| Full)
}

/// `v` as a coordinate, held at the nearest end of the range.
pub(crate) fn saturate(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

/// A line's pixels: walked from the end with the smaller major coordinate,
/// the minor stepping only when the error is past half, and given in order
/// from its first point. Each pixel is worked out from its place, so a part
/// of the line costs only that part however long the line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Walk {
    /// The end walked from.
    from: (i64, i64),
    x_major: bool,
    major: u64,
    minor: u64,
    /// The way the minor coordinate steps from `from`.
    minor_sign: i64,
    /// Walked from the line's last point, so its places run backward.
    reversed: bool,
}

impl Walk {
    /// The line from `a` to `b`.
    pub(crate) fn new(a: Point, b: Point) -> Self {
        Self::between(
            (i64::from(a.0), i64::from(a.1)),
            (i64::from(b.0), i64::from(b.1)),
        )
    }

    /// The line from `a` to `b`, whose ends may lie past the coordinates a
    /// pixel has, as a ray's do.
    pub(crate) fn between(a: (i64, i64), b: (i64, i64)) -> Self {
        let (dx, dy) = (b.0.abs_diff(a.0), b.1.abs_diff(a.1));
        let x_major = dx >= dy;
        let reversed = if x_major { b.0 < a.0 } else { b.1 < a.1 };
        let (from, to) = if reversed { (b, a) } else { (a, b) };
        let (major, minor, minor_sign) = if x_major {
            (dx, dy, (to.1 - from.1).signum())
        } else {
            (dy, dx, (to.0 - from.0).signum())
        };
        Self {
            from,
            x_major,
            major,
            minor,
            minor_sign,
            reversed,
        }
    }

    /// Its pixels.
    pub(crate) const fn len(&self) -> u64 {
        self.major + 1
    }

    /// The minor steps taken by walked pixel `i`.
    fn steps(&self, i: u64) -> u64 {
        if self.major == 0 {
            return 0;
        }
        let (major, minor) = (u128::from(self.major), u128::from(self.minor));
        u64::try_from((2 * minor * u128::from(i) + major - 1) / (2 * major)).unwrap_or(self.minor)
    }

    /// The first walked pixel to have taken `t` minor steps, if any has.
    fn first_after(&self, t: u64) -> Option<u64> {
        if t == 0 {
            return Some(0);
        }
        if t > self.minor {
            return None;
        }
        let (major, minor, t) = (
            u128::from(self.major),
            u128::from(self.minor),
            u128::from(t),
        );
        u64::try_from((2 * major * t - major + 1).div_ceil(2 * minor))
            .ok()
            .filter(|&i| i <= self.major)
    }

    /// Walked pixel `i`.
    fn walked(&self, i: u64) -> Point {
        let along = i64::try_from(i).unwrap_or(i64::MAX);
        let across = self.minor_sign * i64::try_from(self.steps(i)).unwrap_or(i64::MAX);
        let (dx, dy) = if self.x_major {
            (along, across)
        } else {
            (across, along)
        };
        (saturate(self.from.0 + dx), saturate(self.from.1 + dy))
    }

    /// The walked pixels whose `along` coordinate is within `low` to
    /// `high`, taken first.
    fn along_within(&self, low: i64, high: i64) -> Option<(u64, u64)> {
        let start = if self.x_major {
            self.from.0
        } else {
            self.from.1
        };
        let first = u64::try_from((low - start).max(0)).ok()?;
        let last = u64::try_from(high - start).ok()?.min(self.major);
        (first <= last).then_some((first, last))
    }

    /// The walked pixels whose minor coordinate is within `low` to `high`.
    fn across_within(&self, low: i64, high: i64) -> Option<(u64, u64)> {
        let start = if self.x_major {
            self.from.1
        } else {
            self.from.0
        };
        let (fewest, most) = match self.minor_sign {
            0 => return (low..=high).contains(&start).then_some((0, self.major)),
            1 => (low - start, high - start),
            _ => (start - high, start - low),
        };
        let fewest = u64::try_from(fewest.max(0)).ok()?;
        let most = u64::try_from(most).ok()?.min(self.minor);
        if fewest > most {
            return None;
        }
        let first = self.first_after(fewest)?;
        let last = self
            .first_after(most + 1)
            .map_or(self.major, |next| next - 1);
        Some((first, last))
    }

    /// The pixels within `window`, each with its place counted from the
    /// line's first point, in that order.
    pub(crate) fn within(&self, window: Rect) -> impl Iterator<Item = (u64, Point)> + '_ {
        let (along, across) = if self.x_major {
            ((window.left, window.right), (window.bottom, window.top))
        } else {
            ((window.bottom, window.top), (window.left, window.right))
        };
        let walked = self
            .along_within(i64::from(along.0), i64::from(along.1))
            .zip(self.across_within(i64::from(across.0), i64::from(across.1)))
            .map(|((a, b), (c, d))| (a.max(c), b.min(d)))
            .filter(|&(first, last)| first <= last);
        let places = walked.map(|(first, last)| {
            if self.reversed {
                self.major - last..=self.major - first
            } else {
                first..=last
            }
        });
        places
            .into_iter()
            .flatten()
            .map(|place| (place, self.at(place)))
    }

    /// The pixel at `place` from the line's first point.
    pub(crate) fn at(&self, place: u64) -> Point {
        self.walked(if self.reversed {
            self.major - place
        } else {
            place
        })
    }

    /// The leftmost and rightmost of its pixels on row `y`, if it has any
    /// there.
    pub(crate) fn row(&self, y: i32) -> Option<(i32, i32)> {
        self.row_within(y, (0, self.major))
    }

    /// As [`Walk::row`], of the pixels at places from `places.0` to
    /// `places.1` alone.
    pub(crate) fn row_within(&self, y: i32, places: (u64, u64)) -> Option<(i32, i32)> {
        let y = i64::from(y);
        let (first, last) = if self.x_major {
            self.across_within(y, y)?
        } else {
            self.along_within(y, y)?
        };
        let (low, high) = if self.reversed {
            (
                self.major.saturating_sub(places.1),
                self.major.saturating_sub(places.0),
            )
        } else {
            places
        };
        let (first, last) = (first.max(low), last.min(high));
        if first > last {
            return None;
        }
        let (left, right) = (self.walked(first).0, self.walked(last).0);
        Some((left.min(right), left.max(right)))
    }
}

/// Pixel `n` of the line from the origin through `to` and on past it,
/// counted from the origin: the pixels [`Walk`] gives to `to`, repeated a
/// period of `to` at a time.
pub(crate) fn ray_pixel(to: Point, n: u64) -> Point {
    let walk = Walk::new((0, 0), to);
    if walk.major == 0 {
        return (0, 0);
    }
    let (lap, place) = (n / walk.major, n % walk.major);
    let (x, y) = walk.at(place);
    let lap = i64::try_from(lap).unwrap_or(i64::MAX);
    (
        saturate(i64::from(x).saturating_add(lap.saturating_mul(i64::from(to.0)))),
        saturate(i64::from(y).saturating_add(lap.saturating_mul(i64::from(to.1)))),
    )
}

/// Rows of one span each: every row's leftmost and rightmost pixel.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rows {
    /// The lowest row held.
    bottom: i32,
    /// Each row's span from `bottom` up, [`UNREACHED`] where nothing has
    /// reached it.
    spans: VecDeque<(i32, i32)>,
}

/// A row nothing has reached.
const UNREACHED: (i32, i32) = (i32::MAX, i32::MIN);

impl Rows {
    /// Widens row `y` over `x`.
    pub(crate) fn add(&mut self, (x, y): Point) -> Result<(), Full> {
        if self.spans.is_empty() {
            self.bottom = y;
        }
        if y < self.bottom {
            let below = (self.bottom - y).unsigned_abs() as usize;
            self.spans.try_reserve(below).map_err(|_| Full)?;
            for _ in 0..below {
                self.spans.push_front(UNREACHED);
            }
            self.bottom = y;
        }
        let at = (y - self.bottom).unsigned_abs() as usize;
        if at >= self.spans.len() {
            self.spans
                .try_reserve(at + 1 - self.spans.len())
                .map_err(|_| Full)?;
            self.spans.resize(at + 1, UNREACHED);
        }
        if let Some(span) = self.spans.get_mut(at) {
            span.0 = span.0.min(x);
            span.1 = span.1.max(x);
        }
        Ok(())
    }

    /// Row `y`'s span, if anything reached it.
    pub(crate) fn get(&self, y: i32) -> Option<(i32, i32)> {
        let at = usize::try_from(y.checked_sub(self.bottom)?).ok()?;
        self.spans
            .get(at)
            .copied()
            .filter(|&span| span != UNREACHED)
    }

    /// Every row something reached, lowest first.
    pub(crate) fn spans(&self) -> impl Iterator<Item = Span> + '_ {
        (self.bottom..)
            .zip(&self.spans)
            .filter(|&(_, &span)| span != UNREACHED)
            .map(|(y, &(left, right))| (y, left, right))
    }
}

/// The rows within `rows` a polygon fills: each between the leftmost and
/// rightmost pixel its edges' lines reach there.
pub(crate) fn polygon(vertices: &[Point], rows: View) -> Result<Rows, Full> {
    let mut filled = Rows::default();
    for (&a, &b) in vertices.iter().zip(vertices.iter().cycle().skip(1)) {
        let edge = Walk::new(a, b);
        for y in a.1.min(b.1).max(rows.bottom)..=a.1.max(b.1).min(rows.top) {
            if let Some((left, right)) = edge.row(y) {
                filled.add((left, y))?;
                filled.add((right, y))?;
            }
        }
    }
    Ok(filled)
}

/// A circle's squared radius in pixels `tall` times as tall as they are
/// wide, or `wide` times as wide as tall, through `edge`: softened by its
/// own root.
pub(crate) fn soft_radius2(edge: Point, tall: u32, wide: u32) -> u64 {
    let scale = |v: i32, by: u32| u64::from(v.unsigned_abs()) * u64::from(by);
    let (ex, ey) = (scale(edge.0, wide), scale(edge.1, tall));
    let d2 = ex.saturating_mul(ex).saturating_add(ey.saturating_mul(ey));
    d2.saturating_add(d2.isqrt())
}

/// A circle or an ellipse about the origin, each row's span worked out as it
/// is asked for, those below the centre mirrored through it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Round {
    shape: Shape,
    /// The highest row.
    top: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// Squared radius `radius2`, in pixels `tall` times as tall as wide or
    /// `wide` times as wide as tall.
    Circle { radius2: u64, tall: u64, wide: u64 },
    /// The width's ratio to the height to eight fractional bits, the shear's
    /// step a row to sixteen, and the height.
    Ellipse { ratio: i64, step: i64, height: i64 },
    /// No height: one row reaching this far either way.
    Flat(i32),
}

/// Half a unit of the sixteen fractional bits an ellipse's rows round from.
const HALF: i64 = 1 << 15;

impl Round {
    /// The circle of squared radius `radius2` about the origin, in pixels
    /// `tall` times as tall as they are wide, or `wide` times as wide as
    /// tall.
    pub(crate) fn circle(radius2: u64, tall: u32, wide: u32) -> Self {
        let (tall, wide) = (u64::from(tall.max(1)), u64::from(wide.max(1)));
        Self {
            shape: Shape::Circle {
                radius2,
                tall,
                wide,
            },
            top: saturate(i64::try_from(radius2.isqrt() / tall).unwrap_or(i64::MAX)),
        }
    }

    /// The ellipse whose half-width across its centre's row is `a` and whose
    /// highest point is `top`, in RISC OS's fixed point: the width's ratio
    /// to the height held to eight fractional bits, each row's root to
    /// eight, the shear stepped at sixteen; a row is widened to meet the one
    /// above where the shear would part them.
    pub(crate) fn ellipse(a: i32, top: Point) -> Self {
        let (shear, height) = if top.1 < 0 {
            (-i64::from(top.0), -i64::from(top.1))
        } else {
            (i64::from(top.0), i64::from(top.1))
        };
        let a = i64::from(a.unsigned_abs());
        if height == 0 {
            return Self {
                shape: Shape::Flat(saturate(a)),
                top: 0,
            };
        }
        Self {
            shape: Shape::Ellipse {
                ratio: (a << 8) / height,
                step: (shear << 16) / height,
                height,
            },
            top: saturate(height),
        }
    }

    /// The highest row.
    pub(crate) const fn top(&self) -> i32 {
        self.top
    }

    /// Row `k`'s span, `k` from nought to the top.
    fn upper(&self, k: u64) -> (i32, i32) {
        match self.shape {
            Shape::Circle {
                radius2,
                tall,
                wide,
            } => {
                let row = k * tall;
                let across = saturate(
                    i64::try_from((radius2 - row * row).isqrt() / wide).unwrap_or(i64::MAX),
                );
                (-across, across)
            }
            Shape::Ellipse {
                ratio,
                step,
                height,
            } => {
                let k = i64::try_from(k).unwrap_or(height);
                let unjoined = |k: i64| {
                    let root = (u128::from((height * height - k * k).unsigned_abs()) << 16).isqrt();
                    let reach = ratio * i64::try_from(root).unwrap_or(i64::MAX);
                    let centre = k * step;
                    (
                        saturate((centre - reach + HALF) >> 16),
                        saturate((centre + reach + HALF) >> 16),
                    )
                };
                let mut row = unjoined(k);
                if k < height {
                    let above = unjoined(k + 1);
                    row.1 = row.1.max(above.0);
                    row.0 = row.0.min(above.1);
                }
                row
            }
            Shape::Flat(a) => (-a, a),
        }
    }

    /// Row `k`'s span from the centre's column.
    pub(crate) fn span(&self, k: i32) -> Option<(i32, i32)> {
        if k.unsigned_abs() > self.top.unsigned_abs() {
            return None;
        }
        let (left, right) = self.upper(u64::from(k.unsigned_abs()));
        Some(if k < 0 {
            (right.saturating_neg(), left.saturating_neg())
        } else {
            (left, right)
        })
    }

    /// Whether `p` lies within it.
    pub(crate) fn contains(&self, (x, y): Point) -> bool {
        self.span(y)
            .is_some_and(|(left, right)| x >= left && x <= right)
    }

    /// Whether `p` is on its outline: within it, at a row's end or not
    /// covered by both rows beside it.
    pub(crate) fn on_outline(&self, (x, y): Point) -> bool {
        let Some((left, right)) = self.span(y) else {
            return false;
        };
        let beside = |dy: i32| y.checked_add(dy).is_some_and(|y| self.contains((x, y)));
        x >= left && x <= right && (x == left || x == right || !(beside(-1) && beside(1)))
    }

    /// The highest row column `x` reaches, for a round whose rows are
    /// centred on its column, as a circle's are.
    pub(crate) fn column_top(&self, x: i32) -> Option<i32> {
        let x = x.saturating_abs();
        let reaches = |k: i64| self.upper(k.unsigned_abs()).1 >= x;
        if !reaches(0) {
            return None;
        }
        let (mut low, mut high) = (0, i64::from(self.top));
        while low < high {
            let middle = low + (high - low + 1) / 2;
            if reaches(middle) {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        i32::try_from(low).ok()
    }

    /// The rows it fills within `view`, lowest first.
    pub(crate) fn fill(&self, view: View) -> impl Iterator<Item = Span> + '_ {
        let top = self.top;
        (view.bottom.max(-top)..=view.top.min(top))
            .filter_map(|k| self.span(k).map(|(left, right)| (k, left, right)))
    }

    /// Its outline's runs on the rows of `view`, lowest row first: each
    /// row's span less the part both rows beside it cover, its ends kept.
    pub(crate) fn outline(&self, view: View) -> impl Iterator<Item = Span> + '_ {
        self.fill(view).flat_map(move |(k, left, right)| {
            let beside = |dy: i32| k.checked_add(dy).and_then(|k| self.span(k));
            let covered = match (beside(-1), beside(1)) {
                (Some(below), Some(above)) => (
                    below.0.max(above.0).max(left.saturating_add(1)),
                    below.1.min(above.1).min(right.saturating_sub(1)),
                ),
                _ => (1, 0),
            };
            let runs = if covered.0 > covered.1 {
                [(left, right), (1, 0)]
            } else {
                [(left, covered.0 - 1), (covered.1 + 1, right)]
            };
            runs.into_iter()
                .filter(|(from, to)| from <= to)
                .map(move |(from, to)| (k, from, to))
        })
    }
}

/// The part of the plane that can be seen, about a shape's centre: columns
/// `left` to `right` and rows `bottom` to `top`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct View {
    pub(crate) left: i32,
    pub(crate) bottom: i32,
    pub(crate) right: i32,
    pub(crate) top: i32,
}

impl View {
    /// Whether row `k` is within it.
    pub(crate) const fn holds(self, k: i32) -> bool {
        k >= self.bottom && k <= self.top
    }

    /// The run from `from` to `to` cut to the columns, a pixel left on either
    /// side where it reaches past them so a row filled to it still meets the
    /// edge.
    pub(crate) fn cut(self, (from, to): (i32, i32)) -> (i32, i32) {
        let (low, high) = (self.left.saturating_sub(1), self.right.saturating_add(1));
        (from.max(low).min(high), to.max(low).min(high))
    }
}

#[cfg(test)]
mod tests;
