//! Sun/NeXT audio (`.au`, `.snd`): a big-endian header, an annotation, then
//! the samples, in one of the encodings the format's `soundfile.h` numbers.
//!
//! A sampled encoding is read where it lies, a frame at a time; G.721, G.722
//! and G.723 are decoded from the start, their codes packed least significant
//! first as Sun's reference packs them.

use alloc::vec::Vec;

use tairix_abi::driver::audio::{Rate, SampleFormat};

use crate::g711::{ALAW, ULAW};
use crate::g722::G722;
use crate::g72x::{G72x, G72xRate};
use crate::input::{self, SoundInput};
use crate::meta::{Collector, TagKind};
use crate::{
    conventional_channels, pcm, DataLength, DecodeError, DecodeLimits, Encoding, SoundFormat,
    SoundInfo,
};

const MAGIC: &[u8; 4] = b".snd";

/// Bytes of the fixed header, and the least a data offset may be.
const HEADER_LEN: u64 = 24;

/// The data size of a stream written without knowing its length.
const UNKNOWN_SIZE: u32 = u32::MAX;

/// The rate G.722 decodes to.
const G722_RATE_HZ: u32 = 16_000;

/// Whether `bytes` open with the AU signature.
pub(crate) fn has_signature(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

/// How the stream's samples lie.
#[allow(
    clippy::large_enum_variant,
    reason = "one a stream, so its size buys nothing to box away"
)]
enum Samples {
    /// `width` bytes a sample, big-endian, signed.
    Linear {
        width: usize,
    },
    Float32,
    Float64,
    Law(&'static [i16; 256]),
    G72x(G72x),
    /// G.722, and the second sample of the last code when a block had room
    /// for only its first.
    G722(G722, Option<i16>),
}

/// An AU stream past its header.
pub(crate) struct Au {
    data_start: u64,
    data_end: u64,
    channels: usize,
    samples: Samples,
}

fn be_u32(header: &[u8; 24], at: usize) -> u32 {
    u32::from_be_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
}

/// The encoding a header's code names, and how its samples lie.
fn encoding(code: u32) -> Result<(Encoding, Samples), DecodeError> {
    let linear = |bits: u8| Samples::Linear {
        width: usize::from(bits / 8),
    };
    let width = |code: u32| match code {
        2 | 11 => 8,
        3 | 12 => 16,
        4 | 13 => 24,
        _ => 32,
    };
    Ok(match code {
        1 => (Encoding::MuLaw, Samples::Law(&ULAW)),
        2..=5 => {
            let bits = width(code);
            (Encoding::Linear { bits }, linear(bits))
        }
        6 => (Encoding::Float { bits: 32 }, Samples::Float32),
        7 => (Encoding::Float { bits: 64 }, Samples::Float64),
        8 => return Err(DecodeError::AuFragmentedData),
        9 => return Err(DecodeError::AuNestedSound),
        10 => return Err(DecodeError::AuDspProgram),
        11..=14 => {
            let bits = width(code);
            (Encoding::Fixed { bits }, linear(bits))
        }
        16 => return Err(DecodeError::AuDisplayData),
        17..=20 => return Err(DecodeError::AuUnspecifiedEncoding),
        21 | 22 => return Err(DecodeError::AuDspCommands),
        23 => (Encoding::G721, Samples::G72x(G72x::new(G72xRate::Kbit32))),
        24 => (Encoding::G722, Samples::G722(G722::new(), None)),
        25 => (
            Encoding::G723Kbit24,
            Samples::G72x(G72x::new(G72xRate::Kbit24)),
        ),
        26 => (
            Encoding::G723Kbit40,
            Samples::G72x(G72x::new(G72xRate::Kbit40)),
        ),
        27 => (Encoding::ALaw, Samples::Law(&ALAW)),
        other => return Err(DecodeError::AuUnknownEncoding(other)),
    })
}

/// Read the header and annotation of the AU file `input` holds.
pub(crate) fn open(
    input: &mut (impl SoundInput + ?Sized),
    limits: &DecodeLimits,
    collector: &mut Collector,
) -> Result<(SoundInfo, Au), DecodeError> {
    let mut header = [0u8; 24];
    input::read_exact(input, 0, &mut header, DecodeError::AuHeaderTruncated)?;
    if !has_signature(&header) {
        return Err(DecodeError::AuBadMagic);
    }
    let len = input.len();
    let data_start = u64::from(be_u32(&header, 4));
    if data_start < HEADER_LEN || data_start > len {
        return Err(DecodeError::AuDataOffsetBad);
    }
    let declared = be_u32(&header, 8);
    let (encoding, samples) = encoding(be_u32(&header, 12))?;
    let rate = Rate::new(be_u32(&header, 16)).map_err(|_| DecodeError::RateOutOfRange)?;
    let channels = conventional_channels(be_u32(&header, 20), limits)?;
    let adpcm = matches!(samples, Samples::G72x(_) | Samples::G722(..));
    if adpcm && channels.channels() != 1 {
        return Err(DecodeError::AuAdpcmChannels);
    }
    if matches!(samples, Samples::G722(..)) && rate.hz() != G722_RATE_HZ {
        return Err(DecodeError::AuG722Rate);
    }
    let held = len - data_start;
    let (data_end, data_length) = if declared == UNKNOWN_SIZE {
        (len, None)
    } else if u64::from(declared) > held {
        let disagreement = DataLength {
            declared: u64::from(declared),
            held,
        };
        (len, Some(disagreement))
    } else {
        (data_start + u64::from(declared), None)
    };
    let annotation = data_start - HEADER_LEN;
    if annotation > 0 && collector.has_room(annotation) {
        let mut text = Vec::new();
        let size = usize::try_from(annotation).map_err(|_| DecodeError::OutOfMemory)?;
        if !tairix_util::fallible::grow_to(&mut text, size, 0u8) {
            return Err(DecodeError::OutOfMemory);
        }
        input::read_exact(input, HEADER_LEN, &mut text, DecodeError::AuDataOffsetBad)?;
        collector.tag(TagKind::Comment, &text)?;
    }
    let au = Au {
        data_start,
        data_end,
        channels: usize::from(channels.channels()),
        samples,
    };
    let info = SoundInfo {
        format: SoundFormat::Au,
        encoding,
        rate,
        channels,
        sample: au.sample_format(),
        frames: Some(au.frames()),
        seekable: !adpcm,
        data_length,
    };
    Ok((info, au))
}

impl Au {
    const fn sample_format(&self) -> SampleFormat {
        match self.samples {
            Samples::Linear { width: 1 } => SampleFormat::U8,
            Samples::Linear { width: 3 } => SampleFormat::S24,
            Samples::Linear { width: 4 } => SampleFormat::S32,
            Samples::Float32 | Samples::Float64 => SampleFormat::F32,
            Samples::Linear { .. } | Samples::Law(_) | Samples::G72x(_) | Samples::G722(..) => {
                SampleFormat::S16
            }
        }
    }

