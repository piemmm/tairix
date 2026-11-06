//! Opus packets a container other than Ogg carries: a `WebM` track's
//! identification header once, as its codec private data, then a packet a
//! block.
//!
//! The header is read as the Ogg mapping reads it, so a stream decodes alike in
//! either container. Where the stream starts and ends is the container's: its
//! timestamps place each packet, and its codec delay and padding trim the
//! decode.

use tairix_abi::driver::audio::{ChannelMap, Rate};

use super::{Multistream, MAX_PACKET_SAMPLES};
use crate::ogg::{gain_factor, opus_channel_map, parse_head};
use crate::{DecodeError, DecodeLimits};

/// An Opus stream decoded a packet at a time.
pub struct OpusPackets {
    decoder: Multistream,
    channels: ChannelMap,
    gain: f32,
    pre_skip: u64,
}

impl OpusPackets {
    /// The most samples a channel one packet decodes to: 120 ms.
    pub const MAX_SAMPLES: usize = MAX_PACKET_SAMPLES;

    /// The rate every stream decodes at.
    pub const RATE: Rate = Rate::HZ_48000;

    /// The stream the identification header `head` describes, its decoded
    /// channels held to `limits`.
    ///
    /// # Errors
    ///
    /// The header's refusal, as the Ogg mapping states it, or
    /// [`DecodeError::OutOfMemory`].
    pub fn new(head: &[u8], limits: &DecodeLimits) -> Result<Self, DecodeError> {
        let head = parse_head(head)?;
        let channels = opus_channel_map(&head, limits)?;
        Ok(Self {
            decoder: Multistream::new(&head.layout)?,
            channels,
            gain: gain_factor(head.gain),
            pre_skip: head.pre_skip,
        })
    }

    /// The channels each packet decodes to, in Vorbis's order.
    #[must_use]
    pub const fn channels(&self) -> ChannelMap {
        self.channels
    }

    /// Samples a channel the decoder makes at the stream's start before its
    /// sound: what the container's codec delay trims.
    #[must_use]
    pub const fn pre_skip(&self) -> u64 {
        self.pre_skip
    }

    /// Decode `packet` into `out`, interleaved, the header's output gain
    /// applied; the samples a channel it held.
    ///
    /// # Errors
    ///
    /// [`DecodeError::OpusPacketInvalid`] for a packet RFC 6716's framing
    /// refuses, and [`DecodeError::BufferTooSmall`] for an `out` shorter than
    /// the packet decodes to.
    pub fn decode(&mut self, packet: &[u8], out: &mut [f32]) -> Result<usize, DecodeError> {
        let samples = self.decoder.decode(packet, out)?;
        let held = samples * usize::from(self.channels.channels());
        for sample in out.iter_mut().take(held) {
            *sample *= self.gain;
        }
        Ok(samples)
    }

    /// Forget every packet decoded, as a stream entered afresh at a seek.
    pub fn reset(&mut self) {
        self.decoder.reset();
    }
}

#[cfg(test)]
#[path = "packets_tests.rs"]
mod tests;
