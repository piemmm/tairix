//! Texels as the software device stores them and as a shader reads them.
//!
//! A texel is up to four little-endian words as stored, and four 32-bit
//! values as read: a float component's bits, an integer component's value.
//! A channel a format lacks reads as zero, alpha as one.

use tairix_colour::srgb_to_linear;
use tairix_spirv::Component;
use tairix_util::mathf::{from_half, single, to_half};

use crate::format::{Encoding, Format};

/// One plane of a texel: its colour, or one aspect of depth and stencil.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Plane {
    Colour,
    Depth,
    Stencil,
}

/// The sRGB transfer at the 256 codes of an 8-bit channel, and the linear
/// values where the encoding of one code gives way to the next: what
/// `tairix_colour` defines, tabulated once a device.
pub(crate) struct Srgb {
    linear: [f32; 256],
    thresholds: [f32; 255],
}

impl Srgb {
    pub(crate) fn new() -> Self {
        Self {
            linear: core::array::from_fn(|code| single(srgb_to_linear(byte_share(code, 0.0)))),
            thresholds: core::array::from_fn(|code| single(srgb_to_linear(byte_share(code, 0.5)))),
        }
    }

    fn to_linear(&self, code: u32) -> f32 {
        self.linear
            .get(usize::try_from(code).unwrap_or(0))
            .copied()
            .unwrap_or(1.0)
    }

    /// The code `linear` encodes to, rounded to nearest: the count of
    /// thresholds it reaches. A `NaN` reaches none.
    fn to_code(&self, linear: f32) -> u32 {
        let reached = self.thresholds.partition_point(|&t| t <= linear);
        u32::try_from(reached).unwrap_or(255)
    }
}

/// `(code + offset) / 255`.
fn byte_share(code: usize, offset: f64) -> f64 {
    (f64::from(u8::try_from(code).unwrap_or(u8::MAX)) + offset) / 255.0
}

/// The bits of a single one.
pub(crate) const ONE: u32 = 0x3f80_0000;

/// The value of a channel a format lacks: zero, and one for alpha.
pub(crate) const fn missing(component: Component) -> [u32; 4] {
    match component {
        Component::Float => [0, 0, 0, ONE],
        Component::Sint | Component::Uint => [0, 0, 0, 1],
    }
}

/// The low `bits` bits.
const fn mask(bits: u32) -> u32 {
    if bits >= 32 {
        u32::MAX
    } else {
        (1 << bits) - 1
    }
}

/// Channel `index` of `bits`-bit channels packed from the first word's low
/// bit.
fn channel(raw: &[u32; 4], bits: u32, index: u32) -> u32 {
    let bit = index * bits;
    let word = raw.get((bit / 32) as usize).copied().unwrap_or(0);
    (word >> (bit % 32)) & mask(bits)
}

fn set_channel(raw: &mut [u32; 4], bits: u32, index: u32, value: u32) {
    let bit = index * bits;
    if let Some(word) = raw.get_mut((bit / 32) as usize) {
        *word |= (value & mask(bits)) << (bit % 32);
    }
}

/// `value`, a `bits`-bit two's complement integer, sign-extended.
const fn extend(value: u32, bits: u32) -> i32 {
    let shift = 32 - bits;
    (value << shift).cast_signed() >> shift
}

/// The bits of `channel` of a stored `encoding`, as the shader reads it.
fn read_channel(encoding: Encoding, bits: u32, stored: u32, srgb: &Srgb, alpha: bool) -> u32 {
    let max = mask(bits);
    match encoding {
        Encoding::Unorm8 | Encoding::Unorm16 | Encoding::Depth16 => unorm_to_float(stored, max),
        Encoding::Srgb8 if alpha => unorm_to_float(stored, max),
        Encoding::Srgb8 => srgb.to_linear(stored).to_bits(),
        Encoding::Snorm8 | Encoding::Snorm16 => snorm_to_float(extend(stored, bits), max >> 1),
        Encoding::Sint8 | Encoding::Sint16 => extend(stored, bits).cast_unsigned(),
        Encoding::Float16 => from_half(u16::try_from(stored).unwrap_or(0)).to_bits(),
        _ => stored,
    }
}

