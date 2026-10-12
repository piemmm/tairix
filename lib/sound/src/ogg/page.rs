//! Pages, their CRC, and the packets one logical stream lays across them.

use alloc::vec::Vec;

use crate::crc::crc32;
use crate::input::{self, Region, SoundInput};
use crate::DecodeError;

/// The capture pattern every page opens with.
pub(crate) const CAPTURE: &[u8; 4] = b"OggS";

/// Bytes of a page header before its lacing.
pub(super) const HEADER_LEN: usize = 27;

/// The most bytes a page takes.
pub(crate) const MAX_PAGE_LEN: usize = HEADER_LEN + 255 + 255 * 255;

pub(super) const CONTINUED: u8 = 0x01;
pub(super) const FIRST_PAGE: u8 = 0x02;
pub(super) const LAST_PAGE: u8 = 0x04;

/// The granule position of a page on which no packet completes.
pub(super) const NO_GRANULE: u64 = u64::MAX;

/// Bytes a search for a page reads at a time.
pub(super) const SCAN_LEN: usize = 4096;

/// A span of a link this narrow is walked a page at a time rather than
/// bisected.
pub(super) const LINEAR_SPAN: u64 = 128 * 1024;

/// How far back from a link's end its stream's last page is looked for. A
/// stream's pages are laid out by time with its link's others, so its last
/// lies among the link's final pages; one ending further back than this
/// states no length, and still plays and seeks.
const LAST_PAGE_REACH: u64 = 16 * MAX_PAGE_LEN as u64;

/// One page's header; its lacing is read from the page's bytes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct Page {
    pub(super) offset: u64,
    pub(super) flags: u8,
    pub(super) granule: u64,
    pub(super) serial: u32,
    pub(super) sequence: u32,
    pub(super) segments: usize,
    pub(super) body_len: u64,
}

impl Page {
    pub(super) fn header_len(&self) -> usize {
        HEADER_LEN + self.segments
    }

    pub(super) fn end(&self) -> u64 {
        self.offset + self.header_len() as u64 + self.body_len
    }

    pub(super) const fn is(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }

    /// The length of segment `segment`, from the page's bytes.
    pub(super) fn lacing(&self, bytes: &[u8], segment: usize) -> Option<usize> {
        (segment < self.segments)
            .then(|| bytes.get(HEADER_LEN + segment).copied())
            .flatten()
            .map(usize::from)
    }

    /// Whether a packet completes on the page past segment `segment`.
    pub(super) fn completes_after(&self, bytes: &[u8], segment: usize) -> bool {
        (segment..self.segments).any(|at| self.lacing(bytes, at).is_some_and(|len| len < 255))
    }
}

/// Read the page header at `offset`.
pub(super) fn page_header(
    input: &mut (impl SoundInput + ?Sized),
    offset: u64,
) -> Result<Page, DecodeError> {
    page_lacing(input, offset).map(|(page, _)| page)
}

/// Read the page header at `offset` and its lacing.
pub(super) fn page_lacing(
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
    let mut granule = [0u8; 8];
    granule.copy_from_slice(&head[6..14]);
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
        granule: u64::from_le_bytes(granule),
        serial: le32(14),
        sequence: le32(18),
        segments,
        body_len: lacing[..segments].iter().map(|&len| u64::from(len)).sum(),
    };
    Ok((page, lacing))
}

