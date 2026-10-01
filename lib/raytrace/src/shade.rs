//! The shade a wood's crowns cast over the ground: how far under a crown a
//! place lies, which is where the leaves and needles fall, and how much of
//! the sky the crowns about it hide, which is how little light reaches the
//! floor there.
//!
//! A lone tree hides a small share of the sky from the ground beneath it, so
//! grass grows there; a closed wood hides nearly all of it, so none does.
//! The share hidden is the share of the ground nearby that crowns cover,
//! which is what the sky a place sees is roofed by.

use alloc::vec::Vec;

use tairix_util::{fallible, mathf};

use crate::noise::{cell, smoothstep};
use crate::vector::real;

/// A crown: where its trunk stands, and how far it reaches from it.
pub(crate) type Crown = ((f64, f64), f64);

/// A rectangle of the ground, its least and greatest corners.
pub(crate) type Rect = ((f64, f64), (f64, f64));

/// The share of its reach at which a crown's edge begins thinning out.
const EDGE: f64 = 0.55;

/// How far about a place the crowns roofing its sky stand.
pub(crate) const ROOFED: f64 = 12.0;

/// How far either way of the eye the fine shade over a land reaches, how far
/// apart it is sampled, and the most samples a side of the coarse shade over
/// the rest of the land holds.
const NEAR: f64 = 256.0;
pub(crate) const NEAR_CELL: f64 = 1.0;
const FAR_SIDE: usize = 512;

/// A grid over a rectangle of the ground, holding at each corner of its
/// cells how far under a crown it lies and how much of the sky is hidden
/// there, each `0` to `255`; or open ground, holding nothing, where no crown
/// reaches.
#[derive(Clone, Debug)]
pub(crate) struct Shade {
    least: (f64, f64),
    cell: f64,
    columns: usize,
    rows: usize,
    under: Vec<u8>,
    hidden: Vec<u8>,
}

impl Shade {
    /// Open ground, which no crown shades.
    const OPEN: Self = Self {
        least: (0.0, 0.0),
        cell: 1.0,
        columns: 0,
        rows: 0,
        under: Vec::new(),
        hidden: Vec::new(),
    };

    /// The shade `crowns` cast over `rect`, sampled `cell` apart, the sky a
    /// place sees roofed by the crowns within `spread` of it: open if no crown
    /// reaches over the rectangle; `None` when the heap will not hold it.
    pub(crate) fn of(crowns: &[Crown], (from, to): Rect, cell: f64, spread: f64) -> Option<Self> {
        let cell = cell.max(1e-3);
        let (columns, rows) = (samples(to.0 - from.0, cell)?, samples(to.1 - from.1, cell)?);
        let mut under = Vec::new();
        for &((x, z), reach) in crowns {
            let reach = reach.max(1e-3);
            if x + reach < from.0 || x - reach > to.0 || z + reach < from.1 || z - reach > to.1 {
                continue;
            }
            if under.is_empty() {
                under = fallible::filled(columns * rows, 0u8)?;
            }
            let first = |low: f64, least: f64, most: usize| {
                let at = mathf::round_i32(mathf::floor((low - least) / cell))
                    .clamp(0, i32::try_from(most).unwrap_or(i32::MAX) - 1);
                usize::try_from(at).unwrap_or(0)
            };
            let (west, east) = (
                first(x - reach, from.0, columns),
                first(x + reach, from.0, columns) + 1,
            );
            let (south, north) = (
                first(z - reach, from.1, rows),
                first(z + reach, from.1, rows) + 1,
            );
            for row in south..=north.min(rows - 1) {
                let dz = from.1 + real(row) * cell - z;
                for column in west..=east.min(columns - 1) {
                    let dx = from.0 + real(column) * cell - x;
                    let over = 1.0 - smoothstep(EDGE * reach, reach, mathf::hypot(dx, dz));
                    if let Some(value) = under.get_mut(row * columns + column) {
                        *value = (*value).max(byte(over));
                    }
                }
            }
        }
        if under.is_empty() {
            return Some(Self::OPEN);
        }
        let radius = usize::try_from(mathf::round_i32(spread / cell).max(1)).ok()?;
        let hidden = spread_over(&under, (columns, rows), radius)?;
        Some(Self {
            least: from,
            cell,
            columns,
            rows,
            under,
            hidden,
        })
    }

