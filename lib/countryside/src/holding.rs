//! The holdings a land is farmed in: the cells of a jittered triangular
//! lattice, each the land about one of its vertices out to the middles of
//! the six triangles about it. Every corner is the middle of three vertices
//! summed in whole millimetres, so any holding works out the corners it
//! shares with its neighbours to the bit, and three holdings meet at each.

use core::hash::Hasher;

use alloc::vec::Vec;

use tairix_util::mathf;

use crate::key::{Key, Stage};
use crate::plane::{Convex, Point, Rect};

/// Where a holding lies on its lattice.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct HoldingId {
    /// Its vertex along the lattice's first axis.
    pub i: i32,
    /// Along its second, sixty degrees anticlockwise of the first.
    pub j: i32,
}

impl HoldingId {
    /// The holding at vertex `(i, j)`.
    #[must_use]
    pub const fn new(i: i32, j: i32) -> Self {
        Self { i, j }
    }

    /// Its place as a key's place.
    pub(crate) fn place(self) -> (i64, i64) {
        (i64::from(self.i), i64::from(self.j))
    }

    /// Write what names it into `hasher`.
    pub(crate) fn write(self, hasher: &mut impl Hasher) {
        hasher.write_i32(self.i);
        hasher.write_i32(self.j);
    }
}

/// The six neighbours of a vertex, anticlockwise from the first axis.
pub(crate) const AROUND: [(i32, i32); 6] = [(1, 0), (0, 1), (-1, 1), (-1, 0), (0, -1), (1, -1)];

/// How far a vertex wanders from its place on the lattice, as a share of its
/// spacing: little enough that every holding stays convex.
const JITTER: f64 = 0.16;

/// The height of one row of the lattice over its spacing.
const ROW: f64 = 0.866_025_403_784_438_6;

/// The lattice the holdings lie on.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Lattice {
    spacing: f64,
    key: Key,
}

impl Lattice {
    /// A lattice of vertices `spacing` metres apart, jittered under `key`.
    pub(crate) const fn new(spacing: f64, key: Key) -> Self {
        Self { spacing, key }
    }

    /// The spacing of its vertices.
    pub(crate) const fn spacing(&self) -> f64 {
        self.spacing
    }

    /// Vertex `(i, j)` in whole millimetres: its place on the lattice,
    /// wandered a drawn way.
    fn vertex_mm(&self, (i, j): (i32, i32)) -> (i64, i64) {
        let mut draws = self.key.draws(Stage::Holding, (i64::from(i), i64::from(j)));
        let (angle, reach) = (draws.range(0.0, core::f64::consts::TAU), JITTER * mathf::sqrt(draws.unit()));
        let x = (f64::from(i) + 0.5 * f64::from(j) + reach * mathf::cos(angle)) * self.spacing;
        let y = (f64::from(j) * ROW + reach * mathf::sin(angle)) * self.spacing;
        let mm = |metres: f64| i64::from(mathf::round_i32((metres * 1000.0).clamp(-2.0e9, 2.0e9)));
        (mm(x), mm(y))
    }

    /// The vertex of `holding`, in metres.
    pub(crate) fn vertex(&self, holding: HoldingId) -> Point {
        let (x, y) = self.vertex_mm((holding.i, holding.j));
        Point::new(millimetres(x), millimetres(y))
    }

    /// The outline of `holding`: the middles of the six triangles about its
    /// vertex, anticlockwise.
    pub(crate) fn outline(&self, holding: HoldingId) -> Convex {
        let (i, j) = (holding.i, holding.j);
        let centre = self.vertex_mm((i, j));
        let around = AROUND.map(|(di, dj)| self.vertex_mm((i + di, j + dj)));
        let corners = (0..6)
            .map(|k| {
                let (a, b) = (around[k], around[(k + 1) % 6]);
                let sum = (centre.0 + a.0 + b.0, centre.1 + a.1 + b.1);
                Point::new(millimetres(sum.0) / 3.0, millimetres(sum.1) / 3.0)
            })
            .collect();
        Convex { corners }
    }

    /// The lattice vertex nearest `at`, as the lattice lies before it wanders.
    fn nearest_vertex(&self, at: Point) -> (i32, i32) {
        let j = at.y / (ROW * self.spacing);
        let i = at.x / self.spacing - 0.5 * j;
        let whole = |value: f64| mathf::round_i32(mathf::round(value));
        (whole(i), whole(j))
    }

    /// The holding `at` lies in: of those whose vertices lie about the
    /// nearest, the first in their order to hold it, `known` telling of each
    /// holding asked about whether its outline holds `at` and where its
    /// vertex lies, for a caller that holds the outlines already.
    pub(crate) fn locate(&self, at: Point, known: &dyn Fn(HoldingId) -> (bool, Point)) -> HoldingId {
        let (i, j) = self.nearest_vertex(at);
        let mut nearest = (f64::INFINITY, HoldingId::new(i, j));
        for dj in -1..=1 {
            for di in -1..=1 {
                let holding = HoldingId::new(i + di, j + dj);
                let (holds, vertex) = known(holding);
                if holds {
                    return holding;
                }
                let apart = (vertex - at).length();
                if apart < nearest.0 {
                    nearest = (apart, holding);
                }
            }
        }
        // Rounding can leave a point on an edge outside both outlines.
        nearest.1
    }

    /// Every holding any of `rect` lies in, in their order; `None` where the
    /// heap will not hold them.
    pub(crate) fn holdings_over(&self, rect: Rect) -> Option<Vec<HoldingId>> {
        let (low, high) = (
            self.nearest_vertex(rect.low),
            self.nearest_vertex(rect.high),
        );
        let corners = [
            low,
            high,
            self.nearest_vertex(Point::new(rect.low.x, rect.high.y)),
            self.nearest_vertex(Point::new(rect.high.x, rect.low.y)),
        ];
        let (i0, i1) = (
            corners.iter().map(|c| c.0).min().unwrap_or(0) - 2,
            corners.iter().map(|c| c.0).max().unwrap_or(0) + 2,
        );
        let (j0, j1) = (
            corners.iter().map(|c| c.1).min().unwrap_or(0) - 2,
            corners.iter().map(|c| c.1).max().unwrap_or(0) + 2,
        );
        let mut holdings = Vec::new();
        for j in j0..=j1 {
            for i in i0..=i1 {
                let holding = HoldingId::new(i, j);
                if self
                    .outline(holding)
                    .bounds()
                    .is_some_and(|bounds| bounds.overlaps(rect))
                {
                    holdings.try_reserve(1).ok()?;
                    holdings.push(holding);
                }
            }
        }
        Some(holdings)
    }
}

/// `value` millimetres in metres.
#[allow(
    clippy::cast_precision_loss,
    reason = "a place on a land, in millimetres, lies far within an f64's whole numbers"
)]
fn millimetres(value: i64) -> f64 {
    value as f64 / 1000.0
}

#[cfg(test)]
#[path = "holding_tests.rs"]
mod tests;
