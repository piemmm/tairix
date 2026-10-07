//! One face of a wall of stones as it is built up: each stone let fall
//! where the work stands, rolling off what it lands on and down into the
//! nook where it comes to rest, between two stones under it, on one and the
//! foundation, or against the wall's head, as a heap of unequal stones
//! settles (Visscher and Bolsterli, "Random packing of equal and unequal
//! spheres in two and three dimensions", Nature 239, 1972).

use alloc::vec::Vec;
use core::f64::consts::FRAC_PI_2;

use tairix_util::mathf;

/// A stone set in a face: its middle `x` along the stretch and `y` up from
/// the foundation, and how far it reaches about it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct Stone {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) r: f64,
}

/// How finely the top of the work is kept along a face.
const SAMPLE: f64 = 0.01;

/// How near two stones may come and still only touch.
const TOUCH: f64 = 1e-6;

/// The most falls and rolls a stone takes to come to rest. Each takes it
/// lower, so it rests within a few; a stone still moving after this many
/// has met rounding, not a heap, and is not set.
const MOST_MOVES: u32 = 64;

/// One face of a stretch of wall from `start` to `end` along it: every
/// stone set in it, in the order set, the farthest any reaches, and their ids
/// in order along the stretch; and the top of the work every `SAMPLE` along
/// it.
#[derive(Debug)]
pub(super) struct Pile {
    start: f64,
    end: f64,
    stones: Vec<Stone>,
    largest: f64,
    along: Vec<u32>,
    tops: Vec<f64>,
}

/// How a stone rolling over another stops: at rest where it is, touching
/// a stone it rolls on over, or off the side to fall again.
enum Rolled {
    Rest((f64, f64)),
    Over((f64, f64), Stone),
    Off((f64, f64)),
}

impl Pile {
    /// A face from `start` to `end` with nothing yet laid on its foundation;
    /// `None` when the heap will not hold it.
    pub(super) fn new((start, end): (f64, f64)) -> Option<Self> {
        let samples = sample_at(start, end)? + 1;
        let mut tops = Vec::new();
        tops.try_reserve_exact(samples).ok()?;
        tops.resize(samples, 0.0);
        Some(Self {
            start,
            end,
            stones: Vec::new(),
            largest: 0.0,
            along: Vec::new(),
            tops,
        })
    }

    /// Every stone set, in the order set.
    #[cfg(test)]
    pub(super) fn stones(&self) -> &[Stone] {
        &self.stones
    }

    /// Where along the face sample `at` lies.
    pub(super) fn x_of(&self, at: usize) -> f64 {
        self.start + SAMPLE * f64::from(u32::try_from(at).unwrap_or(u32::MAX))
    }

    /// The samples from `from` to `to` along the face.
    fn samples(&self, (from, to): (f64, f64)) -> core::ops::Range<usize> {
        let last = self.tops.len();
        let index = |x: f64| sample_at(self.start, x).unwrap_or(0).min(last);
        index(from)..index(to)
    }

