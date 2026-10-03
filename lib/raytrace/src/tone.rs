//! From radiance to a pixel: a filmic curve that rolls highlights off into
//! white, then the sRGB encoding and the desktop's own ordered dither, so a
//! sky's slow gradient shows no bands.

use alloc::vec::Vec;

use tairix_colour::linear_to_srgb;
use tairix_raster::DitherRow;
use tairix_raster::Pixel;
use tairix_util::{fallible, mathf};

use crate::vector::Vec3;

/// Steps of display-linear light the encoding table holds.
const STEPS: usize = 4096;

/// The filmic curve (Narkowicz's fit of the ACES reference transform) on one
/// channel of an exposed radiance, into `0.0..=1.0` of display-linear light.
pub(crate) fn filmic(x: f64) -> f64 {
    let x = x.max(0.0);
    (x * (2.51 * x + 0.03) / (x * (2.43 * x + 0.59) + 0.14)).clamp(0.0, 1.0)
}

/// An exposed radiance through the filmic curve.
pub(crate) fn display(radiance: Vec3) -> Vec3 {
    if !radiance.is_finite() {
        return Vec3::ZERO;
    }
    Vec3::new(filmic(radiance.x), filmic(radiance.y), filmic(radiance.z))
}

/// Display-linear light to dithered 8-bit sRGB: a table built once and
/// shared by every tracer.
#[derive(Debug)]
pub struct Encoder {
    /// sRGB-encoded levels in 8.8 fixed point, one per step, and one past
    /// the last so every step can be interpolated.
    table: Vec<u16>,
}

impl Encoder {
    /// The encoder, or `None` when the heap will not hold its table.
    #[must_use]
    pub fn new() -> Option<Self> {
        let top = f64::from(u32::try_from(STEPS - 1).ok()?);
        let table = fallible::collected(
            STEPS + 1,
            (0..=STEPS).map(|step| {
                let linear = (f64::from(u32::try_from(step).unwrap_or(0)) / top).min(1.0);
                let level = linear_to_srgb(linear) * 255.0 * 256.0;
                u16::try_from(mathf::round_i32(level)).unwrap_or(u16::MAX)
            }),
        )?;
        Some(Self { table })
    }

    /// The opaque pixel at `(x, y)` showing `light`, each channel in
    /// `0.0..=1.0` of display-linear light.
    pub(crate) fn pixel(&self, light: Vec3, (x, y): (u32, u32)) -> Pixel {
        let bias = DitherRow::at(y).bias(x);
        let channel = |value: f64| {
            let level = self.level(value) + bias;
            u8::try_from(level >> 8).unwrap_or(u8::MAX)
        };
        Pixel {
            r: channel(light.x),
            g: channel(light.y),
            b: channel(light.z),
            a: u8::MAX,
        }
    }

    /// The sRGB level of display-linear `value`, in 8.8 fixed point.
    fn level(&self, value: f64) -> u32 {
        let position = value.clamp(0.0, 1.0) * f64::from(u32::try_from(STEPS - 1).unwrap_or(0));
        let below = mathf::floor(position);
        let step = usize::try_from(mathf::round_i32(below)).unwrap_or(0);
        let (low, high) = (
            self.table.get(step).copied().unwrap_or(0),
            self.table.get(step + 1).copied().unwrap_or(u16::MAX),
        );
        let blend = position - below;
        let level = f64::from(low) + (f64::from(high) - f64::from(low)) * blend;
        u32::try_from(mathf::round_i32(level)).unwrap_or(0)
    }
}

#[cfg(test)]
#[path = "tone_tests.rs"]
mod tests;
