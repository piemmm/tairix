//! The ways between the countryside's places: a relative-neighbourhood graph
//! over its settlements and its holdings' gateways, ranked by what each way
//! joins, so it loops where its places do rather than branching as a tree.

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
    /// A lane between farmsteads, and from them to a village or a road.
    Lane,
    /// A track from a farmstead to the gateways of its holding's fields.
    Track,
    /// A footpath across the fields between neighbouring places.
    Path,
}

impl Rank {
    /// Every rank, the greatest first: the order ways are routed in, each
    /// rank drawn to the greater ways it may follow.
    pub const ALL: [Self; 5] = [Self::Highway, Self::Road, Self::Lane, Self::Track, Self::Path];

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
    /// A holding's `n`th gateway.
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
        let span = i64::from(tairix_util::mathf::round_i32(tairix_util::mathf::ceil(reach / self.cell)));
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
    let whole = |value: f64| i64::from(tairix_util::mathf::round_i32(tairix_util::mathf::floor(value / cell)));
    (whole(at.x), whole(at.y))
}

/// The edges of the relative-neighbourhood graph over `nodes` no longer
/// than `longest`, each a pair of indices into `nodes`, the lesser first,
/// that `keeps`: two nodes are joined where no third lies nearer both of
/// them than they lie to each other. Ties go to the nodes' order, so the
/// graph is the same whichever of its nodes are asked about first. `None`
/// where the heap will not hold the edges.
pub(crate) fn neighbourhood(
    nodes: &[Node],
    longest: f64,
    keeps: &dyn Fn(&Node, &Node) -> bool,
) -> Option<Vec<(usize, usize)>> {
    joined(nodes, longest, keeps, &|(a, b), span, third| {
        let far = squared(third.1, nodes[a].at).max(squared(third.1, nodes[b].at));
        // Of three equidistant nodes, the edge between the later two yields.
        far < span || (far <= span && third.0 < a.min(b))
    })
}

/// The edges of the Gabriel graph over `nodes` no longer than `longest`
/// that `keeps` and the relative-neighbourhood graph lacks: two nodes are
/// joined where no third lies within the circle the two span. Footpaths
/// take these, the shortcuts the lanes leave out. `None` where the heap
/// will not hold them.
pub(crate) fn gabriel_only(
    nodes: &[Node],
    longest: f64,
    keeps: &dyn Fn(&Node, &Node) -> bool,
) -> Option<Vec<(usize, usize)>> {
    let lanes = neighbourhood(nodes, longest, keeps)?;
    let mut edges = joined(nodes, longest, keeps, &|(a, b), span, third| {
        let middle = nodes[a].at.lerp(nodes[b].at, 0.5);
        squared(third.1, middle) < 0.25 * span
    })?;
    edges.retain(|edge| lanes.binary_search(edge).is_err());
    Some(edges)
}

/// The squared distance between `a` and `b`.
fn squared(a: Point, b: Point) -> f64 {
    (a - b).dot(a - b)
}

/// The edges between `nodes` no longer than `longest` that `keeps`, and no
/// third node `blocks` — told the pair, their squared span, and the third's
/// index and place — in the pairs' order. `None` where the heap will not
/// hold them.
fn joined(
    nodes: &[Node],
    longest: f64,
    keeps: &dyn Fn(&Node, &Node) -> bool,
    blocks: &dyn Fn((usize, usize), f64, (usize, Point)) -> bool,
) -> Option<Vec<(usize, usize)>> {
    let filed = Filed::new(nodes.iter().map(|node| node.at), longest.max(1e-6))?;
    let mut edges = Vec::new();
    for a in 0..nodes.len() {
        for b in filed.near(nodes[a].at, longest) {
            let span = squared(nodes[a].at, nodes[b].at);
            if b <= a || span > longest * longest || !keeps(&nodes[a], &nodes[b]) {
                continue;
            }
            // Every node that could block the pair lies within its span of
            // either end.
            let reach = tairix_util::mathf::sqrt(span);
            let blocked = filed.near(nodes[a].at, reach).any(|c| {
                c != a && c != b && blocks((a, b), span, (c, nodes[c].at))
            });
            if !blocked {
                edges.try_reserve(1).ok()?;
                edges.push((a, b));
            }
        }
    }
    edges.sort_unstable();
    Some(edges)
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
