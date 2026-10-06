//! Where the countryside's people live: villages spaced by a priority rule,
//! and on every holding a village does not gather in, its own farmstead on
//! the best ground it has.

use core::hash::Hasher;

use alloc::vec::Vec;

use tairix_util::mathf;

use crate::ground::{self, Ground};
use crate::holding::{HoldingId, Lattice};
use crate::key::{Key, Stage};
use crate::plane::{Point, Rect};

/// A tier of villages: one offered in every square of a lattice `spacing`
/// apart, standing where its ground suits it and no better-ranked village
/// lies within `exclusion` of it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Villages {
    /// The side of a square of the lattice villages are offered on.
    pub spacing: f64,
    /// How close no two villages stand, as a share of the spacing: at most
    /// one, so a village's fate turns on the squares about its own alone.
    pub exclusion: f64,
    /// How far from a village its holdings' farms are gathered into it
    /// rather than standing on their own, as a share of the spacing.
    pub gathers: f64,
}

/// What a settlement is.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Settled {
    /// A village, offered in square `(x, y)` of its lattice.
    Village(i32, i32),
    /// The farmstead of a holding.
    Farmstead(HoldingId),
}

impl Settled {
    /// Its place as a key's place, apart from any other settlement's.
    pub(crate) fn place(self) -> (i64, i64) {
        match self {
            Self::Village(x, y) => (i64::from(x) * 2 + 1, i64::from(y) * 2),
            Self::Farmstead(holding) => (i64::from(holding.i) * 2, i64::from(holding.j) * 2),
        }
    }

    /// Write what names it into `hasher`.
    pub(crate) fn write(self, hasher: &mut impl Hasher) {
        let (tag, (x, y)) = match self {
            Self::Village(x, y) => (0, (x, y)),
            Self::Farmstead(holding) => (1, (holding.i, holding.j)),
        };
        hasher.write_u8(tag);
        hasher.write_i32(x);
        hasher.write_i32(y);
    }
}

/// A settlement and where it stands.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Settlement {
    /// What it is.
    pub settled: Settled,
    /// Where it stands: a village's middle, a farmstead's yard.
    pub at: Point,
}

/// How well the ground at `at` suits a settlement, in `0.0..=1.0`: dry,
/// level, and good to farm about.
pub(crate) fn suitability(ground: &dyn Ground, at: Point) -> f64 {
    if ground::wet(ground, at) {
        return 0.0;
    }
    let lie = ground.lie(at);
    let (slope, _) = ground::slope(ground, at, 8.0);
    let level = (1.0 - slope / 0.25).clamp(0.0, 1.0);
    (1.0 - lie.wet) * level * (0.35 + 0.65 * lie.fertile)
}

/// The village square `(x, y)` offers, and its rank: where it would stand,
/// if its ground suits it.
fn offer(key: Key, tier: &Villages, ground: &dyn Ground, (x, y): (i32, i32)) -> Option<(Point, u64)> {
    let mut draws = key.draws(Stage::Village, (i64::from(x), i64::from(y)));
    let at = Point::new(
        (f64::from(x) + 0.15 + 0.7 * draws.unit()) * tier.spacing,
        (f64::from(y) + 0.15 + 0.7 * draws.unit()) * tier.spacing,
    );
    let stands = draws.unit() < suitability(ground, at);
    // The rank comes of the square alone, so a village refused its ground
    // leaves every other's as it was.
    stands.then(|| (at, key.word(Stage::Village, (i64::from(x), i64::from(y)), u32::MAX)))
}

/// The villages standing in `rect`, in their squares' order; `None` where
/// the heap will not hold them.
pub(crate) fn villages(
    key: Key,
    tier: &Villages,
    ground: &dyn Ground,
    rect: Rect,
) -> Option<Vec<Settlement>> {
    let square = |value: f64| mathf::round_i32(mathf::floor(value / tier.spacing));
    let (x0, x1) = (square(rect.low.x), square(rect.high.x));
    let (y0, y1) = (square(rect.low.y), square(rect.high.y));
    let reach = tier.exclusion.clamp(0.0, 1.0) * tier.spacing;
    let mut standing = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            let Some((at, rank)) = offer(key, tier, ground, (x, y)) else {
                continue;
            };
            if !rect.contains(at) {
                continue;
            }
            // A strict order on rank, then square: of any two offers within
            // reach exactly one yields.
            let outranked = (-1..=1).any(|dy| {
                (-1..=1).any(|dx| {
                    (dx, dy) != (0, 0)
                        && offer(key, tier, ground, (x + dx, y + dy)).is_some_and(|(other, theirs)| {
                            (theirs, (x + dx, y + dy)) > (rank, (x, y)) && (other - at).length() < reach
                        })
                })
            });
            if !outranked {
                standing.try_reserve(1).ok()?;
                standing.push(Settlement {
                    settled: Settled::Village(x, y),
                    at,
                });
            }
        }
    }
    Some(standing)
}

/// How many spots of a holding its farmstead is looked for among.
const FARM_SPOTS: u32 = 16;

/// The least a holding's ground must suit a farm for one to stand on it.
const FARMABLE: f64 = 0.2;

/// The farmstead of `holding`, where it has ground good enough to farm and
/// none of `villages` lies within `gathers` of its vertex to gather its farm:
/// of a few spots about its middle, the one whose ground suits it best,
/// nearer the middle on ties of suit.
pub(crate) fn farmstead(
    key: Key,
    lattice: &Lattice,
    ground: &dyn Ground,
    (holding, villages, gathers): (HoldingId, &[Settlement], f64),
) -> Option<Settlement> {
    let middle = lattice.vertex(holding);
    if gatherer(villages, middle, gathers).is_some() {
        return None;
    }
    let outline = lattice.outline(holding);
    let mut draws = key.draws(Stage::Farmstead, holding.place());
    let reach = 0.32 * lattice.spacing();
    let mut best: Option<(f64, Point)> = None;
    for _ in 0..FARM_SPOTS {
        let (angle, distance) = (
            draws.range(0.0, core::f64::consts::TAU),
            reach * mathf::sqrt(draws.unit()),
        );
        let spot = middle + Point::toward(angle) * distance;
        if !outline.contains(spot) {
            continue;
        }
        let score = suitability(ground, spot) - 0.15 * distance / reach;
        if best.is_none_or(|(most, _)| score > most) {
            best = Some((score, spot));
        }
    }
    let (score, at) = best?;
    (score + 0.15 >= FARMABLE).then_some(Settlement {
        settled: Settled::Farmstead(holding),
        at,
    })
}

/// The nearest of `villages` within `gathers` of `at`, which gathers the
/// farm of a holding whose vertex stands there.
pub(crate) fn gatherer(villages: &[Settlement], at: Point, gathers: f64) -> Option<&Settlement> {
    villages
        .iter()
        .map(|village| ((village.at - at).length(), village))
        .filter(|&(apart, _)| apart < gathers)
        .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.settled.cmp(&b.1.settled)))
        .map(|(_, village)| village)
}

#[cfg(test)]
#[path = "site_tests.rs"]
mod tests;
