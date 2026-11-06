//! Writing Opus (RFC 6716): packets of one fullband CELT frame of 20 ms,
//! mono or stereo, at a constant bitrate, coded by the CELT decoder's own
//! model so the two halves cannot drift apart.
//!
//! Only CELT-only packets are written, never SILK or hybrid ones, and every
//! Opus decoder plays them. The decode lags the input by [`PRE_SKIP`]
//! samples, the pre-skip a container states, so a stream ends with one more
//! frame, of silence, and the container trims the decode at the input's end.
//! Behind the off-by-default `encode-opus` feature.

use crate::ogg::{HEAD_LEN, HEAD_SIGNATURE};
use crate::opus::{CeltEncoder, TOC_STEREO};

/// The rate a stream is coded at.
pub const RATE: u32 = crate::opus::RATE;
/// The samples a channel each packet holds: 20 ms.
pub const FRAME_SAMPLES: usize = CeltEncoder::FRAME;
/// The samples a decoder's output lags the input by.
pub const PRE_SKIP: u16 = CeltEncoder::DELAY;
/// The least bitrate, in bits a second (RFC 6716, section 2.1.1).
pub const MIN_BITRATE: u32 = 6_000;
/// The most bitrate, in bits a second (RFC 6716, section 2.1.1).
pub const MAX_BITRATE: u32 = 510_000;

/// The identification header's version (RFC 7845, section 5.1).
const VERSION: u8 = 1;
/// The table-of-contents byte of a packet of one fullband CELT frame of
/// 20 ms, mono (RFC 6716, section 3.1).
const TOC: u8 = 31 << 3;

/// Why a stream or a frame could not be written.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EncodeError {
    /// A channel count other than one or two, or a bitrate outside
    /// [`MIN_BITRATE`] to [`MAX_BITRATE`].
    Unsupported,
    /// A frame that is not [`FRAME_SAMPLES`] samples a channel, or holds a
    /// sample that is not finite.
    BadSamples,
    /// An output shorter than [`Encoder::packet_len`].
    BufferTooSmall,
    /// The encoder's working memory could not be had.
    OutOfMemory,
    /// A frame whose coding outran its bytes, which its allocation exists to
    /// prevent: an encoder defect, reported rather than written, and undone.
    Overrun,
}

/// One Opus stream's encoder.
pub struct Encoder {
    channels: u8,
    packet_len: usize,
    celt: CeltEncoder,
}

impl Encoder {
    /// An encoder of `channels`, one or two, at `bitrate` bits a second,
    /// rounded down to whole bytes a packet.
    pub fn new(channels: u8, bitrate: u32) -> Result<Self, EncodeError> {
        if !(1..=2).contains(&channels) || !(MIN_BITRATE..=MAX_BITRATE).contains(&bitrate) {
            return Err(EncodeError::Unsupported);
        }
        let bits = u64::from(bitrate) * FRAME_SAMPLES as u64 / u64::from(RATE);
        let packet_len = usize::try_from(bits / 8).map_err(|_| EncodeError::Unsupported)?;
        let celt = CeltEncoder::new(usize::from(channels), packet_len - 1)
            .ok_or(EncodeError::OutOfMemory)?;
        Ok(Self {
            channels,
            packet_len,
            celt,
        })
    }

    /// The bytes every packet takes.
    #[must_use]
    pub const fn packet_len(&self) -> usize {
        self.packet_len
    }

    /// The identification header (RFC 7845, section 5.1) a container carries
    /// before the packets: Ogg's first packet, and a Matroska track's codec
    /// private data. Channel mapping family 0, no output gain.
    #[must_use]
    pub fn head(&self) -> [u8; HEAD_LEN] {
        let mut head = [0u8; HEAD_LEN];
        head[..HEAD_SIGNATURE.len()].copy_from_slice(HEAD_SIGNATURE);
        head[8] = VERSION;
        head[9] = self.channels;
        head[10..12].copy_from_slice(&PRE_SKIP.to_le_bytes());
        head[12..16].copy_from_slice(&RATE.to_le_bytes());
        head
    }

    /// Code one frame of `pcm`, [`FRAME_SAMPLES`] samples a channel
    /// interleaved, full scale at ±1 and clipped there, into `out` as one
    /// packet; the bytes written, always [`Self::packet_len`]. A refused
    /// frame leaves the encoder as it was.
    pub fn encode(&mut self, pcm: &[f32], out: &mut [u8]) -> Result<usize, EncodeError> {
        let samples = FRAME_SAMPLES * usize::from(self.channels);
        if pcm.len() != samples || pcm.iter().any(|sample| !sample.is_finite()) {
            return Err(EncodeError::BadSamples);
        }
        let (toc, frame) = out
            .get_mut(..self.packet_len)
            .and_then(<[u8]>::split_first_mut)
            .ok_or(EncodeError::BufferTooSmall)?;
        *toc = if self.channels == 2 {
            TOC | TOC_STEREO
        } else {
            TOC
        };
        if self.celt.encode(pcm, frame) {
            Ok(self.packet_len)
        } else {
            Err(EncodeError::Overrun)
        }
    }

    /// The range coder's state after the last packet.
    #[cfg(test)]
    pub(crate) const fn final_range(&self) -> u32 {
        self.celt.final_range()
    }
}

#[cfg(test)]
#[path = "opus_encode_tests.rs"]
mod tests;
