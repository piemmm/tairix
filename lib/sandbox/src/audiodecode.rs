//! Sound files decoded in the sandbox (`tairix-sound`).
//!
//! The owner hands the worker a file's length, never the file. The worker asks
//! for the bytes its decoder reads ([`DecodeEvent::Need`]), the owner reads
//! them and hands them over, and the worker answers the request it was
//! serving. A decode therefore holds the worker's page cache however long the
//! file, and nothing the worker answers is believed past the checks
//! [`AudioDecodeClient::on_frame`] makes: it has read hostile bytes and may
//! be compromised.
//!
//! [`AudioDecodeService`] is the worker's [`SessionService`];
//! [`AudioDecodeClient`] is the owner's side, which encodes each request,
//! checks each reply and keeps the stream's state. It runs over a
//! [`crate::supervise::SupervisedSession`], so a worker that fails is
//! replaced, and [`AudioDecodeClient::restart`] brings the replacement back to
//! where the stream was.
//!
//! # The protocol
//!
//! One request at a time: open, decode or seek, then any number of need and
//! supply exchanges, then that request's one answer. The owner sends nothing
//! else until the answer arrives, so every frame either way has exactly one
//! reading.
//!
//! # A request makes progress or is refused
//!
//! A decoder call that meets bytes the worker does not hold changes nothing
//! (`tairix_sound`'s streaming contract), so the worker asks for them and runs
//! the call again. Every page a request has read stays held until the request
//! is answered, so each retry gets further than the last; a request that needs
//! more of the file at once than the cache holds is refused as
//! [`AudioRefusal::WorkingSetExceeded`] rather than asking for ever.

use alloc::vec::Vec;

use tairix_abi::driver::audio::{
    ChannelMap, Rate, SampleFormat, CHANNEL_MAP_WIRE_LEN, MAX_CHANNELS,
};
use tairix_collections::LruMap;
use tairix_hash::{BuildSipHash13, HashSeed};
use tairix_sound::{
    CoverRange, Cue, DataLength, DecodeError, DecodeLimits, Encoding, InputError, Loop, LoopKind,
    Metadata, PcmSource, SoundFormat, SoundInfo, SoundInput, Tag, TagKey, TagKind,
};

use crate::host::Unbelieved;
use crate::proto::FRAME_HEADER_LEN;
use crate::session::{FrameOut, SessionBounds, SessionError, SessionService, SessionStep};
use crate::wire::{Reader, WireError, Writer};

/// Bytes one page of the worker's cache holds; every need starts on a page.
pub const PAGE_BYTES: usize = 4096;

/// Pages the worker holds: the most of a file one request can read.
///
/// A fixed containment bound on a worker that reads hostile files, taken from
/// what the decoders state one decode reads at the limits — a FLAC stream's
/// largest frame, at most — with the read-ahead behind it. Pages are held
/// only as a file needs them. An open whose metadata structure alone spans
/// more is refused rather than asked for for ever.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a working set of a few mebibytes is a few thousand pages, far within usize"
)]
pub const CACHE_PAGES: usize = (tairix_sound::max_working_set(&LIMITS, MAX_BLOCK_FRAMES)
    .div_ceil(PAGE)
    + READ_AHEAD_PAGES
    + 2) as usize;

/// Most bytes one need asks for, so one supply is bounded however a file is
/// laid out.
pub const MAX_NEED_BYTES: usize = MAX_NEED_PAGES * PAGE_BYTES;

/// Most frames one decode answers.
pub const MAX_BLOCK_FRAMES: u32 = 4096;

/// What a file may make the worker keep.
pub const LIMITS: DecodeLimits = DecodeLimits::new(STREAM_CHANNELS, 64 * 1024, 1024);

/// Bytes of the largest frame the owner sends: a supply of the largest need.
pub const MAX_TO_WORKER: usize = SUPPLY_HEADER_LEN + MAX_NEED_BYTES;

/// Bytes of the largest frame the worker sends: a block of the widest frames,
/// or an open's answer at the limits.
pub const MAX_FROM_WORKER: usize = if MAX_BLOCK_LEN > MAX_OPENED_LEN {
    MAX_BLOCK_LEN
} else {
    MAX_OPENED_LEN
};

/// The bound an owner admits a session under: one largest frame queued each
/// way, which is all a one-request-at-a-time protocol ever has in flight.
///
/// # Errors
///
/// None in practice: both bounds exceed the session's minimum, which
/// [`SessionBounds::new`] still checks.
pub const fn session_bounds() -> Result<SessionBounds, SessionError> {
    SessionBounds::new(
        FRAME_HEADER_LEN + MAX_TO_WORKER,
        FRAME_HEADER_LEN + MAX_FROM_WORKER,
    )
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a channel map names at most eight channels, asserted below"
)]
const STREAM_CHANNELS: u8 = MAX_CHANNELS as u8;

const _: () = assert!(MAX_CHANNELS <= u8::MAX as usize);

const PAGE: u64 = PAGE_BYTES as u64;

const MAX_NEED_PAGES: usize = 128;

/// Pages a need asks for past those the read that missed wants: the bytes a
/// sequential decode reads next, so a stream costs one exchange a window.
const READ_AHEAD_PAGES: u64 = 32;

const _: () = assert!(MAX_NEED_PAGES <= CACHE_PAGES);

// One read, wherever it falls, misses at most the pages it straddles, and
// every need of them fits one supply.
const _: () = assert!(tairix_sound::MAX_READ / PAGE_BYTES < MAX_NEED_PAGES);

/// The widest frame a block carries: every channel in a four-byte sample.
const MAX_FRAME_BYTES: usize = MAX_CHANNELS * 4;

const TO_OPEN: u8 = 1;
const TO_DECODE: u8 = 2;
const TO_SEEK: u8 = 3;
const TO_SUPPLY: u8 = 4;
const TO_UNREADABLE: u8 = 5;

const FROM_NEED: u8 = 1;
const FROM_OPENED: u8 = 2;
const FROM_BLOCK: u8 = 3;
const FROM_SOUGHT: u8 = 4;
const FROM_REFUSED: u8 = 5;

const OPEN_LEN: usize = 1 + HashSeed::LEN + 8 + 1;
const DECODE_LEN: usize = 1 + 4;
const SEEK_LEN: usize = 1 + 8;
const SUPPLY_HEADER_LEN: usize = 1 + 8;
const UNREADABLE_LEN: usize = 1 + 8;

const NEED_LEN: usize = 1 + 8 + 4;
const BLOCK_HEADER_LEN: usize = 1 + 8 + 4;
const SOUGHT_LEN: usize = 1 + 8;
const REFUSED_LEN: usize = 1 + 1 + 4;

/// A stream's description: format, encoding and its width, rate, channel
/// map, sample format, frame count, seekability, data-length disagreement.
const INFO_LEN: usize = 1 + 2 + 4 + CHANNEL_MAP_WIRE_LEN + 1 + 9 + 1 + 17;

/// A loop's wire form, the widest marker.
const LOOP_LEN: usize = 8 + 8 + 1 + 4 + 4;

/// An open's answer at the limits: a tag's wire form is smaller than its share
/// of the metadata budget, and every marker at most a loop.
const MAX_OPENED_LEN: usize = 1
    + INFO_LEN
    + 4
    + LIMITS.max_metadata_bytes() as usize
    + 4
    + 4
    + LIMITS.max_markers() as usize * LOOP_LEN
    + 2
    + COVER_LEN;

