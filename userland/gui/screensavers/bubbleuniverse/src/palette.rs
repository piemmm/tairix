//! The colours the curves are drawn in.
//!
//! The original colours each point by where it lies: red rising across the
//! curves, green along each curve, over a constant blue. The others run a ramp
//! of their own across the curves twice — once for a curve's first points,
//! once for its last — and carry each point from the one to the other as it
//! ages, mixed in CIE L\*a\*b\* so every ramp is even to the eye.

use tairix_colour::{srgb_to_linear, Lab, Rgb};
use tairix_util::mathf;

use crate::orbit::{CURVES, STEPS};

/// A palette the picture is drawn in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Palette {
    /// The original's colours.
    Original,
    /// Indigo and rose ageing to cyan, lavender and gold.
    Nebula,
    /// Deep teal and green ageing to aqua, mint and orchid.
    Aurora,
    /// Crimson and amber ageing to gold and cream.
    Ember,
    /// Navy and slate ageing to ice blue and white.
    Ice,
    /// The spectrum across the curves, each lightening as it ages.
    Spectrum,
}

/// The original's constant blue, in encoded levels.
const ORIGINAL_BLUE: usize = 99;

/// A designed palette: its ramp across the curves for young points, and for
/// old ones.
struct Ramps {
    young: &'static [Rgb],
    old: &'static [Rgb],
}

const NEBULA: Ramps = Ramps {
    young: &[
        Rgb::new(0x2A, 0x14, 0x6E),
        Rgb::new(0x6A, 0x1B, 0x9A),
        Rgb::new(0xC2, 0x18, 0x5B),
        Rgb::new(0xFF, 0x6F, 0x3C),
    ],
    old: &[
        Rgb::new(0x22, 0xD3, 0xEE),
        Rgb::new(0xA7, 0x8B, 0xFA),
        Rgb::new(0xF9, 0xA8, 0xD4),
        Rgb::new(0xFD, 0xE6, 0x8A),
    ],
};

const AURORA: Ramps = Ramps {
    young: &[
        Rgb::new(0x06, 0x4E, 0x3B),
        Rgb::new(0x0F, 0x76, 0x6E),
        Rgb::new(0x15, 0x80, 0x3D),
        Rgb::new(0x43, 0x38, 0xCA),
    ],
    old: &[
        Rgb::new(0x10, 0xE0, 0x9A),
        Rgb::new(0x22, 0xD3, 0xEE),
        Rgb::new(0x8B, 0x5C, 0xF6),
        Rgb::new(0xE8, 0x50, 0xF0),
    ],
};

const EMBER: Ramps = Ramps {
    young: &[
        Rgb::new(0x5C, 0x07, 0x07),
        Rgb::new(0x99, 0x1B, 0x1B),
        Rgb::new(0xC2, 0x41, 0x0C),
        Rgb::new(0xB4, 0x53, 0x09),
    ],
    old: &[
        Rgb::new(0xFB, 0x92, 0x3C),
        Rgb::new(0xFB, 0xBF, 0x24),
        Rgb::new(0xFD, 0xE0, 0x47),
        Rgb::new(0xFF, 0xF7, 0xD6),
    ],
};

const ICE: Ramps = Ramps {
    young: &[
        Rgb::new(0x0B, 0x1A, 0x4A),
        Rgb::new(0x1E, 0x3A, 0x8A),
        Rgb::new(0x0E, 0x74, 0x90),
        Rgb::new(0x31, 0x2E, 0x81),
    ],
    old: &[
        Rgb::new(0x93, 0xC5, 0xFD),
        Rgb::new(0x67, 0xE8, 0xF9),
        Rgb::new(0xE0, 0xF2, 0xFE),
        Rgb::new(0xC7, 0xD2, 0xFE),
    ],
};

