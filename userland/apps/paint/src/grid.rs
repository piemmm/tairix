//! The grid laid over a picture: which screen pixels of a paint its lines
//! cross, drawn as its style says, and what lands on it when snapping is on.
//!
//! A line stands at every picture-pixel boundary `offset + k × spacing`, drawn
//! in the screen column (or row) where the pixel after it starts, so a line
//! crosses the same pixels at every zoom and scroll.

use alloc::vec::Vec;

use tairix_geometry::Scale;
use tairix_raster::{Color, Pixel};
use tairix_util::fallible;

use crate::preferences::{Grid, GridStyle};
use crate::shape::{Bounds, Point as Fx, FX};

/// The narrowest cell worth drawing, in logical pixels: a grid denser than
/// this is a wash of colour, not lines.
const LEAST_CELL: u32 = 4;

/// A dash and the gap after it, in logical pixels.
const DASH: u32 = 4;

/// The step between two dots, in logical pixels.
const DOT: u32 = 3;

/// How far a crossing's arms reach from it, in logical pixels.
const ARM: u32 = 3;

/// The grid's lines across one paint.
pub struct GridLines {
    style: GridStyle,
    ink: Pixel,
    /// For each screen column of the paint, its distance in screen pixels to
    /// the nearest vertical line, at most `u8::MAX`.
    columns: Vec<u8>,
    first_column: u32,
    /// Where the picture's top left falls on screen.
    origin: (i64, i64),
    /// Screen pixels across and down per `den` picture pixels.
    span: (u64, u64, u64),
    spacing: (i64, i64),
    offset: (i64, i64),
    dash: i64,
    dot: i64,
    arm: u8,
}

/// Where one screen row stands against the grid's horizontal lines.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RowLines {
    /// Its distance in screen pixels to the nearest horizontal line, at most
    /// `u8::MAX`.
    distance: u8,
}

impl GridLines {
    /// The lines of `grid` across screen columns `columns` of a paint whose
    /// picture's top left is at `origin` and which spans `span`; `None` where
    /// its cells are too small to show or the room to hold the paint's
    /// columns is refused.
    #[must_use]
    pub fn new(
        grid: &Grid,
        origin: (i64, i64),
        span: (u64, u64, u64),
        columns: (u32, u32),
        scale: Scale,
    ) -> Option<Self> {
        let (across, down, den) = span;
        let den = den.max(1);
        let least = u64::from(scale.scale_length(LEAST_CELL).max(1));
        if u64::from(grid.spacing.0) * across < least * den
            || u64::from(grid.spacing.1) * down < least * den
        {
            return None;
        }
        let alpha = u8::try_from(grid.opacity.min(1000) * 255 / 1000).unwrap_or(u8::MAX);
        let ink = Color::rgba(grid.colour.r, grid.colour.g, grid.colour.b, alpha).premultiply();
        let spacing = (
            i64::from(grid.spacing.0.max(1)),
            i64::from(grid.spacing.1.max(1)),
        );
        let offset = (
            i64::from(grid.offset.0) % spacing.0,
            i64::from(grid.offset.1) % spacing.1,
        );
        let width = usize::try_from(columns.1.saturating_sub(columns.0)).ok()?;
        let mut distances = Vec::new();
        if !fallible::grow_to(&mut distances, width, u8::MAX) {
            return None;
        }
        let mut lines = Self {
            style: grid.style,
            ink,
            columns: distances,
            first_column: columns.0,
            origin,
            span: (across, down, den),
            spacing,
            offset,
            dash: i64::from(scale.scale_length(DASH).max(1)),
            dot: i64::from(scale.scale_length(DOT).max(2)),
            arm: u8::try_from(scale.scale_length(ARM).max(1)).unwrap_or(u8::MAX),
        };
        lines.measure_columns();
        Some(lines)
    }

    /// Fill in each column's distance to its nearest vertical line, in two
    /// sweeps.
    fn measure_columns(&mut self) {
        let (across, _, den) = self.span;
        let mut last: Option<usize> = None;
        for index in 0..self.columns.len() {
            let column = i64::from(self.first_column) + i64::try_from(index).unwrap_or(0);
            let on = crosses(
                column - self.origin.0,
                across,
                den,
                self.offset.0,
                self.spacing.0,
            );
            if on {
                last = Some(index);
            }
            self.columns[index] = last.map_or(u8::MAX, |at| saturated(index - at));
        }
        let mut next: Option<usize> = None;
        for index in (0..self.columns.len()).rev() {
            if self.columns[index] == 0 {
                next = Some(index);
            }
            if let Some(at) = next {
                self.columns[index] = self.columns[index].min(saturated(at - index));
            }
        }
    }

