//! Particle hydraulic erosion: droplets that run down the slope they read,
//! pick up sediment while they speed up and have room to carry it, and drop
//! it where they slow, fill a pit, or evaporate (Beyer, "Implementation of a
//! method for hydraulic erosion", 2015).
//!
//! It carves what a drainage network at a coarser scale cannot: rills down
//! every slope, gullies where they gather, and the fans and valley fills the
//! sediment builds. Droplets run one after another, each over the ground the
//! last left, drawn from one seeded stream, so a run split across any number
//! of calls leaves exactly the ground one call would. Heights are stored in
//! single precision, as a renderer's grids hold them; the arithmetic is
//! double.

use alloc::vec::Vec;

use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::mathf;

use crate::grid::Grid;
use crate::{fits, TerrainError};

/// How droplets behave. Heights and slopes are in the grid's height units
/// per sample: a caller eroding a finer grid scales them to its spacing.
#[derive(Copy, Clone, Debug)]
pub struct Droplets {
    /// The most steps a droplet takes before it has run dry.
    pub lifetime: u32,
    /// How much of its heading a droplet keeps each step, against turning
    /// down the slope: `0.0` follows the slope exactly.
    pub inertia: f64,
    /// Sediment carried per unit of fall, speed and water.
    pub capacity: f64,
    /// The least fall a step reads, so a droplet on the level still carries.
    pub min_slope: f64,
    /// The share of its spare capacity a droplet takes up each step.
    pub erosion: f64,
    /// The share of what it carries past its capacity it drops each step.
    pub deposition: f64,
    /// The share of its water it loses each step.
    pub evaporation: f64,
    /// Speed gained per unit of height fallen.
    pub gravity: f64,
    /// The share of its speed friction takes each step, so a droplet slows
    /// where the ground levels out and drops what it carried there.
    pub friction: f64,
    /// How many samples around it a droplet wears at once.
    pub radius: u32,
}

/// A run of droplets over one grid.
pub struct Erosion {
    grid: Grid,
    law: Droplets,
    /// The samples a droplet wears, as offsets from its own index and their
    /// shares: a droplet runs no nearer the edge than its brush reaches, so
    /// every offset lands on the grid.
    brush: Vec<(isize, f64)>,
    dice: NonCryptoRng,
}

impl core::fmt::Debug for Erosion {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Erosion")
            .field("grid", &self.grid)
            .field("law", &self.law)
            .finish_non_exhaustive()
    }
}

impl Erosion {
    /// Droplets behaving as `law` over `grid`, drawn under `seed`.
    pub fn new(grid: Grid, law: Droplets, seed: u64) -> Result<Self, TerrainError> {
        let radius = law.radius.max(1);
        let reach = i32::try_from(radius).map_err(|_| TerrainError::Shape)?;
        let across = usize::try_from(2 * radius + 1).map_err(|_| TerrainError::Shape)?;
        let side = isize::try_from(grid.side()).map_err(|_| TerrainError::Shape)?;
        let mut brush = Vec::new();
        brush
            .try_reserve_exact(across * across)
            .map_err(|_| TerrainError::OutOfMemory)?;
        let mut total = 0.0;
        for oy in -reach..=reach {
            for ox in -reach..=reach {
                let weight = f64::from(radius) - mathf::sqrt(f64::from(ox * ox + oy * oy));
                if weight > 0.0 {
                    total += weight;
                    let offset = isize::try_from(oy).map_err(|_| TerrainError::Shape)? * side
                        + isize::try_from(ox).map_err(|_| TerrainError::Shape)?;
                    brush.push((offset, weight));
                }
            }
        }
        for (_, weight) in &mut brush {
            *weight /= total;
        }
        Ok(Self {
            grid,
            law: Droplets { radius, ..law },
            brush,
            dice: NonCryptoRng::seed_from_u64(seed),
        })
    }

    /// Run `count` more droplets over `height`, adding the water each carries
    /// past a sample into that sample of `flux` if one is given.
    pub fn run(
        &mut self,
        height: &mut [f32],
        count: u32,
        mut flux: Option<&mut [f32]>,
    ) -> Result<(), TerrainError> {
        fits(height, self.grid)?;
        if let Some(flux) = flux.as_deref() {
            fits(flux, self.grid)?;
        }
        // A droplet keeps its whole brush on the grid.
        let margin = f64::from(self.law.radius + 1);
        let span = f64::from(self.grid.side()) - 2.0 * margin - 1.0;
        if span <= 0.0 {
            return Ok(());
        }
        let bounds = (margin, margin + span);
        for _ in 0..count {
            let start = (
                margin + span * self.dice.next_f64(),
                margin + span * self.dice.next_f64(),
            );
            self.droplet(height, flux.as_deref_mut(), start, bounds);
        }
        Ok(())
    }

