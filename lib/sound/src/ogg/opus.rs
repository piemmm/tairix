//! The Opus mapping (RFC 7845): the identification header alone on the
//! stream's first page, the comment header finishing the pages after it, then
//! audio packets whose pages' granule positions count 48 kHz samples.
//!
//! The decoder's first `pre-skip` samples are not audio and are never played.
//! The first audio page's granule, less the samples its packets decode,
//! places the stream; the last page's may end it early, trimming its final
//! samples. Every other page's granule is held to the samples decoded before
//! it, so a stream's timeline and its decode cannot drift apart.
//!
//! A seek bisects the pages on their granules and decodes from 160 ms before
//! its target, twice the mapping's least: Opus carries its decoder's state
//! across packets, so a stream entered part-way converges onto the decode
//! from its start over that pre-roll rather than matching it from the first
//! sample. A seek near enough ahead decodes on instead, and one before the
//! pre-roll's reach starts from the beginning; both are exact.

use alloc::vec::Vec;

use tairix_abi::driver::audio::{ChannelMap, Rate, SampleFormat};
use tairix_util::fallible::filled;

use super::link::{first_packet, Group};
use super::page::{
    commit, last_page, length, packets_on, verified_page, PacketRegion, Page, Pages, Spot, Stream,
    MAX_PAGE_LEN,
};
use crate::input::{Region, SoundInput};
use crate::meta::Collector;
use crate::opus::{Layout, Multistream, MAX_PACKET_SAMPLES, RATE, SILENT};
use crate::{comment, vorbis, DecodeError, DecodeLimits, Encoding, SoundFormat, SoundInfo};

/// The identification and comment headers open with these.
pub(super) const HEAD_SIGNATURE: &[u8; 8] = b"OpusHead";
const TAGS_SIGNATURE: &[u8; 8] = b"OpusTags";
/// The identification header before its mapping table, and the table's
/// stream and coupled counts.
const HEAD_LEN: usize = 19;
/// The most bytes an audio packet may take: RFC 7845's bound for the channel
/// mapping families read here, past which a packet can only be padding.
const MAX_PACKET_LEN: usize = 61_440;
/// The decode ahead of a seek's target: 160 ms, twice the mapping's least,
/// as after 80 ms a seek into SILK can still be a sixteenth of full scale off
/// the decode from the start, and after 160 ms is under a hundredth.
const PRE_ROLL: u64 = 7680;

/// The identification header's fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Head {
    pub(super) layout: Layout,
    pub(super) pre_skip: u64,
    /// The output gain, Q7.8 dB.
    pub(super) gain: i16,
    pub(super) family: u8,
}

/// Parse an identification header (RFC 7845, section 5.1).
pub(super) fn parse_head(packet: &[u8]) -> Result<Head, DecodeError> {
    let malformed = DecodeError::OpusBadIdentification;
    let fields = packet
        .get(..HEAD_LEN)
        .filter(|fields| fields.starts_with(HEAD_SIGNATURE))
        .ok_or(malformed)?;
    if fields[8] >> 4 != 0 {
        return Err(DecodeError::OpusUnsupportedVersion);
    }
    let channels = usize::from(fields[9]);
    if channels == 0 {
        return Err(DecodeError::NoChannels);
    }
    let pre_skip = u64::from(u16::from_le_bytes([fields[10], fields[11]]));
    let gain = i16::from_le_bytes([fields[16], fields[17]]);
    let family = fields[18];
    let layout = if family == 0 {
        if channels > 2 {
            return Err(malformed);
        }
        Layout::single(channels)
    } else {
        let table = packet
            .get(HEAD_LEN..HEAD_LEN + 2 + channels)
            .ok_or(malformed)?;
        let mut mapping = [SILENT; 255];
        mapping[..channels].copy_from_slice(&table[2..]);
        let layout = Layout {
            channels,
            streams: usize::from(table[0]),
            coupled: usize::from(table[1]),
            mapping,
        };
        if !layout.is_valid() {
            return Err(malformed);
        }
        layout
    };
    Ok(Head {
        layout,
        pre_skip,
        gain,
        family,
    })
}

/// The output's channel positions: family 0's mono or stereo, family 1's
/// Vorbis order; any other family's channels have no positions to play.
fn channel_map(head: &Head, limits: &DecodeLimits) -> Result<ChannelMap, DecodeError> {
    let channels =
        u8::try_from(head.layout.channels).map_err(|_| DecodeError::ChannelsExceedLimit)?;
    let decoded = head.layout.streams + head.layout.coupled;
    if decoded > usize::from(limits.max_channels()) {
        return Err(DecodeError::ChannelsExceedLimit);
    }
    match head.family {
        0 | 1 => vorbis::channel_map(channels, limits),
        _ => Err(DecodeError::ChannelLayoutUnsupported),
    }
}