    /// Where screen row `y` stands against the horizontal lines.
    #[must_use]
    pub fn row(&self, y: u32) -> RowLines {
        let (_, down, den) = self.span;
        let row = i64::from(y) - self.origin.1;
        RowLines {
            distance: nearest(row, down, den, self.offset.1, self.spacing.1),
        }
    }

    /// The pixel at screen `(x, y)`, in row `row`, as the grid leaves it.
    #[must_use]
    pub fn over(&self, row: RowLines, x: u32, y: u32, pixel: Pixel) -> Pixel {
        let column = x
            .checked_sub(self.first_column)
            .and_then(|index| self.columns.get(usize::try_from(index).ok()?))
            .copied()
            .unwrap_or(u8::MAX);
        let (along_x, along_y) = (i64::from(x) - self.origin.0, i64::from(y) - self.origin.1);
        let vertical = column == 0;
        let horizontal = row.distance == 0;
        let inked = match self.style {
            GridStyle::Lines => vertical || horizontal,
            GridStyle::Dashes => {
                (vertical && along_y.rem_euclid(self.dash * 2) < self.dash)
                    || (horizontal && along_x.rem_euclid(self.dash * 2) < self.dash)
            }
            GridStyle::Dots => {
                (vertical && along_y.rem_euclid(self.dot) == 0)
                    || (horizontal && along_x.rem_euclid(self.dot) == 0)
            }
            GridStyle::Crossings => {
                (vertical && row.distance <= self.arm) || (horizontal && column <= self.arm)
            }
        };
        if inked {
            self.ink.over(pixel)
        } else {
            pixel
        }
    }
}

/// Whether screen offset `at` from the picture's edge is where a line of
/// boundaries `offset + k × spacing` is drawn, at `across` screen pixels per
/// `den` picture pixels.
fn crosses(at: i64, across: u64, den: u64, offset: i64, spacing: i64) -> bool {
    let (from, to) = covered(at, across, den);
    let first = first_line_from(from, offset, spacing);
    first < to
}

/// The picture boundaries whose line is drawn at screen offset `at`: those
/// whose pixel starts there, `from..to`.
fn covered(at: i64, across: u64, den: u64) -> (i64, i64) {
    let (across, den) = (i128::from(across.max(1)), i128::from(den.max(1)));
    let start = |at: i128| (at * den + across - 1).div_euclid(across);
    let from = start(i128::from(at));
    let to = start(i128::from(at) + 1);
    (
        i64::try_from(from).unwrap_or(i64::MAX),
        i64::try_from(to).unwrap_or(i64::MAX),
    )
}

/// The first boundary `offset + k × spacing` at or past `from`.
fn first_line_from(from: i64, offset: i64, spacing: i64) -> i64 {
    from + (offset - from).rem_euclid(spacing)
}

/// The screen offset boundary `boundary`'s line is drawn at.
fn screen_of(boundary: i64, along: u64, den: u64) -> i64 {
    let at = (i128::from(boundary) * i128::from(along)).div_euclid(i128::from(den.max(1)));
    i64::try_from(at).unwrap_or(i64::MAX)
}

/// The distance in screen pixels from row offset `at` to the nearest line.
fn nearest(at: i64, along: u64, den: u64, offset: i64, spacing: i64) -> u8 {
    let (from, to) = covered(at, along, den);
    let after = first_line_from(from, offset, spacing);
    if after < to {
        return 0;
    }
    let before = after - spacing;
    let below = screen_of(after, along, den) - at;
    let above = at - screen_of(before, along, den);
    saturated(usize::try_from(below.min(above).max(0)).unwrap_or(usize::MAX))
}

fn saturated(distance: usize) -> u8 {
    u8::try_from(distance).unwrap_or(u8::MAX)
}