/// Read the page at `offset` whole into `bytes`, checking its CRC.
pub(super) fn verified_page(
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

/// Whether a page read during a search failed in a way that only means the
/// pattern found was not a page, as a stream's bytes may hold it by chance.
const fn not_a_page(err: DecodeError) -> bool {
    matches!(
        err,
        DecodeError::OggNoCapture
            | DecodeError::OggBadPage
            | DecodeError::OggPageCrc
            | DecodeError::OggPageTruncated
    )
}

/// What a page search keeps: a page whose header `admit` takes is verified
/// whole into the search's bytes, and kept if `accept` takes it too.
pub(super) struct Search<A, B> {
    pub(super) admit: A,
    pub(super) accept: B,
}

impl<A: FnMut(&Page) -> bool, B: FnMut(&Page, &[u8]) -> bool> Search<A, B> {
    /// The page at `offset`, if it is one this search keeps.
    fn try_at(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        offset: u64,
        bytes: &mut Vec<u8>,
    ) -> Result<Option<Page>, DecodeError> {
        match page_header(input, offset) {
            Ok(header) if (self.admit)(&header) => {}
            Ok(_) => return Ok(None),
            Err(err) if not_a_page(err) => return Ok(None),
            Err(err) => return Err(err),
        }
        match verified_page(input, offset, bytes) {
            Ok(page) => Ok((self.accept)(&page, bytes).then_some(page)),
            Err(err) if not_a_page(err) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// The first page this search keeps that starts at or after `from` and
    /// before `before`.
    pub(super) fn first(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        from: u64,
        before: u64,
        bytes: &mut Vec<u8>,
    ) -> Result<Option<Page>, DecodeError> {
        let mut window = [0u8; SCAN_LEN];
        let mut at = from;
        while at < before {
            let span = usize::try_from((before - at).min(SCAN_LEN as u64)).unwrap_or(SCAN_LEN);
            let held = input::read(input, at, &mut window[..span])?;
            for skip in 0..held.saturating_sub(3) {
                if &window[skip..skip + 4] == CAPTURE {
                    if let Some(page) = self.try_at(input, at + skip as u64, bytes)? {
                        return Ok(Some(page));
                    }
                }
            }
            if held < span {
                break;
            }
            at += span.saturating_sub(3).max(1) as u64;
        }
        Ok(None)
    }

    /// The last page this search keeps that starts at or after `from` and
    /// ends by `before`, looking back at most `budget` bytes from `before`.
    pub(super) fn last(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        from: u64,
        before: u64,
        budget: u64,
        bytes: &mut Vec<u8>,
    ) -> Result<Option<Page>, DecodeError> {
        let floor = from.max(before.saturating_sub(budget));
        let mut window = [0u8; SCAN_LEN];
        let mut end = before;
        while end > floor {
            let start = end.saturating_sub(SCAN_LEN as u64).max(floor);
            let span = usize::try_from(end - start).unwrap_or(SCAN_LEN);
            let held = input::read(input, start, &mut window[..span])?;
            for skip in (0..held.saturating_sub(3)).rev() {
                if &window[skip..skip + 4] != CAPTURE {
                    continue;
                }
                if let Some(page) = self.try_at(input, start + skip as u64, bytes)? {
                    if page.end() <= before {
                        return Ok(Some(page));
                    }
                }
            }
            if start == floor {
                break;
            }
            end = start + 3;
        }
        Ok(None)
    }
}

/// A place in a logical stream: a page, and the segment the next packet
/// starts at with the body bytes before it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct Spot {
    pub(super) page: Page,
    pub(super) segment: usize,
    pub(super) within: usize,
}

impl Spot {
    pub(super) const fn start_of(page: Page) -> Self {
        Self {
            page,
            segment: 0,
            within: 0,
        }
    }

    /// The first packet that starts on `page`, past the tail of one continued
    /// from the page before; `None` where none starts on it.
    pub(super) fn first_on(page: &Page, bytes: &[u8]) -> Option<Self> {
        let mut spot = Self::start_of(*page);
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
        (spot.segment < page.segments).then_some(spot)
    }
}

/// One packet that starts on a page: where, its bytes on that page, and
/// whether it completes there.
#[derive(Copy, Clone)]
pub(super) struct OnPage<'b> {
    pub(super) spot: Spot,
    pub(super) bytes: &'b [u8],
    pub(super) completes: bool,
}

/// The packets that start on `page`, whose bytes `bytes` holds, in order.
pub(super) fn packets_on<'b>(
    page: &'b Page,
    bytes: &'b [u8],
) -> impl Iterator<Item = OnPage<'b>> + 'b {
    let mut next = Spot::first_on(page, bytes);
    core::iter::from_fn(move || {
        let spot = next?;
        let mut here = spot;
        let completes = loop {
            let Some(len) = page.lacing(bytes, here.segment) else {
                break false;
            };
            here.segment += 1;
            here.within += len;
            if len < 255 {
                break true;
            }
        };
        let body = page.header_len();
        let packet = bytes.get(body + spot.within..body + here.within)?;
        next = (completes && here.segment < page.segments).then_some(here);
        Some(OnPage {
            spot,
            bytes: packet,
            completes,
        })
    })
}

