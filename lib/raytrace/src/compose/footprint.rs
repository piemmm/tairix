//! What stands on a stage's ground, as circles a new piece keeps clear of.
//!
//! A still life holds a handful, and is asked of them all; a land's woods
//! hold thousands, so once a land lays its extent the circles are indexed
//! over a grid of it and a question looks only at its neighbours.

use alloc::vec::Vec;

use tairix_util::{fallible, mathf};

use crate::vector::real;

/// The clearance two pieces' circles keep between them.
const GAP: f64 = 0.08;

/// The most cells a side of the grid holds, and the least a cell spans:
/// room for a stage-sized square at a tree's spacing.
const MOST_SIDE: usize = 512;
const LEAST_CELL: f64 = 4.0;

/// No link: an empty cell, or a chain's end.
const END: u32 = u32::MAX;

/// The least and greatest columns of a run of the grid's cells, and its
/// least and greatest rows.
type Cells = ((usize, usize), (usize, usize));

/// Circles `(x, z, radius)` on the ground, each taken by a piece.
#[derive(Debug, Default)]
pub(super) struct Footprints {
    circles: Vec<(f64, f64, f64)>,
    index: Option<Index>,
}

/// A square grid over the ground, each cell chaining the circles reaching
/// into it.
#[derive(Debug)]
struct Index {
    least: (f64, f64),
    cell: f64,
    side: usize,
    heads: Vec<u32>,
    /// A circle, and the next link along its cell's chain.
    links: Vec<(u32, u32)>,
    /// The circles reaching past the grid, which every question asks of.
    beyond: Vec<u32>,
}

impl Footprints {
    /// Whether a piece `radius` across at `at` keeps clear of every circle.
    pub(super) fn clear(&self, at: (f64, f64), radius: f64) -> bool {
        let apart = |id: u32| {
            self.circles.get(id as usize).is_none_or(|&(x, z, taken)| {
                let (dx, dz) = (at.0 - x, at.1 - z);
                let least = radius + taken + GAP;
                dx * dx + dz * dz > least * least
            })
        };
        let Some(index) = &self.index else {
            return (0..self.circles.len()).all(|id| u32::try_from(id).is_ok_and(apart));
        };
        let ((columns, rows), _) = index.span(at, radius);
        index.beyond.iter().all(|&id| apart(id))
            && (rows.0..=rows.1).all(|row| {
                (columns.0..=columns.1)
                    .all(|column| index.chain(row * index.side + column).all(apart))
            })
    }

    /// Take a circle `radius` across at `at`; `None` when the heap will not
    /// hold it.
    pub(super) fn claim(&mut self, at: (f64, f64), radius: f64) -> Option<()> {
        let id = u32::try_from(self.circles.len()).ok()?;
        if !fallible::reserve(&mut self.circles, 1) {
            return None;
        }
        self.circles.push((at.0, at.1, radius));
        match &mut self.index {
            Some(index) => index.link(id, at, radius),
            None => Some(()),
        }
    }

    /// Index the circles over the square `reach` either way of `centre`, the
    /// ground a land lays; `None` when the heap will not hold the grid.
    pub(super) fn index(&mut self, centre: (f64, f64), reach: f64) -> Option<()> {
        let span = 2.0 * reach.max(0.0);
        let cell = (span / real(MOST_SIDE)).max(LEAST_CELL);
        let side = usize::try_from(mathf::round_i32(mathf::ceil(span / cell)).max(1)).ok()?;
        let mut index = Index {
            least: (centre.0 - reach, centre.1 - reach),
            cell,
            side,
            heads: fallible::filled(side * side, END)?,
            links: Vec::new(),
            beyond: Vec::new(),
        };
        for (id, &(x, z, radius)) in self.circles.iter().enumerate() {
            index.link(u32::try_from(id).ok()?, (x, z), radius)?;
        }
        self.index = Some(index);
        Some(())
    }
}

impl Index {
    /// The columns and rows of the cells a circle `radius` across at `at`
    /// reaches into, clamped to the grid, and whether it reaches past it.
    ///
    /// Each circle is chained into the cells of its square widened by half
    /// the gap, and a question asks the cells of its own: two circles too
    /// close together have squares that overlap, so they share a cell.
    fn span(&self, (x, z): (f64, f64), radius: f64) -> (Cells, bool) {
        let half = radius + 0.5 * GAP;
        let side = real(self.side);
        let place = |value: f64, least: f64| (value - least) / self.cell;
        let (west, east) = (place(x - half, self.least.0), place(x + half, self.least.0));
        let (south, north) = (place(z - half, self.least.1), place(z + half, self.least.1));
        let past = west < 0.0 || south < 0.0 || east >= side || north >= side;
        let at = |value: f64| {
            usize::try_from(mathf::round_i32(mathf::floor(value.clamp(0.0, side - 1.0))))
                .unwrap_or(0)
        };
        (((at(west), at(east)), (at(south), at(north))), past)
    }

    /// Chain circle `id`, `radius` across at `at`, into every cell it
    /// reaches, or set it beyond the grid.
    fn link(&mut self, id: u32, at: (f64, f64), radius: f64) -> Option<()> {
        let ((columns, rows), past) = self.span(at, radius);
        if past {
            if !fallible::reserve(&mut self.beyond, 1) {
                return None;
            }
            self.beyond.push(id);
            return Some(());
        }
        let cells = (columns.1 - columns.0 + 1) * (rows.1 - rows.0 + 1);
        if !fallible::reserve(&mut self.links, cells) {
            return None;
        }
        for row in rows.0..=rows.1 {
            for column in columns.0..=columns.1 {
                let head = self.heads.get_mut(row * self.side + column)?;
                let link = u32::try_from(self.links.len()).ok()?;
                self.links.push((id, *head));
                *head = link;
            }
        }
        Some(())
    }

    /// The circles chained into cell `cell`.
    fn chain(&self, cell: usize) -> impl Iterator<Item = u32> + '_ {
        let mut next = self.heads.get(cell).copied().unwrap_or(END);
        core::iter::from_fn(move || {
            let &(id, after) = self.links.get(next as usize)?;
            next = after;
            Some(id)
        })
    }
}

#[cfg(test)]
#[path = "footprint_tests.rs"]
mod tests;
