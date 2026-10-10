//! Ogg (RFC 3533): pages, their CRC, and the packets laid across them; and
//! over it the FLAC mapping (RFC 9639, section 10.1), a FLAC frame a packet.
//!
//! A physical stream may interleave several logical ones: the reader follows
//! the FLAC stream's pages by serial and steps over the rest. Every page whose
//! bytes are read is checked against its CRC and its place in its stream's
//! sequence. A stream chained after the FLAC one is refused where the FLAC
//! one ends, by name, rather than quietly ending the sound there.
//!
//! Seeking bisects the file's pages on the frame header the first packet
//! starting on each states, exactly as the native stream bisects its frames,
//! so a granule position is never trusted to place a sample.

use alloc::vec::Vec;

use tairix_abi::driver::audio::Rate;

use crate::crc::crc32;
use crate::flac::{self, Blocks, Decoder, Numbering, StreamInfo, BLOCK_HEADER_LEN, STREAMINFO_LEN};
use crate::flac_frame::{self, FrameError, MAX_HEADER_LEN};
use crate::input::{self, Region, SoundInput};
use crate::meta::Collector;
use crate::{DecodeError, DecodeLimits, Encoding, SoundFormat, SoundInfo};

/// The capture pattern every page opens with.
pub(crate) const CAPTURE: &[u8; 4] = b"OggS";

/// Bytes of a page header before its lacing.
const HEADER_LEN: usize = 27;

/// The most bytes a page takes.
pub(crate) const MAX_PAGE_LEN: usize = HEADER_LEN + 255 + 255 * 255;

const CONTINUED: u8 = 0x01;
const FIRST_PAGE: u8 = 0x02;
const LAST_PAGE: u8 = 0x04;

/// The FLAC mapping's first packet: its prefix, and its length.
const FLAC_PREFIX: &[u8; 5] = b"\x7fFLAC";
const FLAC_HEADER_LEN: usize = 5 + 2 + 2 + 4 + BLOCK_HEADER_LEN + STREAMINFO_LEN;
const MAPPING_MAJOR: u8 = 1;

/// Bytes a seek's page search reads at a time.
const SCAN_LEN: usize = 4096;

/// A seek bracket this narrow is searched packet by packet.
const LINEAR_SPAN: u64 = 128 * 1024;

/// Whether `bytes` open with an Ogg page.
pub(crate) fn has_signature(bytes: &[u8]) -> bool {
    bytes.starts_with(CAPTURE)
}

/// One page's header; its lacing is read from the page's bytes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Page {
    offset: u64,
    flags: u8,
    serial: u32,
    sequence: u32,
    segments: usize,
    body_len: u64,
}

impl Page {
    fn header_len(&self) -> usize {
        HEADER_LEN + self.segments
    }

    fn end(&self) -> u64 {
        self.offset + self.header_len() as u64 + self.body_len
    }

    const fn is(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }

    /// The length of segment `segment`, from the page's bytes.
    fn lacing(&self, bytes: &[u8], segment: usize) -> Option<usize> {
        (segment < self.segments)
            .then(|| bytes.get(HEADER_LEN + segment).copied())
            .flatten()
            .map(usize::from)
    }
}

/// Read the page header at `offset`.
fn page_header(input: &mut (impl SoundInput + ?Sized), offset: u64) -> Result<Page, DecodeError> {
    page_lacing(input, offset).map(|(page, _)| page)
}

/// Read the page header at `offset` and its lacing.
fn page_lacing(
    input: &mut (impl SoundInput + ?Sized),
    offset: u64,
) -> Result<(Page, [u8; 255]), DecodeError> {
    let mut head = [0u8; HEADER_LEN];
    input::read_exact(input, offset, &mut head, DecodeError::OggPageTruncated)?;
    if &head[..4] != CAPTURE {
        return Err(DecodeError::OggNoCapture);
    }
    if head[4] != 0 {
        return Err(DecodeError::OggBadPage);
    }
    let le32 = |at: usize| u32::from_le_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]]);
    let segments = usize::from(head[26]);
    let mut lacing = [0u8; 255];
    input::read_exact(
        input,
        offset + HEADER_LEN as u64,
        &mut lacing[..segments],
        DecodeError::OggPageTruncated,
    )?;
    let page = Page {
        offset,
        flags: head[5],
        serial: le32(14),
        sequence: le32(18),
        segments,
        body_len: lacing[..segments].iter().map(|&len| u64::from(len)).sum(),
    };
    Ok((page, lacing))
}

