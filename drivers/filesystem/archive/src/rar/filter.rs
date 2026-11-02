//! The transforms RAR applies to a decoded block before it is given out:
//! executable call addresses made relative again (x86, ARM, Itanium),
//! interleaved channels' deltas summed (bytes, RGB pixels, audio samples).
//!
//! Each runs over a block its caller has already bounded; none trusts the
//! parameters a stream gives it beyond what it checks itself.

use tairix_abi::DriverError;

/// The most filters a stream may have pending at once.
pub(super) const PENDING_MAX: usize = 8192;

/// Restore x86 `CALL` (and, where `e9`, `JMP`) targets made relative to the
/// position `file_pos` of the block's first byte in its file, the position
/// counted modulo `wrap`: two to the 32 for RAR 3, to the 24 for RAR 5.
pub(super) fn e8(buf: &mut [u8], file_pos: u64, e9: bool, wrap: u64) {
    let mut i = 0;
    while i + 5 <= buf.len() {
        let op = buf[i];
        if op != 0xE8 && !(e9 && op == 0xE9) {
            i += 1;
            continue;
        }
        let cur = low32((file_pos + i as u64 + 1) % wrap);
        let field = &mut buf[i + 1..i + 5];
        let addr = u32::from_le_bytes([field[0], field[1], field[2], field[3]]);
        if addr & 0x8000_0000 != 0 {
            if addr.wrapping_add(cur) & 0x8000_0000 == 0 {
                field.copy_from_slice(&addr.wrapping_add(0x0100_0000).to_le_bytes());
            }
        } else if addr < 0x0100_0000 {
            field.copy_from_slice(&addr.wrapping_sub(cur).to_le_bytes());
        }
        i += 5;
    }
}

/// Restore ARM `BL` targets made relative to the block's place in its file.
pub(super) fn arm(buf: &mut [u8], file_pos: u64) {
    let mut i = 0;
    while i + 4 <= buf.len() {
        if buf[i + 3] == 0xEB {
            let offset = u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], 0]);
            let back = low32((file_pos + i as u64) / 4);
            let fixed = offset.wrapping_sub(back).to_le_bytes();
            buf[i..i + 3].copy_from_slice(&fixed[..3]);
        }
        i += 4;
    }
}

/// Restore the branch targets of Itanium bundles made relative to the
/// bundle's place in its file.
pub(super) fn itanium(buf: &mut [u8], file_pos: u64) {
    const MASKS: [u8; 16] = [4, 4, 6, 6, 0, 0, 7, 7, 4, 4, 0, 0, 4, 4, 0, 0];
    let mut bundle = low32(file_pos >> 4);
    let mut pos = 0;
    while buf.len() - pos > 21 {
        let template = usize::from(buf[pos] & 0x1F);
        let mask = template.checked_sub(0x10).map_or(0, |t| MASKS[t]);
        for slot in 0..3u32 {
            if mask & (1 << slot) == 0 {
                continue;
            }
            let at = slot * 41 + 18;
            if bit_field(&buf[pos..], at + 24, 4) == 5 {
                let target = bit_field(&buf[pos..], at, 20).wrapping_sub(bundle) & 0xF_FFFF;
                set_bit_field(&mut buf[pos..], at, 20, target);
            }
        }
        bundle = bundle.wrapping_add(1);
        pos += 16;
    }
}

/// `width` bits at bit `at` of `bytes`, least significant first; the bits
/// lie within the bundle's sixteen bytes.
fn bit_field(bytes: &[u8], at: u32, width: u32) -> u32 {
    let byte = usize::try_from(at / 8).unwrap_or(usize::MAX);
    let mut word = [0u8; 4];
    for (k, slot) in word.iter_mut().enumerate() {
        *slot = bytes.get(byte + k).copied().unwrap_or(0);
    }
    (u32::from_le_bytes(word) >> (at % 8)) & ((1 << width) - 1)
}

fn set_bit_field(bytes: &mut [u8], at: u32, width: u32, value: u32) {
    let byte = usize::try_from(at / 8).unwrap_or(usize::MAX);
    let mut word = [0u8; 4];
    for (k, slot) in word.iter_mut().enumerate() {
        *slot = bytes.get(byte + k).copied().unwrap_or(0);
    }
    let shift = at % 8;
    let mask = ((1u32 << width) - 1) << shift;
    let updated = (u32::from_le_bytes(word) & !mask) | ((value << shift) & mask);
    for (k, byte_value) in updated.to_le_bytes().into_iter().enumerate() {
        if let Some(slot) = bytes.get_mut(byte + k) {
            *slot = byte_value;
        }
    }
}

