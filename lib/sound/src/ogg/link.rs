//! One link of a physical stream (RFC 3533, section 4): the logical streams
//! it opens together, the one decoded, and where it ends — where the next
//! link of a chain begins, if one follows.
//!
//! A link's pages lie together, and every link's logical streams carry
//! serials of their own, so the link's end is found by bisecting the file on
//! whether a page belongs to it.

use alloc::vec::Vec;

use super::page::{
    page_header, verified_page, Page, Pages, Search, CONTINUED, FIRST_PAGE, LINEAR_SPAN,
    MAX_PAGE_LEN,
};
use crate::input::SoundInput;
use crate::{vorbis, DecodeError};

/// The most logical streams one link may open: a film with its sound, a
/// commentary, captions and an index is a handful.
const MAX_STREAMS: usize = 64;

/// The FLAC mapping's first packet opens with this.
pub(super) const FLAC_PREFIX: &[u8; 5] = b"\x7fFLAC";

/// A codec this crate decodes from a logical stream.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Codec {
    Flac,
    Vorbis,
    Opus,
}

impl Codec {
    /// The codec whose stream opens with `packet`.
    fn of(packet: &[u8]) -> Option<Self> {
        if packet.starts_with(FLAC_PREFIX) {
            Some(Self::Flac)
        } else if vorbis::is_identification(packet) {
            Some(Self::Vorbis)
        } else if packet.starts_with(super::opus::HEAD_SIGNATURE) {
            Some(Self::Opus)
        } else {
            None
        }
    }
}

/// The streams one link opens, and the one decoded, whose opening page
/// [`Pages::current`] holds.
pub(super) struct Group {
    pub(super) serials: Vec<u32>,
    pub(super) codec: Codec,
    pub(super) page: Page,
    /// Where the link's pages past its opening ones begin.
    pub(super) body: u64,
}

/// The first packet of the opening page `page`, whose bytes `bytes` holds,
/// where it completes on that page.
pub(super) fn first_packet<'b>(page: &Page, bytes: &'b [u8]) -> Option<&'b [u8]> {
    if page.is(CONTINUED) {
        return None;
    }
    let mut len = 0;
    for segment in 0..page.segments {
        let lacing = page.lacing(bytes, segment)?;
        len += lacing;
        if lacing < 255 {
            let start = page.header_len();
            return bytes.get(start..start + len);
        }
    }
    None
}

/// Read the pages that open the link at `start`: every logical stream's
/// first, together, the first decoded here chosen.
pub(super) fn group(
    input: &mut (impl SoundInput + ?Sized),
    start: u64,
    pages: &mut Pages,
) -> Result<Group, DecodeError> {
    let mut serials = Vec::new();
    let mut chosen = None;
    let mut at = start;
    while at < input.len() {
        let bytes = if chosen.is_some() {
            &mut pages.staging
        } else {
            &mut pages.current
        };
        let page = verified_page(input, at, bytes)?;
        if !page.is(FIRST_PAGE) {
            break;
        }
        if serials.contains(&page.serial) {
            return Err(DecodeError::OggBadLink);
        }
        if serials.len() == MAX_STREAMS {
            return Err(DecodeError::OggTooManyStreams);
        }
        if serials.try_reserve(1).is_err() {
            return Err(DecodeError::OutOfMemory);
        }
        serials.push(page.serial);
        if chosen.is_none() {
            chosen = first_packet(&page, bytes)
                .and_then(Codec::of)
                .map(|codec| (codec, page));
        }
        at = page.end();
    }
    if serials.is_empty() {
        return Err(DecodeError::OggBadLink);
    }
    let (codec, page) = chosen.ok_or(DecodeError::OggNoCodec)?;
    Ok(Group {
        serials,
        codec,
        page,
        body: at,
    })
}