/// A cover range's wire form: a presence flag, its offset and its length.
const COVER_LEN: usize = 1 + 8 + 8;

const MAX_BLOCK_LEN: usize = BLOCK_HEADER_LEN + MAX_BLOCK_FRAMES as usize * MAX_FRAME_BYTES;

const REFUSED_MALFORMED: u8 = 1;
const REFUSED_NOT_OPEN: u8 = 2;
const REFUSED_WORKING_SET: u8 = 3;

const NO_NOTE: u8 = u8::MAX;

/// Why the worker refused a request.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AudioRefusal {
    /// The decoder refused the file, or the part of it the request read.
    Decode(DecodeError),
    /// The request needs more of the file at once than the worker holds.
    WorkingSetExceeded,
    /// A decode or seek with no file open.
    NotOpen,
    /// A frame the protocol does not admit at that point. Whatever was open
    /// is closed.
    MalformedRequest,
}

impl AudioRefusal {
    fn to_wire(self) -> (u8, u32) {
        match self {
            Self::Decode(err) => decode_to_wire(err),
            Self::WorkingSetExceeded => (REFUSED_WORKING_SET, 0),
            Self::NotOpen => (REFUSED_NOT_OPEN, 0),
            Self::MalformedRequest => (REFUSED_MALFORMED, 0),
        }
    }

    fn from_wire(code: u8, detail: u32) -> Option<Self> {
        match code {
            REFUSED_MALFORMED => (detail == 0).then_some(Self::MalformedRequest),
            REFUSED_NOT_OPEN => (detail == 0).then_some(Self::NotOpen),
            REFUSED_WORKING_SET => (detail == 0).then_some(Self::WorkingSetExceeded),
            _ => decode_from_wire(code, detail).map(Self::Decode),
        }
    }
}

impl core::fmt::Display for AudioRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Decode(err) => write!(f, "{err}"),
            Self::WorkingSetExceeded => {
                f.write_str("the file needs more of itself at once than the decoder holds")
            }
            Self::NotOpen => f.write_str("no sound file is open"),
            Self::MalformedRequest => f.write_str("malformed decode request"),
        }
    }
}

const AU_UNKNOWN_ENCODING: u8 = 64;
const WAV_UNKNOWN_FORMAT_TAG: u8 = 65;

/// One table for both directions, so a code cannot mean one thing to the
/// worker and another to the owner; the encoder's match is exhaustive, so a
/// new decoder refusal cannot be left without one.
macro_rules! decode_codes {
    ($($code:literal => $variant:ident,)+) => {
        fn decode_to_wire(err: DecodeError) -> (u8, u32) {
            match err {
                $(DecodeError::$variant => ($code, 0),)+
                DecodeError::AuUnknownEncoding(code) => (AU_UNKNOWN_ENCODING, code),
                DecodeError::WavUnknownFormatTag(tag) => (WAV_UNKNOWN_FORMAT_TAG, u32::from(tag)),
            }
        }

        fn decode_from_wire(code: u8, detail: u32) -> Option<DecodeError> {
            match code {
                $($code => (detail == 0).then_some(DecodeError::$variant),)+
                AU_UNKNOWN_ENCODING => Some(DecodeError::AuUnknownEncoding(detail)),
                WAV_UNKNOWN_FORMAT_TAG => {
                    u16::try_from(detail).ok().map(DecodeError::WavUnknownFormatTag)
                }
                _ => None,
            }
        }
    };
}

decode_codes! {
    16 => UnknownFormat,
    17 => InputUnavailable,
    18 => InputFailed,
    19 => OutOfMemory,
    20 => ChannelsExceedLimit,
    21 => NoChannels,
    22 => ChannelLayoutUnsupported,
    23 => RateOutOfRange,
    24 => BufferTooSmall,
    25 => SeekUnsupported,
    26 => SeekPastEnd,
    27 => AuBadMagic,
    28 => AuHeaderTruncated,
    29 => AuDataOffsetBad,
    30 => AuFragmentedData,
    31 => AuNestedSound,
    32 => AuDspProgram,
    33 => AuDisplayData,
    34 => AuDspCommands,
    35 => AuUnspecifiedEncoding,
    36 => AuAdpcmChannels,
    37 => AuG722Rate,
    38 => WavBadMagic,
    39 => WavChunkTruncated,
    40 => WavTooManyChunks,
    41 => WavMissingDs64,
    42 => WavMissingFormat,
    43 => WavMissingData,
    44 => WavDuplicateFormat,
    45 => WavDuplicateData,
    46 => WavFormatTruncated,
    47 => WavMpegAudio,
    48 => WavGsm610,
    49 => WavBadBlockAlign,
    50 => WavBadBitDepth,
    51 => WavBadExtensible,
    52 => WavUnknownSubformat,
    53 => WavChannelMask,
    54 => WavBadAdpcmFormat,
    55 => WavAdpcmBlockCorrupt,
    66 => FlacBadMarker,
    67 => FlacMissingStreamInfo,
    68 => FlacBadStreamInfo,
    69 => FlacDuplicateBlock,
    70 => FlacForbiddenBlock,
    71 => FlacMetadataTruncated,
    72 => FlacBadSeekTable,
    73 => FlacBadComment,
    74 => FlacBadCuesheet,
    75 => FlacBadPicture,
    76 => FlacBadApplication,
    77 => FlacChannelMask,
    78 => FlacNoSync,
    79 => FlacHeaderCrc,
    80 => FlacFrameCrc,
    81 => FlacReserved,
    82 => FlacFrameInvalid,
    83 => FlacFrameMismatch,
    84 => FlacFrameOutOfOrder,
    85 => FlacFrameTooLarge,
    86 => FlacTruncated,
    87 => FlacDigestMismatch,
    88 => OggNoCapture,
    89 => OggBadPage,
    90 => OggPageTruncated,
    91 => OggPageCrc,
    92 => OggPageLost,
    93 => OggBadPacket,
    94 => OggPacketTooLarge,
    95 => OggNoFlacStream,
    96 => OggBadFlacMapping,
    97 => OggNoAudio,
    98 => OggChained,
}

/// What the worker is asked to do.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Request {
    Open,
    Decode { frames: u32 },
    Seek { frame: u64 },
}

/// The pages a need asks for: those the read that missed wants, then the
/// read-ahead behind them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Need {
    first: u64,
    needed_end: u64,
    end: u64,
}

/// Why supplied pages could not be held.
enum Shortfall {
    WorkingSet,
    Memory,
}

/// One held page of the file: a whole page, or the file's tail.
struct Page {
    bytes: Vec<u8>,
    /// The request that last read it. A page the current request has read is
    /// never given up before that request is answered.
    read_in: u64,
}

/// The pages of one file the worker holds, read as the decoder's input.
struct Pages {
    held: LruMap<u64, Page, BuildSipHash13>,
    len: u64,
    /// The request being served, counted from one; zero is no request.
    request: u64,
    /// The first and last page the failing read of this attempt lacked.
    miss: Option<(u64, u64)>,
}

impl Pages {
    fn new(len: u64, seed: HashSeed) -> Option<Self> {
        let held =
            LruMap::try_with_capacity_and_hasher(CACHE_PAGES, BuildSipHash13::with_seed(seed))
                .ok()?;
        Some(Self {
            held,
            len,
            request: 0,
            miss: None,
        })
    }

    fn page_count(&self) -> u64 {
        self.len.div_ceil(PAGE)
    }

