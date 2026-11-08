//! Colour spaces (ISO 32000-2 §8.6): what a colour's components mean, and
//! the sRGB colour they come to.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_collections::try_arc;
use tairix_colour::{linear_to_srgb, Cmyk, Fraction, Xyz};
use tairix_util::mathf;

use crate::document::Document;
use crate::function::{numbers, values, Function, Values, Work, MAX_COMPONENTS};
use crate::object::{owned, Dict, Object};
use crate::PdfError;

/// How deeply colour spaces nest: an Indexed or Separation space's base is a
/// colour space too.
const MAX_SPACE_DEPTH: usize = 4;

/// An encoded sRGB colour, each channel `0..=1`.
pub(crate) type Srgb = [f64; 3];

/// Bradford's cone response matrix, and its inverse, that adapt a colour seen
/// under one white to how it looks under another.
const BRADFORD: [f64; 9] = [
    0.8951, 0.2664, -0.1614, -0.7502, 1.7135, 0.0367, 0.0389, -0.0685, 1.0296,
];
const BRADFORD_INVERSE: [f64; 9] = [
    0.986_992_9,
    -0.147_054_3,
    0.159_962_7,
    0.432_305_3,
    0.518_360_3,
    0.049_291_2,
    -0.008_528_7,
    0.040_042_8,
    0.968_486_7,
];

/// A colour space.
#[derive(Debug)]
pub(crate) enum Space {
    Gray,
    Rgb,
    Cmyk,
    /// A calibrated space: `CalGray`, `CalRGB` or L\*a\*b\*, each read through CIE
    /// XYZ under its own white.
    Cie {
        kind: Cie,
        /// XYZ under its white into linear sRGB.
        to_linear: [f64; 9],
    },
    Indexed {
        base: Arc<Space>,
        high: u8,
        table: Vec<u8>,
    },
    /// `Separation` and `DeviceN`: tints through a function into another space,
    /// or nothing at all for the colorant `/None`.
    Tinted {
        components: usize,
        alternate: Arc<Space>,
        tint: Arc<Function>,
        paints: bool,
    },
    Pattern(Option<Arc<Space>>),
}

#[derive(Debug)]
pub(crate) enum Cie {
    Gray { white: [f64; 3], gamma: f64 },
    Rgb { gamma: [f64; 3], matrix: [f64; 9] },
    Lab { white: [f64; 3], range: [f64; 4] },
}

impl Space {
    /// The colour space `object` names or describes, a name looked up in
    /// `spaces` — a resources dictionary's colour spaces — where it is not a
    /// device space's; with none, only a device space may be named.
    pub(crate) fn load(
        document: &Document<'_>,
        object: &Object,
        spaces: Option<&Dict>,
    ) -> Result<Arc<Self>, PdfError> {
        Self::load_at(document, object, spaces, 0)
    }

