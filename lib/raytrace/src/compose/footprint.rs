//! What stands on a stage's ground, as circles a new piece keeps clear of.
//!
//! A still life holds a handful, and is asked of them all; a land's woods
//! hold thousands, so once a land lays its extent the circles are indexed
//! over a grid of it and a question looks only at its neighbours.
//!
//! A circle is taken either by a piece standing there or as ground the
//! composition keeps open of pieces — the eye's own, a pond — where what
//! grows wild may still stand.

use alloc::vec::Vec;

use super::chains::Chains;
use crate::vector::real;

/// The clearance two pieces' circles keep between them.
pub(super) const GAP: f64 = 0.08;

/// The most cells a side of the grid holds, and the least a cell spans:
/// room for a stage-sized square at a tree's spacing.
const MOST_SIDE: usize = 512;
const LEAST_CELL: f64 = 4.0;

/// Circles on the ground, each taken by a piece or kept open.
#[derive(Debug, Default)]
pub(super) struct Footprints {
    circles: Vec<Circle>,
    index: Option<Index>,
}

/// What takes a circle of the ground.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Taken {
    /// A piece stands there: a trunk, a stone, a wall.
    Piece,
    /// The composition keeps it open of pieces.
    Open,
}

#[derive(Copy, Clone, Debug)]
struct Circle {
    at: (f64, f64),
    radius: f64,
    taken: Taken,
}

/// The circles chained into the cells of a grid over the ground, each into
/// the cells of its square widened by half the gap: two circles too close
/// together have squares that overlap, so they share a cell. Those reaching
/// past the grid are kept apart and asked of by every question.
#[derive(Debug)]
struct Index {
    chains: Chains,
    beyond: Vec<u32>,
}

impl Footprints {
    /// Whether a piece `radius` across at `at` keeps clear of every circle;
    /// a negative radius takes no room of its own.
    pub(super) fn clear(&self, at: (f64, f64), radius: f64) -> bool {
        self.clear_of(at, radius, |_| true)
    }

    /// Whether what grows `radius` across at `at` keeps clear of every
    /// piece, open ground being no bar to it.
    pub(super) fn clear_of_pieces(&self, at: (f64, f64), radius: f64) -> bool {
        self.clear_of(at, radius, |taken| taken == Taken::Piece)
    }

    fn clear_of(&self, at: (f64, f64), radius: f64, bars: impl Fn(Taken) -> bool) -> bool {
        let radius = radius.max(0.0);
        let apart = |id: u32| {
            self.circles.get(id as usize).is_none_or(|circle| {
                let (dx, dz) = (at.0 - circle.at.0, at.1 - circle.at.1);
                let least = radius + circle.radius + GAP;
                !bars(circle.taken) || dx * dx + dz * dz > least * least
            })
        };
        let Some(index) = &self.index else {
            return (0..self.circles.len()).all(|id| u32::try_from(id).is_ok_and(apart));
        };
        let (cells, _) = index.chains.span(at, radius + 0.5 * GAP);
        index.beyond.iter().all(|&id| apart(id)) && index.chains.within(cells).all(apart)
    }

    /// Take a circle `radius` across at `at`, no room for a negative one;
    /// `None` when the heap will not hold it.
    pub(super) fn claim(&mut self, at: (f64, f64), radius: f64, taken: Taken) -> Option<()> {
        let radius = radius.max(0.0);
        let id = u32::try_from(self.circles.len()).ok()?;
        self.circles.try_reserve(1).ok()?;
        self.circles.push(Circle { at, radius, taken });
        match &mut self.index {
            Some(index) => index.link(id, at, radius),
            None => Some(()),
        }
    }

    /// Index the circles over the square `reach` either way of `centre`, the
    /// ground a land lays; `None` when the heap will not hold the grid.
    pub(super) fn index(&mut self, centre: (f64, f64), reach: f64) -> Option<()> {
        let cell = (2.0 * reach.max(0.0) / real(MOST_SIDE)).max(LEAST_CELL);
        let mut index = Index {
            chains: Chains::new((centre, reach), cell, MOST_SIDE)?,
            beyond: Vec::new(),
        };
        for (id, circle) in self.circles.iter().enumerate() {
            index.link(u32::try_from(id).ok()?, circle.at, circle.radius)?;
        }
        self.index = Some(index);
        Some(())
    }
}

impl Index {
    /// Chain circle `id`, `radius` across at `at`, into every cell it
    /// reaches, or set it beyond the grid.
    fn link(&mut self, id: u32, at: (f64, f64), radius: f64) -> Option<()> {
        let (cells, past) = self.chains.span(at, radius + 0.5 * GAP);
        if past {
            self.beyond.try_reserve(1).ok()?;
            self.beyond.push(id);
            return Some(());
        }
        self.chains.link(id, cells)
    }
}

#[cfg(test)]
#[path = "footprint_tests.rs"]
mod tests;
