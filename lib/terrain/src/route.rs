//! The least-cost path across a grid: A\* with integer costs.
//!
//! Integer, because a priority queue ordered by `f64` pops in an order that
//! depends on how the numbers were reached, while integer costs with an index
//! tiebreak pop identically everywhere. The caller prices each step; the
//! heuristic is octile distance times the cheapest straight step, a diagonal
//! counted as three halves of it, so it never overestimates while the caller
//! prices no straight step below that least and no diagonal below three
//! halves of it — and the path found is then genuinely the cheapest.
//!
//! A search settles a bounded number of samples per
//! [`advance`](Router::advance), within the box its two ends span widened by a
//! margin: a route may detour around a hill, and bounding it so keeps its cost
//! proportional to the distance between its ends rather than to the grid.

use alloc::collections::BinaryHeap;
use alloc::vec::Vec;
use core::cmp::Reverse;

use crate::grid::{Grid, NEIGHBOURS};
use crate::{filled, TerrainError};

/// What pricing a step means: the step from a sample to its neighbour, and
/// whether it is diagonal; `None` where no route may step at all.
pub type Pricing<'a> = &'a dyn Fn(usize, usize, bool) -> Option<u32>;

/// Where a search stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Routed {
    /// Still searching.
    Pending,
    /// The least-cost path, from its start to its goal, as sample indices.
    Found(Vec<usize>),
    /// No admissible path lies inside the search's box.
    Unreachable,
}

/// A search under way.
#[derive(Copy, Clone, Debug)]
struct Search {
    grid: Grid,
    start: usize,
    goal: usize,
    /// The box the search keeps to, its corners inclusive.
    low: (u32, u32),
    high: (u32, u32),
    least: u32,
}

/// A\* scratch for one grid, allocated once and reset per search.
#[derive(Debug)]
pub struct Router {
    cost: Vec<u32>,
    came: Vec<u32>,
    settled: Vec<bool>,
    open: BinaryHeap<Reverse<(u32, u32)>>,
    search: Option<Search>,
}

impl Router {
    /// Scratch for a grid of `area` samples.
    pub fn new(area: usize) -> Result<Self, TerrainError> {
        Ok(Self {
            cost: filled(area, u32::MAX)?,
            came: filled(area, u32::MAX)?,
            settled: filled(area, false)?,
            open: BinaryHeap::new(),
            search: None,
        })
    }

    /// Begin a search over `grid` from `start` to `goal`, within the box the
    /// two span widened by `margin` samples each way, no straight step
    /// costing less than `least`.
    pub fn begin(
        &mut self,
        grid: Grid,
        (start, goal): (usize, usize),
        margin: u32,
        least: u32,
    ) -> Result<(), TerrainError> {
        if grid.area() != self.cost.len() || start >= grid.area() || goal >= grid.area() {
            return Err(TerrainError::Shape);
        }
        self.search = None;
        self.open.clear();
        self.open
            .try_reserve(1)
            .map_err(|_| TerrainError::OutOfMemory)?;
        let ((sx, sy), (gx, gy)) = (grid.position(start), grid.position(goal));
        let top = grid.side().saturating_sub(1);
        self.search = Some(Search {
            grid,
            start,
            goal,
            low: (
                sx.min(gx).saturating_sub(margin),
                sy.min(gy).saturating_sub(margin),
            ),
            high: (
                sx.max(gx).saturating_add(margin).min(top),
                sy.max(gy).saturating_add(margin).min(top),
            ),
            least,
        });
        self.cost.fill(u32::MAX);
        self.came.fill(u32::MAX);
        self.settled.fill(false);
        self.cost[start] = 0;
        self.open.push(Reverse((
            heuristic(grid, (start, goal), least),
            narrow(start),
        )));
        Ok(())
    }