    /// Bytes a frame occupies in the file, for a stream with a whole number
    /// of them.
    const fn stored_frame(&self) -> Option<usize> {
        let width = match self.samples {
            Samples::Linear { width } => width,
            Samples::Float32 => 4,
            Samples::Float64 => 8,
            Samples::Law(_) => 1,
            Samples::G72x(_) | Samples::G722(..) => return None,
        };
        Some(width * self.channels)
    }

    fn frames(&self) -> u64 {
        let bytes = self.data_end - self.data_start;
        match &self.samples {
            Samples::G72x(decoder) => bytes * 8 / u64::from(decoder.bits()),
            Samples::G722(..) => bytes * 2,
            _ => self
                .stored_frame()
                .and_then(|frame| u64::try_from(frame).ok())
                .map_or(0, |frame| bytes / frame),
        }
    }

    /// Write the frames from `position` that `out` has room for: a sampled
    /// stream reads from wherever it is asked, and an ADPCM one is only ever
    /// asked for the frames after its last.
    pub(crate) fn read(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        position: u64,
        out: &mut [u8],
        scratch: &mut Vec<u8>,
    ) -> Result<usize, DecodeError> {
        let frame_bytes = self.sample_format().bytes_per_sample() * self.channels;
        let left = self.frames().saturating_sub(position);
        let room = out.len() / frame_bytes;
        let frames = usize::try_from(left).map_or(room, |left| left.min(room));
        if frames == 0 {
            return Ok(0);
        }
        let out = &mut out[..frames * frame_bytes];
        match self.stored_frame() {
            Some(stored) => self.read_sampled(input, position, stored, out, scratch)?,
            None => self.read_adpcm(input, position, out, scratch)?,
        }
        Ok(frames)
    }

