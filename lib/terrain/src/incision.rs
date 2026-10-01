//! Fluvial incision: rivers cutting their beds down along the drainage.
//!
//! Detachment-limited stream power, `E = K · Aᵐ · S`: the more ground drains
//! through a sample and the steeper its reach, the faster it is cut. Two
//! schemes over the same law:
//!
//! - [`incise`], explicit (`m = ½`), each sample cut by a snapshot of its own
//!   reach and never below the sample it drains into. A few passes deepen a
//!   network without changing its shape.
//! - [`incise_implicit`], after Braun and Willett ("A very efficient O(n),
//!   implicit and parallel method to solve the stream power equation
//!   governing fluvial incision and landscape evolution", 2013): each sample
//!   is solved against its receiver's already-updated height, walking the
//!   drainage downstream first. Stable at any time step, so a handful of
//!   steps carve valleys and dendritic ridges from bare relief.

use core::ops::Range;

use tairix_util::mathf;

use crate::drainage::{downstream, Network};
use crate::grid::Grid;
use crate::{filled, fits, TerrainError};

/// The explicit law: how hard the ground is, and the base level below which
/// nothing is cut.
#[derive(Copy, Clone, Debug)]
pub struct Explicit {
    /// Incision per unit of the square root of drained samples and of slope.
    pub k: f64,
    /// Heights at or below this are never cut, and no cut goes below it.
    pub floor: f64,
}

/// One explicit pass of `law` over `height`, along `network` solved from it,
/// its samples `spacing` apart.
pub fn incise(
    height: &mut [f64],
    network: &Network,
    grid: Grid,
    spacing: f64,
    law: Explicit,
) -> Result<(), TerrainError> {
    fits(height, grid)?;
    fits(&network.flow, grid)?;
    fits(&network.filled, grid)?;
    fits(&network.discharge, grid)?;
    let mut cut = filled(height.len(), 0.0_f64)?;
    for index in 0..height.len() {
        if height[index] <= law.floor {
            continue;
        }
        let Some(next) = downstream(grid, &network.flow, index) else {
            continue;
        };
        let distance = spacing * network.flow[index].length();
        let drop = network.filled[index] - network.filled[next];
        if drop <= 0.0 {
            continue;
        }
        let area = f64::from(network.discharge[index]);
        let incision = law.k * mathf::sqrt(area) * (drop / distance);
        // Never below the sample downstream: incision deepens a valley, it
        // does not invert the gradient that drives it.
        cut[index] = mathf::fmin(incision, mathf::fmax(height[index] - height[next], 0.0));
    }
    for (slot, amount) in height.iter_mut().zip(cut.iter().copied()) {
        if amount > 0.0 {
            *slot = mathf::fmax(*slot - amount, law.floor);
        }
    }
    Ok(())
}

/// The implicit law for one time step.
#[derive(Copy, Clone, Debug)]
pub struct Implicit {
    /// Erodibility times the time step.
    pub k_dt: f64,
    /// The drainage-area exponent `m`, about a half in nature.
    pub m: f64,
    /// The ground one sample stands for, so drainage area is an area.
    pub cell_area: f64,
    /// The distance between neighbouring samples.
    pub spacing: f64,
    /// The drained area, in the units `cell_area` is in, at which a channel
    /// begins: below it creep rules the slope and nothing is cut, and the
    /// cutting sets in smoothly up to three times it, so a planar slope is
    /// not furrowed along every line the drainage runs down.
    pub head: f64,
}

/// One implicit step of `law` over `height`, along `network` solved from it,
/// for the flood-order positions `span`; `erodibility` scales `law.k_dt` per
/// sample index and height, so harder layers resist.
///
/// A sample is solved against its receiver's height, which must already be
/// this step's: walk the whole order front to back, each span after the one
/// before it. A sample in a lake or on a flat, whose receiver is no lower, is
/// left as it is.
pub fn incise_implicit(
    height: &mut [f64],
    network: &Network,
    grid: Grid,
    law: Implicit,
    (span, erodibility): (Range<usize>, &dyn Fn(usize, f64) -> f64),
) -> Result<(), TerrainError> {
    fits(height, grid)?;
    fits(&network.flow, grid)?;
    fits(&network.discharge, grid)?;
    let Some(positions) = network.order.get(span) else {
        return Err(TerrainError::Shape);
    };
    for &raw in positions {
        let index = raw as usize;
        let Some(next) = downstream(grid, &network.flow, index) else {
            continue;
        };
        let (here, below) = (height[index], height[next]);
        if below >= here {
            continue;
        }
        let area = f64::from(network.discharge[index]) * law.cell_area;
        let distance = law.spacing * network.flow[index].length();
        let factor =
            law.k_dt * erodibility(index, here) * power(area, law.m) * onset(area, law.head)
                / distance;
        if factor > 0.0 {
            height[index] = (here + factor * below) / (1.0 + factor);
        }
    }
    Ok(())
}

/// How far a channel has set in where `area` drains, one beginning at
/// `head`: nothing below it, all of it from three times it, smoothly between.
fn onset(area: f64, head: f64) -> f64 {
    if head <= 0.0 {
        return 1.0;
    }
    mathf::smoothstep((area - head) / (2.0 * head))
}

/// `base` to the power `exponent`, for a positive base.
fn power(base: f64, exponent: f64) -> f64 {
    if base <= 0.0 {
        0.0
    } else {
        mathf::exp(exponent * mathf::ln(base))
    }
}

#[cfg(test)]
#[path = "incision_tests.rs"]
mod tests;