/// The verified bytes of the page a spot is on, and room to read the next.
#[derive(Default)]
pub(super) struct Pages {
    pub(super) current: Vec<u8>,
    pub(super) staging: Vec<u8>,
}

/// The last page of stream `serial` holding a granule among a link's pages
/// from `from` to its end `end`, within [`LAST_PAGE_REACH`] of that end.
pub(super) fn last_page(
    input: &mut (impl SoundInput + ?Sized),
    serial: u32,
    from: u64,
    end: u64,
    bytes: &mut Vec<u8>,
) -> Result<Option<Page>, DecodeError> {
    Search {
        admit: |page: &Page| page.serial == serial && page.granule != NO_GRANULE,
        accept: |_: &Page, _: &[u8]| true,
    }
    .last(input, from, end, LAST_PAGE_REACH, bytes)
}

/// The frames from granule `origin` to the end of the stream whose last page
/// was found as `last`: an ending page before `origin` contradicts the
/// stream.
pub(super) fn length(last: Option<Page>, origin: u64) -> Result<Option<u64>, DecodeError> {
    match last {
        Some(page) if page.is(LAST_PAGE) && page.granule < origin => {
            Err(DecodeError::OggGranuleMismatch)
        }
        Some(page) => Ok(Some(page.granule.saturating_sub(origin))),
        None => Ok(None),
    }
}

/// One logical stream of a physical one, by its serial, read no further
/// than `end`.
pub(super) struct Stream {
    pub(super) serial: u32,
    pub(super) end: u64,
    /// The page the stream's length was read from, where one was found: no
    /// page of the stream lies before it and ends it, and none after it is
    /// whole.
    pub(super) last: Option<Page>,
}

/// A packet read: where the next starts, and whether it starts on a page
/// whose bytes [`Pages::staging`] now holds.
#[derive(Copy, Clone)]
pub(super) struct Read {
    pub(super) next: Spot,
    pub(super) moved: bool,
}

impl Read {
    /// Whether the packet was the last to complete on its page; `pages` as
    /// the read left them.
    pub(super) fn ends_page(&self, pages: &Pages) -> bool {
        let bytes = if self.moved {
            &pages.staging
        } else {
            &pages.current
        };
        !self.next.page.completes_after(bytes, self.next.segment)
    }

    /// The granule of the page the packet completed on, when it was the last
    /// packet to complete there; `pages` as the read left them.
    pub(super) fn granule(&self, pages: &Pages) -> Option<u64> {
        let page = &self.next.page;
        (self.ends_page(pages) && page.granule != NO_GRANULE).then_some(page.granule)
    }

    /// Whether the packet completed on its stream's last page.
    pub(super) const fn on_last_page(&self) -> bool {
        self.next.page.is(LAST_PAGE)
    }
}

/// Take a packet read as the stream's place.
pub(super) fn commit(pages: &mut Pages, spot: &mut Spot, read: &Read) {
    if read.moved {
        core::mem::swap(&mut pages.current, &mut pages.staging);
    }
    *spot = read.next;
}