/// The stored bits of `value`, read as the shader holds it, in a
/// `bits`-bit channel of `encoding`.
fn write_channel(encoding: Encoding, bits: u32, value: u32, srgb: &Srgb, alpha: bool) -> u32 {
    let max = mask(bits);
    let float = f32::from_bits(value);
    match encoding {
        Encoding::Unorm8 | Encoding::Unorm16 | Encoding::Depth16 => float_to_unorm(float, max),
        Encoding::Srgb8 if alpha => float_to_unorm(float, max),
        Encoding::Srgb8 => srgb.to_code(float),
        Encoding::Snorm8 | Encoding::Snorm16 => float_to_snorm(float, max >> 1) & max,
        Encoding::Float16 => u32::from(to_half(float)),
        _ => value & max,
    }
}

/// Plane `plane` of the texel `raw` of `format`, as a shader reads it.
pub(crate) fn decode(format: Format, plane: Plane, raw: [u32; 4], srgb: &Srgb) -> [u32; 4] {
    let encoding = format.encoding();
    let mut out = missing(format.component());
    match (encoding, plane) {
        (Encoding::Depth32Stencil8, Plane::Stencil) => {
            return [raw[1] & 0xff, 0, 0, 1];
        }
        (Encoding::Depth32Stencil8, _) => out[0] = raw[0],
        (Encoding::Rgb10A2Unorm, _) => {
            for index in 0..3 {
                out[index as usize] = unorm_to_float(channel(&raw, 10, index), 1023);
            }
            out[3] = unorm_to_float(raw[0] >> 30, 3);
        }
        (Encoding::Rgb10A2Uint, _) => {
            for index in 0..3 {
                out[index as usize] = channel(&raw, 10, index);
            }
            out[3] = raw[0] >> 30;
        }
        (Encoding::Rg11B10Float, _) => {
            out[0] = small_float(raw[0] & 0x7ff, 6).to_bits();
            out[1] = small_float((raw[0] >> 11) & 0x7ff, 6).to_bits();
            out[2] = small_float(raw[0] >> 22, 5).to_bits();
        }
        (Encoding::Rgb9E5Float, _) => {
            let scale = power_of_two(exponent_of(raw[0] >> 27) - 15 - 9);
            for index in 0..3 {
                out[index as usize] = (mantissa_of(channel(&raw, 9, index)) * scale).to_bits();
            }
        }
        _ => {
            let bits = channel_bits(encoding);
            let channels = format.channels();
            for index in 0..channels {
                let alpha = index == 3;
                let stored = channel(&raw, bits, index);
                if let Some(slot) = out.get_mut(index as usize) {
                    *slot = read_channel(encoding, bits, stored, srgb, alpha);
                }
            }
            if format.reversed() {
                out.swap(0, 2);
            }
        }
    }
    out
}

/// The texel of `format` that holds `value`, read as a shader holds it, in
/// plane `plane`; a depth-stencil texel's other plane is left zero.
pub(crate) fn encode(format: Format, plane: Plane, value: [u32; 4], srgb: &Srgb) -> [u32; 4] {
    let encoding = format.encoding();
    let mut raw = [0; 4];
    match (encoding, plane) {
        (Encoding::Depth32Stencil8, Plane::Stencil) => raw[1] = value[0] & 0xff,
        (Encoding::Depth32Stencil8, _) => raw[0] = value[0],
        (Encoding::Rgb10A2Unorm, _) => {
            for index in 0..3 {
                let stored = float_to_unorm(f32::from_bits(value[index as usize]), 1023);
                set_channel(&mut raw, 10, index, stored);
            }
            raw[0] |= float_to_unorm(f32::from_bits(value[3]), 3) << 30;
        }
        (Encoding::Rgb10A2Uint, _) => {
            for index in 0..3 {
                set_channel(&mut raw, 10, index, value[index as usize]);
            }
            raw[0] |= (value[3] & 3) << 30;
        }
        (Encoding::Rg11B10Float, _) => {
            raw[0] = to_small_float(f32::from_bits(value[0]), 6)
                | to_small_float(f32::from_bits(value[1]), 6) << 11
                | to_small_float(f32::from_bits(value[2]), 5) << 22;
        }
        (Encoding::Rgb9E5Float, _) => raw[0] = to_shared_exponent(value),
        _ => {
            let bits = channel_bits(encoding);
            let mut value = value;
            if format.reversed() {
                value.swap(0, 2);
            }
            for index in 0..format.channels() {
                let alpha = index == 3;
                let read = value.get(index as usize).copied().unwrap_or(0);
                set_channel(
                    &mut raw,
                    bits,
                    index,
                    write_channel(encoding, bits, read, srgb, alpha),
                );
            }
        }
    }
    raw
}

