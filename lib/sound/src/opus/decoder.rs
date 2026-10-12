//! One Opus stream's decoder (RFC 6716, section 4.5, and the reference
//! implementation's `opus_decoder.c`): each frame through SILK, CELT or
//! both, and the switches between them — the redundant CELT frames an
//! encoder sends across a switch, the concealment that covers a switch
//! without one, and the cross-fades that join them.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "the reference's arithmetic, conversion for conversion: lengths and bit counts the format bounds far within 16 bits"
)]

use alloc::vec::Vec;

use tairix_util::fallible::filled;

use super::celt::{Celt, Input, Work, WINDOW};
use super::range::RangeDecoder;
use super::silk::{InternalRate, Request, Silk};
use super::{Bandwidth, Mode, Packet};
use crate::DecodeError;

const F20: usize = 960;
const F10: usize = F20 / 2;
const F5: usize = F10 / 2;
const F2_5: usize = F5 / 2;
/// The longest frame: 60 ms.
const MAX_FRAME: usize = 3 * F20;

/// One stream's decoder, for one or two output channels.
pub(crate) struct Decoder {
    channels: usize,
    silk: Silk,
    celt: Celt,
    stream_channels: usize,
    bandwidth: Bandwidth,
    mode: Mode,
    prev_mode: Option<Mode>,
    frame_size: usize,
    prev_redundancy: bool,
    silk_rate: InternalRate,
    silk_channels: usize,
    /// The range decoder's final state over the last packet: the encoder's
    /// exactly when every symbol was read as written, which only the tests
    /// compare.
    #[cfg(test)]
    range: u32,
    silk_out: Vec<i16>,
    transition: Vec<f32>,
    redundant: Vec<f32>,
}

/// `out = w² · second + (1 − w²) · first` over the cross-fade's 2.5 ms, the
/// window's square rising from the first signal to the second.
fn smooth_fade(first: &[f32], second: &[f32], out: &mut [f32], channels: usize) {
    for c in 0..channels {
        for (i, &w) in WINDOW.iter().enumerate().take(F2_5) {
            let w = w * w;
            let at = i * channels + c;
            out[at] = w * second[at] + (1.0 - w) * first[at];
        }
    }
}

impl Decoder {
    /// A decoder for `channels` output channels, one or two.
    pub(crate) fn new(channels: usize) -> Result<Self, DecodeError> {
        let oom = DecodeError::OutOfMemory;
        Ok(Self {
            channels,
            silk: Silk::new(channels),
            celt: Celt::new(channels)?,
            stream_channels: channels,
            bandwidth: Bandwidth::Full,
            mode: Mode::Celt,
            prev_mode: None,
            frame_size: F2_5,
            prev_redundancy: false,
            silk_rate: InternalRate::Wide,
            silk_channels: channels,
            #[cfg(test)]
            range: 0,
            silk_out: filled(MAX_FRAME * channels, 0i16).ok_or(oom)?,
            transition: filled(F5 * channels, 0.0f32).ok_or(oom)?,
            redundant: filled(F5 * channels, 0.0f32).ok_or(oom)?,
        })
    }

    /// Forget every packet decoded, as a stream starting afresh.
    pub(crate) fn reset(&mut self) {
        self.silk.reset();
        self.celt.reset();
        self.stream_channels = self.channels;
        self.bandwidth = Bandwidth::Full;
        self.mode = Mode::Celt;
        self.prev_mode = None;
        self.frame_size = F2_5;
        self.prev_redundancy = false;
    }

    #[cfg(test)]
    pub(crate) const fn final_range(&self) -> u32 {
        self.range
    }

    /// Decode `packet` into `out`, interleaved; the samples a channel.
    pub(crate) fn decode(
        &mut self,
        packet: &Packet<'_>,
        out: &mut [f32],
        work: &mut Work,
    ) -> Result<usize, DecodeError> {
        let samples = packet.samples();
        if out.len() < samples * self.channels {
            return Err(DecodeError::BufferTooSmall);
        }
        self.mode = packet.toc.mode;
        self.bandwidth = packet.toc.bandwidth;
        self.frame_size = packet.toc.frame_size;
        self.stream_channels = if packet.toc.stereo { 2 } else { 1 };
        let mut at = 0;
        for frame in packet.frames() {
            let size = self.decode_frame(
                Some(frame),
                &mut out[at * self.channels..],
                self.frame_size,
                work,
            )?;
            at += size;
        }
        Ok(at)
    }

