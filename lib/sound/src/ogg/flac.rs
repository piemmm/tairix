//! The FLAC mapping (RFC 9639, section 10.1): a header packet carrying
//! `STREAMINFO`, the metadata blocks a packet each, then a FLAC frame a
//! packet.
//!
//! Seeking bisects the stream's pages on the frame header the first packet
//! starting on each states, exactly as the native stream bisects its frames,
//! so a granule position is never trusted to place a sample.

use alloc::vec::Vec;

use tairix_abi::driver::audio::Rate;

use super::link::{Group, FLAC_PREFIX};
use super::page::{
    commit, verified_page, PacketRegion, Page, Pages, Search, Spot, Stream, CONTINUED, LINEAR_SPAN,
    MAX_PAGE_LEN,
};
use crate::flac::{self, Blocks, Decoder, Numbering, StreamInfo, BLOCK_HEADER_LEN, STREAMINFO_LEN};
use crate::flac_frame::{self, FrameError, MAX_HEADER_LEN};
use crate::input::{Region, SoundInput};
use crate::meta::Collector;
use crate::{DecodeError, DecodeLimits, Encoding, SoundFormat, SoundInfo};

/// The mapping's first packet: its length, and the version it states.
const HEADER_LEN: usize = 5 + 2 + 2 + 4 + BLOCK_HEADER_LEN + STREAMINFO_LEN;
const MAPPING_MAJOR: u8 = 1;

/// FLAC in Ogg, past its headers.
pub(crate) struct OggFlac {
    decoder: Decoder,
    numbering: Numbering,
    stream: Stream,
    /// The page the first audio packet starts on.
    first_audio: Spot,
    /// The spot the next packet starts at and its frame's position; the
    /// bytes of its page are [`Pages::current`].
    next: Option<(Spot, u64)>,
    pages: Pages,
    /// The bytes of the largest packet a frame may take.
    frame_limit: usize,
    packet: Vec<u8>,
}

/// Read the headers of the FLAC stream `group` chose, whose opening page
/// `pages` holds, in a link that ends at `end`.
pub(super) fn open(
    input: &mut (impl SoundInput + ?Sized),
    group: &Group,
    end: u64,
    mut pages: Pages,
    limits: &DecodeLimits,
    collector: &mut Collector,
) -> Result<(SoundInfo, OggFlac), DecodeError> {
    let first = group.page;
    let header = mapping_packet(&first, &pages.current)?;
    let stream = Stream {
        serial: first.serial,
        end,
        last: None,
    };
    let info = mapping_header(&header)?;
    let rate = Rate::new(info.rate).map_err(|_| DecodeError::RateOutOfRange)?;
    let declared = u16::from_be_bytes([header[7], header[8]]);
    let mut spot = Spot {
        page: first,
        segment: 1,
        within: HEADER_LEN,
    };
    let mut blocks = Blocks::default();
    let mut taken = 0u16;
    let mut last_block = header[13] & 0x80 != 0;
    while !last_block && (declared == 0 || taken < declared) {
        let (mut region, after) = PacketRegion::new(
            &mut *input,
            &stream,
            spot,
            &pages.current,
            &mut pages.staging,
        )?
        .ok_or(DecodeError::OggNoAudio)?;
        let mut block = [0u8; BLOCK_HEADER_LEN];
        region.read_exact(0, &mut block, DecodeError::FlacMetadataTruncated)?;
        let size = u32::from_be_bytes([0, block[1], block[2], block[3]]);
        if u64::from(size) != region.len() - BLOCK_HEADER_LEN as u64 {
            return Err(DecodeError::FlacMetadataTruncated);
        }
        region.base = BLOCK_HEADER_LEN as u64;
        blocks.take(block[0] & 0x7F, &mut region, None, &info, collector)?;
        last_block = block[0] & 0x80 != 0;
        taken += 1;
        if after.page != spot.page {
            let page = verified_page(input, after.page.offset, &mut pages.current)?;
            if page != after.page {
                return Err(DecodeError::OggBadPage);
            }
        }
        spot = after;
    }
    if spot.segment != spot.page.segments {
        return Err(DecodeError::OggBadPacket);
    }
    let mut packet = Vec::new();
    let channels = blocks.channels(&info, limits)?;
    let decoder = Decoder::new(&info);
    let frame_limit = decoder.stream().max_frame_len();
    let numbering = match stream.packet(input, spot, &mut pages, &mut packet, frame_limit)? {
        Some(_) => Numbering::of(
            &info,
            &flac_frame::header(&packet, decoder.stream()).map_err(flac::frame_error)?,
        ),
        None => Numbering::unframed(&info),
    };
    let reader = OggFlac {
        decoder,
        numbering,
        stream,
        first_audio: spot,
        next: Some((spot, 0)),
        pages,
        frame_limit,
        packet,
    };
    let sound = SoundInfo {
        format: SoundFormat::Ogg,
        encoding: Encoding::Flac,
        rate,
        channels,
        sample: reader.decoder.sample_format(),
        frames: info.total(),
        seekable: true,
        data_length: None,
    };
    Ok((sound, reader))
}

