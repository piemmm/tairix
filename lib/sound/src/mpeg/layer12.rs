//! Layers I and II (ISO/IEC 11172-3 §2.4.1.5–6 and §2.4.3.2–3): each
//! subband's allocation and scalefactors, then its samples requantised.
//!
//! Both layers requantise a code `v` of a quantiser of `steps` levels to
//! `(2v + 1 − steps) / steps`: the standard's two-step form
//! (`c × (s‴ + d)`, Table 3-B.4) reduced to the value it computes, so no
//! rounded constant stands between a code and its sample.

use super::frame::{Header, Mode, CRC_LEN, HEADER_LEN};
use super::synth::SUBBANDS;
use super::tables::{alloc_row, AllocRow, QUANT_CLASSES};
use crate::bits::{BitReader, Exhausted};
use crate::crc::crc16_bits;
use crate::single;
use crate::DecodeError;

/// Slots a frame of each layer holds per channel.
pub(super) const LAYER1_SLOTS: usize = 12;
pub(super) const LAYER2_SLOTS: usize = 36;

/// The subband samples one frame decodes to, slot by slot, per channel.
pub(super) type Slots = [[[f32; SUBBANDS]; LAYER2_SLOTS]; 2];

/// `2^(1 − index/3)`, the scalefactor `index` stands for; 63 is none.
fn scalefactor(index: u32) -> Result<f32, DecodeError> {
    const THIRDS: [f64; 3] = [1.0, 0.793_700_525_984_099_7, 0.629_960_524_947_436_6];
    if index >= 63 {
        return Err(DecodeError::MpegFrameInvalid);
    }
    let tier = index / 3;
    let power = if tier <= 1 {
        f64::from(1u32 << (1 - tier))
    } else {
        1.0 / f64::from(1u32 << (tier - 1))
    };
    Ok(single(power * THIRDS[(index % 3) as usize]))
}

fn requantise(code: u32, steps: u32) -> f32 {
    single((2.0 * f64::from(code) + 1.0 - f64::from(steps)) / f64::from(steps))
}

/// Subbands below which each channel has its own allocation: the bound
/// joint stereo states, or every subband.
fn bound(header: Header, limit: usize) -> usize {
    match header.mode {
        Mode::JointStereo => (4 + 4 * usize::from(header.mode_extension)).min(limit),
        _ => limit,
    }
}

/// Check a protected frame's CRC, over the header's last two bytes and the
/// `bits` of audio data that follow the CRC.
fn check_crc(header: Header, frame: &[u8], bits: usize) -> Result<(), DecodeError> {
    if !header.protected {
        return Ok(());
    }
    let start = HEADER_LEN + CRC_LEN;
    let stated = frame
        .get(HEADER_LEN..start)
        .map(|crc| u16::from_be_bytes([crc[0], crc[1]]))
        .ok_or(DecodeError::MpegFrameInvalid)?;
    let crc = crc16_bits(0xFFFF, &frame[2..HEADER_LEN], 16);
    let crc = crc16_bits(crc, &frame[start..], bits);
    if crc == stated {
        Ok(())
    } else {
        Err(DecodeError::MpegFrameCrc)
    }
}

fn short(_: Exhausted) -> DecodeError {
    DecodeError::MpegFrameInvalid
}

/// A Layer I allocation: bits a sample holds, less one; zero is none.
fn layer1_allocation(bits: &mut BitReader<'_>) -> Result<u32, DecodeError> {
    match bits.read(4).map_err(short)? {
        15 => Err(DecodeError::MpegFrameInvalid),
        allocation => Ok(allocation),
    }
}

/// A Layer I sample of `allocation`, its code read unless `shared` gives it.
fn layer1_code(
    bits: &mut BitReader<'_>,
    allocation: u32,
    shared: Option<u32>,
) -> Result<u32, DecodeError> {
    match (allocation, shared) {
        (0, _) => Ok(0),
        (_, Some(code)) => Ok(code),
        (_, None) => bits.read(allocation + 1).map_err(short),
    }
}

fn layer1_sample(code: u32, allocation: u32, factor: f32) -> f32 {
    if allocation == 0 {
        return 0.0;
    }
    requantise(code, (1 << (allocation + 1)) - 1) * factor
}

