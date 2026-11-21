//! How gathered light shows: `1 − e^(−exposure · light)`, encoded in sRGB.
//!
//! Light adds without bound where points crowd, and the curve rolls it off
//! into the display's range rather than clipping it, so a crowd whitens and a
//! lone point keeps its hue. The curve is a table indexed by a channel's light
//! itself, every step of it: a pixel looks its level up in one load, however
//! bright. Each entry is an encoded channel in 8.8 fixed point, so a pixel's
//! ordered dither bias rounds it.

use alloc::boxed::Box;

use tairix_colour::linear_to_srgb;
use tairix_util::{fallible, mathf};

/// The fraction bits light is counted in.
pub(crate) const LIGHT_SHIFT: u32 = 12;

/// Light one, the most a lone point of the brightest colour lights its pixel.
pub(crate) const LIGHT_ONE: u32 = 1 << LIGHT_SHIFT;

/// The table's entries: one for every light a channel holds.
const LEN: usize = 1 << 16;

/// The tone curve.
pub(crate) struct Tone {
    table: Box<[u16; LEN]>,
}

impl Tone {
    /// The curve `1 − e^(−exposure · light)`; `None` when the heap will not
    /// hold its table.
    pub(crate) fn new(exposure: f64) -> Option<Self> {
        let mut table = fallible::boxed_array::<u16, LEN>(0)?;
        // Entry nought stays darkness, so no ground is lifted off black.
        for (light, entry) in table.iter_mut().enumerate().skip(1) {
            let light = mathf::real(light) / f64::from(LIGHT_ONE);
            let shown = 1.0 - mathf::exp(-exposure * light);
            let encoded = linear_to_srgb(mathf::clamp(shown, 0.0, 1.0)) * 255.0 * 256.0;
            *entry = u16::try_from(mathf::round_i32(encoded).clamp(0, 255 << 8)).unwrap_or(0);
        }
        Some(Self { table })
    }

    /// The level `light` shows at, rounded at `bias`, a 256th of a level.
    #[inline]
    pub(crate) fn level(&self, light: u16, bias: u32) -> u8 {
        let shade = u32::from(self.table[usize::from(light)]);
        u8::try_from((shade + bias) >> 8).unwrap_or(u8::MAX)
    }
}

#[cfg(test)]
#[path = "tone_tests.rs"]
mod tests;