/// Read the page at `offset` whole into `bytes`, checking its CRC.
fn verified_page(
    input: &mut (impl SoundInput + ?Sized),
    offset: u64,
    bytes: &mut Vec<u8>,
) -> Result<Page, DecodeError> {
    let page = page_header(input, offset)?;
    let len = usize::try_from(page.end() - offset).map_err(|_| DecodeError::OggBadPage)?;
    if !tairix_util::fallible::grow_to(bytes, len, 0u8) {
        return Err(DecodeError::OutOfMemory);
    }
    bytes.truncate(len);
    input::read_exact(input, offset, bytes, DecodeError::OggPageTruncated)?;
    let stored = u32::from_le_bytes([bytes[22], bytes[23], bytes[24], bytes[25]]);
    let crc = crc32(crc32(crc32(0, &bytes[..22]), &[0; 4]), &bytes[26..]);
    if crc != stored {
        return Err(DecodeError::OggPageCrc);
    }
    Ok(page)
}

/// A place in a logical stream: a page, and the segment the next packet
/// starts at with the body bytes before it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Spot {
    page: Page,
    segment: usize,
    within: usize,
}

impl Spot {
    const fn start_of(page: Page) -> Self {
        Self {
            page,
            segment: 0,
            within: 0,
        }
    }
}

/// The verified bytes of the page a spot is on, and room to read the next.
#[derive(Default)]
struct Pages {
    current: Vec<u8>,
    staging: Vec<u8>,
}

/// One logical stream of a physical one, by its serial.
struct Stream {
    serial: u32,
    end: u64,
}

/// A packet read: where the next starts, and whether it starts on a page
/// whose bytes [`Pages::staging`] now holds.
#[derive(Copy, Clone)]
struct Read {
    next: Spot,
    moved: bool,
}

impl Stream {
    /// The stream's next page after `page`, stepping over other streams', read
    /// whole into `bytes`; `None` past its last.
    fn next_page(
        &self,
        input: &mut (impl SoundInput + ?Sized),
        page: &Page,
        bytes: &mut Vec<u8>,
    ) -> Result<Option<Page>, DecodeError> {
        let Some((header, _)) = self.next_header(input, page)? else {
            return Ok(None);
        };
        let next = verified_page(input, header.offset, bytes)?;
        if next != header {
            return Err(DecodeError::OggBadPage);
        }
        Ok(Some(next))
    }

    /// The header and lacing of the stream's next page after `page`, its
    /// body unread; `None` past its last.
    fn next_header(
        &self,
        input: &mut (impl SoundInput + ?Sized),
        page: &Page,
    ) -> Result<Option<(Page, [u8; 255])>, DecodeError> {
        if page.is(LAST_PAGE) {
            return Ok(None);
        }
        let mut at = page.end();
        while at < self.end {
            let (header, lacing) = page_lacing(input, at)?;
            if header.serial == self.serial {
                if header.sequence != page.sequence.wrapping_add(1) {
                    return Err(DecodeError::OggPageLost);
                }
                return Ok(Some((header, lacing)));
            }
            at = header.end();
        }
        Ok(None)
    }