/// Decode a Layer I frame's subband samples into `out`.
///
/// # Errors
///
/// [`DecodeError::MpegFrameCrc`] or [`DecodeError::MpegFrameInvalid`].
pub(super) fn layer1(header: Header, frame: &[u8], out: &mut Slots) -> Result<(), DecodeError> {
    let stereo = header.channels() == 2;
    let data = frame
        .get(header.data_start()..)
        .ok_or(DecodeError::MpegFrameInvalid)?;
    let mut bits = BitReader::new(data);
    let bound = bound(header, SUBBANDS);
    let mut allocation = [[0u32; 2]; SUBBANDS];
    for (subband, allocation) in allocation.iter_mut().enumerate() {
        allocation[0] = layer1_allocation(&mut bits)?;
        if stereo {
            allocation[1] = if subband < bound {
                layer1_allocation(&mut bits)?
            } else {
                allocation[0]
            };
        }
    }
    check_crc(header, frame, bits.bits_consumed())?;
    let mut scale = [[0f32; 2]; SUBBANDS];
    for (allocation, scale) in allocation.iter().zip(&mut scale) {
        for (&allocation, factor) in allocation.iter().zip(scale) {
            if allocation != 0 {
                *factor = scalefactor(bits.read(6).map_err(short)?)?;
            }
        }
    }
    let [left, right] = out;
    for (left, right) in left.iter_mut().zip(right).take(LAYER1_SLOTS) {
        for (subband, (allocation, factor)) in allocation.iter().zip(&scale).enumerate() {
            let code = layer1_code(&mut bits, allocation[0], None)?;
            left[subband] = layer1_sample(code, allocation[0], factor[0]);
            if stereo {
                let shared = (subband >= bound).then_some(code);
                let code = layer1_code(&mut bits, allocation[1], shared)?;
                right[subband] = layer1_sample(code, allocation[1], factor[1]);
            }
        }
    }
    Ok(())
}

/// The allocation table a Layer II frame of `bitrate` bit/s takes.
fn table_of(header: Header, bitrate: u32) -> usize {
    if header.lsf() {
        return 4;
    }
    let per_channel = bitrate / 1000 / u32::from(header.channels());
    let rate = header.rate();
    if (rate == 48_000 && per_channel >= 56) || (56..=80).contains(&per_channel) {
        0
    } else if rate != 48_000 && per_channel >= 96 {
        1
    } else if rate != 32_000 && per_channel <= 48 {
        2
    } else {
        3
    }
}

/// A Layer II allocation under `row`: the class it selects, plus one; zero
/// is none.
fn layer2_class(bits: &mut BitReader<'_>, row: &AllocRow) -> Result<u8, DecodeError> {
    match bits.read(u32::from(row.nbal)).map_err(short)? {
        0 => Ok(0),
        index => row
            .classes
            .get(index as usize - 1)
            .map(|&class| class + 1)
            .ok_or(DecodeError::MpegFrameInvalid),
    }
}

/// A granule's three codes for a subband of `class`, read unless `shared`
/// gives them.
fn layer2_codes(
    bits: &mut BitReader<'_>,
    class: u8,
    shared: Option<[u32; 3]>,
) -> Result<[u32; 3], DecodeError> {
    match (class, shared) {
        (0, _) => Ok([0; 3]),
        (_, Some(codes)) => Ok(codes),
        (_, None) => {
            let quant = QUANT_CLASSES[usize::from(class - 1)];
            samples(bits, quant.steps, quant.bits, quant.grouped)
        }
    }
}

/// Decode a Layer II frame of `bitrate` bit/s — stated, or a free-format
/// stream's measured — into `out`.
///
/// # Errors
///
/// [`DecodeError::MpegFrameCrc`] or [`DecodeError::MpegFrameInvalid`].
pub(super) fn layer2(
    header: Header,
    frame: &[u8],
    bitrate: u32,
    out: &mut Slots,
) -> Result<(), DecodeError> {
    let stereo = header.channels() == 2;
    let data = frame
        .get(header.data_start()..)
        .ok_or(DecodeError::MpegFrameInvalid)?;
    let mut bits = BitReader::new(data);
    let table = table_of(header, bitrate);
    let limit = super::tables::ALLOC_TABLES[table].len();
    let bound = bound(header, limit);
    let mut classes = [[0u8; 2]; SUBBANDS];
    for (subband, class) in classes.iter_mut().enumerate().take(limit) {
        let row = alloc_row(table, subband);
        class[0] = layer2_class(&mut bits, row)?;
        if stereo {
            class[1] = if subband < bound {
                layer2_class(&mut bits, row)?
            } else {
                class[0]
            };
        }
    }
    let mut selections = [[0u32; 2]; SUBBANDS];
    for (class, selection) in classes.iter().zip(&mut selections).take(limit) {
        for (&class, selection) in class.iter().zip(selection) {
            if class != 0 {
                *selection = bits.read(2).map_err(short)?;
            }
        }
    }
    check_crc(header, frame, bits.bits_consumed())?;
    let mut scale = [[[0f32; 3]; 2]; SUBBANDS];
    let rows = classes.iter().zip(&selections).zip(&mut scale).take(limit);
    for ((class, selection), scale) in rows {
        for ((&class, &selection), factors) in class.iter().zip(selection).zip(scale) {
            if class == 0 {
                continue;
            }
            let mut read = || bits.read(6).map_err(short).and_then(scalefactor);
            *factors = match selection {
                0 => [read()?, read()?, read()?],
                1 => {
                    let first = read()?;
                    [first, first, read()?]
                }
                2 => [read()?; 3],
                _ => {
                    let first = read()?;
                    let rest = read()?;
                    [first, rest, rest]
                }
            };
        }
    }
    let [left, right] = out;
    let granules = left
        .as_chunks_mut::<3>()
        .0
        .iter_mut()
        .zip(right.as_chunks_mut::<3>().0);
    for (granule, (left, right)) in granules.enumerate() {
        let part = granule / 4;
        for (subband, (class, factors)) in classes.iter().zip(&scale).enumerate() {
            let codes = layer2_codes(&mut bits, class[0], None)?;
            put_granule(left, subband, codes, class[0], factors[0][part]);
            if stereo {
                let shared = (subband >= bound).then_some(codes);
                let codes = layer2_codes(&mut bits, class[1], shared)?;
                put_granule(right, subband, codes, class[1], factors[1][part]);
            }
        }
    }
    Ok(())
}

