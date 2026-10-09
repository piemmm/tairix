//! The CIE spaces a colour is measured in — 1931 XYZ, L\*a\*b\* and its polar
//! form LCh(ab) — all under D65, the white sRGB is defined on.

use tairix_util::mathf;

use crate::rgb::Rgb;
use crate::srgb::{linear_to_srgb, srgb_to_linear};

/// sRGB's primaries in XYZ (IEC 61966-2-1).
const RGB_TO_XYZ: [[f64; 3]; 3] = [
    [0.412_456_4, 0.357_576_1, 0.180_437_5],
    [0.212_672_9, 0.715_152_2, 0.072_175_0],
    [0.019_333_9, 0.119_192_0, 0.950_304_1],
];

/// The inverse of [`RGB_TO_XYZ`].
const XYZ_TO_RGB: [[f64; 3]; 3] = [
    [3.240_454_2, -1.537_138_5, -0.498_531_4],
    [-0.969_266_0, 1.876_010_8, 0.041_556_0],
    [0.055_643_4, -0.204_025_9, 1.057_225_2],
];

/// The 6/29 the L\*a\*b\* transfer turns linear at.
const LAB_KNEE: f64 = 6.0 / 29.0;

/// A colour in CIE 1931 XYZ, Y the luminance: 1 at the D65 white.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Xyz {
    /// X.
    pub x: f64,
    /// Y, the luminance.
    pub y: f64,
    /// Z.
    pub z: f64,
}

impl Xyz {
    /// The D65 white, at luminance one: what sRGB's white measures.
    pub const D65: Self = Self {
        x: 0.950_47,
        y: 1.0,
        z: 1.088_83,
    };

    /// The colour of linear sRGB light `rgb`.
    #[must_use]
    pub fn from_linear(rgb: [f64; 3]) -> Self {
        let [x, y, z] = multiply(&RGB_TO_XYZ, rgb);
        Self { x, y, z }
    }

    /// This colour as linear sRGB light, outside `0.0..=1.0` where it lies
    /// outside the gamut.
    #[must_use]
    pub fn to_linear(self) -> [f64; 3] {
        multiply(&XYZ_TO_RGB, [self.x, self.y, self.z])
    }

    /// The colour of the encoded sRGB colour `rgb`.
    #[must_use]
    pub fn from_rgb(rgb: Rgb) -> Self {
        Self::from_linear(linear_of(rgb))
    }

    /// The CIE 1931 chromaticity `(x, y)`, or `None` for black, which has
    /// none.
    #[must_use]
    pub fn chromaticity(self) -> Option<(f64, f64)> {
        let sum = self.x + self.y + self.z;
        (sum > f64::EPSILON).then(|| (self.x / sum, self.y / sum))
    }
}

/// An encoded sRGB colour as linear light.
#[must_use]
pub fn linear_of(rgb: Rgb) -> [f64; 3] {
    [rgb.r, rgb.g, rgb.b].map(|channel| srgb_to_linear(f64::from(channel) / 255.0))
}

/// Linear light as it lands in 8-bit sRGB: the nearest colour, each channel
/// held to the gamut, and whether holding one there changed its level — a
/// channel past the gamut by less than half a level rounds to the same level
/// either way, so a colour on sRGB's edge reached by rounding is not clipped.
#[must_use]
pub fn encode_linear(linear: [f64; 3]) -> InGamut {
    // The transfer mirrored below black, so a channel just under it measures
    // how far under in encoded levels too.
    let level = |channel: f64| {
        let encoded = if channel < 0.0 {
            -linear_to_srgb(-channel)
        } else {
            linear_to_srgb(channel)
        };
        encoded * 255.0
    };
    let clipped = linear
        .iter()
        .any(|&channel| !(-0.5..=255.5).contains(&level(channel)));
    let [r, g, b] = linear.map(|channel| {
        let encoded = linear_to_srgb(mathf::clamp(channel, 0.0, 1.0));
        u8::try_from(mathf::round_i32(encoded * 255.0).clamp(0, 255)).unwrap_or(u8::MAX)
    });
    InGamut {
        rgb: Rgb::new(r, g, b),
        clipped,
    }
}