    /// One droplet from `start`, kept within `bounds` on either axis.
    fn droplet(
        &self,
        height: &mut [f32],
        mut flux: Option<&mut [f32]>,
        (mut x, mut y): (f64, f64),
        (low, high): (f64, f64),
    ) {
        let law = self.law;
        let (mut dx, mut dy) = (0.0_f64, 0.0_f64);
        let (mut speed, mut water, mut sediment) = (1.0_f64, 1.0_f64, 0.0_f64);
        for _ in 0..law.lifetime {
            let Some(here) = self.sample(height, (x, y)) else {
                return;
            };
            dx = dx * law.inertia - here.gradient.0 * (1.0 - law.inertia);
            dy = dy * law.inertia - here.gradient.1 * (1.0 - law.inertia);
            let length = mathf::sqrt(dx * dx + dy * dy);
            if length < 1e-12 {
                // On the dead level it has nowhere to run.
                self.deposit(height, &here, sediment);
                return;
            }
            let (nx, ny) = (x + dx / length, y + dy / length);
            if !(low..high).contains(&nx) || !(low..high).contains(&ny) {
                return;
            }
            let Some(next) = self.sample(height, (nx, ny)) else {
                return;
            };
            let fall = next.height - here.height;
            let capacity = (-fall).max(law.min_slope) * speed * water * law.capacity;
            if sediment > capacity || fall > 0.0 {
                let dropped = if fall > 0.0 {
                    fall.min(sediment)
                } else {
                    (sediment - capacity) * law.deposition
                };
                sediment -= dropped;
                self.deposit(height, &here, dropped);
            } else {
                // Never deeper than the ground it is running to, or it would
                // dig the pit it then has to fill.
                let taken = ((capacity - sediment) * law.erosion).min(-fall);
                self.wear(height, &here, taken);
                sediment += taken;
            }
            if let Some(flux) = flux.as_deref_mut() {
                flux[here.index] += single(water);
            }
            speed =
                mathf::sqrt((speed * speed - fall * law.gravity).max(0.0)) * (1.0 - law.friction);
            water *= 1.0 - law.evaporation;
            (x, y) = (nx, ny);
        }
        // Run dry, it lays down whatever it still carries where it stands.
        if let Some(here) = self.sample(height, (x, y)) {
            self.deposit(height, &here, sediment);
        }
    }

    /// The ground under `(x, y)`: its cell, its height and its slope.
    fn sample(&self, height: &[f32], (x, y): (f64, f64)) -> Option<Ground> {
        let (column, row) = (mathf::floor(x), mathf::floor(y));
        let (across, down) = (x - column, y - row);
        let (column, row) = (whole(column)?, whole(row)?);
        if column + 1 >= self.grid.side() || row + 1 >= self.grid.side() {
            return None;
        }
        let index = self.grid.index(column, row);
        let side = self.grid.side() as usize;
        let corner = |at: usize| f64::from(height[at]);
        let (h00, h10) = (corner(index), corner(index + 1));
        let (h01, h11) = (corner(index + side), corner(index + side + 1));
        Some(Ground {
            index,
            across,
            down,
            height: h00 * (1.0 - across) * (1.0 - down)
                + h10 * across * (1.0 - down)
                + h01 * (1.0 - across) * down
                + h11 * across * down,
            gradient: (
                (h10 - h00) * (1.0 - down) + (h11 - h01) * down,
                (h01 - h00) * (1.0 - across) + (h11 - h10) * across,
            ),
        })
    }

    /// Lay `amount` on the four samples of `at`'s cell, each by its nearness.
    fn deposit(&self, height: &mut [f32], at: &Ground, amount: f64) {
        if amount <= 0.0 {
            return;
        }
        let side = self.grid.side() as usize;
        let (across, down) = (at.across, at.down);
        let mut add = |index: usize, share: f64| height[index] += single(amount * share);
        add(at.index, (1.0 - across) * (1.0 - down));
        add(at.index + 1, across * (1.0 - down));
        add(at.index + side, (1.0 - across) * down);
        add(at.index + side + 1, across * down);
    }

    /// Wear `amount` from the brush's samples about `at`'s cell.
    fn wear(&self, height: &mut [f32], at: &Ground, amount: f64) {
        if amount <= 0.0 {
            return;
        }
        for &(offset, share) in &self.brush {
            if let Some(slot) = height.get_mut(at.index.wrapping_add_signed(offset)) {
                *slot -= single(amount * share);
            }
        }
    }
}

/// Where a droplet stands.
struct Ground {
    /// The sample at the corner of its cell nearest the grid's origin.
    index: usize,
    across: f64,
    down: f64,
    height: f64,
    gradient: (f64, f64),
}

/// A non-negative whole float below `u32::MAX` as a `u32`.
fn whole(value: f64) -> Option<u32> {
    if !(0.0..4_294_967_295.0).contains(&value) {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "range-checked above, and whole, so the conversion is exact"
    )]
    Some(value as u32)
}

/// A value in the grid's single precision.
fn single(value: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "heights and water are stored in single precision"
    )]
    {
        value as f32
    }
}

#[cfg(test)]
#[path = "droplet_tests.rs"]
mod tests;
