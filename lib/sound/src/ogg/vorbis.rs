//! The Vorbis mapping (Vorbis I, appendix A): the identification header alone
//! on the stream's first page, the comment and setup headers after it, then
//! audio packets whose pages' granule positions count samples.
//!
//! The first audio page's granule places the stream: less than its packets
//! return trims the samples before time zero, more starts the stream later.
//! The last page's granule trims the samples past the end, from its final
//! packet. A seek bisects the pages on their granules and decodes from a
//! packet whose position its page fixes, that packet only priming the
//! overlap.
//!
//! Every other page's granule is held to the samples decoded before it, and
//! an audio packet that is not one is refused rather than discarded: either
//! would leave the samples' positions and the stream's own timeline apart, so
//! a seek could no longer land where decoding from the start does. An empty
//! packet carries nothing and is passed over.

use alloc::vec::Vec;

use tairix_abi::driver::audio::{Rate, SampleFormat};

use super::link::{first_packet, Group};
use super::page::{
    commit, last_page, length, packets_on, verified_page, PacketRegion, Page, Pages, Spot, Stream,
    MAX_PAGE_LEN,
};
use crate::input::{Region, SoundInput};
use crate::meta::Collector;
use crate::vorbis::{self, Decoder, Identification, HEADER_PREFIX_LEN, MAX_SETUP_LEN};
use crate::{comment, DecodeError, DecodeLimits, Encoding, SoundFormat, SoundInfo};

/// Vorbis in Ogg, past its headers.
pub(crate) struct OggVorbis {
    decoder: Decoder,
    identification: Identification,
    stream: Stream,
    first_audio: Spot,
    /// The first audio page's granule: a frame before it is reached by
    /// decoding from the first audio packet.
    first_granule: u64,
    /// A frame is a granule position less this.
    base: u64,
    /// The frame the first audio packets' first sample is: zero, or before
    /// zero where the stream trims samples before time zero.
    start_frame: i64,
    /// Frames the stream holds, where its last page was found.
    total: Option<u64>,
    /// The frame the stream's last page ends it at, once that page is read.
    end: Option<u64>,
    /// The packet decoded next.
    next: Option<Spot>,
    /// The frame the samples of the packet after the held block begin at.
    next_frame: i64,
    /// The frame the decoder's held samples begin at, and how many.
    held_at: i64,
    held: usize,
    /// What the stream met that it cannot read past: decoding on from the
    /// same place meets it again, and only a seek, which rebuilds the
    /// decoder's state, leaves it behind.
    broken: Option<DecodeError>,
    pages: Pages,
    packet: Vec<u8>,
}

/// Read the headers of the Vorbis stream `group` chose, whose opening page
/// `pages` holds, in a link that ends at `end`.
pub(super) fn open(
    input: &mut (impl SoundInput + ?Sized),
    group: &Group,
    end: u64,
    mut pages: Pages,
    limits: &DecodeLimits,
    collector: &mut Collector,
) -> Result<(SoundInfo, OggVorbis), DecodeError> {
    let first = group.page;
    let identification_packet =
        first_packet(&first, &pages.current).ok_or(DecodeError::VorbisBadIdentification)?;
    let identification = Identification::parse(identification_packet)?;
    let channels = vorbis::channel_map(identification.channels, limits)?;
    let rate = Rate::new(identification.rate).map_err(|_| DecodeError::RateOutOfRange)?;
    let stream = Stream {
        serial: first.serial,
        end,
        last: last_page(input, first.serial, group.body, end, &mut pages.staging)?,
    };
    let mut spot = Spot::start_of(first);
    spot.within = identification_packet.len();
    spot.segment = identification_packet.len() / 255 + 1;
    let (mut region, after) = PacketRegion::new(
        &mut *input,
        &stream,
        spot,
        &pages.current,
        &mut pages.staging,
    )?
    .ok_or(DecodeError::OggNoAudio)?;
    let mut prefix = [0u8; HEADER_PREFIX_LEN];
    region.read_exact(0, &mut prefix, DecodeError::VorbisBadComment)?;
    if !vorbis::is_comment(&prefix) {
        return Err(DecodeError::VorbisBadComment);
    }
    region.base = HEADER_PREFIX_LEN as u64;
    let comments = comment::read(&mut region, collector, DecodeError::VorbisBadComment)?;
    let mut framing = [0u8];
    region.read_exact(comments.len, &mut framing, DecodeError::VorbisBadComment)?;
    if !vorbis::is_framed(framing[0]) {
        return Err(DecodeError::VorbisBadComment);
    }
    if after.page != spot.page {
        let page = verified_page(input, after.page.offset, &mut pages.current)?;
        if page != after.page {
            return Err(DecodeError::OggBadPage);
        }
    }
    spot = after;
    let mut packet = Vec::new();
    let read = stream
        .packet(input, spot, &mut pages, &mut packet, MAX_SETUP_LEN)?
        .ok_or(DecodeError::OggNoAudio)?;
    let decoder = Decoder::new(&identification, &packet)?;
    commit(&mut pages, &mut spot, &read);
    let first_audio = spot;
    let mut reader = OggVorbis {
        decoder,
        identification,
        stream,
        first_audio,
        first_granule: 0,
        base: 0,
        start_frame: 0,
        total: None,
        end: None,
        next: Some(first_audio),
        next_frame: 0,
        held_at: 0,
        held: 0,
        broken: None,
        pages,
        packet,
    };
    reader.place(input)?;
    reader.total = length(reader.stream.last, reader.base)?;
    let sound = SoundInfo {
        format: SoundFormat::Ogg,
        encoding: Encoding::Vorbis,
        rate,
        channels,
        sample: SampleFormat::F32,
        frames: reader.total,
        seekable: true,
        data_length: None,
    };
    Ok((sound, reader))
}

