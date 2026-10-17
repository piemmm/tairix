//! The land generator: landforms worn down by the water that drains them,
//! carved by their rivers, graded for their roads, laid out where they are
//! farmed, and refined level by level down to the hummocks underfoot.
//!
//! Every stage is a pure function of the land's plan and the place: no part
//! of a land depends on who reads it, from where, or in what order, so the
//! desktop's ray tracer and the world of the `WinterSun` game build the same
//! land the same way, bit for bit on every Tier-1 target.

#![no_std]
#![forbid(unsafe_code)]
// Bit for bit on every target: no float max or min, which may pick either zero.
#![deny(clippy::disallowed_methods)]
#![deny(missing_docs)]

extern crate alloc;

pub mod attributes;
pub mod channel;
pub mod coarse;
pub mod course;
mod error;
pub mod grown;
pub mod level;
pub mod plan;
pub mod relief;
mod river;
pub mod snow;
pub mod ways;

#[cfg(test)]
mod testing;

pub use error::Error;
pub use river::{DAMP_BANK, SOAKED};

/// The time of year a land is laid out in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Season {
    /// Spring.
    Spring,
    /// Summer.
    Summer,
    /// Its leaves turned, and this share of them fallen, in 255ths.
    Autumn {
        /// How many 255ths of the leaves have fallen.
        fallen: u8,
    },
    /// Winter.
    Winter,
}

/// The item after a unit of `each` items a core from item `first` across
/// `runner`, counted without overflow whatever its width.
fn unit_end(first: usize, each: usize, runner: &dyn tairix_parallel::JobRunner) -> usize {
    first.saturating_add(each.saturating_mul(runner.width().max(1)))
}

/// The column and row of each of `count` samples of a grid `across` a row,
/// from the one numbered `first`, row by row.
fn run_cells(
    across: usize,
    (first, count): (usize, usize),
) -> impl Iterator<Item = (usize, usize)> {
    let across = across.max(1);
    let (mut column, mut row) = (first % across, first / across);
    (0..count).map(move |_| {
        let cell = (column, row);
        column += 1;
        if column == across {
            (column, row) = (0, row + 1);
        }
        cell
    })
}