    /// Where from `from` to `to` the work stands lowest, and how high; the
    /// first of those as low.
    pub(super) fn lowest(&self, span: (f64, f64)) -> Option<(usize, f64)> {
        let range = self.samples(span);
        let first = range.start;
        self.tops
            .get(range)?
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(index, &top)| (first + index, top))
    }

    /// How high the work stands at its highest from `from` to `to`, and on
    /// average.
    pub(super) fn top(&self, span: (f64, f64)) -> (f64, f64) {
        let tops = self.tops.get(self.samples(span)).unwrap_or(&[]);
        let highest = tops.iter().fold(0.0, |high: f64, &top| high.max(top));
        let count = u32::try_from(tops.len()).unwrap_or(u32::MAX).max(1);
        (highest, tops.iter().sum::<f64>() / f64::from(count))
    }

    /// Raise the work at sample `at` over a notch no stone fills, leaving
    /// it a void, by `by`.
    pub(super) fn bridge(&mut self, at: usize, by: f64) {
        if let Some(top) = self.tops.get_mut(at) {
            *top += by;
        }
    }

    /// Set `stone` in the face, where it rests; `None` when the heap will
    /// not hold it.
    pub(super) fn set(&mut self, stone: Stone) -> Option<()> {
        let Stone { x, y, r } = stone;
        let id = u32::try_from(self.stones.len()).ok()?;
        self.stones.try_reserve(1).ok()?;
        self.along.try_reserve(1).ok()?;
        self.stones.push(stone);
        self.largest = self.largest.max(r);
        let place = self
            .along
            .partition_point(|&other| self.stone(other).is_some_and(|other| other.x <= x));
        self.along.insert(place, id);
        for sample in self.samples((x - r, x + r)) {
            let dx = self.x_of(sample) - x;
            let top = y + mathf::sqrt((r * r - dx * dx).max(0.0));
            if let Some(height) = self.tops.get_mut(sample) {
                *height = height.max(top);
            }
        }
        Some(())
    }

    fn stone(&self, id: u32) -> Option<Stone> {
        self.stones.get(usize::try_from(id).ok()?).copied()
    }

    /// The stones whose middles lie from `from` to `to` along the face.
    fn near(&self, (from, to): (f64, f64)) -> impl Iterator<Item = Stone> + '_ {
        let first = self
            .along
            .partition_point(|&id| self.stone(id).is_some_and(|stone| stone.x < from));
        self.along
            .get(first..)
            .unwrap_or(&[])
            .iter()
            .filter_map(|&id| self.stone(id))
            .take_while(move |stone| stone.x <= to)
    }

    /// Where a stone reaching `r` let fall at `x` along the face comes to
    /// rest, if it does.
    pub(super) fn rest(&self, r: f64, x: f64) -> Option<(f64, f64)> {
        let (left, right) = (self.start + r, self.end - r);
        if left > right {
            return None;
        }
        let mut at = (x.clamp(left, right), f64::INFINITY);
        let mut moves = 0;
        while moves < MOST_MOVES {
            moves += 1;
            let (landing, on) = self.fall(at, r);
            at.1 = landing;
            let Some(mut pivot) = on else {
                return Some(at);
            };
            let side = if at.0 >= pivot.x { 1.0 } else { -1.0 };
            while moves < MOST_MOVES {
                moves += 1;
                match self.roll(at, r, (pivot, side), (left, right)) {
                    Rolled::Rest(place) => return Some(place),
                    Rolled::Over(place, next) => (at, pivot) = (place, next),
                    Rolled::Off(place) => {
                        at = place;
                        break;
                    }
                }
            }
        }
        None
    }

    /// Where a stone reaching `r` falling straight down from `(x, y)` first
    /// lands: on the foundation, or on the stone it then rests on.
    fn fall(&self, (x, y): (f64, f64), r: f64) -> (f64, Option<Stone>) {
        let reach = r + self.largest;
        self.near((x - reach, x + reach))
            .fold((r, None), |(high, on), stone| {
                let (touch, run) = (r + stone.r, x - stone.x);
                if run.abs() >= touch {
                    return (high, on);
                }
                let contact = stone.y + mathf::sqrt(touch * touch - run * run);
                if contact < y - TOUCH && contact > high {
                    (contact, Some(stone))
                } else {
                    (high, on)
                }
            })
    }

    /// Roll a stone reaching `r` at `at` over `pivot` toward `side` until it
    /// stops: on the foundation, against the wall's head at `heads`, in the
    /// nook it meets a stone ahead of it in, rolling on over a stone it
    /// meets behind, or off the pivot's side to fall again.
    fn roll(
        &self,
        at: (f64, f64),
        r: f64,
        (pivot, side): (Stone, f64),
        (left, right): (f64, f64),
    ) -> Rolled {
        let head = if side > 0.0 { right } else { left };
        if side * (at.0 - head) >= -TOUCH {
            return Rolled::Rest(at);
        }
        let around = r + pivot.r;
        let from = mathf::atan2(at.0 - pivot.x, at.1 - pivot.y);
        let ahead = |angle: f64| {
            let gone = side * (angle - from);
            (gone > TOUCH && side * angle <= FRAC_PI_2 + TOUCH).then_some(gone)
        };
        let place = |angle: f64| {
            (
                pivot.x + around * mathf::sin(angle),
                pivot.y + around * mathf::cos(angle),
            )
        };
        // The first thing it meets, how far round the pivot, and what.
        let mut first: Option<(f64, f64, Option<Stone>)> = None;
        let mut meet = |angle: f64, what: Option<Stone>| {
            if let Some(gone) = ahead(angle) {
                if first.is_none_or(|(least, _, _)| gone < least) {
                    first = Some((gone, angle, what));
                }
            }
        };
        let floor = (r - pivot.y) / around;
        if floor.abs() <= 1.0 {
            meet(side * mathf::acos(floor), None);
        }
        let reach = (head - pivot.x) / around;
        if reach.abs() <= 1.0 {
            meet(mathf::asin(reach), None);
        }
        let span = (
            pivot.x - around - r - self.largest,
            pivot.x + around + r + self.largest,
        );
        for stone in self.near(span).filter(|stone| *stone != pivot) {
            for angle in crossings(pivot, stone, r) {
                meet(angle, Some(stone));
            }
        }
        match first {
            None => Rolled::Off(place(side * FRAC_PI_2)),
            Some((_, angle, None)) => Rolled::Rest(place(angle)),
            Some((_, angle, Some(stone))) => {
                let stopped = place(angle);
                if side * (stone.x - stopped.0) >= 0.0 {
                    Rolled::Rest(stopped)
                } else {
                    Rolled::Over(stopped, stone)
                }
            }
        }
    }
}

/// The first sample at or past `x` along a face starting at `start`.
fn sample_at(start: f64, x: f64) -> Option<usize> {
    usize::try_from(mathf::round_i32(mathf::ceil(
        ((x - start) / SAMPLE).clamp(0.0, 1.0e8),
    )))
    .ok()
}

/// How far round `pivot` from straight above it, toward its right, a
/// stone reaching `r` rolling over it touches `other`: none, one or two
/// angles.
fn crossings(pivot: Stone, other: Stone, r: f64) -> impl Iterator<Item = f64> {
    let (to_pivot, to_other) = (r + pivot.r, r + other.r);
    let (dx, dy) = (other.x - pivot.x, other.y - pivot.y);
    let apart = mathf::hypot(dx, dy);
    let meets =
        apart > TOUCH && apart <= to_pivot + to_other && apart >= (to_pivot - to_other).abs();
    let toward =
        (to_pivot * to_pivot - to_other * to_other + apart * apart) / (2.0 * apart.max(TOUCH));
    let off = mathf::sqrt((to_pivot * to_pivot - toward * toward).max(0.0));
    let (ux, uy) = (dx / apart.max(TOUCH), dy / apart.max(TOUCH));
    let (px, py) = (toward * ux, toward * uy);
    [
        (px - off * uy, py + off * ux),
        (px + off * uy, py - off * ux),
    ]
    .into_iter()
    .filter(move |_| meets)
    .map(|(x, y)| mathf::atan2(x, y))
}

#[cfg(test)]
#[path = "pile_tests.rs"]
mod tests;
