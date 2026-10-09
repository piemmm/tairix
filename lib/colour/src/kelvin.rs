//! Correlated colour temperature: the white of a light named by its
//! temperature in kelvin and its tint off the Planckian locus, and the
//! temperature and tint a colour lies at.
//!
//! The locus is Krystek's rational fit in the CIE 1960 uv plane, "An algorithm
//! to calculate correlated colour temperature" (Color Research & Application,
//! 1985), within 8e-5 of the true locus from 1000 K to 15000 K and smooth
//! across it, so a light off the locus measures back to the temperature it was
//! named by; the tint is Duv, the signed distance from the locus in that plane,
//! positive towards green.

use tairix_util::mathf;

use crate::cie::Xyz;

/// The warmest (reddest) temperature the locus fit holds, in kelvin.
pub const KELVIN_MIN: f64 = 1000.0;

/// The coolest (bluest) temperature the locus fit holds, in kelvin.
pub const KELVIN_MAX: f64 = 15000.0;

/// The mired steps a temperature search samples the locus at before it
/// refines: about one mired apart across the whole fit.
const SEARCH_STEPS: u32 = 935;

/// How many times a search halves its bracket after sampling.
const REFINE_STEPS: u32 = 40;

/// A light's white: its correlated colour temperature and how far it lies off
/// the Planckian locus.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Illuminant {
    /// The correlated colour temperature, in kelvin, within
    /// [`KELVIN_MIN`]`..=`[`KELVIN_MAX`].
    pub kelvin: f64,
    /// Duv: the distance off the locus in the CIE 1960 uv plane, positive
    /// towards green and negative towards magenta.
    pub duv: f64,
}

impl Illuminant {
    /// The light `kelvin` degrees warm, `duv` off the locus; the temperature
    /// is held to the fit's range.
    #[must_use]
    pub fn new(kelvin: f64, duv: f64) -> Self {
        Self {
            kelvin: mathf::clamp(kelvin, KELVIN_MIN, KELVIN_MAX),
            duv,
        }
    }

    /// This light's white in the CIE 1960 uv plane.
    #[must_use]
    pub fn uv(self) -> (f64, f64) {
        let (u, v) = locus_uv(self.kelvin);
        let (nu, nv) = normal(self.kelvin);
        (u + self.duv * nu, v + self.duv * nv)
    }

    /// The CIE 1931 chromaticity `(x, y)` of this light's white.
    #[must_use]
    pub fn chromaticity(self) -> (f64, f64) {
        chromaticity_of(self.uv())
    }

    /// This light's white as linear sRGB at luminance one.
    #[must_use]
    pub fn white(self) -> [f64; 3] {
        white_of(self.chromaticity())
    }

    /// The temperature and tint the linear sRGB colour `linear` lies at, or
    /// `None` for black, which has no colour to measure.
    #[must_use]
    pub fn of_linear(linear: [f64; 3]) -> Option<Self> {
        let chromaticity = Xyz::from_linear(linear).chromaticity()?;
        Some(Self::of_uv(uv_of(chromaticity)?))
    }

    /// The light whose white lies at `at` in the CIE 1960 uv plane: the
    /// temperature of the point of the locus nearest it, and how far off the
    /// locus it lies there.
    #[must_use]
    pub fn of_uv(at: (f64, f64)) -> Self {
        let distance = |mireds: f64| {
            let locus = locus_uv(1e6 / mireds);
            (at.0 - locus.0) * (at.0 - locus.0) + (at.1 - locus.1) * (at.1 - locus.1)
        };
        let (least, most) = (1e6 / KELVIN_MAX, 1e6 / KELVIN_MIN);
        let step = (most - least) / f64::from(SEARCH_STEPS);
        let nearest = (0..=SEARCH_STEPS)
            .map(|index| least + step * f64::from(index))
            .fold((least, f64::INFINITY), |best, mireds| {
                let off = distance(mireds);
                if off < best.1 {
                    (mireds, off)
                } else {
                    best
                }
            })
            .0;
        let (mut low, mut high) = (
            mathf::fmax(nearest - step, least),
            mathf::fmin(nearest + step, most),
        );
        for _ in 0..REFINE_STEPS {
            let third = (high - low) / 3.0;
            let (nearer, farther) = (low + third, high - third);
            if distance(nearer) < distance(farther) {
                high = farther;
            } else {
                low = nearer;
            }
        }
        let kelvin = 1e6 / low.midpoint(high);
        let locus = locus_uv(kelvin);
        let toward = normal(kelvin);
        Self::new(
            kelvin,
            (at.0 - locus.0) * toward.0 + (at.1 - locus.1) * toward.1,
        )
    }
}

/// The CIE 1960 `(u, v)` of chromaticity `(x, y)`, or `None` at the
/// degenerate point where the projection divides by nothing.
#[must_use]
pub fn uv_of((x, y): (f64, f64)) -> Option<(f64, f64)> {
    let denominator = -2.0 * x + 12.0 * y + 3.0;
    (denominator.abs() > f64::EPSILON).then(|| (4.0 * x / denominator, 6.0 * y / denominator))
}

/// The chromaticity `(x, y)` of CIE 1960 `(u, v)`.
#[must_use]
pub fn chromaticity_of((u, v): (f64, f64)) -> (f64, f64) {
    let denominator = 2.0 * u - 8.0 * v + 4.0;
    (3.0 * u / denominator, 2.0 * v / denominator)
}

/// The white of chromaticity `(x, y)` as linear sRGB at luminance one.
#[must_use]
pub fn white_of((x, y): (f64, f64)) -> [f64; 3] {
    Xyz {
        x: x / y,
        y: 1.0,
        z: (1.0 - x - y) / y,
    }
    .to_linear()
}

/// The Planckian locus at `kelvin`, in CIE 1960 `(u, v)`.
fn locus_uv(kelvin: f64) -> (f64, f64) {
    let t = mathf::clamp(kelvin, KELVIN_MIN, KELVIN_MAX);
    let t2 = t * t;
    let u = (0.860_117_757 + 1.541_182_54e-4 * t + 1.286_412_12e-7 * t2)
        / (1.0 + 8.424_202_35e-4 * t + 7.081_451_63e-7 * t2);
    let v = (0.317_398_726 + 4.228_062_45e-5 * t + 4.204_816_91e-8 * t2)
        / (1.0 - 2.897_418_16e-5 * t + 1.614_560_53e-7 * t2);
    (u, v)
}

/// The unit normal to the locus at `kelvin`, pointing towards green.
fn normal(kelvin: f64) -> (f64, f64) {
    let reach = kelvin * 1e-4;
    let (u0, v0) = locus_uv(kelvin - reach);
    let (u1, v1) = locus_uv(kelvin + reach);
    let (du, dv) = (u1 - u0, v1 - v0);
    let length = mathf::hypot(du, dv);
    if length <= f64::EPSILON {
        return (0.0, 1.0);
    }
    // As the temperature rises the locus runs towards lower u, so its
    // direction turned a quarter clockwise points to higher v: towards green.
    (dv / length, -du / length)
}