    fn load_at(
        document: &Document<'_>,
        object: &Object,
        spaces: Option<&Dict>,
        depth: usize,
    ) -> Result<Arc<Self>, PdfError> {
        // No space in a file written in earnest nests deeper: past it lies
        // one that names itself.
        if depth > MAX_SPACE_DEPTH {
            return Err(PdfError::Malformed);
        }
        let object = document.resolve(object)?;
        if let Some(name) = object.as_name() {
            if let Some(device) = Self::device(name) {
                return Ok(try_arc(device)?);
            }
            let named = spaces
                .and_then(|spaces| spaces.get(name))
                .ok_or(PdfError::Malformed)?;
            return Self::load_at(document, named, spaces, depth + 1);
        }
        let items = object.as_array().ok_or(PdfError::Malformed)?;
        let (family, rest) = items.split_first().ok_or(PdfError::Malformed)?;
        let family = document.resolve(family)?;
        let family = family.as_name().ok_or(PdfError::Malformed)?;
        let parameter = |at: usize| rest.get(at).ok_or(PdfError::Malformed);
        let space = match family {
            b"CalGray" | b"CalRGB" | b"Lab" => {
                let dict = document.resolve(parameter(0)?)?;
                Self::cie(document, family, dict.as_dict().ok_or(PdfError::Malformed)?)?
            }
            b"ICCBased" => {
                let stream = document.resolve(parameter(0)?)?;
                let dict = stream.as_dict().ok_or(PdfError::Malformed)?;
                if let Some(alternate) = dict.get(b"Alternate") {
                    if let Ok(space) = Self::load_at(document, alternate, spaces, depth + 1) {
                        return Ok(space);
                    }
                }
                match document.get(dict, b"N")?.as_int() {
                    Some(1) => Self::Gray,
                    Some(3) => Self::Rgb,
                    Some(4) => Self::Cmyk,
                    _ => return Err(PdfError::Malformed),
                }
            }
            b"Indexed" | b"I" => {
                let base = Self::load_at(document, parameter(0)?, spaces, depth + 1)?;
                let high = document
                    .resolve(parameter(1)?)?
                    .as_int()
                    .and_then(|high| u8::try_from(high).ok());
                let high = high.ok_or(PdfError::Malformed)?;
                let table = match owned(document.resolve(parameter(2)?)?)? {
                    Object::String(bytes) => bytes,
                    Object::Stream(stream) => document.decode(&stream)?.bytes,
                    _ => return Err(PdfError::Malformed),
                };
                Self::Indexed { base, high, table }
            }
            b"Separation" | b"DeviceN" => {
                let names = document.resolve(parameter(0)?)?;
                let (components, paints) = if family == b"Separation" {
                    (1, names.as_name() != Some(b"None"))
                } else {
                    let names = names.as_array().ok_or(PdfError::Malformed)?;
                    let paints = names.iter().any(|name| name.as_name() != Some(b"None"));
                    (names.len(), paints)
                };
                if components == 0 || components > MAX_COMPONENTS {
                    return Err(PdfError::Malformed);
                }
                let alternate = Self::load_at(document, parameter(1)?, spaces, depth + 1)?;
                let tint = try_arc(Function::load(document, parameter(2)?)?)?;
                Self::Tinted {
                    components,
                    alternate,
                    tint,
                    paints,
                }
            }
            b"Pattern" => Self::Pattern(match rest.first() {
                Some(underlying) => Some(Self::load_at(document, underlying, spaces, depth + 1)?),
                None => None,
            }),
            name => Self::device(name).ok_or(PdfError::Unsupported)?,
        };
        Ok(try_arc(space)?)
    }

    /// The device space, or the pattern space, `name` names.
    fn device(name: &[u8]) -> Option<Self> {
        Some(match name {
            b"DeviceGray" | b"G" | b"CalGray" => Self::Gray,
            b"DeviceRGB" | b"RGB" | b"CalRGB" => Self::Rgb,
            b"DeviceCMYK" | b"CMYK" => Self::Cmyk,
            b"Pattern" => Self::Pattern(None),
            _ => return None,
        })
    }