/// The multiplier an output gain in Q7.8 dB scales samples by, computed as
/// the reference decoder computes it.
#[allow(
    clippy::cast_possible_truncation,
    reason = "rounded to single precision once, as the reference's float build rounds it"
)]
fn gain_factor(gain: i16) -> f32 {
    const LOG2_10_OVER_5120: f32 = 6.488_141e-4;
    let exponent = LOG2_10_OVER_5120 * f32::from(gain);
    tairix_util::mathf::exp(core::f64::consts::LN_2 * f64::from(exponent)) as f32
}

/// Opus in Ogg, past its headers.
pub(crate) struct OggOpus {
    decoder: Multistream,
    channels: usize,
    gain: f32,
    pre_skip: u64,
    stream: Stream,
    first_audio: Spot,
    /// The first audio page's granule.
    first_granule: u64,
    /// The granule position before the first decoded sample: the first audio
    /// page's granule less what its packets decode, unless that page ended
    /// the stream early.
    start: u64,
    /// Frames the stream holds, where its last page was found.
    total: Option<u64>,
    /// The frame the stream's last page ends it at, once that page is read.
    end: Option<u64>,
    /// The packet decoded next, and the decoded sample its samples begin at.
    next: Option<Spot>,
    next_sample: u64,
    /// The decoded sample the latest granule-bearing page ended at.
    page_sample: u64,
    /// The decoded sample the held samples begin at, and how many.
    held_at: u64,
    held: usize,
    broken: Option<DecodeError>,
    pages: Pages,
    packet: Vec<u8>,
    pcm: Vec<f32>,
}

/// Read the headers of the Opus stream `group` chose, whose opening page
/// `pages` holds, in a link that ends at `end`.
pub(super) fn open(
    input: &mut (impl SoundInput + ?Sized),
    group: &Group,
    end: u64,
    mut pages: Pages,
    limits: &DecodeLimits,
    collector: &mut Collector,
) -> Result<(SoundInfo, OggOpus), DecodeError> {
    let first = group.page;
    let head_packet =
        first_packet(&first, &pages.current).ok_or(DecodeError::OpusBadIdentification)?;
    if head_packet.len() / 255 + 1 != first.segments {
        return Err(DecodeError::OpusBadIdentification);
    }
    let head = parse_head(head_packet)?;
    let channels = channel_map(&head, limits)?;
    let stream = Stream {
        serial: first.serial,
        end,
        last: last_page(input, first.serial, group.body, end, &mut pages.staging)?,
    };
    let spot = Spot {
        page: first,
        segment: first.segments,
        within: head_packet.len(),
    };
    let (mut region, after) = PacketRegion::new(
        &mut *input,
        &stream,
        spot,
        &pages.current,
        &mut pages.staging,
    )?
    .ok_or(DecodeError::OggNoAudio)?;
    let mut signature = [0u8; TAGS_SIGNATURE.len()];
    region.read_exact(0, &mut signature, DecodeError::OpusBadComment)?;
    if &signature != TAGS_SIGNATURE {
        return Err(DecodeError::OpusBadComment);
    }
    region.base = TAGS_SIGNATURE.len() as u64;
    comment::read(&mut region, collector, DecodeError::OpusBadComment)?;
    if after.segment != after.page.segments {
        return Err(DecodeError::OpusBadComment);
    }
    if after.page != spot.page {
        let page = verified_page(input, after.page.offset, &mut pages.current)?;
        if page != after.page {
            return Err(DecodeError::OggBadPage);
        }
    }
    let oom = DecodeError::OutOfMemory;
    let mut reader = OggOpus {
        decoder: Multistream::new(&head.layout)?,
        channels: head.layout.channels,
        gain: gain_factor(head.gain),
        pre_skip: head.pre_skip,
        stream,
        first_audio: after,
        first_granule: 0,
        start: 0,
        total: None,
        end: None,
        next: Some(after),
        next_sample: 0,
        page_sample: 0,
        held_at: 0,
        held: 0,
        broken: None,
        pages,
        packet: Vec::new(),
        pcm: filled(MAX_PACKET_SAMPLES * head.layout.channels, 0.0).ok_or(oom)?,
    };
    reader.place(input)?;
    reader.total = length(reader.stream.last, reader.start + reader.pre_skip)?;
    let sound = SoundInfo {
        format: SoundFormat::Ogg,
        encoding: Encoding::Opus,
        rate: Rate::new(RATE).map_err(|_| DecodeError::RateOutOfRange)?,
        channels,
        sample: SampleFormat::F32,
        frames: reader.total,
        seekable: true,
        data_length: None,
    };
    Ok((sound, reader))
}