    /// Whether no crown shades any of it.
    pub(crate) fn is_open(&self) -> bool {
        self.under.is_empty()
    }

    /// How far under a crown `(x, z)` lies, and how much of the sky is
    /// hidden there, each `0.0..=1.0` and blended between the grid's
    /// samples; nought off the grid.
    pub(crate) fn at(&self, x: f64, z: f64) -> (f64, f64) {
        let Some((across, down)) = self.place(x, z) else {
            return (0.0, 0.0);
        };
        let ((column, right), (row, lower)) = (cell(across), cell(down));
        let (column, row) = (column as usize, row as usize);
        let blend = |values: &[u8]| {
            let sample = |column: usize, row: usize| {
                let (column, row) = (column.min(self.columns - 1), row.min(self.rows - 1));
                f64::from(
                    values
                        .get(row * self.columns + column)
                        .copied()
                        .unwrap_or(0),
                )
            };
            let top = sample(column, row) + (sample(column + 1, row) - sample(column, row)) * right;
            let bottom = sample(column, row + 1)
                + (sample(column + 1, row + 1) - sample(column, row + 1)) * right;
            (top + (bottom - top) * lower) / 255.0
        };
        (blend(&self.under), blend(&self.hidden))
    }

    /// Where `(x, z)` lies on the grid, in samples along and down it; `None`
    /// off it, or over open ground.
    fn place(&self, x: f64, z: f64) -> Option<(f64, f64)> {
        if self.is_open() {
            return None;
        }
        let (across, down) = (
            (x - self.least.0) / self.cell,
            (z - self.least.1) / self.cell,
        );
        let on = (0.0..=real(self.columns - 1)).contains(&across)
            && (0.0..=real(self.rows - 1)).contains(&down);
        on.then_some((across, down))
    }

    /// A copy of this shade; `None` when the heap will not hold it.
    fn copied(&self) -> Option<Self> {
        Some(Self {
            under: fallible::collected(self.under.len(), self.under.iter().copied())?,
            hidden: fallible::collected(self.hidden.len(), self.hidden.iter().copied())?,
            ..*self
        })
    }
}

/// How many samples `cell` apart span `span`, both ends included; `None` for
/// more than a grid can count.
fn samples(span: f64, cell: f64) -> Option<usize> {
    usize::try_from(mathf::round_i32(mathf::ceil(span / cell)).max(1))
        .ok()
        .map(|count| count + 1)
}

/// The shade `at` says lies over `rect`, sampled `cell` apart: open if none
/// does; `None` when the heap will not hold it.
fn sampled((from, to): Rect, cell: f64, at: &dyn Fn(f64, f64) -> (f64, f64)) -> Option<Shade> {
    let cell = cell.max(1e-3);
    let (columns, rows) = (samples(to.0 - from.0, cell)?, samples(to.1 - from.1, cell)?);
    let mut under = fallible::filled(columns * rows, 0u8)?;
    let mut hidden = fallible::filled(columns * rows, 0u8)?;
    let mut touched = false;
    for row in 0..rows {
        for column in 0..columns {
            let (over, roofed) = at(from.0 + real(column) * cell, from.1 + real(row) * cell);
            let index = row * columns + column;
            if let (Some(below), Some(roof)) = (under.get_mut(index), hidden.get_mut(index)) {
                (*below, *roof) = (byte(over), byte(roofed));
                touched |= *below > 0 || *roof > 0;
            }
        }
    }
    Some(if touched {
        Shade {
            least: from,
            cell,
            columns,
            rows,
            under,
            hidden,
        }
    } else {
        Shade::OPEN
    })
}

/// The shade over a land: finely about the eye, where a blade's worth of it
/// shows, and coarsely over the rest.
#[derive(Clone, Debug)]
pub(crate) struct Shades {
    near: Shade,
    far: Shade,
}