    /// The need that answers this attempt's miss, or the refusal when no need
    /// could.
    fn need(&self) -> Result<Need, AudioRefusal> {
        let (first, last) = self
            .miss
            .ok_or(AudioRefusal::Decode(DecodeError::InputFailed))?;
        let needed_end = last + 1;
        if needed_end - first > MAX_NEED_PAGES as u64 {
            return Err(AudioRefusal::WorkingSetExceeded);
        }
        let limit = (first + MAX_NEED_PAGES as u64)
            .min(needed_end + READ_AHEAD_PAGES)
            .min(self.page_count());
        let mut end = needed_end;
        while end < limit && !self.held.contains_key(&end) {
            end += 1;
        }
        Ok(Need {
            first,
            needed_end,
            end,
        })
    }

    /// Bytes `need` covers: its pages, the last cut at the file's end.
    fn need_bytes(&self, need: Need) -> (u64, usize) {
        let offset = need.first * PAGE;
        let end = (need.end * PAGE).min(self.len);
        (offset, usize::try_from(end - offset).unwrap_or(usize::MAX))
    }

    /// Hold the pages `bytes` holds, which are `need`'s.
    ///
    /// A page the request needs displaces any page the request has not read,
    /// and is refused only when every page held is one it has; read-ahead
    /// displaces nothing the request has read, so it stops there instead.
    fn take(&mut self, need: Need, bytes: &[u8]) -> Result<(), Shortfall> {
        for (page, chunk) in (need.first..need.end).zip(bytes.chunks(PAGE_BYTES)) {
            if self.held.contains_key(&page) {
                continue;
            }
            let needed = page < need.needed_end;
            let mut buffer = if self.held.len() < CACHE_PAGES {
                let mut buffer = Vec::new();
                buffer
                    .try_reserve_exact(PAGE_BYTES)
                    .map_err(|_| Shortfall::Memory)?;
                buffer
            } else {
                // Pages are refreshed as they are read, and a retry reads
                // again every page the request read before, so the least
                // recent page is one the request has read only when all are.
                let spare = self
                    .held
                    .peek_lru()
                    .is_some_and(|(_, lru)| lru.read_in != self.request);
                let recycled = if spare { self.held.pop_lru() } else { None };
                match recycled {
                    Some((_, recycled)) => recycled.bytes,
                    None if needed => return Err(Shortfall::WorkingSet),
                    None => return Ok(()),
                }
            };
            buffer.clear();
            buffer.extend_from_slice(chunk);
            let read_in = if needed { self.request } else { 0 };
            self.held
                .try_insert(
                    page,
                    Page {
                        bytes: buffer,
                        read_in,
                    },
                )
                .map_err(|_| Shortfall::Memory)?;
        }
        Ok(())
    }
}

impl SoundInput for Pages {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, InputError> {
        if offset >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let wanted = u64::try_from(buf.len()).map_err(|_| InputError::Failed)?;
        let end = offset.saturating_add(wanted).min(self.len);
        let mut missing: Option<(u64, u64)> = None;
        for page in offset / PAGE..=(end - 1) / PAGE {
            let Some(held) = self.held.get_mut(&page) else {
                missing = Some((missing.map_or(page, |(first, _)| first), page));
                continue;
            };
            held.read_in = self.request;
            if missing.is_some() {
                continue;
            }
            let start = page * PAGE;
            let (from, to) = (offset.max(start), end.min(start + PAGE));
            let (Some(source), Some(target)) = (
                span(from - start, to - start).and_then(|range| held.bytes.get(range)),
                span(from - offset, to - offset).and_then(|range| buf.get_mut(range)),
            ) else {
                return Err(InputError::Failed);
            };
            target.copy_from_slice(source);
        }
        if let Some(span) = missing {
            self.miss.get_or_insert(span);
            return Err(InputError::Unavailable);
        }
        usize::try_from(end - offset).map_err(|_| InputError::Failed)
    }
}

/// `from..to` as indices, both within one read and so within `usize`.
fn span(from: u64, to: u64) -> Option<core::ops::Range<usize>> {
    Some(usize::try_from(from).ok()?..usize::try_from(to).ok()?)
}

/// The file the worker has open.
struct OpenFile {
    pages: Pages,
    format: Option<SoundFormat>,
    source: Option<PcmSource>,
    /// Where a block's answer is built: its header, then as many of the
    /// widest frames the stream has as one block carries.
    block: Vec<u8>,
}

/// The worker side of a sandboxed decode: one file at a time, held in the
/// pages its owner supplies.
pub struct AudioDecodeService {
    file: Option<OpenFile>,
    pending: Option<(Request, Need)>,
    /// Where every answer but a block is built.
    reply: Vec<u8>,
}

