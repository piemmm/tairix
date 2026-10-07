//! The ways between the countryside's places, ranked by what each way joins:
//! roads and lanes over a relative-neighbourhood graph of its settlements, so
//! the network loops where its places do rather than branching as a tree;
//! footpaths along the Gabriel graph's edges the lanes leave out; and tracks
//! from each holding out to its fields' gateways.

use core::hash::Hasher;

use alloc::vec::Vec;

use crate::holding::HoldingId;
use crate::plane::Point;
use crate::site::{Settled, Settlement};

/// What a way is, by what it joins: the rank it is routed and drawn by.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Rank {
    /// A highway the consumer brings: between the towns it knows of.
    Highway,
    /// A road between villages, or from a village to a highway.
    Road,
    /// A lane between farmsteads, and from them to a village, a road or a
    /// highway.
    Lane,
    /// A track from a farmstead to the gateways of its holding's fields.
    Track,
    /// A footpath across the fields between neighbouring places.
    Path,
}

impl Rank {
    /// Every rank, the greatest first: the order ways are routed in, each
    /// rank drawn to the greater ways it may follow.
    pub const ALL: [Self; 5] = [
        Self::Highway,
        Self::Road,
        Self::Lane,
        Self::Track,
        Self::Path,
    ];

    /// Whether a way of this rank runs between boundaries of its own, which
    /// the land beside it is cut to: every rank but a path, which crosses
    /// the fields.
    #[must_use]
    pub const fn bounded(self) -> bool {
        !matches!(self, Self::Path)
    }
}

/// Which way a way is: what it joins, at what rank, its ends in their order.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct WayId {
    /// Its rank.
    pub rank: Rank,
    /// The highway the consumer numbered, or the ends it joins, the lesser
    /// first.
    pub joins: Joins,
}

/// What a way joins.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Joins {
    /// A highway, as its consumer numbered it.
    Highway(u32),
    /// Two places.
    Ends(Placed, Placed),
}

/// A place a way may end at, by what it is rather than where it lies.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Placed {
    /// A settlement.
    Settled(Settled),
    /// A holding's gateway `n`: from one, where its fields' tracks run out
    /// to; nought, where the tracks of a holding whose farm a village
    /// gathers leave the greater way nearest it.
    Gateway(HoldingId, u8),
    /// The point of highway `n` nearest a place, in centimetres along it.
    OnHighway(u32, i64),
    /// The point of a road nearest a place: the road's two ends, and how
    /// far along it, in centimetres.
    OnRoad(Settled, Settled, i64),
}

impl WayId {
    /// Write what names it into `hasher`.
    pub(crate) fn write(self, hasher: &mut impl Hasher) {
        hasher.write_u8(self.rank as u8);
        match self.joins {
            Joins::Highway(number) => {
                hasher.write_u8(0);
                hasher.write_u32(number);
            }
            Joins::Ends(a, b) => {
                hasher.write_u8(1);
                a.write(hasher);
                b.write(hasher);
            }
        }
    }
}

impl Placed {
    /// Write what names it into `hasher`.
    pub(crate) fn write(self, hasher: &mut impl Hasher) {
        match self {
            Self::Settled(settled) => {
                hasher.write_u8(0);
                settled.write(hasher);
            }
            Self::Gateway(holding, index) => {
                hasher.write_u8(1);
                hasher.write_i32(holding.i);
                hasher.write_i32(holding.j);
                hasher.write_u8(index);
            }
            Self::OnHighway(number, along) => {
                hasher.write_u8(2);
                hasher.write_u32(number);
                hasher.write_i64(along);
            }
            Self::OnRoad(a, b, along) => {
                hasher.write_u8(3);
                a.write(hasher);
                b.write(hasher);
                hasher.write_i64(along);
            }
        }
    }
}

/// A node of the graph: what it is and where it stands.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Node {
    pub(crate) placed: Placed,
    pub(crate) at: Point,
}

impl Node {
    pub(crate) const fn of(settlement: &Settlement) -> Self {
        Self {
            placed: Placed::Settled(settlement.settled),
            at: settlement.at,
        }
    }
}

/// Places filed by the square of a grid each stands in, so the places near
/// another are found among the squares about it alone.
#[derive(Debug)]
pub(crate) struct Filed {
    cell: f64,
    /// Each place's square's row and column, then its index, in that order.
    order: Vec<(i64, i64, usize)>,
}