impl OggVorbis {
    /// Read how the first audio page places the stream, then return to its
    /// first packet: the samples the packets completing on the first page
    /// that states a granule return, against that granule (Vorbis I, section
    /// A.2).
    fn place(&mut self, input: &mut (impl SoundInput + ?Sized)) -> Result<(), DecodeError> {
        let mut spot = self.first_audio;
        let mut returned = 0u64;
        let mut previous = None;
        let (granule, last) = loop {
            let Some(read) = self.stream.packet(
                input,
                spot,
                &mut self.pages,
                &mut self.packet,
                self.identification.max_long_packet_len(),
            )?
            else {
                break (returned, true);
            };
            if let Some(size) = self.decoder.blocksize(&self.packet) {
                if let Some(before) = previous {
                    returned += (before + size) as u64 / 4;
                }
                previous = Some(size);
            }
            let granule = read.granule(&self.pages);
            let last = read.on_last_page();
            commit(&mut self.pages, &mut spot, &read);
            if let Some(granule) = granule {
                break (granule, last);
            }
        };
        self.first_granule = granule;
        (self.base, self.start_frame) = if granule >= returned {
            (granule - returned, 0)
        } else if last {
            // A stream on one page has its end cut, never its beginning.
            (0, 0)
        } else {
            (
                0,
                -i64::try_from(returned - granule).map_err(|_| DecodeError::OggBadPacket)?,
            )
        };
        self.restart(input)
    }

    /// Return to the first audio packet, nothing held.
    fn restart(&mut self, input: &mut (impl SoundInput + ?Sized)) -> Result<(), DecodeError> {
        self.resume_at(input, self.first_audio, self.start_frame)
    }