/// The boundary of lines `offset + k × spacing` nearest picture boundary `at`.
fn nearest_line(at: i64, offset: i64, spacing: i64) -> i64 {
    let spacing = spacing.max(1);
    let from = at - (at - offset).rem_euclid(spacing);
    if (at - from) * 2 >= spacing {
        from + spacing
    } else {
        from
    }
}

/// `at` snapped to the crossing of `grid` nearest it: the centre of the pixel
/// whose top left is the crossing, so what is drawn there lies on the lines.
#[must_use]
pub fn snap_point(grid: &Grid, at: Fx) -> Fx {
    let boundary = |at: i64, offset: u32, spacing: u32| {
        let (offset, spacing) = (i64::from(offset) * FX, i64::from(spacing.max(1)) * FX);
        let below = at - (at - offset).rem_euclid(spacing);
        let nearest = if (at - below) * 2 >= spacing {
            below + spacing
        } else {
            below
        };
        nearest.div_euclid(FX)
    };
    Fx::centre_of(
        boundary(at.x, grid.offset.0, grid.spacing.0),
        boundary(at.y, grid.offset.1, grid.spacing.1),
    )
}

/// Corner `at`, a picture-pixel boundary, moved onto the crossing nearest it.
#[must_use]
pub fn snap_corner(grid: &Grid, at: (i64, i64)) -> (i64, i64) {
    (
        nearest_line(at.0, i64::from(grid.offset.0), i64::from(grid.spacing.0)),
        nearest_line(at.1, i64::from(grid.offset.1), i64::from(grid.spacing.1)),
    )
}

/// Box `moved`, an edit of `start`, with each edge the edit moved put on
/// the line nearest it; an edge it left stays, and the box keeps a cell.
#[must_use]
pub fn snap_edges(grid: &Grid, moved: Bounds, start: Bounds) -> Bounds {
    let line = |at: i64, offset: u32, spacing: u32| {
        nearest_line(at, i64::from(offset), i64::from(spacing))
    };
    let (offset, spacing) = (grid.offset, (grid.spacing.0.max(1), grid.spacing.1.max(1)));
    let mut x0 = if moved.x0 == start.x0 {
        moved.x0
    } else {
        line(moved.x0, offset.0, spacing.0)
    };
    let mut x1 = if moved.x1 == start.x1 {
        moved.x1
    } else {
        line(moved.x1, offset.0, spacing.0)
    };
    let mut y0 = if moved.y0 == start.y0 {
        moved.y0
    } else {
        line(moved.y0, offset.1, spacing.1)
    };
    let mut y1 = if moved.y1 == start.y1 {
        moved.y1
    } else {
        line(moved.y1, offset.1, spacing.1)
    };
    if x1 <= x0 {
        (x0, x1) = if moved.x0 == start.x0 {
            (x0, x0 + i64::from(spacing.0))
        } else {
            (x1 - i64::from(spacing.0), x1)
        };
    }
    if y1 <= y0 {
        (y0, y1) = if moved.y0 == start.y0 {
            (y0, y0 + i64::from(spacing.1))
        } else {
            (y1 - i64::from(spacing.1), y1)
        };
    }
    Bounds { x0, y0, x1, y1 }
}

/// The box of pixels from `from` to `to`, both counted, with the edges it
/// covers moved onto the lines of `grid` nearest them; a box that would
/// collapse keeps one cell, the way it was dragged.
#[must_use]
pub fn snap_span(grid: &Grid, from: (i64, i64), to: (i64, i64)) -> ((i64, i64), (i64, i64)) {
    let axis = |from: i64, to: i64, offset: u32, spacing: u32| {
        let (offset, spacing) = (i64::from(offset), i64::from(spacing.max(1)));
        if to >= from {
            let start = nearest_line(from, offset, spacing);
            let end = nearest_line(to + 1, offset, spacing).max(start + spacing);
            (start, end - 1)
        } else {
            let start = nearest_line(from + 1, offset, spacing);
            let end = nearest_line(to, offset, spacing).min(start - spacing);
            (start - 1, end)
        }
    };
    let (fx, tx) = axis(from.0, to.0, grid.offset.0, grid.spacing.0);
    let (fy, ty) = axis(from.1, to.1, grid.offset.1, grid.spacing.1);
    ((fx, fy), (tx, ty))
}

#[cfg(test)]
#[path = "grid_tests.rs"]
mod tests;
