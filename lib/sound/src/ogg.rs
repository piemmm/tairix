//! Ogg (RFC 3533), the container in full: pages and their CRC, packets laid
//! across pages, logical streams multiplexed together, and links chained one
//! after another; and over it the FLAC (RFC 9639, section 10.1), Vorbis
//! (Vorbis I, appendix A) and Opus (RFC 7845) mappings.
//!
//! A link opens its logical streams together and the first whose codec is
//! decoded here is read; the others' pages are stepped over. Every page whose
//! bytes are read is checked against its CRC and its place in its stream's
//! sequence. A link is decoded on its own — its rate, channels and tags may
//! differ from the last — and where it ends, the next of the chain is opened
//! by the caller, so a chain is read to its end rather than truncated.

mod flac;
mod link;
mod opus;
mod page;
mod vorbis;

#[cfg(any(test, feature = "encode"))]
use alloc::vec::Vec;

use self::link::Codec;
pub(crate) use self::page::CAPTURE;
#[cfg(any(test, feature = "encode"))]
use self::page::{CONTINUED, FIRST_PAGE, LAST_PAGE};
#[cfg(any(test, feature = "encode"))]
use crate::crc::crc32;
use crate::input::SoundInput;
use crate::meta::Collector;
use crate::{DecodeError, DecodeLimits, SoundInfo};

#[cfg(any(test, feature = "encode"))]
pub(crate) use self::flac::header as flac_header;

/// Whether `bytes` open with an Ogg page.
pub(crate) fn has_signature(bytes: &[u8]) -> bool {
    bytes.starts_with(CAPTURE)
}

/// The codec reading a link's stream.
#[allow(
    clippy::large_enum_variant,
    reason = "one a link, so its size buys nothing to box away"
)]
enum Reader {
    Flac(flac::OggFlac),
    Vorbis(vorbis::OggVorbis),
    Opus(opus::OggOpus),
}

/// One link of an Ogg file, past its headers.
pub(crate) struct Ogg {
    reader: Reader,
    /// Where the next link begins, if one follows; a link boundary that does
    /// not hold together is reported when the next link is asked for, so
    /// this one still plays.
    next: Result<Option<u64>, DecodeError>,
}

/// Whether `err` says the input could not serve a read, rather than
/// anything about the file.
pub(super) const fn is_input(err: DecodeError) -> bool {
    matches!(
        err,
        DecodeError::InputUnavailable | DecodeError::InputFailed | DecodeError::OutOfMemory
    )
}

/// Read the headers of the link that begins at `start`.
pub(crate) fn open(
    input: &mut (impl SoundInput + ?Sized),
    start: u64,
    limits: &DecodeLimits,
    collector: &mut Collector,
) -> Result<(SoundInfo, Ogg), DecodeError> {
    let mut pages = page::Pages::default();
    let group = link::group(input, start, &mut pages)?;
    let next = match link::end(input, &group.serials, group.body, &mut pages.staging) {
        Err(err) if is_input(err) => return Err(err),
        next => next,
    };
    let end = match next {
        Ok(Some(end)) => end,
        Ok(None) | Err(_) => input.len(),
    };
    let (info, reader) = match group.codec {
        Codec::Flac => {
            let (info, reader) = flac::open(input, &group, end, pages, limits, collector)?;
            (info, Reader::Flac(reader))
        }
        Codec::Vorbis => {
            let (info, reader) = vorbis::open(input, &group, end, pages, limits, collector)?;
            (info, Reader::Vorbis(reader))
        }
        Codec::Opus => {
            let (info, reader) = opus::open(input, &group, end, pages, limits, collector)?;
            (info, Reader::Opus(reader))
        }
    };
    Ok((info, Ogg { reader, next }))
}

impl Ogg {
    /// Whether the link, decoded whole and in order, matched a digest it
    /// carries.
    pub(crate) fn verified(&self) -> bool {
        match &self.reader {
            Reader::Flac(flac) => flac.verified(),
            Reader::Vorbis(_) | Reader::Opus(_) => false,
        }
    }

    /// Write the frames from `position` that `out` has room for.
    pub(crate) fn read(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        position: u64,
        out: &mut [u8],
    ) -> Result<usize, DecodeError> {
        match &mut self.reader {
            Reader::Flac(flac) => flac.read(input, position, out),
            Reader::Vorbis(vorbis) => vorbis.read(input, position, out),
            Reader::Opus(opus) => opus.read(input, position, out),
        }
    }

    /// Where the next link of the chain begins, if one follows.
    pub(crate) const fn next_link(&self) -> Result<Option<u64>, DecodeError> {
        self.next
    }
}

/// The most bytes one call reads for a link within `limits` writing
/// `frames` frames.
pub(crate) const fn max_working_set(limits: &DecodeLimits, frames: u32) -> u64 {
    let flac = flac::max_working_set(limits);
    let vorbis = vorbis::max_working_set(limits.max_channels() as usize, frames as u64);
    let opus = opus::max_working_set(frames as u64);
    let largest = if flac > vorbis { flac } else { vorbis };
    if largest > opus {
        largest
    } else {
        opus
    }
}

/// Pages of one logical stream: each packet laid on as many segments as it
/// takes, a page ended when its lacing fills or when asked.
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

#[cfg(test)]
mod test_support;

#[cfg(test)]
#[path = "ogg_tests.rs"]
mod tests;