/// A colour as it lands in sRGB.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct InGamut {
    /// The nearest 8-bit colour, each channel held to the gamut.
    pub rgb: Rgb,
    /// Whether a channel lay outside the gamut far enough that holding it
    /// there changed its level.
    pub clipped: bool,
}

/// A colour in CIE L\*a\*b\* under D65: lightness from 0 to 100, and the
/// green–red and blue–yellow opponents.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Lab {
    /// Lightness, 0 to 100.
    pub l: f64,
    /// Green (negative) to red (positive).
    pub a: f64,
    /// Blue (negative) to yellow (positive).
    pub b: f64,
}

impl Lab {
    /// The L\*a\*b\* of `xyz`.
    #[must_use]
    pub fn from_xyz(xyz: Xyz) -> Self {
        let white = Xyz::D65;
        let fx = lab_f(xyz.x / white.x);
        let fy = lab_f(xyz.y / white.y);
        let fz = lab_f(xyz.z / white.z);
        Self {
            l: 116.0 * fy - 16.0,
            a: 500.0 * (fx - fy),
            b: 200.0 * (fy - fz),
        }
    }

    /// This colour in XYZ.
    #[must_use]
    pub fn to_xyz(self) -> Xyz {
        let white = Xyz::D65;
        let fy = (self.l + 16.0) / 116.0;
        Xyz {
            x: white.x * lab_f_inverse(fy + self.a / 500.0),
            y: white.y * lab_f_inverse(fy),
            z: white.z * lab_f_inverse(fy - self.b / 200.0),
        }
    }

    /// The L\*a\*b\* of the sRGB colour `rgb`.
    #[must_use]
    pub fn from_rgb(rgb: Rgb) -> Self {
        Self::from_xyz(Xyz::from_rgb(rgb))
    }

    /// This colour in sRGB, and whether it had to be clipped to get there.
    #[must_use]
    pub fn to_rgb(self) -> InGamut {
        encode_linear(self.to_xyz().to_linear())
    }
}

/// A colour in CIE LCh(ab): L\*a\*b\* by its lightness, its chroma and its
/// hue angle in degrees.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Lch {
    /// Lightness, 0 to 100.
    pub l: f64,
    /// Chroma: how far from grey.
    pub c: f64,
    /// Hue angle in degrees, `0.0..360.0`.
    pub h: f64,
}

impl Lch {
    /// `lab` in polar form; a grey keeps hue `0`.
    #[must_use]
    pub fn from_lab(lab: Lab) -> Self {
        let degrees = mathf::atan2(lab.b, lab.a).to_degrees();
        Self {
            l: lab.l,
            c: mathf::hypot(lab.a, lab.b),
            h: if degrees < 0.0 {
                degrees + 360.0
            } else {
                degrees
            },
        }
    }

    /// This colour in L\*a\*b\*.
    #[must_use]
    pub fn to_lab(self) -> Lab {
        let radians = self.h.to_radians();
        Lab {
            l: self.l,
            a: self.c * mathf::cos(radians),
            b: self.c * mathf::sin(radians),
        }
    }

    /// The `LCh` of the sRGB colour `rgb`.
    #[must_use]
    pub fn from_rgb(rgb: Rgb) -> Self {
        Self::from_lab(Lab::from_rgb(rgb))
    }

    /// This colour in sRGB, and whether it had to be clipped to get there.
    #[must_use]
    pub fn to_rgb(self) -> InGamut {
        self.to_lab().to_rgb()
    }
}

/// The L\*a\*b\* transfer: a cube root, linear near black.
fn lab_f(t: f64) -> f64 {
    if t > LAB_KNEE * LAB_KNEE * LAB_KNEE {
        mathf::exp(mathf::ln(t) / 3.0)
    } else {
        t / (3.0 * LAB_KNEE * LAB_KNEE) + 4.0 / 29.0
    }
}

/// The inverse of [`lab_f`].
fn lab_f_inverse(f: f64) -> f64 {
    if f > LAB_KNEE {
        f * f * f
    } else {
        3.0 * LAB_KNEE * LAB_KNEE * (f - 4.0 / 29.0)
    }
}

fn multiply(matrix: &[[f64; 3]; 3], [a, b, c]: [f64; 3]) -> [f64; 3] {
    matrix.map(|[x, y, z]| x * a + y * b + z * c)
}