impl Default for AudioDecodeService {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioDecodeService {
    /// A worker with nothing open.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            file: None,
            pending: None,
            reply: Vec::new(),
        }
    }

    fn close(&mut self) {
        self.file = None;
        self.pending = None;
    }

    fn open(&mut self, seed: HashSeed, len: u64, format: Option<SoundFormat>) -> Answer {
        self.close();
        let Some(pages) = Pages::new(len, seed) else {
            return Answer::Refused(AudioRefusal::Decode(DecodeError::OutOfMemory));
        };
        self.file = Some(OpenFile {
            pages,
            format,
            source: None,
            block: Vec::new(),
        });
        self.begin(Request::Open)
    }

    fn begin(&mut self, request: Request) -> Answer {
        if self.pending.is_some() {
            self.close();
            return Answer::Refused(AudioRefusal::MalformedRequest);
        }
        let Some(file) = self.file.as_mut() else {
            return Answer::Refused(AudioRefusal::NotOpen);
        };
        file.pages.request += 1;
        self.attempt(request)
    }

    fn attempt(&mut self, request: Request) -> Answer {
        let Some(file) = self.file.as_mut() else {
            return Answer::Refused(AudioRefusal::NotOpen);
        };
        file.pages.miss = None;
        let outcome = match request {
            Request::Open => Self::open_source(file),
            Request::Decode { frames } => Self::decode(file, frames),
            Request::Seek { frame } => match file.source.as_mut() {
                Some(source) => source
                    .seek(frame)
                    .map(|()| Answer::Sought(source.position())),
                None => return Answer::Refused(AudioRefusal::NotOpen),
            },
        };
        match outcome {
            Ok(answer) => answer,
            Err(DecodeError::InputUnavailable) => match file.pages.need() {
                Ok(need) => {
                    self.pending = Some((request, need));
                    Answer::Need(file.pages.need_bytes(need))
                }
                Err(refusal) => self.refuse(request, refusal),
            },
            Err(err) => self.refuse(request, AudioRefusal::Decode(err)),
        }
    }

    /// A refusal of `request`; a file whose open was refused is not open.
    fn refuse(&mut self, request: Request, refusal: AudioRefusal) -> Answer {
        if request == Request::Open {
            self.close();
        }
        Answer::Refused(refusal)
    }

    fn open_source(file: &mut OpenFile) -> Result<Answer, DecodeError> {
        let source = match file.format {
            Some(format) => PcmSource::open_as(format, &mut file.pages, &LIMITS)?,
            None => PcmSource::open(&mut file.pages, &LIMITS)?,
        };
        let block_len = BLOCK_HEADER_LEN + MAX_BLOCK_FRAMES as usize * source.info().frame_bytes();
        if !tairix_util::fallible::grow_to(&mut file.block, block_len, 0u8) {
            return Err(DecodeError::OutOfMemory);
        }
        file.source = Some(source);
        Ok(Answer::Opened)
    }

    fn decode(file: &mut OpenFile, frames: u32) -> Result<Answer, DecodeError> {
        let Some(source) = file.source.as_mut() else {
            return Ok(Answer::Refused(AudioRefusal::NotOpen));
        };
        let frame_bytes = source.info().frame_bytes();
        let span = frames as usize * frame_bytes;
        let position = source.position();
        let out = file
            .block
            .get_mut(BLOCK_HEADER_LEN..BLOCK_HEADER_LEN + span)
            .ok_or(DecodeError::OutOfMemory)?;
        let written = source.next_block(&mut file.pages, out)?;
        let count = u32::try_from(written).map_err(|_| DecodeError::OutOfMemory)?;
        let mut header = [0u8; BLOCK_HEADER_LEN];
        header[0] = FROM_BLOCK;
        header[1..9].copy_from_slice(&position.to_le_bytes());
        header[9..].copy_from_slice(&count.to_le_bytes());
        let slot = file
            .block
            .first_chunk_mut::<BLOCK_HEADER_LEN>()
            .ok_or(DecodeError::OutOfMemory)?;
        *slot = header;
        Ok(Answer::Block(BLOCK_HEADER_LEN + written * frame_bytes))
    }

    /// The outstanding request and its need, taken, if `answers` the bytes
    /// it asked for; otherwise the owner has broken the protocol.
    fn answered(&mut self, answers: impl FnOnce(u64, usize) -> bool) -> Option<(Request, Need)> {
        let (request, need) = self.pending.take()?;
        let (offset, len) = self.file.as_ref()?.pages.need_bytes(need);
        answers(offset, len).then_some((request, need))
    }

    fn supply(&mut self, offset: u64, bytes: &[u8]) -> Answer {
        let answered = self.answered(|asked, len| (asked, len) == (offset, bytes.len()));
        let (Some((request, need)), Some(file)) = (answered, self.file.as_mut()) else {
            self.close();
            return Answer::Refused(AudioRefusal::MalformedRequest);
        };
        match file.pages.take(need, bytes) {
            Ok(()) => self.attempt(request),
            Err(Shortfall::WorkingSet) => self.refuse(request, AudioRefusal::WorkingSetExceeded),
            Err(Shortfall::Memory) => {
                self.refuse(request, AudioRefusal::Decode(DecodeError::OutOfMemory))
            }
        }
    }

    fn unreadable(&mut self, offset: u64) -> Answer {
        if let Some((request, _)) = self.answered(|asked, _| asked == offset) {
            self.refuse(request, AudioRefusal::Decode(DecodeError::InputFailed))
        } else {
            self.close();
            Answer::Refused(AudioRefusal::MalformedRequest)
        }
    }

    fn serve(&mut self, frame: &[u8]) -> Answer {
        match Inbound::read(frame) {
            Ok(Inbound::Open(seed, len, format)) => self.open(seed, len, format),
            Ok(Inbound::Decode(frames)) if (1..=MAX_BLOCK_FRAMES).contains(&frames) => {
                self.begin(Request::Decode { frames })
            }
            Ok(Inbound::Seek(frame)) => self.begin(Request::Seek { frame }),
            Ok(Inbound::Supply(offset, bytes)) => self.supply(offset, bytes),
            Ok(Inbound::Unreadable(offset)) => self.unreadable(offset),
            Ok(Inbound::Decode(_)) | Err(_) => {
                self.close();
                Answer::Refused(AudioRefusal::MalformedRequest)
            }
        }
    }

    /// Encode `answer` into the reply buffer, or lend the block it names.
    fn encoded(&mut self, answer: Answer) -> Option<&[u8]> {
        let mut out = Writer::reusing(core::mem::take(&mut self.reply));
        match answer {
            Answer::Block(len) => {
                self.reply = out.finish();
                return self.file.as_ref()?.block.get(..len);
            }
            Answer::Need((offset, len)) => {
                out.u8(FROM_NEED);
                out.u64(offset);
                out.u32(u32::try_from(len).unwrap_or(u32::MAX));
            }
            Answer::Opened => {
                let source = self.file.as_ref()?.source.as_ref()?;
                out.u8(FROM_OPENED);
                encode_info(&mut out, source.info());
                encode_metadata(&mut out, source.metadata());
            }
            Answer::Sought(position) => {
                out.u8(FROM_SOUGHT);
                out.u64(position);
            }
            Answer::Refused(refusal) => {
                let (code, detail) = refusal.to_wire();
                out.u8(FROM_REFUSED);
                out.u8(code);
                out.u32(detail);
            }
        }
        self.reply = out.finish();
        Some(&self.reply)
    }
}

impl SessionService for AudioDecodeService {
    fn handle(&mut self, request: &[u8], out: &mut dyn FrameOut) -> SessionStep {
        let answer = self.serve(request);
        let sent = if let Some(frame) = self.encoded(answer) {
            out.frame(frame)
        } else {
            self.close();
            let refusal = Answer::Refused(AudioRefusal::Decode(DecodeError::OutOfMemory));
            let Some(frame) = self.encoded(refusal) else {
                return SessionStep::Finished;
            };
            out.frame(frame)
        };
        if sent.is_ok() {
            SessionStep::Continue
        } else {
            SessionStep::Finished
        }
    }
}

/// A frame from the owner, read.
enum Inbound<'a> {
    Open(HashSeed, u64, Option<SoundFormat>),
    Decode(u32),
    Seek(u64),
    Supply(u64, &'a [u8]),
    Unreadable(u64),
}

impl<'a> Inbound<'a> {
    fn read(frame: &'a [u8]) -> Result<Self, WireError> {
        let mut reader = Reader::new(frame);
        match (reader.u8()?, frame.len()) {
            (TO_OPEN, OPEN_LEN) => {
                let seed = HashSeed::from_words(reader.u64()?, reader.u64()?);
                let len = reader.u64()?;
                let format = match reader.u8()? {
                    0 => None,
                    raw => Some(format_from_wire(raw).ok_or(WireError::Malformed)?),
                };
                Ok(Self::Open(seed, len, format))
            }
            (TO_DECODE, DECODE_LEN) => reader.u32().map(Self::Decode),
            (TO_SEEK, SEEK_LEN) => reader.u64().map(Self::Seek),
            (TO_SUPPLY, len) if len >= SUPPLY_HEADER_LEN => {
                let offset = reader.u64()?;
                Ok(Self::Supply(offset, reader.take(reader.remaining())?))
            }
            (TO_UNREADABLE, UNREADABLE_LEN) => reader.u64().map(Self::Unreadable),
            _ => Err(WireError::Malformed),
        }
    }
}

/// What the worker answers a frame with.
#[derive(Copy, Clone)]
enum Answer {
    Need((u64, usize)),
    Opened,
    /// A block of this many bytes, built in the open file's block buffer.
    Block(usize),
    Sought(u64),
    Refused(AudioRefusal),
}

const fn format_to_wire(format: SoundFormat) -> u8 {
    match format {
        SoundFormat::Au => 1,
        SoundFormat::Wav => 2,
        SoundFormat::Flac => 3,
        SoundFormat::Ogg => 4,
    }
}

const fn format_from_wire(raw: u8) -> Option<SoundFormat> {
    match raw {
        1 => Some(SoundFormat::Au),
        2 => Some(SoundFormat::Wav),
        3 => Some(SoundFormat::Flac),
        4 => Some(SoundFormat::Ogg),
        _ => None,
    }
}