/// Where the next link begins after the link whose streams are `serials`
/// and whose pages past its opening ones begin at `body`; `None` where the
/// link runs to the end of the physical stream, whatever bytes trail its
/// last page.
pub(super) fn end(
    input: &mut (impl SoundInput + ?Sized),
    serials: &[u32],
    body: u64,
    bytes: &mut Vec<u8>,
) -> Result<Option<u64>, DecodeError> {
    let len = input.len();
    let member = |page: &Page| serials.contains(&page.serial);
    let mut any = Search {
        admit: |_: &Page| true,
        accept: |_: &Page, _: &[u8]| true,
    };
    match any.last(input, body, len, MAX_PAGE_LEN as u64, bytes)? {
        Some(last) if member(&last) => return Ok(None),
        None if len - body.min(len) <= MAX_PAGE_LEN as u64 => return Ok(None),
        _ => {}
    }
    let (mut lo, mut hi) = (body, len);
    while hi - lo > LINEAR_SPAN {
        let mid = lo + (hi - lo) / 2;
        match any.first(input, mid, hi, bytes)? {
            Some(page) if member(&page) => lo = page.end(),
            Some(page) => hi = page.offset,
            None => hi = mid,
        }
    }
    let mut at = lo;
    while at < len {
        let header = page_header(input, at)?;
        if !member(&header) {
            return if header.is(FIRST_PAGE) {
                Ok(Some(at))
            } else {
                Err(DecodeError::OggBadLink)
            };
        }
        at = header.end();
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::super::page::Pages;
    use super::super::PageWriter;
    use super::{end, group, Codec};
    use crate::DecodeError;

    /// A stream of `pages` pages of `serial`, its first packet `first`.
    fn stream(serial: u32, first: &[u8], pages: u8) -> Vec<u8> {
        let mut writer = PageWriter::new(serial);
        writer.packet(first, 0);
        writer.flush(false);
        for page in 1..pages {
            writer.packet(&[page; 1000], u64::from(page));
            writer.flush(page + 1 == pages);
        }
        writer.into_bytes()
    }

    const VORBIS: &[u8] = b"\x01vorbis-and-the-rest";

    /// The length of the page `file` opens with.
    fn first_page_len(file: &[u8]) -> usize {
        let segments = usize::from(file[26]);
        27 + segments
            + file[27..27 + segments]
                .iter()
                .map(|&len| usize::from(len))
                .sum::<usize>()
    }

    #[test]
    fn the_first_stream_with_a_codec_decoded_here_is_chosen() {
        let mut file = stream(9, b"unknown codec", 3);
        let vorbis = stream(4, VORBIS, 3);
        let split = first_page_len(&file);
        file.splice(split..split, vorbis.iter().copied());
        let mut input = &file[..];
        let mut pages = Pages::default();
        let opened = group(&mut input, 0, &mut pages);
        let opened = opened.expect("a group");
        assert_eq!(opened.codec, Codec::Vorbis);
        assert_eq!(opened.serials, [9, 4]);
        assert_eq!(opened.page.serial, 4);
    }

    #[test]
    fn a_link_with_no_codec_or_a_repeated_serial_is_refused() {
        let file = stream(9, b"unknown codec", 2);
        let mut input = &file[..];
        assert_eq!(
            group(&mut input, 0, &mut Pages::default()).err(),
            Some(DecodeError::OggNoCodec)
        );
        let mut twice = stream(9, VORBIS, 1);
        twice.extend_from_slice(&stream(9, VORBIS, 1));
        let mut input = &twice[..];
        assert_eq!(
            group(&mut input, 0, &mut Pages::default()).err(),
            Some(DecodeError::OggBadLink)
        );
    }

    /// A link's end is found by bisection however long it is, and a link
    /// running to the end of the file has no successor whatever trails it.
    #[test]
    fn the_end_of_a_link_is_the_next_link_s_first_page() {
        let first = stream(1, VORBIS, 250);
        let second = stream(2, VORBIS, 5);
        let mut file = first.clone();
        file.extend_from_slice(&second);
        let body = first_page_len(&first) as u64;
        let mut input = &file[..];
        let mut bytes = Vec::new();
        let found = end(&mut input, &[1], body, &mut bytes).expect("searches");
        assert_eq!(found, Some(first.len() as u64));
        let mut input = &file[..];
        let last = end(&mut input, &[2], first.len() as u64 + body, &mut bytes).expect("searches");
        assert_eq!(last, None);
        let mut trailed = first.clone();
        trailed.extend_from_slice(b"TAG and an ID3v1 tag after the last page");
        let mut input = &trailed[..];
        assert_eq!(end(&mut input, &[1], body, &mut bytes), Ok(None));
    }

    #[test]
    fn a_link_followed_by_a_page_that_opens_no_stream_is_refused() {
        let first = stream(1, VORBIS, 4);
        let mut stray = stream(2, VORBIS, 3);
        stray.drain(..first_page_len(&stray));
        let body = first_page_len(&first) as u64;
        let mut file = first;
        file.extend_from_slice(&stray);
        let mut input = &file[..];
        assert_eq!(
            end(&mut input, &[1], body, &mut Vec::new()),
            Err(DecodeError::OggBadLink)
        );
    }
}