/// The mapping's first packet, which must be alone on the stream's first
/// page.
fn mapping_packet(page: &Page, bytes: &[u8]) -> Result<[u8; HEADER_LEN], DecodeError> {
    let alone = page.segments == 1 && page.lacing(bytes, 0) == Some(HEADER_LEN);
    if !alone || page.is(CONTINUED) {
        return Err(DecodeError::OggBadFlacMapping);
    }
    bytes
        .get(page.header_len()..)
        .and_then(|packet| packet.first_chunk::<HEADER_LEN>())
        .copied()
        .ok_or(DecodeError::OggBadFlacMapping)
}

/// The `STREAMINFO` the mapping's first packet carries.
fn mapping_header(header: &[u8; HEADER_LEN]) -> Result<StreamInfo, DecodeError> {
    if &header[..5] != FLAC_PREFIX || header[5] != MAPPING_MAJOR || &header[9..13] != flac::MARKER {
        return Err(DecodeError::OggBadFlacMapping);
    }
    let block = &header[13..17];
    let size = u32::from_be_bytes([0, block[1], block[2], block[3]]);
    if block[0] & 0x7F != 0 || size as usize != STREAMINFO_LEN {
        return Err(DecodeError::FlacMissingStreamInfo);
    }
    let mut info = [0u8; STREAMINFO_LEN];
    info.copy_from_slice(&header[17..]);
    StreamInfo::parse(&info)
}

impl OggFlac {
    /// Whether the stream, decoded whole and in order, matched its digest.
    pub(crate) fn verified(&self) -> bool {
        self.decoder.verified()
    }

    /// Write the frames from `position` that `out` has room for.
    pub(crate) fn read(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        position: u64,
        out: &mut [u8],
    ) -> Result<usize, DecodeError> {
        let frame_bytes = self.decoder.frame_bytes();
        let room = out.len() / frame_bytes;
        let mut written = 0;
        while written < room {
            let at = position + written as u64;
            if self.decoder.is_past_end(at) {
                break;
            }
            if !self.decoder.holds(at) {
                match self.advance_to(input, at) {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(_) if written > 0 => return Ok(written),
                    Err(err) => return Err(err),
                }
            }
            written += self.decoder.emit(at, &mut out[written * frame_bytes..]);
        }
        if written == 0 {
            self.decoder.finish(position)?;
        }
        Ok(written)
    }

    fn advance_to(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        position: u64,
    ) -> Result<bool, DecodeError> {
        let near = 2 * u64::from(self.decoder.stream().max_block);
        let sequential = self
            .next
            .is_some_and(|(_, next)| next <= position && position - next <= near);
        if !sequential {
            self.locate(input, position)?;
        }
        loop {
            let Some((spot, expected)) = self.next else {
                return Ok(false);
            };
            let read = self.stream.packet(
                input,
                spot,
                &mut self.pages,
                &mut self.packet,
                self.frame_limit,
            )?;
            let Some(read) = read else {
                return Ok(false);
            };
            let (_, at, len) = self
                .decoder
                .decode(&self.packet, &self.numbering, Some(expected))
                .map_err(|err| match err {
                    FrameError::Truncated => DecodeError::OggBadPacket,
                    other => flac::frame_error(other),
                })?;
            if len != self.packet.len() {
                return Err(DecodeError::OggBadPacket);
            }
            let mut next = spot;
            commit(&mut self.pages, &mut next, &read);
            self.next = Some((next, at + self.decoder.held_len()));
            if self.decoder.holds(position) {
                return Ok(true);
            }
        }
    }