const fn encoding_to_wire(encoding: Encoding) -> (u8, u8) {
    match encoding {
        Encoding::Linear { bits } => (1, bits),
        Encoding::Fixed { bits } => (2, bits),
        Encoding::Float { bits } => (3, bits),
        Encoding::MuLaw => (4, 0),
        Encoding::ALaw => (5, 0),
        Encoding::G721 => (6, 0),
        Encoding::G722 => (7, 0),
        Encoding::G723Kbit24 => (8, 0),
        Encoding::G723Kbit40 => (9, 0),
        Encoding::MsAdpcm => (10, 0),
        Encoding::ImaAdpcm => (11, 0),
        Encoding::Flac => (12, 0),
    }
}

fn encoding_from_wire(code: u8, bits: u8) -> Option<Encoding> {
    let sized = |max| (1..=max).contains(&bits);
    match code {
        1 if sized(32) => Some(Encoding::Linear { bits }),
        2 if sized(32) => Some(Encoding::Fixed { bits }),
        3 if sized(64) => Some(Encoding::Float { bits }),
        4..=12 if bits == 0 => Some(match code {
            4 => Encoding::MuLaw,
            5 => Encoding::ALaw,
            6 => Encoding::G721,
            7 => Encoding::G722,
            8 => Encoding::G723Kbit24,
            9 => Encoding::G723Kbit40,
            10 => Encoding::MsAdpcm,
            11 => Encoding::ImaAdpcm,
            _ => Encoding::Flac,
        }),
        _ => None,
    }
}

fn encode_info(out: &mut Writer, info: &SoundInfo) {
    out.u8(format_to_wire(info.format));
    let (encoding, bits) = encoding_to_wire(info.encoding);
    out.u8(encoding);
    out.u8(bits);
    out.u32(info.rate.hz());
    out.raw(&info.channels.to_wire());
    out.u8(info.sample.as_u8());
    out.u8(u8::from(info.frames.is_some()));
    out.u64(info.frames.unwrap_or(0));
    out.u8(u8::from(info.seekable));
    out.u8(u8::from(info.data_length.is_some()));
    let disagreement = info.data_length.unwrap_or(DataLength {
        declared: 0,
        held: 0,
    });
    out.u64(disagreement.declared);
    out.u64(disagreement.held);
}

fn flag(reader: &mut Reader<'_>) -> Result<bool, WireError> {
    match reader.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(WireError::Malformed),
    }
}

fn decode_info(reader: &mut Reader<'_>) -> Result<SoundInfo, WireError> {
    let format = format_from_wire(reader.u8()?).ok_or(WireError::Malformed)?;
    let (code, bits) = (reader.u8()?, reader.u8()?);
    let encoding = encoding_from_wire(code, bits).ok_or(WireError::Malformed)?;
    let rate = Rate::new(reader.u32()?).map_err(|_| WireError::Malformed)?;
    let channels = ChannelMap::from_wire(reader.take(CHANNEL_MAP_WIRE_LEN)?)
        .map_err(|_| WireError::Malformed)?;
    let sample = SampleFormat::from_u8(reader.u8()?).map_err(|_| WireError::Malformed)?;
    let stated = flag(reader)?;
    let frames = reader.u64()?;
    let seekable = flag(reader)?;
    let disagrees = flag(reader)?;
    let (declared, held) = (reader.u64()?, reader.u64()?);
    let frames = if stated {
        Some(frames)
    } else if frames == 0 {
        None
    } else {
        return Err(WireError::Malformed);
    };
    let data_length = match (disagrees, declared, held) {
        (true, declared, held) if held < declared => Some(DataLength { declared, held }),
        (false, 0, 0) => None,
        _ => return Err(WireError::Malformed),
    };
    Ok(SoundInfo {
        format,
        encoding,
        rate,
        channels,
        sample,
        frames,
        seekable,
        data_length,
    })
}

fn tag_kind_to_wire(kind: TagKind) -> (u8, Option<TagKey>) {
    match kind {
        TagKind::Title => (1, None),
        TagKind::Artist => (2, None),
        TagKind::Album => (3, None),
        TagKind::Comment => (4, None),
        TagKind::Date => (5, None),
        TagKind::Genre => (6, None),
        TagKind::Copyright => (7, None),
        TagKind::Software => (8, None),
        TagKind::Track => (9, None),
        TagKind::Other(key) => (10, Some(key)),
    }
}

fn tag_kind_from_wire(reader: &mut Reader<'_>) -> Result<TagKind, WireError> {
    Ok(match reader.u8()? {
        1 => TagKind::Title,
        2 => TagKind::Artist,
        3 => TagKind::Album,
        4 => TagKind::Comment,
        5 => TagKind::Date,
        6 => TagKind::Genre,
        7 => TagKind::Copyright,
        8 => TagKind::Software,
        9 => TagKind::Track,
        10 => {
            let len = usize::from(reader.u8()?);
            TagKind::Other(TagKey::new(reader.take(len)?).ok_or(WireError::Malformed)?)
        }
        _ => return Err(WireError::Malformed),
    })
}

fn encode_metadata(out: &mut Writer, metadata: &Metadata) {
    out.u32(count(metadata.tags.len()));
    for tag in &metadata.tags {
        let (kind, key) = tag_kind_to_wire(tag.kind);
        out.u8(kind);
        if let Some(key) = key {
            out.u8(u8::try_from(key.as_str().len()).unwrap_or(0));
            out.raw(key.as_str().as_bytes());
        }
        out.str(&tag.value);
    }
    out.u32(count(metadata.cues.len()));
    for cue in &metadata.cues {
        out.u32(cue.id);
        out.u64(cue.frame);
    }
    out.u32(count(metadata.loops.len()));
    for sampler_loop in &metadata.loops {
        out.u64(sampler_loop.start);
        out.u64(sampler_loop.end);
        match sampler_loop.kind {
            LoopKind::Forward => out.u8(1),
            LoopKind::Alternating => out.u8(2),
            LoopKind::Backward => out.u8(3),
            LoopKind::Other(kind) => {
                out.u8(4);
                out.u32(kind);
            }
        }
        out.u32(sampler_loop.count);
    }
    out.u8(metadata.unity_note.unwrap_or(NO_NOTE));
    out.u8(u8::from(metadata.omitted));
    if let Some(cover) = metadata.cover {
        out.u8(1);
        out.u64(cover.offset);
        out.u64(cover.len);
    } else {
        out.u8(0);
        out.u64(0);
        out.u64(0);
    }
}