impl Shades {
    /// The shade `crowns` cast over the square `reach` either way of
    /// `centre`, finely about `eye`; `None` when the heap will not hold it.
    pub(crate) fn of(
        crowns: &[Crown],
        (centre, reach): ((f64, f64), f64),
        eye: (f64, f64),
    ) -> Option<Self> {
        let about = |middle: (f64, f64), reach: f64| {
            (
                (middle.0 - reach, middle.1 - reach),
                (middle.0 + reach, middle.1 + reach),
            )
        };
        let coarse = (2.0 * reach / real(FAR_SIDE)).max(NEAR_CELL);
        Some(Self {
            near: Shade::of(crowns, about(eye, NEAR), NEAR_CELL, ROOFED)?,
            far: Shade::of(crowns, about(centre, reach), coarse, ROOFED)?,
        })
    }

    /// A copy of this shade; `None` when the heap will not hold it.
    pub(crate) fn copied(&self) -> Option<Self> {
        Some(Self {
            near: self.near.copied()?,
            far: self.far.copied()?,
        })
    }

    /// How far under a crown `(x, z)` lies, and how much of the sky is
    /// hidden there, each `0.0..=1.0`.
    pub(crate) fn at(&self, x: f64, z: f64) -> (f64, f64) {
        if self.near.place(x, z).is_some() {
            self.near.at(x, z)
        } else {
            self.far.at(x, z)
        }
    }

    /// This shade over `rect` alone, sampled `cell` apart: open if no crown
    /// reaches over it; `None` when the heap will not hold it.
    pub(crate) fn within(&self, rect: Rect, cell: f64) -> Option<Shade> {
        sampled(rect, cell, &|x, z| self.at(x, z))
    }
}

/// `share`, `0.0..=1.0`, as a byte.
fn byte(share: f64) -> u8 {
    u8::try_from(mathf::round_i32(255.0 * share.clamp(0.0, 1.0))).unwrap_or(u8::MAX)
}

/// The mean of `values`, a grid `columns` by `rows`, over the square
/// `radius` cells either way of each: twice over, so each crown's share is
/// weighed by how near it stands.
fn spread_over(values: &[u8], (columns, rows): (usize, usize), radius: usize) -> Option<Vec<u8>> {
    let mut now = fallible::collected(values.len(), values.iter().map(|&value| f64::from(value)))?;
    let mut line = fallible::filled(columns.max(rows) + 1, 0.0f64)?;
    for _ in 0..2 {
        for row in 0..rows {
            let start = row * columns;
            smooth(now.get_mut(start..start + columns)?, 1, radius, &mut line);
        }
        for column in 0..columns {
            smooth(now.get_mut(column..)?, columns, radius, &mut line);
        }
    }
    fallible::collected(
        now.len(),
        now.iter()
            .map(|&value| u8::try_from(mathf::round_i32(value).clamp(0, 255)).unwrap_or(0)),
    )
}

/// Each of the values `stride` apart along `run` replaced by the mean of
/// those within `radius` of it, the run's first and last held beyond its
/// ends; `sums` is room for the running sums.
fn smooth(run: &mut [f64], stride: usize, radius: usize, sums: &mut [f64]) {
    let stride = stride.max(1);
    let count = run.len().div_ceil(stride);
    if count == 0 || sums.len() <= count {
        return;
    }
    let value = |index: usize| run.get(index * stride).copied().unwrap_or(0.0);
    let (first, last) = (value(0), value(count - 1));
    let mut total = 0.0;
    for index in 0..=count {
        if let Some(sum) = sums.get_mut(index) {
            *sum = total;
        }
        total += value(index);
    }
    let width = real(2 * radius + 1);
    for index in 0..count {
        let (start, end) = (
            index.saturating_sub(radius),
            (index + radius + 1).min(count),
        );
        let (before, after) = (
            radius.saturating_sub(index),
            (index + radius + 1).saturating_sub(count),
        );
        let within =
            sums.get(end).copied().unwrap_or(0.0) - sums.get(start).copied().unwrap_or(0.0);
        let mean = (within + real(before) * first + real(after) * last) / width;
        if let Some(slot) = run.get_mut(index * stride) {
            *slot = mean;
        }
    }
}

#[cfg(test)]
#[path = "shade_tests.rs"]
mod tests;
