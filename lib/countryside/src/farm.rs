//! A farmstead's layout: a yard on its site, squared to the contour or to
//! the way that serves it, its house on the side it faces out from, its
//! barn across the yard and its other ranges about it as its plan has them,
//! and a garden before the house.

use alloc::vec::Vec;

use tairix_util::mathf;

use crate::ground::{self, Ground};
use crate::key::{Key, Stage};
use crate::plane::{Convex, Point};
use crate::site::{Settled, Settlement};
use crate::Error;

/// What a building is among its farmstead's or plot's: what the consumer
/// makes of it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Standing {
    /// Where its people live.
    Dwelling,
    /// A great range about the yard: a barn.
    Range,
    /// A lesser range: a byre, a stable.
    Lesser,
    /// A shed: a cart shed, a store.
    Shed,
}

/// Where a building stands: its middle, the unit way its ridge runs, how
/// long it runs that way and how deep across it, and what it is.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Footprint {
    /// Its middle.
    pub middle: Point,
    /// The unit way its ridge runs.
    pub along: Point,
    /// How long it runs along its ridge.
    pub length: f64,
    /// How deep it stands across it.
    pub depth: f64,
    /// The unit way its front faces, across its ridge: to its garden, its
    /// yard or its street.
    pub front: Point,
    /// What it is.
    pub standing: Standing,
}

impl Footprint {
    /// Its outline, anticlockwise.
    #[must_use]
    pub fn outline(&self) -> Convex {
        rectangle(self.middle, self.along, (self.length, self.depth))
    }
}

/// How a farmstead's buildings stand about its yard.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Plan {
    /// On three sides, the yard opening on the fourth.
    Courtyard,
    /// On two sides.
    Ell,
    /// Standing apart about it.
    Loose,
}

/// A farmstead laid out.
#[derive(Clone, Debug, PartialEq)]
pub struct Farmstead {
    /// The settlement it is.
    pub settled: Settled,
    /// Where it stands: its yard's middle.
    pub at: Point,
    /// Its yard.
    pub yard: Convex,
    /// Its garden, before its house.
    pub garden: Convex,
    /// Its buildings, its house first.
    pub buildings: Vec<Footprint>,
    /// How they stand about the yard.
    pub plan: Plan,
    /// The middle of the yard's edge it opens on to the way that serves it.
    pub gate: Point,
    /// The whole of its plot: its yard, its buildings and its garden.
    pub plot: Convex,
}

/// The rectangle `length` along the unit `along` and `depth` across it,
/// about `middle`, anticlockwise.
pub(crate) fn rectangle(middle: Point, along: Point, (length, depth): (f64, f64)) -> Convex {
    let (a, b) = (along * (0.5 * length), along.left() * (0.5 * depth));
    Convex {
        corners: alloc::vec![middle - a - b, middle + a - b, middle + a + b, middle - a + b],
    }
}

/// The least convex polygon holding every point of `points`, anticlockwise
/// (Andrew's monotone chain, 1979); `None` where the heap will not hold it.
pub(crate) fn hull(points: &[Point]) -> Option<Convex> {
    let mut sorted = Vec::new();
    sorted.try_reserve_exact(points.len()).ok()?;
    sorted.extend_from_slice(points);
    sorted.sort_unstable_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    sorted.dedup();
    if sorted.len() < 3 {
        return Some(Convex { corners: sorted });
    }
    let mut chain: Vec<Point> = Vec::new();
    chain.try_reserve_exact(2 * sorted.len()).ok()?;
    let turn = |chain: &Vec<Point>, next: Point| {
        let n = chain.len();
        (chain[n - 1] - chain[n - 2]).cross(next - chain[n - 2])
    };
    for &point in &sorted {
        while chain.len() >= 2 && turn(&chain, point) <= 0.0 {
            chain.pop();
        }
        chain.push(point);
    }
    let lower = chain.len() + 1;
    for &point in sorted.iter().rev().skip(1) {
        while chain.len() >= lower && turn(&chain, point) <= 0.0 {
            chain.pop();
        }
        chain.push(point);
    }
    chain.pop();
    Some(Convex { corners: chain })
}