impl OggOpus {
    /// Read how the first audio page places the stream (RFC 7845, section
    /// 4.5), then return to its first packet.
    fn place(&mut self, input: &mut (impl SoundInput + ?Sized)) -> Result<(), DecodeError> {
        let mut spot = self.first_audio;
        let mut decoded = 0u64;
        let (granule, last) = loop {
            let read = self.stream.packet(
                input,
                spot,
                &mut self.pages,
                &mut self.packet,
                MAX_PACKET_LEN,
            )?;
            let Some(read) = read else {
                break (decoded, true);
            };
            decoded += self.decoder.samples(&self.packet)? as u64;
            let ends_page = read.ends_page(&self.pages);
            let granule = read.granule(&self.pages);
            let last = read.on_last_page();
            commit(&mut self.pages, &mut spot, &read);
            if ends_page {
                break (granule.ok_or(DecodeError::OggGranuleMismatch)?, last);
            }
        };
        self.first_granule = granule;
        self.start = if granule >= decoded {
            granule - decoded
        } else if last {
            if granule < self.pre_skip {
                return Err(DecodeError::OggGranuleMismatch);
            }
            0
        } else {
            return Err(DecodeError::OggGranuleMismatch);
        };
        self.restart(input)
    }

    /// Return to the first audio packet, nothing held.
    fn restart(&mut self, input: &mut (impl SoundInput + ?Sized)) -> Result<(), DecodeError> {
        self.resume_at(input, self.first_audio, 0)
    }

    /// Decode on from the packet at `spot`, whose samples begin at decoded
    /// sample `sample`, the decoder starting afresh.
    fn resume_at(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        spot: Spot,
        sample: u64,
    ) -> Result<(), DecodeError> {
        let page = verified_page(input, spot.page.offset, &mut self.pages.current)?;
        if page != spot.page {
            return Err(DecodeError::OggBadPage);
        }
        self.broken = None;
        self.decoder.reset();
        self.next = Some(spot);
        self.next_sample = sample;
        self.page_sample = sample;
        self.held_at = sample;
        self.held = 0;
        Ok(())
    }

    /// The frame the stream stops at, as far as is known.
    fn limit(&self) -> Option<u64> {
        match (self.total, self.end) {
            (Some(total), Some(end)) => Some(total.min(end)),
            (total, end) => total.or(end),
        }
    }

    fn holds(&self, frame: u64) -> bool {
        let sample = frame.saturating_add(self.pre_skip);
        self.held_at <= sample && sample - self.held_at < self.held as u64
    }

    /// Write the frames from `position` that `out` has room for.
    pub(crate) fn read(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        position: u64,
        out: &mut [u8],
    ) -> Result<usize, DecodeError> {
        let read = self.read_from(input, position, out);
        if let Err(err) = read {
            if !super::is_input(err) {
                self.broken = Some(err);
            }
        }
        read
    }

    fn read_from(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        position: u64,
        out: &mut [u8],
    ) -> Result<usize, DecodeError> {
        let channels = self.channels;
        let frame_bytes = channels * 4;
        let room = out.len() / frame_bytes;
        let mut written = 0;
        while written < room {
            let at = position + written as u64;
            let limit = self.limit();
            if limit.is_some_and(|limit| at >= limit) {
                break;
            }
            if !self.holds(at) {
                match self.advance_to(input, at) {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(err) if written > 0 => {
                        if !super::is_input(err) {
                            self.broken = Some(err);
                        }
                        return Ok(written);
                    }
                    Err(err) => return Err(err),
                }
                continue;
            }
            let offset = usize::try_from(at + self.pre_skip - self.held_at).unwrap_or(0);
            let mut count = (self.held - offset).min(room - written);
            if let Some(limit) = limit {
                count = count.min(usize::try_from(limit - at).unwrap_or(usize::MAX));
            }
            let samples = &self.pcm[offset * channels..(offset + count) * channels];
            let bytes = &mut out[written * frame_bytes..(written + count) * frame_bytes];
            for (slot, &sample) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(samples) {
                slot.copy_from_slice(&(sample * self.gain).to_le_bytes());
            }
            written += count;
        }
        if written == 0 && self.end.is_none() {
            self.finish(input)?;
        }
        Ok(written)
    }

    /// Read the stream past the last sample it states, where its last page
    /// has not been seen: a page cut short there is the file's damage.
    fn finish(&mut self, input: &mut (impl SoundInput + ?Sized)) -> Result<(), DecodeError> {
        while self.end.is_none() && self.decode_next(input)? {}
        Ok(())
    }