/// Sum `channels` interleaved byte channels' deltas from `src` into `out`.
pub(super) fn delta(src: &[u8], out: &mut [u8], channels: usize) {
    let mut taken = src.iter();
    for channel in 0..channels {
        let mut prev = 0u8;
        for slot in out.iter_mut().skip(channel).step_by(channels) {
            prev = prev.wrapping_sub(taken.next().copied().unwrap_or(0));
            *slot = prev;
        }
    }
}

/// Undo RAR 3's RGB filter: three colour planes predicted from the pixel to
/// the left and the row above at `stride` bytes, green then added back to
/// red and blue from the red byte at `red`.
///
/// # Errors
///
/// [`DriverError::MediumError`] for a stride or red place out of range.
pub(super) fn rgb(
    src: &[u8],
    out: &mut [u8],
    stride: usize,
    red: usize,
) -> Result<(), DriverError> {
    let n = out.len();
    if n < 3 || !(3..=n).contains(&stride) || red > 2 {
        return Err(DriverError::MediumError);
    }
    let width = stride - 3;
    let mut taken = src.iter();
    for channel in 0..3 {
        let mut prev = 0u8;
        for j in (channel..n).step_by(3) {
            let predicted = match j.checked_sub(width).filter(|&up| up >= 3) {
                Some(up) => paeth(prev, out[up], out[up - 3]),
                None => prev,
            };
            prev = predicted.wrapping_sub(taken.next().copied().unwrap_or(0));
            out[j] = prev;
        }
    }
    let mut j = red;
    while j + 2 < n {
        let green = out[j + 1];
        out[j] = out[j].wrapping_add(green);
        out[j + 2] = out[j + 2].wrapping_add(green);
        j += 3;
    }
    Ok(())
}

/// The neighbour a pixel is predicted from: whichever of left, above and
/// above-left lies nearest their gradient.
fn paeth(left: u8, above: u8, corner: u8) -> u8 {
    let gradient = i32::from(left) + i32::from(above) - i32::from(corner);
    let to_left = (gradient - i32::from(left)).abs();
    let to_above = (gradient - i32::from(above)).abs();
    let to_corner = (gradient - i32::from(corner)).abs();
    if to_left <= to_above && to_left <= to_corner {
        left
    } else if to_above <= to_corner {
        above
    } else {
        corner
    }
}

/// Undo RAR 3's audio filter over `channels` interleaved channels: each
/// sample predicted from its channel's last three deltas by weights the
/// filter itself adapts.
pub(super) fn audio(src: &[u8], out: &mut [u8], channels: usize) {
    let mut taken = src.iter();
    for channel in 0..channels {
        let mut predictor = Predictor::default();
        for slot in out.iter_mut().skip(channel).step_by(channels) {
            *slot = predictor.next(taken.next().copied().unwrap_or(0));
        }
    }
}

/// One audio channel's adaptive predictor.
#[derive(Default)]
struct Predictor {
    last: u8,
    last_delta: i32,
    delta: [i32; 3],
    weight: [i32; 3],
    error: [u32; 7],
    count: u32,
}

impl Predictor {
    fn next(&mut self, coded: u8) -> u8 {
        self.delta[2] = self.delta[1];
        self.delta[1] = self.last_delta - self.delta[0];
        self.delta[0] = self.last_delta;
        let sum = 8 * i32::from(self.last)
            + self.weight[0] * self.delta[0]
            + self.weight[1] * self.delta[1]
            + self.weight[2] * self.delta[2];
        let predicted = (sum >> 3).to_le_bytes()[0];
        let byte = predicted.wrapping_sub(coded);
        let error = i32::from(i8::from_le_bytes([coded])) << 3;
        self.error[0] += error.unsigned_abs();
        for k in 0..3 {
            self.error[1 + 2 * k] += (error - self.delta[k]).unsigned_abs();
            self.error[2 + 2 * k] += (error + self.delta[k]).unsigned_abs();
        }
        self.last_delta = i32::from(i8::from_le_bytes([byte.wrapping_sub(self.last)]));
        self.last = byte;
        if self.count.is_multiple_of(32) {
            adapt(&mut self.error, &mut self.weight);
        }
        self.count = self.count.wrapping_add(1);
        byte
    }
}

/// Nudge the weight whose sign flip would have erred least over the last
/// stretch, and start the next stretch's tally: RAR's audio predictors adapt
/// so, in RAR 2's audio mode and RAR 3's audio filter alike.
pub(super) fn adapt(error: &mut [u32], weight: &mut [i32]) {
    let mut least = 0;
    for (k, &err) in error.iter().enumerate() {
        if err < error[least] {
            least = k;
        }
    }
    error.fill(0);
    if least == 0 {
        return;
    }
    let w = &mut weight[(least - 1) >> 1];
    if least & 1 == 1 {
        if *w >= -16 {
            *w -= 1;
        }
    } else if *w < 16 {
        *w += 1;
    }
}

/// The low 32 bits of `value`.
fn low32(value: u64) -> u32 {
    super::bits::low32(value)
}
