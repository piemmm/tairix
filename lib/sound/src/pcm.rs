//! Samples into the PCM vocabulary's little-endian forms, where a file's own
//! form differs.

/// A float sample as a stream may carry it: a value that is no number is
/// silence, and an infinite one full scale.
pub(crate) fn finite(sample: f32) -> f32 {
    if sample.is_nan() {
        0.0
    } else if sample.is_infinite() {
        sample.signum()
    } else {
        sample
    }
}

/// Reverse each `width`-byte sample of `samples` in place: big-endian to
/// little.
pub(crate) fn swap_each(samples: &mut [u8], width: usize) {
    for sample in samples.chunks_exact_mut(width) {
        sample.reverse();
    }
}

/// Flip each signed 8-bit sample of `samples` to the unsigned form, silence at
/// the middle.
pub(crate) fn unsign(samples: &mut [u8]) {
    for sample in samples {
        *sample ^= 0x80;
    }
}

/// Make each little-endian float of `samples` finite.
pub(crate) fn finite_each(samples: &mut [u8]) {
    for sample in samples.as_chunks_mut::<4>().0 {
        *sample = finite(f32::from_le_bytes(*sample)).to_le_bytes();
    }
}

/// Write each 64-bit float of `wide`, in `big_endian` or little-endian order,
/// as a finite 32-bit one into `out`, which holds half as many bytes.
pub(crate) fn narrow_doubles(wide: &[u8], big_endian: bool, out: &mut [u8]) {
    for (double, single) in wide
        .as_chunks::<8>()
        .0
        .iter()
        .zip(out.as_chunks_mut::<4>().0)
    {
        let value = if big_endian {
            f64::from_be_bytes(*double)
        } else {
            f64::from_le_bytes(*double)
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "32-bit float is the widest the PCM vocabulary carries"
        )]
        let narrowed = value as f32;
        *single = finite(narrowed).to_le_bytes();
    }
}

/// Expand each byte code of the tail half of `out` through `table` into the
/// 16-bit samples that fill it, front to back: a sample is written only over
/// codes already read.
pub(crate) fn expand_codes_in_place(out: &mut [u8], table: &[i16; 256]) {
    let codes = out.len() / 2;
    for at in 0..codes {
        let sample = table[usize::from(out[codes + at])];
        out[2 * at..2 * at + 2].copy_from_slice(&sample.to_le_bytes());
    }
}

/// Interleaved 16-bit frames, written into a block from frame `skip` of a
/// decoded run on, while the block has room.
pub(crate) struct FrameSink<'a> {
    out: &'a mut [u8],
    frame_bytes: usize,
    skip: usize,
    frame: usize,
    written: usize,
}

impl<'a> FrameSink<'a> {
    pub(crate) fn new(out: &'a mut [u8], channels: usize, skip: usize) -> Self {
        Self {
            out,
            frame_bytes: 2 * channels,
            skip,
            frame: 0,
            written: 0,
        }
    }

    /// Take the run's next frame, one sample a channel.
    pub(crate) fn push(&mut self, samples: &[i16]) {
        if self.frame >= self.skip && !self.full() {
            let at = self.written * self.frame_bytes;
            let slots = self.out[at..at + self.frame_bytes].as_chunks_mut::<2>().0;
            for (slot, sample) in slots.iter_mut().zip(samples) {
                *slot = sample.to_le_bytes();
            }
            self.written += 1;
        }
        self.frame += 1;
    }

    /// Whether the block has room for no more.
    pub(crate) const fn full(&self) -> bool {
        (self.written + 1) * self.frame_bytes > self.out.len()
    }

    /// Frames written.
    pub(crate) const fn written(&self) -> usize {
        self.written
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::{expand_codes_in_place, finite, narrow_doubles, swap_each, unsign};

    #[test]
    fn a_float_that_is_no_number_is_silence_and_an_infinite_one_full_scale() {
        let bits = |value: f32| finite(value).to_bits();
        assert_eq!(bits(f32::NAN), 0.0f32.to_bits());
        assert_eq!(bits(f32::INFINITY), 1.0f32.to_bits());
        assert_eq!(bits(f32::NEG_INFINITY), (-1.0f32).to_bits());
        assert_eq!(bits(1.5), 1.5f32.to_bits(), "headroom is the stream's");
    }

    #[test]
    fn samples_are_swapped_unsigned_and_narrowed_where_they_lie() {
        let mut samples = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC];
        swap_each(&mut samples, 3);
        assert_eq!(samples, [0x56, 0x34, 0x12, 0xBC, 0x9A, 0x78]);
        let mut bytes = [0x00, 0x7F, 0x80, 0xFF];
        unsign(&mut bytes);
        assert_eq!(bytes, [0x80, 0xFF, 0x00, 0x7F]);
        let wide: Vec<u8> = [0.5f64, -0.25]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        let mut out = [0u8; 8];
        narrow_doubles(&wide, true, &mut out);
        assert_eq!(out[..4], 0.5f32.to_le_bytes());
        assert_eq!(out[4..], (-0.25f32).to_le_bytes());
    }

    #[test]
    fn codes_in_the_tail_half_expand_over_the_whole() {
        let mut table = [0i16; 256];
        for (code, entry) in table.iter_mut().enumerate() {
            *entry = i16::try_from(code).expect("small") * 100;
        }
        let mut out = [0u8; 8];
        out[4..].copy_from_slice(&[1, 2, 3, 4]);
        expand_codes_in_place(&mut out, &table);
        let samples: Vec<i16> = out
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes(*pair))
            .collect();
        assert_eq!(samples, [100, 200, 300, 400]);
    }
}