    /// Read the packet starting at `spot` into `out`, refusing one longer
    /// than `limit`; `None` where the stream ended first. Nothing but `out`
    /// and [`Pages::staging`] is touched, so the caller commits.
    fn packet(
        &self,
        input: &mut (impl SoundInput + ?Sized),
        spot: Spot,
        pages: &mut Pages,
        out: &mut Vec<u8>,
        limit: usize,
    ) -> Result<Option<Read>, DecodeError> {
        out.clear();
        let mut here = spot;
        let mut moved = false;
        if here.segment == here.page.segments {
            let Some(page) = self.next_page(input, &here.page, &mut pages.staging)? else {
                return Ok(None);
            };
            if page.is(CONTINUED) {
                return Err(DecodeError::OggBadPacket);
            }
            here = Spot::start_of(page);
            moved = true;
        }
        loop {
            let bytes = if moved {
                &pages.staging
            } else {
                &pages.current
            };
            let len = here
                .page
                .lacing(bytes, here.segment)
                .ok_or(DecodeError::OggBadPacket)?;
            let start = here.page.header_len() + here.within;
            if out.len() + len > limit {
                return Err(DecodeError::OggPacketTooLarge);
            }
            let segment = bytes
                .get(start..start + len)
                .ok_or(DecodeError::OggBadPage)?;
            let filled = out.len();
            if !tairix_util::fallible::grow_to(out, filled + len, 0u8) {
                return Err(DecodeError::OutOfMemory);
            }
            out[filled..].copy_from_slice(segment);
            here.segment += 1;
            here.within += len;
            if len < 255 {
                return Ok(Some(Read { next: here, moved }));
            }
            if here.segment == here.page.segments {
                let page = self
                    .next_page(input, &here.page, &mut pages.staging)?
                    .ok_or(DecodeError::OggBadPacket)?;
                if !page.is(CONTINUED) {
                    return Err(DecodeError::OggBadPacket);
                }
                here = Spot::start_of(page);
                moved = true;
            }
        }
    }

    /// Whether a stream follows this one's last page as the next link of a
    /// chain: a page that opens a stream, past the pages of streams that
    /// were interleaved with it.
    fn chained_after(
        &self,
        input: &mut (impl SoundInput + ?Sized),
        page: &Page,
    ) -> Result<bool, DecodeError> {
        let mut at = page.end();
        while at < self.end {
            let header = page_header(input, at)?;
            if header.is(FIRST_PAGE) {
                return Ok(true);
            }
            at = header.end();
        }
        Ok(false)
    }
}

/// A place inside a packet: the page it is on with that page's lacing, and
/// how far into which segment.
#[derive(Copy, Clone)]
struct Mark {
    page: Page,
    lacing: [u8; 255],
    segment: usize,
    /// Bytes into the segment.
    into: usize,
    /// Body bytes before the segment.
    body: usize,
}

/// A packet read in place across its stream's pages, from `base` bytes in: a
/// page's bytes are read and checked only when bytes are taken from it, so a
/// metadata packet of any size costs one page of memory, and stepping over a
/// picture reads none of it.
struct PacketRegion<'r, I: SoundInput + ?Sized> {
    input: &'r mut I,
    stream: &'r Stream,
    start: Mark,
    len: u64,
    base: u64,
    at: u64,
    mark: Mark,
    /// The page whose verified bytes `bytes` holds.
    held: Option<Page>,
    bytes: &'r mut Vec<u8>,
}