/// The bits of each channel of an encoding packed channel by channel.
const fn channel_bits(encoding: Encoding) -> u32 {
    match encoding {
        Encoding::Unorm8
        | Encoding::Snorm8
        | Encoding::Uint8
        | Encoding::Sint8
        | Encoding::Srgb8
        | Encoding::Stencil8 => 8,
        Encoding::Unorm16
        | Encoding::Snorm16
        | Encoding::Uint16
        | Encoding::Sint16
        | Encoding::Float16
        | Encoding::Depth16 => 16,
        _ => 32,
    }
}

/// `stored / max`, `max` at most 65535: exact in single precision but for
/// the one rounding of the division.
fn unorm_to_float(stored: u32, max: u32) -> u32 {
    (small(stored) / small(max)).to_bits()
}

/// `stored / max`, and never below minus one.
fn snorm_to_float(stored: i32, max: u32) -> u32 {
    let value = small_signed(stored) / small(max);
    if value < -1.0 { -1.0f32 } else { value }.to_bits()
}

/// A value of at most sixteen bits as a float, exactly.
fn small(value: u32) -> f32 {
    f32::from(u16::try_from(value).unwrap_or(u16::MAX))
}

/// A signed value within sixteen bits as a float, exactly.
fn small_signed(value: i32) -> f32 {
    f32::from(i16::try_from(value).unwrap_or(if value < 0 { i16::MIN } else { i16::MAX }))
}

/// `value` clamped into `0..=1` and scaled to `0..=max`, rounded to nearest;
/// a `NaN` is zero.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the scaled value is clamped into 0..=max, at most 65535.5, first"
)]
fn float_to_unorm(value: f32, max: u32) -> u32 {
    let clamped = if value > 0.0 {
        if value < 1.0 {
            value
        } else {
            1.0
        }
    } else {
        0.0
    };
    (clamped * small(max) + 0.5) as u32
}

/// `value` clamped into `-1..=1` and scaled to `-max..=max`, rounded half
/// away from zero, as two's complement bits; a `NaN` is zero.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the scaled value is clamped into -max..=max, within 32767.5, first"
)]
fn float_to_snorm(value: f32, max: u32) -> u32 {
    let clamped = if value > -1.0 {
        if value < 1.0 {
            value
        } else {
            1.0
        }
    } else if value <= -1.0 {
        -1.0
    } else {
        0.0
    };
    let scaled = clamped * small(max);
    let rounded = if scaled >= 0.0 {
        (scaled + 0.5) as i32
    } else {
        (scaled - 0.5) as i32
    };
    rounded.cast_unsigned()
}

/// `2^exponent` for an exponent of a normal single.
fn power_of_two(exponent: i32) -> f32 {
    let biased = u32::try_from(exponent.saturating_add(127).clamp(1, 254)).unwrap_or(127);
    f32::from_bits(biased << 23)
}

fn exponent_of(bits: u32) -> i32 {
    i32::try_from(bits & 0x1f).unwrap_or(0)
}

fn mantissa_of(bits: u32) -> f32 {
    small(bits & 0x1ff)
}