    fn cie(document: &Document<'_>, family: &[u8], dict: &Dict) -> Result<Self, PdfError> {
        let triple = |key: &[u8], default: [f64; 3]| -> Result<[f64; 3], PdfError> {
            Ok(match numbers(document, dict, key)? {
                Some(values) => {
                    <[f64; 3]>::try_from(values.as_slice()).map_err(|_| PdfError::Malformed)?
                }
                None => default,
            })
        };
        let white = triple(b"WhitePoint", [0.0; 3])?;
        if white[0] <= 0.0 || white[2] <= 0.0 || (white[1] - 1.0).abs() > 1e-6 {
            return Err(PdfError::Malformed);
        }
        let kind = match family {
            b"CalGray" => Cie::Gray {
                white,
                gamma: document
                    .get(dict, b"Gamma")?
                    .as_number()
                    .filter(|gamma| *gamma > 0.0)
                    .unwrap_or(1.0),
            },
            b"CalRGB" => Cie::Rgb {
                gamma: triple(b"Gamma", [1.0; 3])?,
                matrix: match numbers(document, dict, b"Matrix")? {
                    Some(values) => {
                        <[f64; 9]>::try_from(values.as_slice()).map_err(|_| PdfError::Malformed)?
                    }
                    None => [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
                },
            },
            _ => Cie::Lab {
                white,
                range: match numbers(document, dict, b"Range")? {
                    Some(values) => match <[f64; 4]>::try_from(values.as_slice()) {
                        Ok(range) if range[0] <= range[1] && range[2] <= range[3] => range,
                        _ => return Err(PdfError::Malformed),
                    },
                    None => [-100.0, 100.0, -100.0, 100.0],
                },
            },
        };
        Ok(Self::Cie {
            kind,
            to_linear: adapted_to_linear(white),
        })
    }

    /// How many components a colour in it has.
    pub(crate) fn components(&self) -> usize {
        match self {
            Self::Gray | Self::Indexed { .. } | Self::Pattern(_) => 1,
            Self::Rgb => 3,
            Self::Cmyk => 4,
            Self::Cie { kind, .. } => match kind {
                Cie::Gray { .. } => 1,
                Cie::Rgb { .. } | Cie::Lab { .. } => 3,
            },
            Self::Tinted { components, .. } => *components,
        }
    }

    /// The colour `cs` sets: black, or a full tint.
    pub(crate) fn initial(&self) -> Values {
        match self {
            Self::Cmyk => values([0.0, 0.0, 0.0, 1.0]),
            Self::Tinted { components, .. } => values(core::iter::repeat_n(1.0, *components)),
            Self::Cie {
                kind: Cie::Lab { range, .. },
                ..
            } => values([
                0.0,
                0.0f64.clamp(range[0], range[1]),
                0.0f64.clamp(range[2], range[3]),
            ]),
            _ => values(core::iter::repeat_n(0.0, self.components())),
        }
    }

    /// The sRGB colour `components` come to, or `None` where it paints
    /// nothing, a tint transform's steps charged to `work`. A component left
    /// out is taken as zero.
    ///
    /// # Errors
    ///
    /// [`PdfError::TooComplex`] once `work` is spent.
    pub(crate) fn srgb(&self, components: &[f64], work: &Work) -> Result<Option<Srgb>, PdfError> {
        let at = |index: usize| {
            components
                .get(index)
                .copied()
                .unwrap_or(0.0)
                .clamp(0.0, 1.0)
        };
        Ok(Some(match self {
            Self::Gray | Self::Pattern(_) => [at(0); 3],
            Self::Rgb => [at(0), at(1), at(2)],
            Self::Cmyk => {
                let ink = |index: usize| Fraction::from_f64(at(index));
                let rgb = Cmyk::new(ink(0), ink(1), ink(2), ink(3)).to_rgb();
                [rgb.r, rgb.g, rgb.b].map(|channel| f64::from(channel) / 255.0)
            }
            Self::Cie { kind, to_linear } => cie_srgb(kind, to_linear, components),
            Self::Indexed { base, high, table } => {
                let index = mathf::round(components.first().copied().unwrap_or(0.0))
                    .clamp(0.0, f64::from(*high));
                let start = index_of(index) * base.components();
                let Some(entry) = table.get(start..start + base.components()) else {
                    return Ok(None);
                };
                let decoded = values(
                    entry
                        .iter()
                        .enumerate()
                        .map(|(component, &byte)| base.component_of_byte(component, byte)),
                );
                return base.srgb(&decoded, work);
            }
            Self::Tinted {
                alternate,
                tint,
                paints,
                ..
            } => {
                if !paints {
                    return Ok(None);
                }
                let mut tinted = Values::new();
                tint.evaluate(components, work, &mut tinted)?;
                return alternate.srgb(&tinted, work);
            }
        }))
    }

    /// Component `component`'s value an Indexed table's `byte` stands for: the
    /// byte scaled to the component's range.
    fn component_of_byte(&self, component: usize, byte: u8) -> f64 {
        let unit = f64::from(byte) / 255.0;
        match self {
            Self::Cie {
                kind: Cie::Lab { range, .. },
                ..
            } => match component {
                0 => unit * 100.0,
                1 => range[0] + unit * (range[1] - range[0]),
                _ => range[2] + unit * (range[3] - range[2]),
            },
            _ => unit,
        }
    }

    /// The range each component of an image sample is decoded into, where its
    /// samples are `bits` deep: what an image's `/Decode` defaults to.
    pub(crate) fn decode_ranges(&self, bits: u32) -> Vec<(f64, f64)> {
        match self {
            Self::Indexed { .. } => alloc::vec![(0.0, f64::from((1u32 << bits.min(16)) - 1))],
            Self::Cie {
                kind: Cie::Lab { range, .. },
                ..
            } => alloc::vec![(0.0, 100.0), (range[0], range[1]), (range[2], range[3])],
            _ => alloc::vec![(0.0, 1.0); self.components()],
        }
    }
}

/// An index already rounded and held within `0..=255`, as an index.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "rounded and held within 0..=255 before the cast"
)]
fn index_of(index: f64) -> usize {
    index as usize
}