const SPECTRUM: Ramps = Ramps {
    young: &[
        Rgb::new(0xB0, 0x10, 0x30),
        Rgb::new(0xB8, 0x6E, 0x00),
        Rgb::new(0x2E, 0x9A, 0x22),
        Rgb::new(0x00, 0x7C, 0xB8),
        Rgb::new(0x5A, 0x2C, 0xC8),
        Rgb::new(0xB0, 0x10, 0x30),
    ],
    old: &[
        Rgb::new(0xFF, 0x8A, 0x9A),
        Rgb::new(0xFF, 0xD3, 0x7A),
        Rgb::new(0x9C, 0xF5, 0x8C),
        Rgb::new(0x7A, 0xD8, 0xFF),
        Rgb::new(0xC6, 0xA6, 0xFF),
        Rgb::new(0xFF, 0x8A, 0x9A),
    ],
};

impl Palette {
    /// The palette option word `word` names.
    pub(crate) fn from_word(word: &str) -> Option<Self> {
        Some(match word {
            "original" => Self::Original,
            "nebula" => Self::Nebula,
            "aurora" => Self::Aurora,
            "ember" => Self::Ember,
            "ice" => Self::Ice,
            "spectrum" => Self::Spectrum,
            _ => return None,
        })
    }

    /// The light of every curve's every point, linear sRGB, curve by curve:
    /// `paint(curve, step, light)` for each.
    pub(crate) fn each(self, mut paint: impl FnMut(usize, usize, [f64; 3])) {
        match self.ramps() {
            None => {
                for curve in 0..CURVES {
                    for step in 0..STEPS {
                        paint(curve, step, original(curve, step));
                    }
                }
            }
            Some(ramps) => {
                for curve in 0..CURVES {
                    let along = share(curve, CURVES);
                    let (young, old) = (ramp(ramps.young, along), ramp(ramps.old, along));
                    for step in 0..STEPS {
                        let light = mix(young, old, share(step, STEPS)).to_xyz().to_linear();
                        paint(curve, step, light.map(|c| mathf::clamp(c, 0.0, 1.0)));
                    }
                }
            }
        }
    }

    const fn ramps(self) -> Option<&'static Ramps> {
        match self {
            Self::Original => None,
            Self::Nebula => Some(&NEBULA),
            Self::Aurora => Some(&AURORA),
            Self::Ember => Some(&EMBER),
            Self::Ice => Some(&ICE),
            Self::Spectrum => Some(&SPECTRUM),
        }
    }
}

/// The original's light at `step` along curve `curve`: the encoded levels
/// `(curve, step, 99)`.
fn original(curve: usize, step: usize) -> [f64; 3] {
    [curve, step, ORIGINAL_BLUE].map(|level| srgb_to_linear(mathf::real(level) / 255.0))
}

/// Where `index` lies among `count`, from nought at the first to one at the
/// last.
fn share(index: usize, count: usize) -> f64 {
    mathf::share(index, count.saturating_sub(1).max(1))
}

/// The colour `along` of the way through `stops`, spaced evenly.
fn ramp(stops: &[Rgb], along: f64) -> Lab {
    let last = stops.len().saturating_sub(1);
    let at = mathf::clamp(along, 0.0, 1.0) * mathf::real(last);
    let below = mathf::floor(at);
    let index = usize::try_from(mathf::round_i32(below))
        .unwrap_or(0)
        .min(last);
    let lab = |index: usize| {
        stops
            .get(index)
            .copied()
            .map_or(Lab::default(), Lab::from_rgb)
    };
    mix(lab(index), lab((index + 1).min(last)), at - below)
}

/// `from` carried `share` of the way to `to`.
fn mix(from: Lab, to: Lab, share: f64) -> Lab {
    let lerp = |a: f64, b: f64| a + (b - a) * share;
    Lab {
        l: lerp(from.l, to.l),
        a: lerp(from.a, to.a),
        b: lerp(from.b, to.b),
    }
}

#[cfg(test)]
#[path = "palette_tests.rs"]
mod tests;
