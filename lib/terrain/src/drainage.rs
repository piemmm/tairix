//! Drainage: which way water leaves every sample of a height grid, the
//! surface the lakes it pools in lie at, and how much of the grid drains
//! through each sample.
//!
//! Flow accumulation is only meaningful if every sample is visited after
//! everything that drains through it, and that order exists only if the
//! drainage is acyclic, which a raw height grid's pits and flats are not.
//! Priority-Flood (Barnes, Lehman and Mulla, "Priority-Flood: An Optimal
//! Depression-Filling and Watershed-Labeling Algorithm for Digital Elevation
//! Models", 2014) gives that order for free: flooding inward from the outlets
//! pops samples in non-decreasing filled height, so the pop order is itself
//! downstream first. One flood yields the depression-filled surface (a lake
//! at its outflow's height), an acyclic routing (each sample drains to its
//! steepest neighbour among those popped earlier) and the order accumulation
//! walks in reverse — no epsilon, no iteration, no flats to special-case.

use alloc::collections::BinaryHeap;
use alloc::vec::Vec;
use core::cmp::Reverse;
use core::ops::Range;

use tairix_util::mathf;

use crate::grid::{FlowDir, Grid, NEIGHBOURS};
use crate::{filled, fits, TerrainError};

/// Steps a height is quantised into for the flood's ordering key: a quarter
/// of a millimetre where heights are metres. The key orders the heap and
/// nothing else; the filled surface itself keeps full precision.
const ORDER_STEPS: f64 = 4096.0;

/// The heap ordering key of `height`: quantised so it is totally ordered, and
/// saturating so an extreme height cannot wrap past a modest one.
fn order_key(height: f64) -> i64 {
    let scaled = mathf::clamp(height * ORDER_STEPS, -9.0e15, 9.0e15);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "clamped far inside i64 and rounded to an integer, so the conversion is exact"
    )]
    {
        mathf::round(scaled) as i64
    }
}

/// A grid index narrowed to the `u32` the order and rank hold: a grid's area
/// is a `u32` side squared, far below where the narrowing would saturate for
/// any grid a heap holds.
fn narrow(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

/// A Priority-Flood of one height grid, reaching a bounded number of samples
/// per [`advance`](Self::advance).
#[derive(Debug)]
pub struct Flood {
    grid: Grid,
    filled: Vec<f64>,
    /// Sample indices in the order they were reached: downstream first.
    order: Vec<u32>,
    /// Each sample's position in `order`; `u32::MAX` until it is reached.
    rank: Vec<u32>,
    queued: Vec<bool>,
    heap: BinaryHeap<Reverse<(i64, u32)>>,
}

impl Flood {
    /// A flood of `height` over `grid` from every sample `outlet` names:
    /// those drain out of the grid, so the flood starts from them and never
    /// raises them.
    pub fn new(
        height: &[f64],
        grid: Grid,
        outlet: impl Fn(usize) -> bool,
    ) -> Result<Self, TerrainError> {
        fits(height, grid)?;
        let area = grid.area();
        let mut flood = Self {
            grid,
            filled: filled(area, 0.0_f64)?,
            order: Vec::new(),
            rank: filled(area, u32::MAX)?,
            queued: filled(area, false)?,
            heap: BinaryHeap::new(),
        };
        flood
            .order
            .try_reserve_exact(area)
            .map_err(|_| TerrainError::OutOfMemory)?;
        flood
            .heap
            .try_reserve(area)
            .map_err(|_| TerrainError::OutOfMemory)?;
        for (index, &ground) in height.iter().enumerate() {
            if outlet(index) {
                flood.filled[index] = ground;
                flood.queued[index] = true;
                flood.heap.push(Reverse((order_key(ground), narrow(index))));
            }
        }
        Ok(flood)
    }

    /// Reach up to `budget` more samples of `height`, the grid the flood was
    /// begun over; whether every sample is now reached.
    pub fn advance(&mut self, height: &[f64], budget: usize) -> bool {
        if height.len() != self.grid.area() {
            return self.is_done();
        }
        for _ in 0..budget {
            let Some(Reverse((_, raw))) = self.heap.pop() else {
                break;
            };
            let index = raw as usize;
            self.rank[index] = narrow(self.order.len());
            self.order.push(raw);
            let (x, y) = self.grid.position(index);
            for (_, dx, dy, _) in NEIGHBOURS {
                let Some(next) = self.grid.neighbour(x, y, dx, dy) else {
                    continue;
                };
                if self.queued[next] {
                    continue;
                }
                // A neighbour cannot end up below the sample it was reached
                // from: that is the fill, and it turns a pit into a lake at its
                // outflow's height.
                self.filled[next] = mathf::fmax(height[next], self.filled[index]);
                self.queued[next] = true;
                self.heap
                    .push(Reverse((order_key(self.filled[next]), narrow(next))));
            }
        }
        self.is_done()
    }

    /// Whether every sample the outlets can reach has been.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.heap.is_empty()
    }

    /// The depression-filled surface, final once the flood is done.
    #[must_use]
    pub fn filled(&self) -> &[f64] {
        &self.filled
    }

    /// Sample indices in the order they were reached.
    #[must_use]
    pub fn order(&self) -> &[u32] {
        &self.order
    }

    /// Each sample's position in [`order`](Self::order).
    #[must_use]
    pub fn rank(&self) -> &[u32] {
        &self.rank
    }

    /// The filled surface, the order and the ranks.
    #[must_use]
    pub fn into_parts(self) -> (Vec<f64>, Vec<u32>, Vec<u32>) {
        (self.filled, self.order, self.rank)
    }
}