    /// Decode one frame, or conceal one when `frame` is `None` or empty,
    /// into `pcm`; its samples a channel.
    fn decode_frame(
        &mut self,
        frame: Option<&[u8]>,
        pcm: &mut [f32],
        frame_size: usize,
        work: &mut Work,
    ) -> Result<usize, DecodeError> {
        // Lent out and returned on every path, so a refused frame leaves the
        // decoder whole for the stream it next decodes.
        let mut transition = core::mem::take(&mut self.transition);
        let mut redundant = core::mem::take(&mut self.redundant);
        let decoded = self.decode_frame_with(
            frame,
            pcm,
            frame_size,
            &mut transition,
            &mut redundant,
            work,
        );
        self.transition = transition;
        self.redundant = redundant;
        decoded
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the reference's opus_decode_frame, kept whole so the port can be read against it"
    )]
    fn decode_frame_with(
        &mut self,
        frame: Option<&[u8]>,
        pcm: &mut [f32],
        frame_size: usize,
        transition_audio: &mut [f32],
        redundant_audio: &mut [f32],
        work: &mut Work,
    ) -> Result<usize, DecodeError> {
        let channels = self.channels;
        let mut frame_size = frame_size.min(MAX_FRAME);
        let data = frame.filter(|bytes| bytes.len() > 1);
        if data.is_none() {
            frame_size = frame_size.min(self.frame_size);
        }
        let (mut audio_size, mode, bandwidth);
        if data.is_some() {
            audio_size = self.frame_size;
            mode = self.mode;
            bandwidth = Some(self.bandwidth);
        } else {
            audio_size = frame_size;
            bandwidth = None;
            let Some(previous) = (if self.prev_redundancy {
                Some(Mode::Celt)
            } else {
                self.prev_mode
            }) else {
                pcm[..audio_size * channels].fill(0.0);
                return Ok(audio_size);
            };
            mode = previous;
            if audio_size > F20 {
                let mut done = 0;
                while done < audio_size {
                    let step = self.decode_frame(
                        None,
                        &mut pcm[done * channels..],
                        (audio_size - done).min(F20),
                        work,
                    )?;
                    done += step;
                }
                return Ok(frame_size);
            }
            if audio_size < F20 {
                if audio_size > F10 {
                    audio_size = F10;
                } else if mode != Mode::Silk && audio_size > F5 && audio_size < F10 {
                    audio_size = F5;
                }
            }
        }
        let mut owned = RangeDecoder::new(data.unwrap_or(&[]));
        let mut len = data.map_or(0, <[u8]>::len);
        let switching = data.is_some()
            && self.prev_mode.is_some_and(|previous| {
                (mode == Mode::Celt && previous != Mode::Celt && !self.prev_redundancy)
                    || (mode != Mode::Celt && previous == Mode::Celt)
            });
        let mut transition = switching;
        if transition && mode == Mode::Celt {
            self.decode_frame(None, transition_audio, F5.min(audio_size), work)?;
        }
        if audio_size > frame_size {
            return Err(DecodeError::OpusPacketInvalid);
        }
        frame_size = audio_size;

        if mode != Mode::Celt {
            if self.prev_mode == Some(Mode::Celt) {
                self.silk.reset();
            }
            if data.is_some() {
                self.silk_channels = self.stream_channels;
                self.silk_rate = match (mode, bandwidth) {
                    (Mode::Silk, Some(Bandwidth::Narrow)) => InternalRate::Narrow,
                    (Mode::Silk, Some(Bandwidth::Medium)) => InternalRate::Medium,
                    _ => InternalRate::Wide,
                };
            }
            let mut decoded = 0;
            while decoded < frame_size {
                let request = Request {
                    lost: data.is_none(),
                    first: decoded == 0,
                    payload_ms: (1000 * audio_size / 48_000).max(10),
                    rate: self.silk_rate,
                    stream_channels: self.silk_channels,
                };
                let out = &mut self.silk_out[decoded * channels..];
                match self.silk.decode(&request, &mut owned, out) {
                    Ok(written) => decoded += written,
                    Err(_) if data.is_none() => {
                        self.silk_out[..frame_size * channels].fill(0);
                        decoded = frame_size;
                    }
                    Err(error) => return Err(error),
                }
            }
        }

        let mut redundancy = false;
        let mut celt_to_silk = false;
        let mut redundancy_bytes = 0;
        if mode != Mode::Celt
            && data.is_some()
            && owned.tell() + 17 + 20 * i32::from(mode == Mode::Hybrid) <= 8 * len as i32
        {
            redundancy = mode != Mode::Hybrid || owned.bit_logp(12);
            if redundancy {
                celt_to_silk = owned.bit_logp(1);
                redundancy_bytes = if mode == Mode::Hybrid {
                    owned.uint(256) as usize + 2
                } else {
                    len.saturating_sub(((owned.tell() + 7) >> 3) as usize)
                };
                len = len.saturating_sub(redundancy_bytes);
                if (len as i32) * 8 < owned.tell() || redundancy_bytes > data.map_or(0, <[u8]>::len)
                {
                    len = 0;
                    redundancy_bytes = 0;
                    redundancy = false;
                }
                owned.shrink(redundancy_bytes);
            }
        }
        let start_band = if mode == Mode::Celt { 0 } else { 17 };
        if redundancy {
            transition = false;
        }
        if transition && mode != Mode::Celt {
            self.decode_frame(None, transition_audio, F5.min(audio_size), work)?;
        }
        if let Some(bandwidth) = bandwidth {
            self.celt.set_end_band(bandwidth.end_band());
        }
        self.celt.set_stream_channels(self.stream_channels);
        let redundant_frame = data.map_or(&[][..], |bytes| &bytes[len..len + redundancy_bytes]);
        #[cfg(test)]
        let mut redundant_range = 0;
        if redundancy && celt_to_silk {
            self.celt.set_start_band(0);
            self.celt
                .decode(Input::Alone(redundant_frame), redundant_audio, F5, work)?;
            #[cfg(test)]
            {
                redundant_range = self.celt.final_range();
            }
        }
        self.celt.set_start_band(start_band);
        if mode == Mode::Silk {
            pcm[..frame_size * channels].fill(0.0);
            if self.prev_mode == Some(Mode::Hybrid)
                && !(redundancy && celt_to_silk && self.prev_redundancy)
            {
                self.celt.set_start_band(0);
                self.celt.decode(
                    Input::Alone(&[0xFF, 0xFF]),
                    &mut pcm[..F2_5 * channels],
                    F2_5,
                    work,
                )?;
            }
        } else {
            let celt_frame = F20.min(frame_size);
            if Some(mode) != self.prev_mode && self.prev_mode.is_some() && !self.prev_redundancy {
                self.celt.reset();
            }
            let input = if data.is_some() && len > 1 {
                Input::Shared(&mut owned)
            } else {
                Input::Lost
            };
            self.celt
                .decode(input, &mut pcm[..celt_frame * channels], celt_frame, work)?;
        }
        if mode != Mode::Celt {
            for (value, &silk) in pcm[..frame_size * channels].iter_mut().zip(&self.silk_out) {
                *value += (1.0 / 32768.0) * f32::from(silk);
            }
        }
        if redundancy && !celt_to_silk {
            self.celt.reset();
            self.celt.set_start_band(0);
            self.celt
                .decode(Input::Alone(redundant_frame), redundant_audio, F5, work)?;
            #[cfg(test)]
            {
                redundant_range = self.celt.final_range();
            }
            let at = channels * (frame_size - F2_5);
            let ending: [f32; 2 * F2_5] =
                core::array::from_fn(|i| pcm.get(at + i).copied().unwrap_or(0.0));
            smooth_fade(
                &ending,
                &redundant_audio[channels * F2_5..],
                &mut pcm[at..],
                channels,
            );
        }
        if redundancy
            && celt_to_silk
            && (self.prev_mode != Some(Mode::Silk) || self.prev_redundancy)
        {
            pcm[..F2_5 * channels].copy_from_slice(&redundant_audio[..F2_5 * channels]);
            let at = channels * F2_5;
            let opening: [f32; 2 * F2_5] =
                core::array::from_fn(|i| pcm.get(at + i).copied().unwrap_or(0.0));
            smooth_fade(&redundant_audio[at..], &opening, &mut pcm[at..], channels);
        }
        if transition {
            if audio_size >= F5 {
                pcm[..channels * F2_5].copy_from_slice(&transition_audio[..channels * F2_5]);
                let at = channels * F2_5;
                let opening: [f32; 2 * F2_5] =
                    core::array::from_fn(|i| pcm.get(at + i).copied().unwrap_or(0.0));
                smooth_fade(&transition_audio[at..], &opening, &mut pcm[at..], channels);
            } else {
                let opening: [f32; 2 * F2_5] =
                    core::array::from_fn(|i| pcm.get(i).copied().unwrap_or(0.0));
                smooth_fade(transition_audio, &opening, pcm, channels);
            }
        }
        #[cfg(test)]
        {
            self.range = if len <= 1 {
                0
            } else {
                owned.range() ^ redundant_range
            };
        }
        self.prev_mode = Some(mode);
        self.prev_redundancy = redundancy && !celt_to_silk;
        if pcm[..audio_size * channels]
            .iter()
            .any(|value| !value.is_finite())
        {
            return Err(DecodeError::OpusPacketInvalid);
        }
        Ok(audio_size)
    }
}