impl<'r, I: SoundInput + ?Sized> PacketRegion<'r, I> {
    /// The packet starting at `spot`, whose page's lacing `page_bytes` holds,
    /// walked to its end; `None` where the stream ends first.
    fn new(
        input: &'r mut I,
        stream: &'r Stream,
        spot: Spot,
        page_bytes: &[u8],
        bytes: &'r mut Vec<u8>,
    ) -> Result<Option<(Self, Spot)>, DecodeError> {
        let mut lacing = [0u8; 255];
        let table = page_bytes
            .get(HEADER_LEN..HEADER_LEN + spot.page.segments)
            .ok_or(DecodeError::OggBadPage)?;
        lacing[..spot.page.segments].copy_from_slice(table);
        let mut start = Mark {
            page: spot.page,
            lacing,
            segment: spot.segment,
            into: 0,
            body: spot.within,
        };
        if start.segment == start.page.segments {
            let Some((page, lacing)) = stream.next_header(input, &start.page)? else {
                return Ok(None);
            };
            if page.is(CONTINUED) {
                return Err(DecodeError::OggBadPacket);
            }
            start = Mark {
                page,
                lacing,
                segment: 0,
                into: 0,
                body: 0,
            };
        }
        let mut mark = start;
        let mut len = 0u64;
        let end = loop {
            if mark.segment == mark.page.segments {
                let (page, lacing) = stream
                    .next_header(input, &mark.page)?
                    .ok_or(DecodeError::OggBadPacket)?;
                if !page.is(CONTINUED) {
                    return Err(DecodeError::OggBadPacket);
                }
                mark = Mark {
                    page,
                    lacing,
                    segment: 0,
                    into: 0,
                    body: 0,
                };
            }
            let segment = usize::from(mark.lacing[mark.segment]);
            len += segment as u64;
            mark.segment += 1;
            mark.body += segment;
            if segment < 255 {
                break Spot {
                    page: mark.page,
                    segment: mark.segment,
                    within: mark.body,
                };
            }
        };
        let region = Self {
            input,
            stream,
            start,
            len,
            base: 0,
            at: 0,
            mark: start,
            held: None,
            bytes,
        };
        Ok(Some((region, end)))
    }

    /// Take `count` bytes from the cursor on, into `out` where given.
    fn advance(&mut self, count: usize, mut out: Option<&mut [u8]>) -> Result<(), DecodeError> {
        let mut done = 0;
        while done < count {
            if self.mark.segment == self.mark.page.segments {
                let (page, lacing) = self
                    .stream
                    .next_header(self.input, &self.mark.page)?
                    .ok_or(DecodeError::OggBadPacket)?;
                self.mark = Mark {
                    page,
                    lacing,
                    segment: 0,
                    into: 0,
                    body: 0,
                };
            }
            let segment = usize::from(self.mark.lacing[self.mark.segment]);
            let take = (segment - self.mark.into).min(count - done);
            if let Some(out) = out.as_deref_mut() {
                if self.held != Some(self.mark.page) {
                    self.held = None;
                    let page = verified_page(self.input, self.mark.page.offset, self.bytes)?;
                    if page != self.mark.page {
                        return Err(DecodeError::OggBadPage);
                    }
                    self.held = Some(page);
                }
                let from = self.mark.page.header_len() + self.mark.body + self.mark.into;
                let held = self
                    .bytes
                    .get(from..from + take)
                    .ok_or(DecodeError::OggBadPage)?;
                out[done..done + take].copy_from_slice(held);
            }
            self.mark.into += take;
            done += take;
            if self.mark.into == segment {
                if segment < 255 {
                    if done < count {
                        return Err(DecodeError::OggBadPacket);
                    }
                    break;
                }
                self.mark.segment += 1;
                self.mark.body += segment;
                self.mark.into = 0;
            }
        }
        self.at += count as u64;
        Ok(())
    }
}

impl<I: SoundInput + ?Sized> Region for PacketRegion<'_, I> {
    fn len(&self) -> u64 {
        self.len - self.base
    }

    fn read_exact(
        &mut self,
        at: u64,
        buf: &mut [u8],
        short: DecodeError,
    ) -> Result<(), DecodeError> {
        let wanted = u64::try_from(buf.len()).map_err(|_| short)?;
        let at = at.checked_add(self.base).ok_or(short)?;
        if at.checked_add(wanted).is_none_or(|end| end > self.len) {
            return Err(short);
        }
        if at < self.at {
            self.mark = self.start;
            self.at = 0;
        }
        let skip = usize::try_from(at - self.at).map_err(|_| short)?;
        self.advance(skip, None)?;
        self.advance(buf.len(), Some(buf))
    }
}

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