/// Route the samples of rows `rows` of `grid` to their steepest neighbour
/// among those a flood reached earlier, writing each direction into `flow`;
/// a sample `outlet` names — one the flood started from — drains out of the
/// grid instead.
///
/// Restricting the choice to earlier-reached neighbours is what makes the
/// routing acyclic: a cycle would need a step to an equal or later rank, and
/// there is none.
pub fn route(
    (filled, rank): (&[f64], &[u32]),
    grid: Grid,
    outlet: impl Fn(usize) -> bool,
    flow: &mut [FlowDir],
    rows: Range<u32>,
) -> Result<(), TerrainError> {
    fits(filled, grid)?;
    fits(rank, grid)?;
    fits(flow, grid)?;
    for y in rows.start..rows.end.min(grid.side()) {
        for x in 0..grid.side() {
            let index = grid.index(x, y);
            flow[index] = FlowDir::Sink;
            if outlet(index) {
                continue;
            }
            let mut steepest = 0.0;
            for (dir, dx, dy, distance) in NEIGHBOURS {
                let Some(next) = grid.neighbour(x, y, dx, dy) else {
                    continue;
                };
                if rank[next] >= rank[index] {
                    continue;
                }
                let slope = (filled[index] - filled[next]) / distance;
                if slope > steepest {
                    steepest = slope;
                    flow[index] = dir;
                }
            }
            if flow[index] == FlowDir::Sink {
                // A flat: every earlier neighbour is level with this sample,
                // so none is steepest. The earliest reached is the way the
                // flood came, and so the way out.
                let mut earliest = rank[index];
                for (dir, dx, dy, _) in NEIGHBOURS {
                    let Some(next) = grid.neighbour(x, y, dx, dy) else {
                        continue;
                    };
                    if rank[next] < earliest {
                        earliest = rank[next];
                        flow[index] = dir;
                    }
                }
            }
        }
    }
    Ok(())
}

/// The sample `index` drains into, if it drains into one.
#[must_use]
pub fn downstream(grid: Grid, flow: &[FlowDir], index: usize) -> Option<usize> {
    let (dx, dy) = flow.get(index)?.offset()?;
    let (x, y) = grid.position(index);
    grid.neighbour(x, y, dx, dy)
}

/// Accumulate `discharge` down the routing for the flood-order positions
/// `span`, walked last first; `discharge` starts as each sample's own share.
///
/// Every sample precedes the one it drains to in the reverse of the flood
/// order, so walking the whole order from its end to its start, one span at a
/// time and each span after the one above it, delivers every sample's total.
pub fn accumulate(
    (order, flow): (&[u32], &[FlowDir]),
    grid: Grid,
    discharge: &mut [u32],
    span: Range<usize>,
) -> Result<(), TerrainError> {
    fits(flow, grid)?;
    fits(discharge, grid)?;
    let Some(positions) = order.get(span) else {
        return Err(TerrainError::Shape);
    };
    for &raw in positions.iter().rev() {
        let index = raw as usize;
        let Some(next) = downstream(grid, flow, index) else {
            continue;
        };
        discharge[next] = discharge[next].saturating_add(discharge[index]);
    }
    Ok(())
}

/// One whole solve of the drainage over a height grid.
#[derive(Debug)]
pub struct Network {
    /// The depression-filled surface.
    pub filled: Vec<f64>,
    /// Each sample's downstream direction.
    pub flow: Vec<FlowDir>,
    /// Samples draining through each sample, itself included.
    pub discharge: Vec<u32>,
    /// Sample indices downstream first.
    pub order: Vec<u32>,
}

impl Network {
    /// Flood `height` over `grid` from the samples `outlet` names, route it
    /// with those draining out, and accumulate it.
    pub fn solve(
        height: &[f64],
        grid: Grid,
        outlet: impl Fn(usize) -> bool,
    ) -> Result<Self, TerrainError> {
        let mut flood = Flood::new(height, grid, &outlet)?;
        flood.advance(height, usize::MAX);
        let (filled_surface, order, rank) = flood.into_parts();
        let mut flow = filled(grid.area(), FlowDir::Sink)?;
        route(
            (&filled_surface, &rank),
            grid,
            &outlet,
            &mut flow,
            0..grid.side(),
        )?;
        let mut discharge = filled(grid.area(), 1_u32)?;
        accumulate((&order, &flow), grid, &mut discharge, 0..order.len())?;
        Ok(Self {
            filled: filled_surface,
            flow,
            discharge,
            order,
        })
    }
}

#[cfg(test)]
#[path = "drainage_tests.rs"]
mod tests;