    /// Decode until the frame at `frame` is held, seeking first unless it is
    /// near enough ahead to decode on to; `false` where the stream ends first.
    fn advance_to(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        frame: u64,
    ) -> Result<bool, DecodeError> {
        let target = frame.saturating_add(self.pre_skip);
        let near = PRE_ROLL + MAX_PACKET_SAMPLES as u64;
        let sequential =
            self.next.is_some() && self.next_sample <= target && target - self.next_sample <= near;
        if !sequential {
            self.locate(input, target)?;
        } else if let Some(err) = self.broken {
            return Err(err);
        }
        loop {
            if !self.decode_next(input)? {
                return Ok(false);
            }
            if self.holds(frame) {
                return Ok(true);
            }
        }
    }

    /// Decode the next packet; `false` where the stream has ended.
    fn decode_next(&mut self, input: &mut (impl SoundInput + ?Sized)) -> Result<bool, DecodeError> {
        let Some(spot) = self.next else {
            return Ok(false);
        };
        let read = self.stream.packet(
            input,
            spot,
            &mut self.pages,
            &mut self.packet,
            MAX_PACKET_LEN,
        )?;
        let Some(read) = read else {
            self.next = None;
            return Ok(false);
        };
        let samples = self.decoder.decode(&self.packet, &mut self.pcm)?;
        let held_at = self.next_sample;
        let next_sample = held_at + samples as u64;
        let ends_page = read.ends_page(&self.pages);
        let granule = read.granule(&self.pages);
        let last = read.on_last_page();
        let end = if ends_page {
            let granule = granule.ok_or(DecodeError::OggGranuleMismatch)?;
            self.check(granule, last, next_sample)?
        } else {
            None
        };
        let mut next = spot;
        commit(&mut self.pages, &mut next, &read);
        self.next = Some(next);
        if ends_page {
            self.page_sample = next_sample;
        }
        (self.held_at, self.held, self.next_sample) = (held_at, samples, next_sample);
        self.end = end.or(self.end);
        Ok(true)
    }

    /// Hold the granule of a page to the samples decoded, `next_sample` once
    /// its packets are: the stream's last page may end the stream anywhere
    /// after the page before it, answering the frame it ends at; every other
    /// must name the sample reached.
    fn check(
        &self,
        granule: u64,
        last: bool,
        next_sample: u64,
    ) -> Result<Option<u64>, DecodeError> {
        let sample = granule
            .checked_sub(self.start)
            .ok_or(DecodeError::OggGranuleMismatch)?;
        if !last {
            return if sample == next_sample {
                Ok(None)
            } else {
                Err(DecodeError::OggGranuleMismatch)
            };
        }
        if sample < self.page_sample || sample > next_sample || sample < self.pre_skip {
            return Err(DecodeError::OggGranuleMismatch);
        }
        Ok(Some(sample - self.pre_skip))
    }

    /// Set decoding to resume at least the pre-roll before decoded sample
    /// `target`, by bisecting the stream's pages on their granules; from the
    /// beginning where that reaches before the first audio page's end.
    fn locate(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        target: u64,
    ) -> Result<(), DecodeError> {
        let wanted = self.start.saturating_add(target.saturating_sub(PRE_ROLL));
        if wanted < self.first_granule {
            return self.restart(input);
        }
        let best = self.stream.entry_at_most(
            input,
            self.first_audio.page.offset,
            wanted,
            &mut self.pages.staging,
            |page, bytes| Some((entry(page, bytes), page.granule)),
        )?;
        match best {
            Some((spot, granule)) if granule >= self.start => {
                self.resume_at(input, spot, granule - self.start)
            }
            _ => self.restart(input),
        }
    }
}

/// Where decoding resumes after `page`: the packet after the last that
/// completes on it, whose samples begin where the page's granule says.
fn entry(page: &Page, bytes: &[u8]) -> Spot {
    packets_on(page, bytes)
        .find(|packet| !packet.completes)
        .map_or(
            Spot {
                page: *page,
                segment: page.segments,
                within: usize::try_from(page.body_len).unwrap_or(usize::MAX),
            },
            |packet| packet.spot,
        )
}

/// The most bytes one call reads for an Opus stream in Ogg writing `frames`
/// frames: every packet a seek's pre-roll and the frames take, at the
/// mapping's largest, with their pages' headers, and a page beyond.
pub(super) const fn max_working_set(frames: u64) -> u64 {
    let packets = (frames + PRE_ROLL).div_ceil(120) + 2;
    let bytes = packets * MAX_PACKET_LEN as u64;
    bytes + bytes / 255 * 2 + 2 * MAX_PAGE_LEN as u64
}

#[cfg(test)]
#[path = "opus_tests.rs"]
mod tests;