impl Filed {
    /// `places` filed in squares `cell` across; `None` where the heap will
    /// not hold them.
    pub(crate) fn new(places: impl ExactSizeIterator<Item = Point>, cell: f64) -> Option<Self> {
        let mut order = Vec::new();
        order.try_reserve_exact(places.len()).ok()?;
        order.extend(places.enumerate().map(|(index, at)| {
            let (column, row) = square(at, cell);
            (row, column, index)
        }));
        order.sort_unstable();
        Some(Self { cell, order })
    }

    /// Every place within the squares `reach` reaches about `at`, by index,
    /// in no particular order: a superset of those within `reach` of it.
    pub(crate) fn near(&self, at: Point, reach: f64) -> impl Iterator<Item = usize> + '_ {
        let span = i64::from(tairix_util::mathf::round_i32(tairix_util::mathf::ceil(
            reach / self.cell,
        )));
        let (column, row) = square(at, self.cell);
        (row - span..=row + span).flat_map(move |within| {
            let from = self
                .order
                .partition_point(|&(r, c, _)| (r, c) < (within, column - span));
            self.order[from..]
                .iter()
                .take_while(move |&&(r, c, _)| (r, c) <= (within, column + span))
                .map(|&(_, _, index)| index)
        })
    }
}

/// The column and row of the square of a grid `cell` across that `at`
/// stands in.
fn square(at: Point, cell: f64) -> (i64, i64) {
    let whole = |value: f64| {
        i64::from(tairix_util::mathf::round_i32(tairix_util::mathf::floor(
            value / cell,
        )))
    };
    (whole(at.x), whole(at.y))
}

/// The squared distance between `a` and `b`.
fn squared(a: Point, b: Point) -> f64 {
    (a - b).dot(a - b)
}

/// Which graph a network's nodes are joined by.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Graph {
    /// The relative-neighbourhood graph: two nodes are joined where no third
    /// lies nearer both of them than they lie to each other.
    Neighbourhood,
    /// The Gabriel graph's edges the relative-neighbourhood graph lacks: two
    /// nodes are joined where no third lies within the circle the two span,
    /// though one lies in their lune. Footpaths take these, the shortcuts the
    /// lanes leave out.
    GabrielOnly,
}

/// The edges of `graph` over `nodes`, filed in `filed`, from node `a` to each
/// later node within `longest` of it that `keeps`, by index, in their order.
/// Ties go to the nodes' order, so the graph is the same whichever of its
/// nodes are asked about first, and in whatever order. `None` where the heap
/// will not hold them.
pub(crate) fn edges_from(
    graph: Graph,
    (nodes, filed): (&[Node], &Filed),
    (a, longest): (usize, f64),
    keeps: &dyn Fn(&Node, &Node) -> bool,
) -> Option<Vec<usize>> {
    let here = nodes.get(a)?;
    let mut ends = Vec::new();
    for b in filed.near(here.at, longest) {
        let Some(there) = nodes.get(b) else {
            continue;
        };
        let span = squared(here.at, there.at);
        if b <= a || span > longest * longest || !keeps(here, there) {
            continue;
        }
        // Every node that could part the pair lies within its span of `a`.
        let thirds = || {
            filed
                .near(here.at, tairix_util::mathf::sqrt(span))
                .filter(|&c| c != a && c != b)
                .filter_map(|c| Some((c, nodes.get(c)?.at)))
        };
        // Of three equidistant nodes, the edge between the later two yields.
        let in_lune = |(c, at): (usize, Point)| {
            let far = squared(at, here.at).max(squared(at, there.at));
            far < span || (far <= span && c < a)
        };
        let middle = here.at.lerp(there.at, 0.5);
        let in_circle = |(_, at): (usize, Point)| squared(at, middle) < 0.25 * span;
        let joined = match graph {
            Graph::Neighbourhood => !thirds().any(in_lune),
            Graph::GabrielOnly => thirds().any(in_lune) && !thirds().any(in_circle),
        };
        if joined {
            ends.try_reserve(1).ok()?;
            ends.push(b);
        }
    }
    ends.sort_unstable();
    Some(ends)
}

/// The two ends of a way between `a` and `b`, the lesser first, as its
/// identity holds them.
pub(crate) fn ordered(a: Placed, b: Placed) -> Joins {
    if a <= b {
        Joins::Ends(a, b)
    } else {
        Joins::Ends(b, a)
    }
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;
