//! A square grid over the ground, each cell chaining the items linked into
//! it, so what stands about a place is found by asking only the cells near
//! it rather than everything set out.

use alloc::vec::Vec;

use tairix_util::{fallible, mathf};

use crate::vector::real;

/// No link: an empty cell, or a chain's end.
const END: u32 = u32::MAX;

/// The least and greatest columns of a run of the grid's cells, and its
/// least and greatest rows.
pub(super) type Cells = ((usize, usize), (usize, usize));

/// Items linked into the cells of a square grid.
#[derive(Debug)]
pub(super) struct Chains {
    least: (f64, f64),
    cell: f64,
    side: usize,
    heads: Vec<u32>,
    /// An item, and the next link along its cell's chain.
    links: Vec<(u32, u32)>,
}

impl Chains {
    /// A grid over the square `reach` either way of `centre`, its cells
    /// `cell` across or broader, so it is at most `most` cells a side;
    /// `None` when the heap will not hold it.
    pub(super) fn new((centre, reach): ((f64, f64), f64), cell: f64, most: usize) -> Option<Self> {
        let span = 2.0 * reach.max(0.0);
        let side = usize::try_from(mathf::round_i32(mathf::ceil(span / cell).max(1.0)))
            .ok()?
            .clamp(1, most.max(1));
        let cell = (span / real(side)).max(cell);
        Some(Self {
            least: (centre.0 - reach, centre.1 - reach),
            cell,
            side,
            heads: fallible::filled(side * side, END)?,
            links: Vec::new(),
        })
    }

    /// How broad each cell is.
    pub(super) const fn cell(&self) -> f64 {
        self.cell
    }

    /// The columns and rows of the cells the square `half` either way of
    /// `at` covers, clamped to the grid, and whether it reaches past it.
    pub(super) fn span(&self, (x, z): (f64, f64), half: f64) -> (Cells, bool) {
        let half = half.max(0.0);
        let side = real(self.side);
        let place = |value: f64, least: f64| (value - least) / self.cell;
        let (west, east) = (place(x - half, self.least.0), place(x + half, self.least.0));
        let (south, north) = (place(z - half, self.least.1), place(z + half, self.least.1));
        let past = west < 0.0 || south < 0.0 || east >= side || north >= side;
        let at = |value: f64| {
            usize::try_from(mathf::round_i32(mathf::floor(mathf::clamp(
                value,
                0.0,
                side - 1.0,
            ))))
            .unwrap_or(0)
        };
        (((at(west), at(east)), (at(south), at(north))), past)
    }

    /// Link `item` into each of the cells `cells`; `None` when the heap will
    /// not hold the links.
    pub(super) fn link(&mut self, item: u32, (columns, rows): Cells) -> Option<()> {
        let cells = (columns.1 - columns.0 + 1) * (rows.1 - rows.0 + 1);
        self.links.try_reserve(cells).ok()?;
        for row in rows.0..=rows.1 {
            for column in columns.0..=columns.1 {
                let head = self.heads.get_mut(row * self.side + column)?;
                let link = u32::try_from(self.links.len()).ok()?;
                self.links.push((item, *head));
                *head = link;
            }
        }
        Some(())
    }

    /// The items linked into the cells `cells`, an item linked into more than
    /// one of them once for each.
    pub(super) fn within(&self, (columns, rows): Cells) -> impl Iterator<Item = u32> + '_ {
        (rows.0..=rows.1).flat_map(move |row| {
            (columns.0..=columns.1).flat_map(move |column| self.chain(row * self.side + column))
        })
    }

    /// The items linked into cell `cell`, the last linked first.
    fn chain(&self, cell: usize) -> impl Iterator<Item = u32> + '_ {
        let mut next = self.heads.get(cell).copied().unwrap_or(END);
        core::iter::from_fn(move || {
            let &(item, after) = self.links.get(next as usize)?;
            next = after;
            Some(item)
        })
    }
}

#[cfg(test)]
#[path = "chains_tests.rs"]
mod tests;