/// An unsigned float of a five-bit exponent biased by 15 and a
/// `mantissa_bits`-bit mantissa, exactly.
fn small_float(bits: u32, mantissa_bits: u32) -> f32 {
    let exponent = (bits >> mantissa_bits) & 0x1f;
    let mantissa = bits & mask(mantissa_bits);
    let shift = 23 - mantissa_bits;
    match exponent {
        0x1f if mantissa == 0 => f32::INFINITY,
        0x1f => f32::NAN,
        0 => small(mantissa) * power_of_two(-14 - i32::try_from(mantissa_bits).unwrap_or(0)),
        _ => f32::from_bits(((exponent + (127 - 15)) << 23) | (mantissa << shift)),
    }
}

/// `value` as an unsigned float of a five-bit exponent and a
/// `mantissa_bits`-bit mantissa, rounded to nearest even: a negative is zero,
/// a finite value past the largest is the largest, and infinity and `NaN`
/// keep.
fn to_small_float(value: f32, mantissa_bits: u32) -> u32 {
    let infinity = 0x1f << mantissa_bits;
    if value.is_nan() {
        return infinity | 1;
    }
    if value == f32::INFINITY {
        return infinity;
    }
    if value <= 0.0 {
        return 0;
    }
    let bits = value.to_bits();
    let exponent = bits >> 23;
    let largest = infinity - 1;
    if exponent >= 127 + 16 {
        return largest;
    }
    let lowest = 127 - 15 - mantissa_bits;
    if exponent < lowest {
        return 0;
    }
    let normal = exponent >= 127 - 14;
    let drop = 23 - mantissa_bits;
    let shift = if normal {
        drop
    } else {
        drop + (127 - 14 - exponent)
    };
    let significand = (bits & 0x007f_ffff) | 0x0080_0000;
    let kept = significand >> shift;
    let rest = significand & mask(shift);
    let halfway = 1 << (shift - 1);
    let up = rest > halfway || (rest == halfway && kept & 1 == 1);
    let rounded = kept + u32::from(up);
    let encoded = if normal {
        ((exponent - (127 - 15)) << mantissa_bits) + rounded - (1 << mantissa_bits)
    } else {
        rounded
    };
    encoded.min(largest)
}

/// Three channels as nine-bit mantissas sharing a five-bit exponent, the
/// shared exponent the least that keeps the largest channel's mantissa in
/// nine bits; a channel below zero or `NaN` is zero, and one past the
/// largest representable is the largest.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "each scaled channel is within 0..=511.5, which the exponent's choice guarantees"
)]
fn to_shared_exponent(value: [u32; 4]) -> u32 {
    const LARGEST: f32 = 511.0 / 512.0 * 65536.0;
    let clamp = |bits: u32| {
        let channel = f32::from_bits(bits);
        if channel > 0.0 {
            if channel < LARGEST {
                channel
            } else {
                LARGEST
            }
        } else {
            0.0
        }
    };
    let rgb = [clamp(value[0]), clamp(value[1]), clamp(value[2])];
    let largest = rgb.iter().fold(0.0f32, |m, &c| if c > m { c } else { m });
    let floor_log2 = if largest > 0.0 {
        i32::try_from(largest.to_bits() >> 23).unwrap_or(0) - 127
    } else {
        -16
    };
    let mut exponent = floor_log2.max(-16) + 1 + 15;
    let scaled_largest = (largest / power_of_two(exponent - 15 - 9) + 0.5) as u32;
    if scaled_largest == 512 {
        exponent += 1;
    }
    let scale = power_of_two(exponent - 15 - 9);
    let mut packed = u32::try_from(exponent.clamp(0, 31)).unwrap_or(0) << 27;
    for (index, channel) in rgb.iter().enumerate() {
        let mantissa = ((channel / scale + 0.5) as u32).min(511);
        packed |= mantissa << (9 * index);
    }
    packed
}

#[cfg(test)]
#[path = "texel_tests.rs"]
mod tests;