/// A calibrated colour's components as sRGB.
fn cie_srgb(kind: &Cie, to_linear: &[f64; 9], components: &[f64]) -> Srgb {
    let at = |index: usize| components.get(index).copied().unwrap_or(0.0);
    let xyz = match kind {
        Cie::Gray { white, gamma } => {
            let luminance = mathf::power(at(0).clamp(0.0, 1.0), *gamma);
            white.map(|channel| channel * luminance)
        }
        Cie::Rgb { gamma, matrix } => {
            let [a, b, c] =
                [0, 1, 2].map(|index| mathf::power(at(index).clamp(0.0, 1.0), gamma[index]));
            [0, 1, 2].map(|row| matrix[row] * a + matrix[3 + row] * b + matrix[6 + row] * c)
        }
        Cie::Lab { white, range } => {
            let lightness = at(0).clamp(0.0, 100.0);
            let a = at(1).clamp(range[0], range[1]);
            let b = at(2).clamp(range[2], range[3]);
            let m = (lightness + 16.0) / 116.0;
            let curve = |x: f64| {
                if x >= 6.0 / 29.0 {
                    x * x * x
                } else {
                    108.0 / 841.0 * (x - 4.0 / 29.0)
                }
            };
            [
                white[0] * curve(m + a / 500.0),
                white[1] * curve(m),
                white[2] * curve(m - b / 200.0),
            ]
        }
    };
    let linear = multiply(to_linear, xyz);
    linear.map(|channel| linear_to_srgb(channel.clamp(0.0, 1.0)))
}

/// The matrix taking XYZ seen under `white` to linear sRGB: Bradford's
/// adaptation to D65, then sRGB's own primaries.
fn adapted_to_linear(white: [f64; 3]) -> [f64; 9] {
    let source = multiply(&BRADFORD, white);
    let target = multiply(&BRADFORD, [Xyz::D65.x, Xyz::D65.y, Xyz::D65.z]);
    let gain = [0, 1, 2].map(|index| target[index] / source[index]);
    let mut scaled = BRADFORD;
    for (index, value) in scaled.iter_mut().enumerate() {
        *value *= gain[index / 3];
    }
    let adapt = product(&BRADFORD_INVERSE, &scaled);
    // Each column the linear sRGB of one XYZ axis, adapted.
    let mut to_linear = [0.0; 9];
    for axis in 0..3 {
        let column = [adapt[axis], adapt[3 + axis], adapt[6 + axis]];
        let linear = Xyz {
            x: column[0],
            y: column[1],
            z: column[2],
        }
        .to_linear();
        for row in 0..3 {
            to_linear[row * 3 + axis] = linear[row];
        }
    }
    to_linear
}

/// `matrix`, row major, times `vector`.
fn multiply(matrix: &[f64; 9], vector: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|row| {
        matrix[row * 3] * vector[0]
            + matrix[row * 3 + 1] * vector[1]
            + matrix[row * 3 + 2] * vector[2]
    })
}

/// `left` times `right`, both row major.
fn product(left: &[f64; 9], right: &[f64; 9]) -> [f64; 9] {
    let mut out = [0.0; 9];
    for row in 0..3 {
        for column in 0..3 {
            out[row * 3 + column] = (0..3)
                .map(|k| left[row * 3 + k] * right[k * 3 + column])
                .sum();
        }
    }
    out
}

#[cfg(test)]
#[path = "colour_tests.rs"]
mod tests;
