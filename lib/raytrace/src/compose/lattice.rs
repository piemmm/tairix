//! A square lattice of cells about the eye, which the bed's stones and the
//! water's edge plants are each drawn over a cell at a time.

use core::ops::Range;

use alloc::vec::Vec;

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use crate::band;
use crate::vector::real;

/// A square lattice of cells about the eye: its first cell's corner, its
/// cells a side and their breadth, and the square a finer lattice reads
/// instead.
#[derive(Copy, Clone, Debug)]
pub(super) struct Lattice {
    pub(super) corner: (f64, f64),
    pub(super) side: usize,
    pub(super) cell: f64,
    pub(super) hole: Option<((f64, f64), (f64, f64))>,
}

/// `value` squared down to the land's own grid of `cell`-broad cells, so a
/// lattice's cells lie on the same places whatever the eye.
pub(super) fn snap(value: f64, cell: f64) -> f64 {
    mathf::floor(value / cell) * cell
}

impl Lattice {
    /// `side` cells a side of `cell` breadth from `corner`.
    pub(super) const fn new(corner: (f64, f64), side: usize, cell: f64) -> Self {
        Self {
            corner,
            side,
            cell,
            hole: None,
        }
    }

    /// The lattice of `cell`-broad cells reaching `reach` about `eye`,
    /// squared to the land's own grid of such cells so each cell lies on the
    /// same place whatever the eye: a cell more than the reach spans, as its
    /// corner lies up to a cell short of it.
    pub(super) fn about(eye: (f64, f64), reach: f64, cell: f64) -> Option<Self> {
        if cell.partial_cmp(&0.0) != Some(core::cmp::Ordering::Greater) {
            return None;
        }
        let snap = |value: f64| snap(value, cell);
        let cells = mathf::round_i32(mathf::ceil(2.0 * reach / cell)).checked_add(1)?;
        let side = usize::try_from(cells).ok()?;
        Some(Self::new(
            (snap(eye.0 - reach), snap(eye.1 - reach)),
            side,
            cell,
        ))
    }

    /// This lattice, but for the square from `low` to `high` a finer lattice
    /// reads instead.
    pub(super) const fn without(self, (low, high): ((f64, f64), (f64, f64))) -> Self {
        Self {
            hole: Some((low, high)),
            ..self
        }
    }

    /// The corner of cell `(column, row)`.
    pub(super) fn corner_of(&self, (column, row): (usize, usize)) -> (f64, f64) {
        (
            self.corner.0 + real(column) * self.cell,
            self.corner.1 + real(row) * self.cell,
        )
    }

    /// The middle of cell `(column, row)`, or `None` where it lies in the
    /// square a finer lattice reads instead.
    pub(super) fn middle(&self, cell: (usize, usize)) -> Option<(f64, f64)> {
        let (x0, z0) = self.corner_of(cell);
        let (x, z) = (x0 + 0.5 * self.cell, z0 + 0.5 * self.cell);
        let hidden = self
            .hole
            .is_some_and(|((x0, z0), (x1, z1))| (x0..x1).contains(&x) && (z0..z1).contains(&z));
        (!hidden).then_some((x, z))
    }

    /// Which of the land's own grid of this lattice's cells holds `value`
    /// along one way, the place a cell's draws are keyed on.
    pub(super) fn place(&self, value: f64) -> u32 {
        mathf::round_i32(mathf::floor(value / self.cell)).cast_unsigned()
    }

    /// Rows `rows` of the lattice across `runner`, each cell to what `hold`
    /// finds in it; `None` when the heap will not hold them.
    pub(super) fn read<T: Send>(
        &self,
        rows: Range<usize>,
        runner: &dyn JobRunner,
        hold: &(dyn Fn((usize, usize)) -> Option<T> + Sync),
    ) -> Option<Vec<Option<T>>> {
        let count = rows.len().checked_mul(self.side)?;
        let mut cells = fallible::collected(count, core::iter::repeat_with(|| None))?;
        band::for_each(
            runner,
            &mut cells,
            (rows.start, self.side),
            &|row, cells| {
                for (column, cell) in cells.iter_mut().enumerate() {
                    *cell = hold((column, row));
                }
            },
        );
        Some(cells)
    }
}

#[cfg(test)]
#[path = "lattice_tests.rs"]
mod tests;
