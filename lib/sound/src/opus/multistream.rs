//! Several Opus streams carried as one packet (RFC 6716, appendix B, and RFC
//! 7845, section 5.1.1): every stream but the last in its self-delimited
//! form, the first `coupled` decoded as stereo pairs and the rest as mono,
//! their decoded channels laid onto the output's as a mapping table says.

use alloc::vec::Vec;

use tairix_util::fallible::filled;

use super::celt::Work;
use super::decoder::Decoder;
use super::{Packet, MAX_PACKET_SAMPLES};
use crate::DecodeError;

/// A mapping entry naming no decoded channel: that output is silent.
pub(crate) const SILENT: u8 = 255;

/// How a packet's streams map onto the output channels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Layout {
    pub(crate) channels: usize,
    pub(crate) streams: usize,
    pub(crate) coupled: usize,
    /// Each output channel's decoded channel, or [`SILENT`].
    pub(crate) mapping: [u8; 255],
}

impl Layout {
    /// One stream, stereo exactly when `channels` is two.
    pub(crate) fn single(channels: usize) -> Self {
        let mut mapping = [SILENT; 255];
        mapping[0] = 0;
        mapping[1] = 1;
        Self {
            channels,
            streams: 1,
            coupled: channels - 1,
            mapping,
        }
    }

    /// Whether the counts hold together and every entry names a decoded
    /// channel or none.
    pub(crate) fn is_valid(&self) -> bool {
        let decoded = self.streams + self.coupled;
        (1..=255).contains(&self.channels)
            && self.streams >= 1
            && self.coupled <= self.streams
            && decoded <= 255
            && self.mapping[..self.channels]
                .iter()
                .all(|&entry| entry == SILENT || usize::from(entry) < decoded)
    }
}

/// The decoders of every stream a packet carries.
pub(crate) struct Multistream {
    layout: Layout,
    decoders: Vec<Decoder>,
    decoded: Vec<f32>,
    work: Work,
}

impl Multistream {
    pub(crate) fn new(layout: &Layout) -> Result<Self, DecodeError> {
        if !layout.is_valid() {
            return Err(DecodeError::ChannelLayoutUnsupported);
        }
        let mut decoders = Vec::new();
        decoders
            .try_reserve_exact(layout.streams)
            .map_err(|_| DecodeError::OutOfMemory)?;
        for stream in 0..layout.streams {
            decoders.push(Decoder::new(if stream < layout.coupled { 2 } else { 1 })?);
        }
        Ok(Self {
            layout: *layout,
            decoders,
            decoded: filled(2 * MAX_PACKET_SAMPLES, 0.0).ok_or(DecodeError::OutOfMemory)?,
            work: Work::new()?,
        })
    }

    /// Forget every packet decoded, as a stream starting afresh.
    pub(crate) fn reset(&mut self) {
        for decoder in &mut self.decoders {
            decoder.reset();
        }
    }

    /// The streams' final range states over the last packet, combined.
    #[cfg(test)]
    pub(crate) fn final_range(&self) -> u32 {
        self.decoders
            .iter()
            .fold(0, |range, decoder| range ^ decoder.final_range())
    }

    /// The samples a channel `bytes` decodes to, every stream's packet
    /// checked to frame correctly and to agree on that count.
    pub(crate) fn samples(&self, bytes: &[u8]) -> Result<usize, DecodeError> {
        let mut rest = bytes;
        let mut samples = None;
        for stream in 0..self.layout.streams {
            let last = stream + 1 == self.layout.streams;
            let packet = Packet::parse(rest, !last)?;
            if samples.is_some_and(|samples| samples != packet.samples()) {
                return Err(DecodeError::OpusPacketInvalid);
            }
            samples = Some(packet.samples());
            rest = &rest[packet.len..];
        }
        samples.ok_or(DecodeError::OpusPacketInvalid)
    }

    /// Decode `bytes` into `out`, interleaved over the layout's channels;
    /// the samples a channel.
    pub(crate) fn decode(&mut self, bytes: &[u8], out: &mut [f32]) -> Result<usize, DecodeError> {
        let samples = self.samples(bytes)?;
        let channels = self.layout.channels;
        if out.len() < samples * channels {
            return Err(DecodeError::BufferTooSmall);
        }
        let out = &mut out[..samples * channels];
        let mut rest = bytes;
        for (stream, decoder) in self.decoders.iter_mut().enumerate() {
            let last = stream + 1 == self.layout.streams;
            let packet = Packet::parse(rest, !last)?;
            rest = &rest[packet.len..];
            decoder.decode(&packet, &mut self.decoded, &mut self.work)?;
            let (width, first) = if stream < self.layout.coupled {
                (2, 2 * stream)
            } else {
                (1, stream + self.layout.coupled)
            };
            for (channel, &entry) in self.layout.mapping[..channels].iter().enumerate() {
                let entry = usize::from(entry);
                if entry == usize::from(SILENT) || !(first..first + width).contains(&entry) {
                    continue;
                }
                let source = self.decoded.iter().skip(entry - first).step_by(width);
                for (slot, &value) in out[channel..].iter_mut().step_by(channels).zip(source) {
                    *slot = value;
                }
            }
        }
        for (channel, &entry) in self.layout.mapping[..channels].iter().enumerate() {
            if entry == SILENT {
                for slot in out[channel..].iter_mut().step_by(channels) {
                    *slot = 0.0;
                }
            }
        }
        Ok(samples)
    }
}