/// Read the headers of the FLAC stream the Ogg file `input` holds.
pub(crate) fn open(
    input: &mut (impl SoundInput + ?Sized),
    limits: &DecodeLimits,
    collector: &mut Collector,
) -> Result<(SoundInfo, OggFlac), DecodeError> {
    let mut pages = Pages::default();
    let (first, header) = first_page(input, &mut pages.current)?;
    let stream = Stream {
        serial: first.serial,
        end: input.len(),
    };
    let info = mapping_header(&header)?;
    let rate = Rate::new(info.rate).map_err(|_| DecodeError::RateOutOfRange)?;
    let declared = u16::from_be_bytes([header[7], header[8]]);
    let mut spot = Spot {
        page: first,
        segment: 1,
        within: FLAC_HEADER_LEN,
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

/// Take a packet read as the stream's place.
fn commit(pages: &mut Pages, spot: &mut Spot, read: &Read) {
    if read.moved {
        core::mem::swap(&mut pages.current, &mut pages.staging);
    }
    *spot = read.next;
}

/// The first page that opens the FLAC logical stream, among those that open
/// the physical stream's others, and its packet.
fn first_page(
    input: &mut (impl SoundInput + ?Sized),
    bytes: &mut Vec<u8>,
) -> Result<(Page, [u8; FLAC_HEADER_LEN]), DecodeError> {
    let len = input.len();
    let mut at = 0;
    while at < len {
        let page = verified_page(input, at, bytes)?;
        if !page.is(FIRST_PAGE) {
            break;
        }
        let packet = &bytes[page.header_len()..];
        if packet.starts_with(FLAC_PREFIX) {
            let alone = page.segments == 1 && page.lacing(bytes, 0) == Some(FLAC_HEADER_LEN);
            if !alone || page.is(CONTINUED) {
                return Err(DecodeError::OggBadFlacMapping);
            }
            let mut header = [0u8; FLAC_HEADER_LEN];
            header.copy_from_slice(&packet[..FLAC_HEADER_LEN]);
            return Ok((page, header));
        }
        at = page.end();
    }
    Err(DecodeError::OggNoFlacStream)
}

/// The `STREAMINFO` the mapping's first packet carries.
fn mapping_header(header: &[u8; FLAC_HEADER_LEN]) -> Result<StreamInfo, DecodeError> {
    if header[5] != MAPPING_MAJOR || &header[9..13] != flac::MARKER {
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
            if let Some((spot, _)) = self.next {
                if self.stream.chained_after(input, &spot.page)? {
                    return Err(DecodeError::OggChained);
                }
            }
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
        let mut window = [0u8; SCAN_LEN];
        let mut at = from;
        while at < before {
            let span = usize::try_from((before - at).min(SCAN_LEN as u64)).unwrap_or(SCAN_LEN);
            let held = input::read(input, at, &mut window[..span])?;
            for skip in 0..held.saturating_sub(3) {
                if &window[skip..skip + 4] != CAPTURE {
                    continue;
                }
                let offset = at + skip as u64;
                let Ok(header) = page_header(input, offset) else {
                    continue;
                };
                if header.serial != self.stream.serial {
                    continue;
                }
                let Ok(page) = verified_page(input, offset, &mut self.pages.staging) else {
                    continue;
                };
                let Some((spot, prefix)) = frame_start(&page, &self.pages.staging) else {
                    continue;
                };
                let position = flac_frame::header(prefix, self.decoder.stream())
                    .ok()
                    .and_then(|header| self.numbering.position(&header));
                if let Some(position) = position {
                    return Ok(Some((spot, position)));
                }
            }
            if held < span {
                break;
            }
            at += span.saturating_sub(3).max(1) as u64;
        }
        Ok(None)
    }
}

/// The first packet that starts on `page`, after the tail of one continued
/// from the page before, with the bytes of it the page holds: enough for a
/// frame header, or `None`.
fn frame_start<'b>(page: &Page, bytes: &'b [u8]) -> Option<(Spot, &'b [u8])> {
    let mut spot = Spot::start_of(*page);
    if page.is(CONTINUED) {
        loop {
            let len = page.lacing(bytes, spot.segment)?;
            spot.segment += 1;
            spot.within += len;
            if len < 255 {
                break;
            }
        }
    }
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
    if segment == spot.segment {
        return None;
    }
    Some((spot, bytes.get(start..end)?))
}

/// The most bytes one call reads for a FLAC stream in Ogg within `limits`:
/// the largest frame's packet with its pages' headers, and a page beyond.
pub(crate) const fn max_working_set(limits: &DecodeLimits) -> u64 {
    let frame = flac::max_working_set(limits);
    frame + frame / 255 * 2 + 2 * MAX_PAGE_LEN as u64
}

/// Pages of one logical stream, laid as the FLAC mapping lays them.
#[cfg(any(test, feature = "encode"))]
pub(crate) struct PageWriter {
    out: Vec<u8>,
    serial: u32,
    sequence: u32,
    lacing: Vec<u8>,
    body: Vec<u8>,
    continued: bool,
    first: bool,
    granule: Option<u64>,
}

#[cfg(any(test, feature = "encode"))]
impl PageWriter {
    pub(crate) const fn new(serial: u32) -> Self {
        Self {
            out: Vec::new(),
            serial,
            sequence: 0,
            lacing: Vec::new(),
            body: Vec::new(),
            continued: false,
            first: true,
            granule: None,
        }
    }

    /// Lay `packet`, whose last sample is `granule`, on the page being
    /// filled, ending pages as their lacing fills.
    pub(crate) fn packet(&mut self, packet: &[u8], granule: u64) {
        let mut rest = packet;
        loop {
            if self.lacing.len() == 255 {
                self.flush(false);
                self.continued = true;
            }
            let take = rest.len().min(255);
            self.lacing.push(u8::try_from(take).unwrap_or(u8::MAX));
            self.body.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if take < 255 {
                break;
            }
        }
        self.granule = Some(granule);
    }

    /// Bytes on the page being filled.
    pub(crate) fn filled(&self) -> usize {
        self.body.len()
    }

    /// End the page being filled, the stream's last where `last`.
    pub(crate) fn flush(&mut self, last: bool) {
        let mut flags = 0;
        if self.continued {
            flags |= CONTINUED;
        }
        if self.first {
            flags |= FIRST_PAGE;
        }
        if last {
            flags |= LAST_PAGE;
        }
        let granule = self.granule.take().unwrap_or(u64::MAX);
        let start = self.out.len();
        self.out.extend_from_slice(CAPTURE);
        self.out.push(0);
        self.out.push(flags);
        self.out.extend_from_slice(&granule.to_le_bytes());
        self.out.extend_from_slice(&self.serial.to_le_bytes());
        self.out.extend_from_slice(&self.sequence.to_le_bytes());
        self.out.extend_from_slice(&[0; 4]);
        self.out
            .push(u8::try_from(self.lacing.len()).unwrap_or(u8::MAX));
        self.out.extend_from_slice(&self.lacing);
        self.out.extend_from_slice(&self.body);
        let crc = crc32(0, &self.out[start..]);
        self.out[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
        self.lacing.clear();
        self.body.clear();
        self.sequence += 1;
        self.first = false;
        self.continued = false;
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.out
    }
}

/// The mapping's first packet for `streaminfo`, announcing `headers` more.
#[cfg(any(test, feature = "encode"))]
pub(crate) fn flac_header(streaminfo: &[u8], headers: u16, last: bool) -> Vec<u8> {
    let mut packet = FLAC_PREFIX.to_vec();
    packet.extend_from_slice(&[MAPPING_MAJOR, 0]);
    packet.extend_from_slice(&headers.to_be_bytes());
    packet.extend_from_slice(flac::MARKER);
    packet.extend_from_slice(&crate::flac_encode::block_header(0, streaminfo.len(), last));
    packet.extend_from_slice(streaminfo);
    packet
}

#[cfg(test)]
#[path = "ogg_tests.rs"]
mod tests;