/// A list's length on the wire; every list a reply carries is bounded far
/// below `u32::MAX` by the limits.
fn count(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// A list of `declared` items read by `item`, its room reserved no further
/// than the bytes left could hold, so a hostile count drives no allocation.
fn list<T>(
    reader: &mut Reader<'_>,
    smallest: usize,
    mut item: impl FnMut(&mut Reader<'_>) -> Result<T, WireError>,
) -> Result<Vec<T>, WireError> {
    let declared = reader.u32()? as usize;
    if declared > reader.remaining() / smallest {
        return Err(WireError::Truncated);
    }
    let mut items = Vec::new();
    items
        .try_reserve_exact(declared)
        .map_err(|_| WireError::Malformed)?;
    for _ in 0..declared {
        items.push(item(reader)?);
    }
    Ok(items)
}

fn decode_metadata(reader: &mut Reader<'_>) -> Result<Metadata, WireError> {
    let budget = LIMITS.max_metadata_bytes() as usize;
    let tags = list(reader, 1 + 4, |reader| {
        let kind = tag_kind_from_wire(reader)?;
        let value = reader.string(budget)?;
        Ok(Tag { kind, value })
    })?;
    let cues = list(reader, 4 + 8, |reader| {
        Ok(Cue {
            id: reader.u32()?,
            frame: reader.u64()?,
        })
    })?;
    let loops = list(reader, 8 + 8 + 1 + 4, |reader| {
        let (start, end) = (reader.u64()?, reader.u64()?);
        let kind = match reader.u8()? {
            1 => LoopKind::Forward,
            2 => LoopKind::Alternating,
            3 => LoopKind::Backward,
            4 => LoopKind::Other(reader.u32()?),
            _ => return Err(WireError::Malformed),
        };
        Ok(Loop {
            start,
            end,
            kind,
            count: reader.u32()?,
        })
    })?;
    let unity_note = match reader.u8()? {
        NO_NOTE => None,
        note => Some(note),
    };
    let omitted = flag(reader)?;
    let (present, offset, len) = (flag(reader)?, reader.u64()?, reader.u64()?);
    let cover = match (present, len) {
        (true, 0) => return Err(WireError::Malformed),
        (true, len) => Some(CoverRange { offset, len }),
        (false, _) if offset != 0 || len != 0 => return Err(WireError::Malformed),
        (false, _) => None,
    };
    let metadata = Metadata {
        tags,
        cues,
        loops,
        unity_note,
        omitted,
        cover,
    };
    if metadata.within(&LIMITS) {
        Ok(metadata)
    } else {
        Err(WireError::Malformed)
    }
}

/// What a reply asked of the owner, or told it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DecodeEvent<'a> {
    /// Read the `len` bytes at `offset` and hand them to
    /// [`AudioDecodeClient::supply`], or report the read failed through
    /// [`AudioDecodeClient::unreadable`]. It is file I/O, so an interactive
    /// owner reads off its event loop.
    Need {
        /// Where the bytes start.
        offset: u64,
        /// How many there are.
        len: usize,
    },
    /// The file is open: [`AudioDecodeClient::info`] and
    /// [`AudioDecodeClient::metadata`] describe it.
    Opened,
    /// Interleaved frames from `position`, in the stream's sample format.
    Block {
        /// The first frame's place in the stream.
        position: u64,
        /// The frames.
        pcm: &'a [u8],
    },
    /// The stream has no frames past `position`.
    Ended {
        /// Where the stream ends.
        position: u64,
    },
    /// The next block starts at `position`.
    Sought {
        /// The frame sought.
        position: u64,
    },
    /// The worker refused the request; a file whose open was refused is not
    /// open, and a refused decode or seek left the stream where it was.
    Refused(AudioRefusal),
    /// A replacement worker has the file open again at `position`.
    Resumed {
        /// Where the stream is.
        position: u64,
    },
    /// A step of bringing a replacement back: the next frame to send is
    /// waiting ([`AudioDecodeClient::outgoing`]).
    Resuming,
}

/// Why an [`AudioDecodeClient`] call failed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AudioDecodeError {
    /// Not a call this point admits: a request is outstanding, nothing is
    /// open, or nothing was asked for.
    OutOfTurn,
    /// Bytes of another length than the need asked for.
    WrongLength,
    /// A decode of no frames, or of more than one block carries.
    BlockSize,
    /// The worker's frame broke the protocol or stated the impossible. It
    /// cannot be believed: condemn the worker and
    /// [`restart`](AudioDecodeClient::restart).
    Unbelievable,
    /// A replacement could not be brought back: the file now reads as
    /// another stream, or the worker failed again where it last did.
    CannotResume,
    /// The request could not be built for want of memory.
    OutOfMemory,
}

impl Unbelieved for AudioDecodeError {
    fn unbelieved(&self) -> bool {
        *self == Self::Unbelievable
    }
}

impl core::fmt::Display for AudioDecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::OutOfTurn => "a decode request was made out of turn",
            Self::WrongLength => "the bytes handed to the decoder were not those it asked for",
            Self::BlockSize => "a decode asked for no frames, or more than one block holds",
            Self::Unbelievable => "the decoder's answer could not be believed",
            Self::CannotResume => "the decoder could not be brought back to the stream",
            Self::OutOfMemory => "there is not enough memory to ask the decoder",
        })
    }
}

/// What the stream is and where it has got to.
struct Stream {
    info: SoundInfo,
    metadata: Metadata,
    position: u64,
}

/// What the client is waiting for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Awaiting {
    /// Nothing is open.
    Closed,
    /// Open, and no request outstanding.
    Idle,
    Open,
    Decode {
        frames: u32,
    },
    Seek {
        frame: u64,
    },
    /// A replacement's open of the same file.
    Reopen,
    /// A replacement's seek back to the stream's position.
    Reposition,
    /// A replacement's decode back to the stream's position, of a stream
    /// that cannot seek: `skipped` frames so far, `frames` asked for.
    Skip {
        skipped: u64,
        frames: u32,
    },
}

/// The owner's side of one sandboxed decode.
///
/// Every request is encoded into [`Self::outgoing`] for the owner to send;
/// every frame the worker sends goes through [`Self::on_frame`], which checks
/// it against what was asked before anything in it is used.
pub struct AudioDecodeClient {
    len: u64,
    format: Option<SoundFormat>,
    stream: Option<Stream>,
    awaiting: Awaiting,
    need: Option<(u64, usize)>,
    /// Needs the outstanding request has made. Every answered need holds at
    /// least one more page until the request is answered, so an honest worker
    /// asks at most once a page and once more to be refused.
    needs: usize,
    outgoing: Option<Vec<u8>>,
    /// Where a frame is built, kept from one request to the next.
    spare: Vec<u8>,
    /// Where the stream was when a worker last failed, so a stream that fails
    /// its worker at one place is brought back once.
    failed_at: Option<u64>,
}

