//! The cell arithmetic of a grid of equal tiles along one axis: how many whole
//! cells the axis holds, where each sits, and which one a coordinate falls on.
//!
//! One definition for every grid on the desktop — the file manager's icon
//! view, the desktop's icon field, a picture chooser — so a layout and its hit
//! test invert the same arithmetic and no two grids disagree about what a line
//! does with the space it has left over.

use core::ops::Range;

/// What a line of cells does with the space it has left over.
///
/// A line holds as many whole cells as it can, which almost never divides it
/// exactly. A fixed field and a resizable view want opposite things done with
/// the remainder, so this is a policy of the view rather than of the axis.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
pub enum GridFill {
    /// Keep one cell-plus-gap pitch from the line's anchored edge and leave the
    /// remainder at the far end, so a fixed field's cells keep their places
    /// when its extent moves by a few pixels.
    #[default]
    FixedPitch,
    /// Share the remainder out: the cells keep their size and order but move
    /// apart, and the margins at the two ends match. The pitch is the floor, so
    /// a line its cells fit exactly is placed identically under either policy,
    /// and a remainder too small to widen every gap is centred.
    Spread,
}

/// The placement of equal cells along one axis: how many there are, the stride
/// from one cell's leading edge to the next's, the first cell's offset from the
/// axis's own leading edge, and the cell's extent.
///
/// Every value is derived by [`new`](Self::new) or [`fixed`](Self::fixed), so
/// the four can never disagree with one another.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GridRun {
    count: usize,
    stride: u32,
    lead: u32,
    cell: u32,
}

impl GridRun {
    /// The whole `cell`-pixel cells an `extent`-pixel axis holds at least `gap`
    /// pixels apart, the remainder spent as `fill` says.
    ///
    /// A cell cut by the axis's end is not placed: a part-drawn cell no scroll
    /// could bring whole is not a legible item.
    #[must_use]
    pub fn new(extent: u32, cell: u32, gap: u32, fill: GridFill) -> Self {
        let pitch = cell.saturating_add(gap);
        // A cell of no extent has no run, which is also what keeps the pitch
        // non-zero and the divisions below defined.
        if cell == 0 || extent < cell {
            return Self {
                count: 0,
                stride: pitch,
                lead: 0,
                cell,
            };
        }
        let count = ((extent - cell) / pitch).saturating_add(1);
        let stride = match fill {
            GridFill::FixedPitch => pitch,
            GridFill::Spread => (extent / count).max(pitch),
        };
        let span = stride
            .saturating_mul(count.saturating_sub(1))
            .saturating_add(cell);
        Self {
            count: usize::try_from(count).unwrap_or(usize::MAX),
            stride,
            lead: match fill {
                GridFill::FixedPitch => 0,
                GridFill::Spread => extent.saturating_sub(span) / 2,
            },
            cell,
        }
    }

    /// `count` cells of `cell` pixels a `gap` apart from the axis's leading
    /// edge: the lines of a grid, which follow one another however far a view
    /// scrolls.
    #[must_use]
    pub const fn fixed(count: usize, cell: u32, gap: u32) -> Self {
        Self {
            count,
            stride: cell.saturating_add(gap),
            lead: 0,
            cell,
        }
    }

    /// The run of each cell's part from `offset` to `offset + extent` within
    /// it, clamped to the cell: same count and stride, so a range of these
    /// parts names the same cells as the run it came from.
    #[must_use]
    pub const fn within(&self, offset: u32, extent: u32) -> Self {
        let offset = if offset < self.cell {
            offset
        } else {
            self.cell
        };
        let room = self.cell - offset;
        Self {
            count: self.count,
            stride: self.stride,
            lead: self.lead.saturating_add(offset),
            cell: if extent < room { extent } else { room },
        }
    }

    /// How many cells the axis holds.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    /// The distance from one cell's leading edge to the next's.
    #[must_use]
    pub const fn stride(&self) -> u32 {
        self.stride
    }

    /// The first cell's leading edge, from the axis's own.
    #[must_use]
    pub const fn lead(&self) -> u32 {
        self.lead
    }

    /// Each cell's extent along the axis.
    #[must_use]
    pub const fn cell(&self) -> u32 {
        self.cell
    }

    /// Cell `index`'s leading edge from the axis's own, or `None` when that
    /// offset does not fit the axis.
    #[must_use]
    pub fn offset(&self, index: usize) -> Option<u32> {
        let step = u32::try_from(index).ok()?;
        self.stride.checked_mul(step)?.checked_add(self.lead)
    }

    /// The cell `pos` — measured from the axis's leading edge — falls on, or
    /// `None` in a margin, a gap, or past the last cell. The exact inverse of
    /// [`offset`](Self::offset).
    #[must_use]
    pub fn cell_at(&self, pos: u32) -> Option<usize> {
        if self.stride == 0 {
            return None;
        }
        let within = pos.checked_sub(self.lead)?;
        let index = usize::try_from(within / self.stride).ok()?;
        (index < self.count && within % self.stride < self.cell).then_some(index)
    }

    /// How far the cells reach from the axis's leading edge: the last cell's
    /// far edge, or nothing for a run of none.
    #[must_use]
    pub fn span(&self) -> u64 {
        let Some(last) = self.count.checked_sub(1) else {
            return 0;
        };
        u64::from(self.stride)
            .saturating_mul(u64::try_from(last).unwrap_or(u64::MAX))
            .saturating_add(u64::from(self.lead))
            .saturating_add(u64::from(self.cell))
    }

    /// The cells any part of which lies within `extent` pixels from `from`,
    /// measured from the axis's leading edge. A cell only whose gap shows is
    /// not among them.
    #[must_use]
    pub fn shown(&self, from: u64, extent: u64) -> Range<usize> {
        let (stride, lead, cell) = (
            u64::from(self.stride),
            u64::from(self.lead),
            u64::from(self.cell),
        );
        if extent == 0 || stride == 0 || cell == 0 {
            return 0..0;
        }
        let last_seen = from.saturating_add(extent - 1);
        if last_seen < lead {
            return 0..0;
        }
        let clamp = |at: u64| usize::try_from(at).unwrap_or(usize::MAX).min(self.count);
        let first = match from.checked_sub(lead.saturating_add(cell)) {
            Some(past) => clamp(past / stride + 1),
            None => 0,
        };
        let end = clamp((last_seen - lead) / stride + 1);
        first..end.max(first)
    }
}

#[cfg(test)]
#[path = "grid_tests.rs"]
mod tests;