    /// Set the next packet at one starting at or before `position` and near
    /// it, by bisecting the pages on the first frame each opens.
    fn locate(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        position: u64,
    ) -> Result<(), DecodeError> {
        let mut lo = (self.first_audio, 0);
        if let Some(next) = self.next.filter(|&(_, next)| next <= position && next > 0) {
            lo = next;
        }
        let mut hi = self.stream.end;
        while hi.saturating_sub(lo.0.page.offset) > LINEAR_SPAN {
            let mid = lo.0.page.offset + (hi - lo.0.page.offset) / 2;
            match self.frame_on_page_from(input, mid, hi)? {
                Some(found) if found.1 <= position => lo = found,
                Some(found) => hi = found.0.page.offset,
                None => hi = mid,
            }
        }
        let page = verified_page(input, lo.0.page.offset, &mut self.pages.staging)?;
        if page != lo.0.page {
            return Err(DecodeError::OggBadPage);
        }
        core::mem::swap(&mut self.pages.current, &mut self.pages.staging);
        self.next = Some(lo);
        Ok(())
    }

    /// The first of the stream's pages at or after `from` and before
    /// `before` on which a frame starts whose header lies on that page, with
    /// that frame's spot and position.
    fn frame_on_page_from(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        from: u64,
        before: u64,
    ) -> Result<Option<(Spot, u64)>, DecodeError> {
        let serial = self.stream.serial;
        let (decoder, numbering) = (&self.decoder, &self.numbering);
        let mut found = None;
        Search {
            admit: |page: &Page| page.serial == serial,
            accept: |page: &Page, bytes: &[u8]| {
                found = frame_start(page, bytes).and_then(|(spot, prefix)| {
                    let header = flac_frame::header(prefix, decoder.stream()).ok()?;
                    Some((spot, numbering.position(&header)?))
                });
                found.is_some()
            },
        }
        .first(input, from, before, &mut self.pages.staging)?;
        Ok(found)
    }
}

/// The first packet that starts on `page`, with the bytes of it the page
/// holds: enough for a frame header, or `None`.
fn frame_start<'b>(page: &Page, bytes: &'b [u8]) -> Option<(Spot, &'b [u8])> {
    let spot = Spot::first_on(page, bytes)?;
    let start = page.header_len() + spot.within;
    let mut end = start;
    let mut segment = spot.segment;
    while let Some(len) = page.lacing(bytes, segment) {
        end += len;
        segment += 1;
        if len < 255 || end - start >= MAX_HEADER_LEN {
            break;
        }
    }
    Some((spot, bytes.get(start..end)?))
}

/// The most bytes one call reads for a FLAC stream in Ogg within `limits`:
/// the largest frame's packet with its pages' headers, and a page beyond.
pub(super) const fn max_working_set(limits: &DecodeLimits) -> u64 {
    let frame = flac::max_working_set(limits);
    frame + frame / 255 * 2 + 2 * MAX_PAGE_LEN as u64
}

/// The mapping's first packet for `streaminfo`, announcing `headers` more.
#[cfg(any(test, feature = "encode"))]
pub(crate) fn header(streaminfo: &[u8], headers: u16, last: bool) -> Vec<u8> {
    let mut packet = FLAC_PREFIX.to_vec();
    packet.extend_from_slice(&[MAPPING_MAJOR, 0]);
    packet.extend_from_slice(&headers.to_be_bytes());
    packet.extend_from_slice(flac::MARKER);
    packet.extend_from_slice(&crate::flac_encode::block_header(0, streaminfo.len(), last));
    packet.extend_from_slice(streaminfo);
    packet
}