/// Requantise a granule's three `codes` of `class` into subband `subband`
/// of `slots`.
fn put_granule(
    slots: &mut [[f32; SUBBANDS]; 3],
    subband: usize,
    codes: [u32; 3],
    class: u8,
    factor: f32,
) {
    for (slot, code) in slots.iter_mut().zip(codes) {
        slot[subband] = match class {
            0 => 0.0,
            _ => requantise(code, QUANT_CLASSES[usize::from(class - 1)].steps) * factor,
        };
    }
}

/// Three consecutive samples of one subband: one grouped code, or three.
fn samples(
    bits: &mut BitReader<'_>,
    steps: u32,
    width: u8,
    grouped: bool,
) -> Result<[u32; 3], DecodeError> {
    if !grouped {
        let mut read = || bits.read(u32::from(width)).map_err(short);
        return Ok([read()?, read()?, read()?]);
    }
    let mut code = bits.read(u32::from(width)).map_err(short)?;
    if code >= steps * steps * steps {
        return Err(DecodeError::MpegFrameInvalid);
    }
    let mut group = [0u32; 3];
    for slot in &mut group {
        *slot = code % steps;
        code /= steps;
    }
    Ok(group)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scalefactor_is_two_to_one_less_a_third_of_its_index() {
        assert_eq!(scalefactor(0), Ok(2.0));
        assert_eq!(scalefactor(3), Ok(1.0));
        assert_eq!(scalefactor(6), Ok(0.5));
        let third = scalefactor(4).expect("a scalefactor");
        assert!((f64::from(third) - 0.793_700_525_984_1).abs() < 1e-7);
        assert_eq!(scalefactor(63), Err(DecodeError::MpegFrameInvalid));
    }

    /// The closed form is the standard's `c × (s‴ + d)` for every class.
    #[test]
    fn requantising_matches_the_standards_two_step_form() {
        for class in &QUANT_CLASSES {
            let width = if class.grouped {
                u32::BITS - (class.steps - 1).leading_zeros()
            } else {
                u32::from(class.bits)
            };
            let c = f64::from(1u32 << width) / f64::from(class.steps);
            let d = if class.grouped {
                0.5
            } else {
                1.0 / f64::from(1u32 << (width - 1))
            };
            for code in [0, class.steps / 2, class.steps - 1] {
                let msb = (code >> (width - 1)) & 1;
                let fraction = f64::from(msb) - 1.0
                    + f64::from(code & ((1 << (width - 1)) - 1)) / f64::from(1u32 << (width - 1));
                let expected = c * (fraction + d);
                let got = f64::from(requantise(code, class.steps));
                assert!(
                    (got - expected).abs() < 1e-6,
                    "{} steps, code {code}",
                    class.steps
                );
            }
        }
    }

    #[test]
    fn a_grouped_code_holds_three_samples_and_no_more() {
        // 22 in base 3: 1, 1, 2 from the least significant digit.
        let code: u8 = 1 + 3 + 9 * 2;
        let bytes = [code << 3];
        assert_eq!(
            samples(&mut BitReader::new(&bytes), 3, 5, true),
            Ok([1, 1, 2])
        );
        // 27 = 3^3 is past the last group of three levels.
        let bytes = [27 << 3];
        assert_eq!(
            samples(&mut BitReader::new(&bytes), 3, 5, true),
            Err(DecodeError::MpegFrameInvalid)
        );
        // Three four-bit samples do not fit in one byte.
        assert_eq!(
            samples(&mut BitReader::new(&[0b1011_0000]), 15, 4, false),
            Err(DecodeError::MpegFrameInvalid)
        );
    }
}