    fn read_sampled(
        &self,
        input: &mut (impl SoundInput + ?Sized),
        position: u64,
        stored: usize,
        out: &mut [u8],
        scratch: &mut Vec<u8>,
    ) -> Result<(), DecodeError> {
        let start = position
            .checked_mul(u64::try_from(stored).map_err(|_| DecodeError::InputFailed)?)
            .and_then(|offset| offset.checked_add(self.data_start))
            .ok_or(DecodeError::InputFailed)?;
        match self.samples {
            Samples::Linear { width } => {
                input::read_exact(input, start, out, DecodeError::InputFailed)?;
                if width == 1 {
                    pcm::unsign(out);
                } else {
                    pcm::swap_each(out, width);
                }
            }
            Samples::Float32 => {
                input::read_exact(input, start, out, DecodeError::InputFailed)?;
                pcm::swap_each(out, 4);
                pcm::finite_each(out);
            }
            Samples::Float64 => {
                if !tairix_util::fallible::grow_to(scratch, out.len() * 2, 0u8) {
                    return Err(DecodeError::OutOfMemory);
                }
                let wide = &mut scratch[..out.len() * 2];
                input::read_exact(input, start, wide, DecodeError::InputFailed)?;
                pcm::narrow_doubles(wide, true, out);
            }
            Samples::Law(table) => {
                let codes = out.len() / 2;
                input::read_exact(input, start, &mut out[codes..], DecodeError::InputFailed)?;
                pcm::expand_codes_in_place(out, table);
            }
            Samples::G72x(_) | Samples::G722(..) => {}
        }
        Ok(())
    }

    /// Decode the mono samples `out` holds, which follow on from the last
    /// block: the codes are read before any is decoded, so a read the input
    /// cannot answer changes nothing.
    fn read_adpcm(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        position: u64,
        out: &mut [u8],
        scratch: &mut Vec<u8>,
    ) -> Result<(), DecodeError> {
        let samples = out.as_chunks_mut::<2>().0;
        let frames = u64::try_from(samples.len()).map_err(|_| DecodeError::OutOfMemory)?;
        match &mut self.samples {
            Samples::G72x(decoder) => {
                let bits = decoder.bits();
                let first_bit = position * u64::from(bits);
                let first_byte = first_bit / 8;
                let end_byte = (first_bit + frames * u64::from(bits)).div_ceil(8);
                let bytes =
                    usize::try_from(end_byte - first_byte).map_err(|_| DecodeError::OutOfMemory)?;
                // A spare zero byte, so every code reads from a two-byte
                // window.
                if !tairix_util::fallible::grow_to(scratch, bytes + 1, 0u8) {
                    return Err(DecodeError::OutOfMemory);
                }
                let packed = &mut scratch[..=bytes];
                packed[bytes] = 0;
                let start = self.data_start + first_byte;
                input::read_exact(input, start, &mut packed[..bytes], DecodeError::InputFailed)?;
                let mask = (1u16 << bits) - 1;
                let mut bit = usize::from(u8::try_from(first_bit % 8).unwrap_or(0));
                for sample in samples {
                    let window = u16::from_le_bytes([packed[bit / 8], packed[bit / 8 + 1]]);
                    let code = u8::try_from((window >> (bit % 8)) & mask).unwrap_or(0);
                    *sample = decoder.decode(code).to_le_bytes();
                    bit += usize::from(u8::try_from(bits).unwrap_or(0));
                }
            }
            Samples::G722(decoder, carry) => {
                let from_carry = u64::from(carry.is_some());
                let codes = usize::try_from((frames - from_carry).div_ceil(2))
                    .map_err(|_| DecodeError::OutOfMemory)?;
                if !tairix_util::fallible::grow_to(scratch, codes, 0u8) {
                    return Err(DecodeError::OutOfMemory);
                }
                // A carry is held exactly when the position is odd, so the
                // next code is the one after the position's half either way.
                let start = self.data_start + position.div_ceil(2);
                input::read_exact(
                    input,
                    start,
                    &mut scratch[..codes],
                    DecodeError::InputFailed,
                )?;
                let mut produced =
                    carry
                        .take()
                        .into_iter()
                        .chain(scratch[..codes].iter().flat_map(|&code| {
                            let (first, second) = decoder.decode(code);
                            [first, second]
                        }));
                for (sample, value) in samples.iter_mut().zip(&mut produced) {
                    *sample = value.to_le_bytes();
                }
                *carry = produced.next();
            }
            Samples::Linear { .. } | Samples::Float32 | Samples::Float64 | Samples::Law(_) => {}
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "au_tests.rs"]
mod tests;