    /// Settle up to `budget` more samples, each step priced by `price`.
    pub fn advance(&mut self, budget: usize, price: Pricing<'_>) -> Result<Routed, TerrainError> {
        let Some(search) = self.search else {
            return Ok(Routed::Unreachable);
        };
        let grid = search.grid;
        for _ in 0..budget {
            let Some(Reverse((_, raw))) = self.open.pop() else {
                self.search = None;
                return Ok(Routed::Unreachable);
            };
            let index = raw as usize;
            if self.settled[index] {
                continue;
            }
            self.settled[index] = true;
            if index == search.goal {
                self.search = None;
                return unwind(&self.came, search.start, search.goal).map(Routed::Found);
            }
            // A sample left half-expanded would leave the search wrong, not
            // merely slow, so a refusal ends it.
            if self.open.try_reserve(NEIGHBOURS.len()).is_err() {
                self.search = None;
                return Err(TerrainError::OutOfMemory);
            }
            let (x, y) = grid.position(index);
            for (_, dx, dy, distance) in NEIGHBOURS {
                let Some(next) = grid.neighbour(x, y, dx, dy) else {
                    continue;
                };
                let (nx, ny) = grid.position(next);
                if nx < search.low.0
                    || ny < search.low.1
                    || nx > search.high.0
                    || ny > search.high.1
                {
                    continue;
                }
                let Some(step) = price(index, next, distance > 1.0) else {
                    continue;
                };
                let Some(total) = self.cost[index].checked_add(step) else {
                    continue;
                };
                if total >= self.cost[next] {
                    continue;
                }
                self.cost[next] = total;
                self.came[next] = narrow(index);
                let Some(priority) =
                    total.checked_add(heuristic(grid, (next, search.goal), search.least))
                else {
                    continue;
                };
                self.open.push(Reverse((priority, narrow(next))));
            }
        }
        Ok(Routed::Pending)
    }

    /// The whole search at once: the least-cost path, or `None` when none
    /// lies inside the box.
    pub fn route(
        &mut self,
        grid: Grid,
        ends: (usize, usize),
        (margin, least): (u32, u32),
        price: Pricing<'_>,
    ) -> Result<Option<Vec<usize>>, TerrainError> {
        self.begin(grid, ends, margin, least)?;
        match self.advance(usize::MAX, price)? {
            Routed::Found(path) => Ok(Some(path)),
            Routed::Pending | Routed::Unreachable => Ok(None),
        }
    }
}

/// Octile distance from `from` to `goal` times the cheapest straight step,
/// each diagonal counted as three halves of it: admissible, so A\* returns a
/// genuinely least-cost path.
fn heuristic(grid: Grid, (from, goal): (usize, usize), least: u32) -> u32 {
    let ((fx, fy), (gx, gy)) = (grid.position(from), grid.position(goal));
    let (dx, dy) = (fx.abs_diff(gx), fy.abs_diff(gy));
    let (long, short) = if dx > dy { (dx, dy) } else { (dy, dx) };
    let scaled =
        u64::from(long - short) * u64::from(least) + u64::from(short) * u64::from(least) * 3 / 2;
    u32::try_from(scaled).unwrap_or(u32::MAX)
}

/// The path back along the predecessor chain from `goal` to `start`, in
/// order from `start`.
fn unwind(came: &[u32], start: usize, goal: usize) -> Result<Vec<usize>, TerrainError> {
    let mut length = 1;
    let mut here = goal;
    while here != start {
        match came.get(here) {
            Some(&previous) if previous != u32::MAX => {
                here = previous as usize;
                length += 1;
            }
            _ => break,
        }
    }
    let mut path = Vec::new();
    path.try_reserve_exact(length)
        .map_err(|_| TerrainError::OutOfMemory)?;
    path.resize(length, 0);
    let mut here = goal;
    for slot in path.iter_mut().rev() {
        *slot = here;
        match came.get(here) {
            Some(&previous) if previous != u32::MAX && here != start => here = previous as usize,
            _ => break,
        }
    }
    Ok(path)
}

/// A grid index narrowed to the `u32` the search arrays hold.
fn narrow(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[path = "route_tests.rs"]
mod tests;