    /// Decode on from the packet at `spot`, the samples of the packet after it
    /// beginning at frame `frame`.
    fn resume_at(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        spot: Spot,
        frame: i64,
    ) -> Result<(), DecodeError> {
        let page = verified_page(input, spot.page.offset, &mut self.pages.current)?;
        if page != spot.page {
            return Err(DecodeError::OggBadPage);
        }
        self.broken = None;
        self.decoder.reset();
        self.next = Some(spot);
        self.next_frame = frame;
        self.held_at = frame;
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
        let Ok(frame) = i64::try_from(frame) else {
            return false;
        };
        let held = i64::try_from(self.held).unwrap_or(i64::MAX);
        self.held_at <= frame && frame - self.held_at < held
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
        let channels = usize::from(self.identification.channels);
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
            let offset =
                usize::try_from(i64::try_from(at).unwrap_or(i64::MAX) - self.held_at).unwrap_or(0);
            let mut count = (self.held - offset).min(room - written);
            if let Some(limit) = limit {
                count = count.min(usize::try_from(limit - at).unwrap_or(usize::MAX));
            }
            let samples = &self.decoder.pcm()[offset * channels..(offset + count) * channels];
            let bytes = &mut out[written * frame_bytes..(written + count) * frame_bytes];
            for (slot, sample) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(samples) {
                slot.copy_from_slice(&sample.to_le_bytes());
            }
            written += count;
        }
        if written == 0 && self.end.is_none() {
            self.finish(input)?;
        }
        Ok(written)
    }

    /// Read the stream past the last sample it states, where its last page
    /// has not been seen: a page cut short there is the file's damage, and
    /// is reported rather than taken for the stream's end.
    fn finish(&mut self, input: &mut (impl SoundInput + ?Sized)) -> Result<(), DecodeError> {
        while self.end.is_none() && self.decode_next(input)? {}
        Ok(())
    }

    /// Decode until the frame at `frame` is held, seeking first unless it is
    /// just ahead; `false` where the stream ends first.
    fn advance_to(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        frame: u64,
    ) -> Result<bool, DecodeError> {
        let target = i64::try_from(frame).map_err(|_| DecodeError::SeekPastEnd)?;
        let near = i64::try_from(2 * self.identification.long_block()).unwrap_or(i64::MAX);
        let sequential =
            self.next.is_some() && self.next_frame <= target && target - self.next_frame <= near;
        if !sequential {
            self.locate(input, frame)?;
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
            self.identification.max_long_packet_len(),
        )?;
        let Some(read) = read else {
            self.next = None;
            return Ok(false);
        };
        if let Some(size) = self.decoder.blocksize(&self.packet) {
            if self.packet.len() > self.identification.max_packet_len(size) {
                return Err(DecodeError::OggPacketTooLarge);
            }
        }
        let granule = read.granule(&self.pages);
        let last = read.on_last_page();
        let (held_at, held, next_frame) = match self.decoder.decode(&self.packet)? {
            Some(frames) => {
                let added = i64::try_from(frames).map_err(|_| DecodeError::OggBadPacket)?;
                (self.next_frame, frames, self.next_frame + added)
            }
            None if self.packet.is_empty() => (self.held_at, self.held, self.next_frame),
            None => return Err(DecodeError::VorbisBadPacket),
        };
        let end = match granule {
            Some(granule) => self.check(granule, last, held_at, next_frame)?,
            None => None,
        };
        let mut next = spot;
        commit(&mut self.pages, &mut next, &read);
        self.next = Some(next);
        (self.held_at, self.held, self.next_frame) = (held_at, held, next_frame);
        self.end = end.or(self.end);
        Ok(true)
    }

    /// Hold the granule of the page a packet finished to the samples decoded,
    /// those of the packet spanning `held_at` to `next_frame`: the stream's
    /// last page may only cut that final packet short, answering where the
    /// stream then ends; every other must name the position reached.
    fn check(
        &self,
        granule: u64,
        last: bool,
        held_at: i64,
        next_frame: i64,
    ) -> Result<Option<u64>, DecodeError> {
        let frame = i64::try_from(granule)
            .ok()
            .and_then(|granule| granule.checked_sub(i64::try_from(self.base).ok()?))
            .ok_or(DecodeError::OggGranuleMismatch)?;
        if !last {
            return if frame == next_frame {
                Ok(None)
            } else {
                Err(DecodeError::OggGranuleMismatch)
            };
        }
        if frame < held_at.max(0) || frame > next_frame {
            return Err(DecodeError::OggGranuleMismatch);
        }
        u64::try_from(frame)
            .map(Some)
            .map_err(|_| DecodeError::OggGranuleMismatch)
    }

    /// Set decoding to resume at a packet at or before `frame` and near it,
    /// by bisecting the stream's pages on their granules.
    fn locate(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        frame: u64,
    ) -> Result<(), DecodeError> {
        let target = frame.saturating_add(self.base);
        if target < self.first_granule {
            return self.restart(input);
        }
        let decoder = &self.decoder;
        let best = self.stream.entry_at_most(
            input,
            self.first_audio.page.offset,
            target,
            &mut self.pages.staging,
            |page, bytes| entry(decoder, page, bytes),
        )?;
        match best {
            Some((spot, granule)) => {
                let frame = i64::try_from(granule).unwrap_or(i64::MAX)
                    - i64::try_from(self.base).unwrap_or(i64::MAX);
                self.resume_at(input, spot, frame)
            }
            None => self.restart(input),
        }
    }
}

/// Where decoding can resume on `page`: its first audio packet that both
/// starts and completes on it, with the granule position the samples of the
/// packet after it begin at — the page's granule, less what the page's later
/// packets return.
fn entry(decoder: &Decoder, page: &Page, bytes: &[u8]) -> Option<(Spot, u64)> {
    let mut entry = None;
    let mut previous = None;
    let mut later = 0u64;
    for packet in packets_on(page, bytes) {
        if !packet.completes {
            break;
        }
        let Some(size) = decoder.blocksize(packet.bytes) else {
            continue;
        };
        match previous {
            Some(before) => later += (before + size) as u64 / 4,
            None => entry = Some(packet.spot),
        }
        previous = Some(size);
    }
    Some((entry?, page.granule.checked_sub(later)?))
}

/// The most bytes one call reads for a Vorbis stream in Ogg of up to
/// `channels` channels writing `frames` frames: the packets, with their
/// pages' headers, and a page beyond.
pub(super) const fn max_working_set(channels: usize, frames: u64) -> u64 {
    let packets = vorbis::max_working_set(channels, frames);
    packets + packets / 255 * 2 + 2 * MAX_PAGE_LEN as u64
}

#[cfg(test)]
#[path = "vorbis_tests.rs"]
mod tests;