impl Stream {
    /// Whether `page`, of the stream's serial, shows a later stream carrying
    /// that serial too, which a link's streams never may: an opening page, or
    /// an ending page before the one the length was read from. One after it
    /// can only be the stream's own, cut short.
    fn foreign(&self, page: &Page) -> bool {
        page.is(FIRST_PAGE)
            || page.is(LAST_PAGE) && self.last.is_some_and(|last| page.offset < last.offset)
    }

    /// What `entry` finds on the last of the stream's pages from `from` whose
    /// granule is at most `granule` and on which it finds anything: the pages
    /// bisected on their granules, the span left walked.
    pub(super) fn entry_at_most<E>(
        &self,
        input: &mut (impl SoundInput + ?Sized),
        from: u64,
        granule: u64,
        bytes: &mut Vec<u8>,
        mut entry: impl FnMut(&Page, &[u8]) -> Option<E>,
    ) -> Result<Option<E>, DecodeError> {
        let serial = self.serial;
        let end = self.last.map_or(self.end, |last| last.end());
        let mut best = None;
        let (mut lo, mut hi) = (from, end);
        while hi - lo > LINEAR_SPAN {
            let mid = lo + (hi - lo) / 2;
            let mut found = None;
            let mut foreign = false;
            Search {
                admit: |page: &Page| page.serial == serial && page.granule != NO_GRANULE,
                accept: |page: &Page, bytes: &[u8]| {
                    foreign = self.foreign(page);
                    found = (!foreign)
                        .then(|| entry(page, bytes).map(|entry| (*page, entry)))
                        .flatten();
                    foreign || found.is_some()
                },
            }
            .first(input, mid, hi, bytes)?;
            if foreign {
                return Err(DecodeError::OggBadLink);
            }
            match found {
                Some((page, entry)) if page.granule <= granule => {
                    best = Some(entry);
                    lo = page.end();
                }
                Some((page, _)) => hi = page.offset,
                None => hi = mid,
            }
        }
        let mut at = lo;
        while at < end {
            let header = page_header(input, at)?;
            if header.serial == serial {
                if self.foreign(&header) {
                    verified_page(input, at, bytes)?;
                    return Err(DecodeError::OggBadLink);
                }
                if header.granule != NO_GRANULE {
                    if header.granule > granule {
                        break;
                    }
                    let page = verified_page(input, at, bytes)?;
                    if let Some(found) = entry(&page, bytes) {
                        best = Some(found);
                    }
                }
            }
            at = header.end();
        }
        Ok(best)
    }

    /// The stream's next page after `page`, stepping over other streams', read
    /// whole into `bytes`; `None` past its last.
    pub(super) fn next_page(
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
    pub(super) fn next_header(
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
                if self.foreign(&header) {
                    return Err(DecodeError::OggBadLink);
                }
                return Ok(Some((header, lacing)));
            }
            at = header.end();
        }
        if self.last.is_some_and(|last| page.offset < last.offset) {
            return Err(DecodeError::OggPageLost);
        }
        Ok(None)
    }

    /// Read the packet starting at `spot` into `out`, refusing one longer
    /// than `limit`; `None` where the stream ended first. Nothing but `out`
    /// and [`Pages::staging`] is touched, so the caller commits.
    pub(super) fn packet(
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
pub(super) struct PacketRegion<'r, I: SoundInput + ?Sized> {
    input: &'r mut I,
    stream: &'r Stream,
    start: Mark,
    len: u64,
    pub(super) base: u64,
    at: u64,
    mark: Mark,
    /// The page whose verified bytes `bytes` holds.
    held: Option<Page>,
    bytes: &'r mut Vec<u8>,
}

impl<'r, I: SoundInput + ?Sized> PacketRegion<'r, I> {
    /// The packet starting at `spot`, whose page's lacing `page_bytes` holds,
    /// walked to its end; `None` where the stream ends first.
    pub(super) fn new(
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
