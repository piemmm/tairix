//! Grid terrain: which way water leaves a height grid, how water and gravity
//! wear it down, and the cheapest way across it.
//!
//! Every algorithm works on a square [`Grid`] of heights stored row-major, so
//! a world generator and a renderer share one definition of each:
//!
//! - [`drainage`] — Priority-Flood (Barnes, Lehman and Mulla, 2014): the
//!   depression-filled surface, an acyclic steepest-descent routing, and the
//!   area draining through every sample.
//! - [`incision`] — fluvial incision by stream power along that drainage,
//!   explicitly or by the implicit scheme of Braun and Willett (2013).
//! - [`hillslope`] — linear diffusion, and talus: ground steeper than its
//!   angle of repose slumps.
//! - [`droplet`] — particle hydraulic erosion: droplets that carry sediment
//!   down the slope they read and drop it where they slow.
//! - [`route`] — A\* with integer costs: the least-cost path between two
//!   samples under a caller's pricing of each step.
//!
//! A pass whose cost grows with the grid — a flood, a route, a run of
//! droplets — advances a bounded amount per call, so a caller answering a
//! frame spreads it over as many calls as it needs; run to its end, it gives
//! exactly what one call would. Every answer is a pure function of its
//! inputs: arithmetic is `f64` or `f32` under `lib/util::mathf`, and every
//! queue breaks a tie on a sample's index.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod drainage;
pub mod droplet;
pub mod grid;
pub mod hillslope;
pub mod incision;
pub mod route;

pub use grid::{FlowDir, Grid, NEIGHBOURS};

/// Why a pass could not run.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TerrainError {
    /// The heap refused room for a working buffer.
    OutOfMemory,
    /// A buffer does not hold one value per sample of its grid.
    Shape,
}

/// `count` copies of `value`, or the refusal.
fn filled<T: Clone>(count: usize, value: T) -> Result<alloc::vec::Vec<T>, TerrainError> {
    tairix_util::fallible::filled(count, value).ok_or(TerrainError::OutOfMemory)
}

/// `Ok` when `values` holds one entry per sample of `grid`.
fn fits<T>(values: &[T], grid: Grid) -> Result<(), TerrainError> {
    if values.len() == grid.area() {
        Ok(())
    } else {
        Err(TerrainError::Shape)
    }
}