impl Default for AudioDecodeClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioDecodeClient {
    /// A client with nothing open.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            len: 0,
            format: None,
            stream: None,
            awaiting: Awaiting::Closed,
            need: None,
            needs: 0,
            outgoing: None,
            spare: Vec::new(),
            failed_at: None,
        }
    }

    /// The stream, once open.
    #[must_use]
    pub fn info(&self) -> Option<&SoundInfo> {
        self.stream.as_ref().map(|stream| &stream.info)
    }

    /// What the file says about its stream, once open.
    #[must_use]
    pub fn metadata(&self) -> Option<&Metadata> {
        self.stream.as_ref().map(|stream| &stream.metadata)
    }

    /// The frame the next block starts at.
    #[must_use]
    pub fn position(&self) -> u64 {
        self.stream.as_ref().map_or(0, |stream| stream.position)
    }

    /// Whether a file is open and no request is outstanding.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.awaiting == Awaiting::Idle && self.outgoing.is_none()
    }

    /// The frame waiting to be sent, until [`Self::sent`].
    #[must_use]
    pub fn outgoing(&self) -> Option<&[u8]> {
        self.outgoing.as_deref()
    }

    /// The waiting frame has been queued to the worker.
    pub fn sent(&mut self) {
        if let Some(frame) = self.outgoing.take() {
            self.spare = frame;
        }
    }

    /// Open a file of `len` bytes, as `format` or in the format its signature
    /// names, the worker's cache keyed by `seed` — a fresh draw from the
    /// owner's random source, so a file cannot choose which of its pages
    /// collide.
    ///
    /// # Errors
    ///
    /// [`AudioDecodeError::OutOfTurn`] while a request is outstanding.
    pub fn open(
        &mut self,
        len: u64,
        format: Option<SoundFormat>,
        seed: HashSeed,
    ) -> Result<(), AudioDecodeError> {
        if !matches!(self.awaiting, Awaiting::Closed | Awaiting::Idle) || self.outgoing.is_some() {
            return Err(AudioDecodeError::OutOfTurn);
        }
        self.len = len;
        self.format = format;
        self.stream = None;
        self.failed_at = None;
        self.send_open(seed)?;
        self.awaiting = Awaiting::Open;
        Ok(())
    }

    /// Ask for the next block, of at most `frames` frames.
    ///
    /// # Errors
    ///
    /// [`AudioDecodeError::OutOfTurn`] unless open and idle, or
    /// [`AudioDecodeError::BlockSize`].
    pub fn decode(&mut self, frames: u32) -> Result<(), AudioDecodeError> {
        if !self.is_idle() {
            return Err(AudioDecodeError::OutOfTurn);
        }
        if frames == 0 || frames > MAX_BLOCK_FRAMES {
            return Err(AudioDecodeError::BlockSize);
        }
        self.send_decode(frames)?;
        self.awaiting = Awaiting::Decode { frames };
        Ok(())
    }

    /// Ask for the next block to start at `frame`.
    ///
    /// # Errors
    ///
    /// [`AudioDecodeError::OutOfTurn`] unless open and idle.
    pub fn seek(&mut self, frame: u64) -> Result<(), AudioDecodeError> {
        if !self.is_idle() {
            return Err(AudioDecodeError::OutOfTurn);
        }
        self.send_seek(frame)?;
        self.awaiting = Awaiting::Seek { frame };
        Ok(())
    }

    /// Hand over the bytes the outstanding need asked for.
    ///
    /// # Errors
    ///
    /// [`AudioDecodeError::OutOfTurn`] with no need outstanding,
    /// [`AudioDecodeError::WrongLength`], or
    /// [`AudioDecodeError::OutOfMemory`].
    pub fn supply(&mut self, bytes: &[u8]) -> Result<(), AudioDecodeError> {
        let (offset, len) = self.need.ok_or(AudioDecodeError::OutOfTurn)?;
        if bytes.len() != len {
            return Err(AudioDecodeError::WrongLength);
        }
        let mut out = self.writer(SUPPLY_HEADER_LEN + len)?;
        out.u8(TO_SUPPLY);
        out.u64(offset);
        out.raw(bytes);
        self.outgoing = Some(out.finish());
        self.need = None;
        Ok(())
    }

    /// Report that the bytes the outstanding need asked for could not be
    /// read; the worker refuses the request it was serving.
    ///
    /// # Errors
    ///
    /// [`AudioDecodeError::OutOfTurn`] with no need outstanding.
    pub fn unreadable(&mut self) -> Result<(), AudioDecodeError> {
        let (offset, _) = self.need.ok_or(AudioDecodeError::OutOfTurn)?;
        let mut out = self.writer(UNREADABLE_LEN)?;
        out.u8(TO_UNREADABLE);
        out.u64(offset);
        self.outgoing = Some(out.finish());
        self.need = None;
        Ok(())
    }

    /// A new worker has replaced the one that failed: open the file in it
    /// again, `seed` a fresh draw, and bring it back to where the stream was.
    /// Whatever was outstanding is gone; once [`DecodeEvent::Resumed`]
    /// arrives the owner asks again.
    ///
    /// # Errors
    ///
    /// [`AudioDecodeError::CannotResume`] when the stream failed a worker at
    /// this place before: the file is closed.
    pub fn restart(&mut self, seed: HashSeed) -> Result<(), AudioDecodeError> {
        self.need = None;
        if let Some(frame) = self.outgoing.take() {
            self.spare = frame;
        }
        if self.awaiting == Awaiting::Closed {
            return Ok(());
        }
        let at = self.position();
        if self.failed_at == Some(at) {
            self.close();
            return Err(AudioDecodeError::CannotResume);
        }
        self.failed_at = Some(at);
        self.send_open(seed)?;
        self.awaiting = if self.stream.is_some() {
            Awaiting::Reopen
        } else {
            Awaiting::Open
        };
        Ok(())
    }

    /// Read one frame from the worker.
    ///
    /// # Errors
    ///
    /// [`AudioDecodeError::Unbelievable`] for a frame that is not an honest
    /// answer to what was asked, and [`AudioDecodeError::CannotResume`] when
    /// a replacement finds a different stream.
    pub fn on_frame<'a>(&mut self, frame: &'a [u8]) -> Result<DecodeEvent<'a>, AudioDecodeError> {
        if self.need.is_some()
            || self.outgoing.is_some()
            || matches!(self.awaiting, Awaiting::Closed | Awaiting::Idle)
        {
            return Err(AudioDecodeError::Unbelievable);
        }
        let mut reader = Reader::new(frame);
        match reader.u8().map_err(|_| AudioDecodeError::Unbelievable)? {
            FROM_NEED if frame.len() == NEED_LEN => self.on_need(&mut reader),
            FROM_OPENED => self.on_opened(&mut reader),
            FROM_BLOCK if frame.len() >= BLOCK_HEADER_LEN => {
                self.on_block(&mut reader, &frame[BLOCK_HEADER_LEN..])
            }
            FROM_SOUGHT if frame.len() == SOUGHT_LEN => self.on_sought(&mut reader),
            FROM_REFUSED if frame.len() == REFUSED_LEN => self.on_refused(&mut reader),
            _ => Err(AudioDecodeError::Unbelievable),
        }
    }

    fn on_need<'a>(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<DecodeEvent<'a>, AudioDecodeError> {
        let (offset, len) = whole(reader, |reader| Ok((reader.u64()?, reader.u32()? as usize)))?;
        let end = offset.checked_add(len as u64);
        let whole = len % PAGE_BYTES == 0 || end == Some(self.len);
        self.needs += 1;
        let honest = offset % PAGE == 0
            && len > 0
            && len <= MAX_NEED_BYTES
            && whole
            && end.is_some_and(|end| end <= self.len)
            && self.needs <= CACHE_PAGES + 1;
        if !honest {
            return Err(AudioDecodeError::Unbelievable);
        }
        self.need = Some((offset, len));
        Ok(DecodeEvent::Need { offset, len })
    }

    fn on_opened<'a>(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<DecodeEvent<'a>, AudioDecodeError> {
        let (info, metadata) = whole(reader, |reader| {
            Ok((decode_info(reader)?, decode_metadata(reader)?))
        })?;
        let honest = self.format.is_none_or(|format| format == info.format)
            && info.channels.channels() <= LIMITS.max_channels()
            && info
                .data_length
                .is_none_or(|length| length.held <= self.len)
            && metadata.cover.is_none_or(|cover| {
                cover
                    .offset
                    .checked_add(cover.len)
                    .is_some_and(|end| end <= self.len)
            });
        if !honest {
            return Err(AudioDecodeError::Unbelievable);
        }
        match self.awaiting {
            Awaiting::Open => {
                self.stream = Some(Stream {
                    info,
                    metadata,
                    position: 0,
                });
                self.awaiting = Awaiting::Idle;
                Ok(DecodeEvent::Opened)
            }
            Awaiting::Reopen => {
                let Some(stream) = self.stream.as_ref() else {
                    return Err(AudioDecodeError::Unbelievable);
                };
                if stream.info != info || stream.metadata != metadata {
                    self.close();
                    return Err(AudioDecodeError::CannotResume);
                }
                let target = stream.position;
                if target == 0 {
                    return Ok(self.resumed());
                }
                if info.seekable {
                    self.send_seek(target)?;
                    self.awaiting = Awaiting::Reposition;
                } else {
                    self.skip(0)?;
                }
                Ok(DecodeEvent::Resuming)
            }
            _ => Err(AudioDecodeError::Unbelievable),
        }
    }

    fn on_block<'a>(
        &mut self,
        reader: &mut Reader<'_>,
        pcm: &'a [u8],
    ) -> Result<DecodeEvent<'a>, AudioDecodeError> {
        let (position, frames) = leading(reader, |reader| Ok((reader.u64()?, reader.u32()?)))?;
        let Some(stream) = self.stream.as_mut() else {
            return Err(AudioDecodeError::Unbelievable);
        };
        let (expected, asked) = match self.awaiting {
            Awaiting::Decode { frames } => (stream.position, frames),
            Awaiting::Skip { skipped, frames } => (skipped, frames),
            _ => return Err(AudioDecodeError::Unbelievable),
        };
        let end = position.checked_add(u64::from(frames));
        let honest = position == expected
            && frames <= asked
            && pcm.len() == frames as usize * stream.info.frame_bytes()
            && end.is_some_and(|end| stream.info.frames.is_none_or(|stated| end <= stated));
        let Some(end) = end.filter(|_| honest) else {
            return Err(AudioDecodeError::Unbelievable);
        };
        if let Awaiting::Skip { .. } = self.awaiting {
            let target = stream.position;
            if frames == 0 || end > target {
                self.close();
                return Err(AudioDecodeError::CannotResume);
            }
            if end == target {
                return Ok(self.resumed());
            }
            self.skip(end)?;
            return Ok(DecodeEvent::Resuming);
        }
        stream.position = end;
        self.awaiting = Awaiting::Idle;
        Ok(if frames == 0 {
            DecodeEvent::Ended { position }
        } else {
            DecodeEvent::Block { position, pcm }
        })
    }

    fn on_sought<'a>(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<DecodeEvent<'a>, AudioDecodeError> {
        let position = whole(reader, Reader::u64)?;
        let Some(stream) = self.stream.as_mut() else {
            return Err(AudioDecodeError::Unbelievable);
        };
        let target = match self.awaiting {
            Awaiting::Seek { frame } => frame,
            Awaiting::Reposition => stream.position,
            _ => return Err(AudioDecodeError::Unbelievable),
        };
        let honest = position == target
            && stream.info.seekable
            && stream.info.frames.is_none_or(|stated| position <= stated);
        if !honest {
            return Err(AudioDecodeError::Unbelievable);
        }
        if self.awaiting == Awaiting::Reposition {
            return Ok(self.resumed());
        }
        stream.position = position;
        self.awaiting = Awaiting::Idle;
        Ok(DecodeEvent::Sought { position })
    }

    fn on_refused<'a>(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<DecodeEvent<'a>, AudioDecodeError> {
        let refusal = whole(reader, |reader| {
            AudioRefusal::from_wire(reader.u8()?, reader.u32()?).ok_or(WireError::Malformed)
        })?;
        // The client never makes a request an honest worker refuses so, and a
        // missing page is asked for rather than refused.
        if matches!(
            refusal,
            AudioRefusal::NotOpen
                | AudioRefusal::MalformedRequest
                | AudioRefusal::Decode(DecodeError::InputUnavailable)
        ) {
            return Err(AudioDecodeError::Unbelievable);
        }
        match self.awaiting {
            Awaiting::Open => {
                self.close();
                Ok(DecodeEvent::Refused(refusal))
            }
            Awaiting::Decode { .. } | Awaiting::Seek { .. } => {
                self.awaiting = Awaiting::Idle;
                Ok(DecodeEvent::Refused(refusal))
            }
            _ => {
                self.close();
                Err(AudioDecodeError::CannotResume)
            }
        }
    }

    /// The replacement is back where the stream was.
    fn resumed<'a>(&mut self) -> DecodeEvent<'a> {
        self.awaiting = Awaiting::Idle;
        DecodeEvent::Resumed {
            position: self.position(),
        }
    }

    /// Ask a replacement that cannot seek for the frames from `skipped` on.
    fn skip(&mut self, skipped: u64) -> Result<(), AudioDecodeError> {
        let left = self.position().saturating_sub(skipped);
        let frames =
            u32::try_from(left.min(u64::from(MAX_BLOCK_FRAMES))).unwrap_or(MAX_BLOCK_FRAMES);
        self.send_decode(frames)?;
        self.awaiting = Awaiting::Skip { skipped, frames };
        Ok(())
    }

    fn close(&mut self) {
        self.stream = None;
        self.need = None;
        self.awaiting = Awaiting::Closed;
    }

    /// Queue `frame` as a new request.
    fn request(&mut self, frame: Writer) {
        self.outgoing = Some(frame.finish());
        self.needs = 0;
    }

    fn writer(&mut self, len: usize) -> Result<Writer, AudioDecodeError> {
        let mut buffer = core::mem::take(&mut self.spare);
        buffer.clear();
        buffer
            .try_reserve(len)
            .map_err(|_| AudioDecodeError::OutOfMemory)?;
        Ok(Writer::reusing(buffer))
    }

    fn send_open(&mut self, seed: HashSeed) -> Result<(), AudioDecodeError> {
        let mut out = self.writer(OPEN_LEN)?;
        out.u8(TO_OPEN);
        let (k0, k1) = seed.words();
        out.u64(k0);
        out.u64(k1);
        out.u64(self.len);
        out.u8(self.format.map_or(0, format_to_wire));
        self.request(out);
        Ok(())
    }

    fn send_decode(&mut self, frames: u32) -> Result<(), AudioDecodeError> {
        let mut out = self.writer(DECODE_LEN)?;
        out.u8(TO_DECODE);
        out.u32(frames);
        self.request(out);
        Ok(())
    }

    fn send_seek(&mut self, frame: u64) -> Result<(), AudioDecodeError> {
        let mut out = self.writer(SEEK_LEN)?;
        out.u8(TO_SEEK);
        out.u64(frame);
        self.request(out);
        Ok(())
    }
}

/// Read a whole reply's fields with `fields`, refusing one with bytes left
/// over.
fn whole<'r, T>(
    reader: &mut Reader<'r>,
    fields: impl FnOnce(&mut Reader<'r>) -> Result<T, WireError>,
) -> Result<T, AudioDecodeError> {
    let value = fields(reader).map_err(|_| AudioDecodeError::Unbelievable)?;
    if reader.is_exhausted() {
        Ok(value)
    } else {
        Err(AudioDecodeError::Unbelievable)
    }
}

/// Read a reply's leading fields with `fields`; what follows is its payload.
fn leading<'r, T>(
    reader: &mut Reader<'r>,
    fields: impl FnOnce(&mut Reader<'r>) -> Result<T, WireError>,
) -> Result<T, AudioDecodeError> {
    fields(reader).map_err(|_| AudioDecodeError::Unbelievable)
}

#[cfg(test)]
#[path = "audiodecode_tests.rs"]
mod tests;