/// The layout of `farm`, its way out toward `toward`, a unit way, over
/// `ground`.
pub(crate) fn lay_out(
    key: Key,
    farm: &Settlement,
    toward: Point,
    ground: &dyn Ground,
) -> Result<Farmstead, Error> {
    let mut draws = key.draws(Stage::Yard, farm.settled.place());
    let (slope, rise) = ground::slope(ground, farm.at, 10.0);
    // Its ranges stand along the contour where the ground falls, else square
    // to its way out; and never quite true to either.
    let base = if slope > 0.02 { rise.left() } else { toward.left() };
    let lean = draws.range(-0.18, 0.18);
    let along = turned(base, lean);
    let across = along.left();
    // The yard opens toward its way: the side of it facing the way's side.
    let front = if across.dot(toward) >= 0.0 { across } else { -across };
    let (width, depth) = (draws.range(24.0, 40.0), draws.range(18.0, 30.0));
    let middle = farm.at;
    let yard = rectangle(middle, along, (width, depth));
    let plan = match draws.below(20) {
        0..=7 => Plan::Courtyard,
        8..=14 => Plan::Ell,
        _ => Plan::Loose,
    };
    let gap = draws.range(2.0, 5.0);
    // The house stands toward one end of the yard's front, its own front to
    // its garden; the yard opens to its way at the front's other end.
    let end = if draws.chance(0.5) { 1.0 } else { -1.0 };
    let house = Footprint {
        middle: middle + front * (0.5 * depth + 3.5 + gap) + along * (end * draws.range(0.2, 0.32) * width),
        along,
        length: draws.range(9.0, 14.0),
        depth: draws.range(6.0, 8.5),
        front,
        standing: Standing::Dwelling,
    };
    let mut buildings = Vec::new();
    buildings.try_reserve_exact(4).map_err(|_| Error::OutOfMemory)?;
    buildings.push(house);
    let barn_depth = draws.range(7.0, 10.0);
    let barn_lean = if plan == Plan::Loose { draws.range(-0.3, 0.3) } else { 0.0 };
    let barn_length = draws.range(16.0, 26.0).min(width + 4.0);
    // A barn standing askew swings its corners toward the yard: it stands
    // back by as much.
    let swing = 0.5 * barn_length * mathf::sin(barn_lean).abs();
    buildings.push(Footprint {
        middle: middle - front * (0.5 * depth + 0.5 * barn_depth + gap + swing)
            + along * draws.range(-0.15, 0.15) * width,
        along: turned(along, barn_lean),
        length: barn_length,
        depth: barn_depth,
        front: turned(front, barn_lean),
        standing: Standing::Range,
    });
    let side = if draws.chance(0.5) { along } else { -along };
    if plan != Plan::Loose {
        let byre_depth = draws.range(5.5, 7.0);
        buildings.push(Footprint {
            middle: middle + side * (0.5 * width + 0.5 * byre_depth + gap),
            along: across,
            length: draws.range(10.0, 16.0).min(depth + 2.0),
            depth: byre_depth,
            front: -side,
            standing: Standing::Lesser,
        });
    }
    if plan == Plan::Courtyard {
        let shed_depth = draws.range(5.0, 6.0);
        buildings.push(Footprint {
            middle: middle - side * (0.5 * width + 0.5 * shed_depth + gap),
            along: across,
            length: draws.range(8.0, 12.0).min(depth),
            depth: shed_depth,
            front: side,
            standing: Standing::Shed,
        });
    } else if plan == Plan::Loose {
        buildings.push(Footprint {
            middle: middle + side * (0.5 * width + draws.range(6.0, 14.0))
                - front * draws.range(0.0, 0.4) * depth,
            along: turned(across, draws.range(-0.4, 0.4)),
            length: draws.range(8.0, 12.0),
            depth: draws.range(5.0, 6.5),
            front: -side,
            standing: Standing::Shed,
        });
    }
    let garden_depth = draws.range(10.0, 18.0);
    let garden = rectangle(
        house.middle + front * (0.5 * house.depth + 0.5 * garden_depth + 1.0),
        along,
        (house.length + draws.range(4.0, 12.0), garden_depth),
    );
    let mut corners = Vec::new();
    corners
        .try_reserve(4 * (buildings.len() + 2))
        .map_err(|_| Error::OutOfMemory)?;
    corners.extend_from_slice(&yard.corners);
    corners.extend_from_slice(&garden.corners);
    for building in &buildings {
        corners.extend_from_slice(&building.outline().corners);
    }
    let plot = hull(&corners).ok_or(Error::OutOfMemory)?;
    Ok(Farmstead {
        settled: farm.settled,
        at: farm.at,
        gate: middle + front * (0.5 * depth) - along * (end * 0.3 * width),
        yard,
        garden,
        buildings,
        plan,
        plot,
    })
}

/// The unit way `way` turned `angle` radians anticlockwise.
fn turned(way: Point, angle: f64) -> Point {
    let (cos, sin) = (mathf::cos(angle), mathf::sin(angle));
    Point::new(way.x * cos - way.y * sin, way.x * sin + way.y * cos)
}

#[cfg(test)]
#[path = "farm_tests.rs"]
mod tests;
