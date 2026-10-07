//! A maize plant's leaves as the season has them: green with a paler midrib
//! and fine parallel veins converging on the tip, drying from the tip and
//! the margins in, the lowest leaves first, until a ripe plant is straw.
//!
//! A leaf's place is read as a limb's: how far along it from its collar, in
//! metres, its length the place's girth, and how far across from its midrib
//! as a share of its half width there. How dry a leaf is rides in the low
//! bits of its key, so the builder that drew its droop drew its drying.

use tairix_util::mathf;

use crate::ground::fade;
use crate::noise::{cell, noise2, smoothstep};
use crate::pigment::Spot;
use crate::sample::{mix32, unit};
use crate::vector::Vec3;

/// How many steps a leaf's dryness is kept in, in its key's low bits.
const DRY_STEPS: u32 = 16;
const DRY_MASK: u32 = DRY_STEPS - 1;

/// How broad a leaf's midrib runs, and how many veins run either side of
/// it, each as a share of its half width; and how far apart those veins lie
/// at a leaf's broadest, in metres, which a pixel must span less than for
/// them to show.
const MIDRIB: f64 = 0.07;
const VEINS: f64 = 26.0;
const VEIN_SPACING: f64 = 0.0017;

/// The leaf keyed `key`, `dry` dried: `0.0` green to `1.0` dead.
pub(crate) fn drying(key: u32, dry: f64) -> u32 {
    let step =
        u32::try_from(mathf::round_i32(dry.clamp(0.0, 1.0) * f64::from(DRY_MASK))).unwrap_or(0);
    (key & !DRY_MASK) | step
}

/// How dry the leaf keyed `key` is.
fn dryness(key: u32) -> f64 {
    f64::from(key & DRY_MASK) / f64::from(DRY_MASK)
}

/// A maize plant's leaves: their two greens and their midribs' paler one,
/// the straw a drying leaf goes to and the brown of a dead one.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Maize {
    pub(crate) greens: [Vec3; 2],
    pub(crate) midrib: Vec3,
    pub(crate) straw: Vec3,
    pub(crate) dead: Vec3,
}

impl Maize {
    /// The colour of a leaf at `spot`, its own key its mark beneath its
    /// plant's.
    pub(crate) fn colour(&self, spot: &Spot) -> Vec3 {
        let key = spot.mark;
        let length = spot.girth.max(1e-3);
        let along = (spot.uv.0 / length).clamp(0.0, 1.0);
        let across = spot.uv.1.clamp(-1.0, 1.0);
        let dry = dryness(key ^ spot.instance);
        let shade = 0.88 + 0.22 * unit(mix32(key >> 4));
        let green = self.greens[usize::from((key >> 4) & 1 != 0)] * shade;
        // Faint streaks down its length, as its veins catch the light unevenly.
        let streaks = 1.0 + 0.06 * noise2(across * 9.0, along * 2.5, key);
        let midrib = 1.0 - smoothstep(0.6 * MIDRIB, MIDRIB, across.abs());
        let (_, vein) = cell(across.abs() * VEINS);
        let veined = fade(
            0.05 * (1.0 - 2.0 * (vein - 0.5).abs()) - 0.025,
            spot.width / VEIN_SPACING,
        );
        let leaf = green.lerp(self.midrib * shade, midrib) * (streaks + veined);
        // It dries from the tip back and from the margins in, its edge
        // ragged; a dead leaf is brown, a drying one straw.
        let ragged = 0.08 * noise2(along * 14.0, across * 3.0, key ^ 0x3d);
        let tip = smoothstep(1.0 - 0.9 * dry, 1.05 - 0.9 * dry, along + ragged);
        let margin = smoothstep(1.0 - 0.7 * dry * dry, 1.0, across.abs() + ragged);
        let withered = self.straw.lerp(self.dead, smoothstep(0.6, 1.0, dry)) * shade;
        let colour = leaf.lerp(withered, tip.max(margin).max(smoothstep(0.85, 1.0, dry)));
        if spot.front {
            colour
        } else {
            // The underside is paler and duller.
            let grey = (colour.x + colour.y + colour.z) / 3.0;
            colour.lerp(Vec3::splat(grey), 0.3) * 1.08
        }
    }
}

#[cfg(test)]
#[path = "maize_tests.rs"]
mod tests;
