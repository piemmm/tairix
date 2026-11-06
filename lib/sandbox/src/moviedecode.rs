//! Movies decoded in the sandbox (`tairix-video`, `tairix-sound`): a `WebM`
//! recording's VP8 pictures and Opus sound.
//!
//! The owner hands the worker a file's length, never the file. The worker reads
//! it through the shared page cache ([`crate::paged`]) as a sound decode does,
//! asking for the bytes its demuxer and decoders read ([`MovieEvent::Need`]),
//! and nothing it answers is believed past the checks
//! [`MovieDecodeClient::on_frame`] makes: it has read hostile bytes.
//!
//! [`MovieDecodeService`] is the worker's [`SessionService`];
//! [`MovieDecodeClient`] is the owner's side, run over a
//! [`crate::supervise::SupervisedSession`], so a worker that fails is replaced
//! and [`MovieDecodeClient::restart`] brings the replacement back to the latest
//! keyframe the stream passed.
//!
//! # The ring
//!
//! Pictures cross in shared memory, never the pipes. The owner hands its worker
//! a region at spawn: a ring of [`RING_SLOTS`] slots, each the size of the
//! largest picture the owner shows, its screen's ([`RingLayout`]), so the ring
//! is fixed before any stream is read. The worker draws each picture into a
//! free slot at the stream's own size, scaled down only where it is larger than
//! a slot, and answers a head naming the slot, the size and the time.
//!
//! The slot is then the owner's until it hands it back, which it does in its
//! next `next`: each states the slots the owner still holds, and the worker
//! draws only into another. A release so needs no frame of its own, cannot
//! cross the request it governs, and leaves a replacement, handed a ring of
//! its own, holding nothing. The owner copies a picture out before anything
//! reads it, since the worker could write the ring at any time.
//!
//! # The protocol
//!
//! One request at a time — open, next, sound or seek — then any number of need
//! and supply exchanges, then that request's answer, each handed over through
//! [`send_waiting`](crate::supervise::send_waiting).
//!
//! The pictures and the sound are walked through the file apart, each from its
//! own place, so the sound is decoded far ahead of the few pictures the ring
//! holds. `next` answers the pictures' next unit: a picture drawn into a slot,
//! a picture decoded and not drawn because it was due before the instant the
//! request names, or the end. `sound` answers the sound's next block, or the
//! end. `seek` moves both walks to the latest keyframe the file places at or
//! before a target, walking the file where it holds no index; until a keyframe
//! no picture is decoded, since those after a seek predict from frames it left
//! behind.
//!
//! # Memory
//!
//! The owner states the worker's memory ceiling in its open, and the worker
//! weighs what decoding the stream holds ([`movie_bytes`], the ring among it)
//! against it before it reserves any, refusing a stream too large as
//! [`MovieRefusal::TooLarge`]; a keyframe that changes the size is weighed
//! again. The ring is mapped in the owner's address space too.

use alloc::vec::Vec;
use core::cmp::Ordering;

use tairix_abi::driver::audio::{ChannelMap, Rate, CHANNEL_MAP_WIRE_LEN, MAX_CHANNELS};
use tairix_hash::HashSeed;
use tairix_raster::{resample_window_in, Region, ResampleScratch, Rgba8Image};
use tairix_sound::{DecodeError, DecodeLimits, InputError, OpusPackets, SoundInput, MAX_READ};
use tairix_util::fallible;
use tairix_video::vp8::{self, stream_peak_bytes, Decoded, Decoder, Vp8Error};
pub use tairix_video::webm::Ending;
use tairix_video::webm::{Codec, DemuxError, Demuxer, Frame, Media, Track, MAGIC, MAX_FRAME_BYTES};

use crate::audiodecode::{decode_from_wire, decode_to_wire, flag};
use crate::host::Unbelieved;
use crate::memory::WORKER_BASELINE_BYTES;
use crate::paged::{
    shaped_need, Need, Pages, Shortfall, Unasked, MAX_NEED_BYTES, MAX_NEED_PAGES, PAGE, PAGE_BYTES,
};
use crate::proto::{ProtoError, FRAME_HEADER_LEN};
use crate::session::{
    FrameOut, SessionBounds, SessionError, SessionService, SessionStep, ShareWrite,
};
use crate::supervise::Requests;
use crate::wire::{Reader, WireError, Writer};

/// Pages the worker holds: a whole need, and the demuxer's largest read and a
/// payload's chunk beside it.
pub const CACHE_PAGES: usize = MAX_NEED_PAGES + 2 * (MAX_READ / PAGE_BYTES + 1);

/// Slots in a ring: the one the window is taking a picture from, and two
/// decoded ahead of it.
pub const RING_SLOTS: usize = 3;

/// The widest and the tallest picture a slot is sized for, past any screen.
pub const MAX_RING_SIDE: u32 = 1 << 14;

/// Every slot of the ring, as a mask.
const ALL_SLOTS: u8 = (1 << RING_SLOTS) - 1;

/// Bytes of a file's start [`is_movie`] reads.
pub const SIGNATURE_LEN: usize = MAGIC.len();

/// Whether a file beginning `head` is one this decode reads: an EBML
/// document, as every `WebM` recording is. Whether it is a `WebM` one, the
/// open finds.
#[must_use]
pub fn is_movie(head: &[u8]) -> bool {
    head.starts_with(&MAGIC)
}

/// Most frames one block of sound carries: the most one packet decodes to.
pub const MAX_SOUND_FRAMES: usize = OpusPackets::MAX_SAMPLES;

/// The rate a movie's sound plays at: the rate Opus decodes at.
pub const SOUND_RATE: Rate = OpusPackets::RATE;

/// Bytes of the largest frame the owner sends: a supply of the largest need.
pub const MAX_TO_WORKER: usize = SUPPLY_HEADER_LEN + MAX_NEED_BYTES;

/// Bytes of the largest frame the worker sends: a block of sound.
pub const MAX_FROM_WORKER: usize = SOUND_HEADER_LEN + MAX_SOUND_BYTES;

/// The bound an owner admits a session under: one largest frame queued each
/// way.
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
    reason = "a channel map names at most eight channels"
)]
const SOUND_CHANNELS: u8 = MAX_CHANNELS as u8;

/// What a movie's sound may make the worker keep: its channels, and no tags.
const SOUND_LIMITS: DecodeLimits = DecodeLimits::new(SOUND_CHANNELS, 0, 0);

/// The widest block of sound: every channel of the largest packet, in
/// four-byte samples.
const MAX_SOUND_BYTES: usize = MAX_SOUND_FRAMES * MAX_CHANNELS * 4;

const _: () = assert!(MAX_NEED_PAGES <= CACHE_PAGES);

const NS_PER_SECOND: u128 = 1_000_000_000;

const TO_OPEN: u8 = 1;
const TO_NEXT: u8 = 2;
const TO_SOUND: u8 = 3;
const TO_SEEK: u8 = 4;
const TO_SUPPLY: u8 = 5;
const TO_UNREADABLE: u8 = 6;

const FROM_NEED: u8 = 1;
const FROM_OPENED: u8 = 2;
const FROM_PICTURE: u8 = 3;
const FROM_SKIPPED: u8 = 4;
const FROM_SOUND: u8 = 5;
const FROM_ENDED: u8 = 6;
const FROM_SOUGHT: u8 = 7;
const FROM_REFUSED: u8 = 8;

const OPEN_LEN: usize = 1 + HashSeed::LEN + 8 + 8 + 4 + 4;
const NEXT_LEN: usize = 1 + 8 + 1;
const SOUND_REQUEST_LEN: usize = 1;
const SEEK_LEN: usize = 1 + 8;
const SUPPLY_HEADER_LEN: usize = 1 + 8;
const UNREADABLE_LEN: usize = 1 + 8;

const NEED_LEN: usize = 1 + 8 + 4;
const OPENED_LEN: usize = 1 + 1 + 8 + 1 + 4 + 4 + 1 + CHANNEL_MAP_WIRE_LEN;
const PICTURE_LEN: usize = 1 + 8 + 1 + 1 + 4 + 4;
const SKIPPED_LEN: usize = 1 + 8 + 1;
const SOUND_HEADER_LEN: usize = 1 + 8 + 4;
const ENDED_LEN: usize = 1 + 1;
const SOUGHT_LEN: usize = 1;
const REFUSED_LEN: usize = 1 + 1 + 1 + 4;

const ENDED_COMPLETE: u8 = 1;
const ENDED_TRUNCATED: u8 = 2;

const REFUSED_DEMUX: u8 = 1;
const REFUSED_PICTURE: u8 = 2;
const REFUSED_SOUND: u8 = 3;
const REFUSED_NO_PICTURE: u8 = 4;
const REFUSED_TOO_LARGE: u8 = 5;
const REFUSED_OUT_OF_ORDER: u8 = 6;
const REFUSED_WORKING_SET: u8 = 7;
const REFUSED_OUT_OF_MEMORY: u8 = 8;
const REFUSED_UNDRAWABLE: u8 = 9;
const REFUSED_NOT_OPEN: u8 = 10;
const REFUSED_MALFORMED: u8 = 11;
const REFUSED_NO_RING: u8 = 12;

/// Why the worker refused a request.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MovieRefusal {
    /// The container refused the file, or the part of it the request read.
    Demux(DemuxError),
    /// A picture of the stream would not decode.
    Picture(Vp8Error),
    /// The sound's header, or a packet of it, would not decode.
    Sound(DecodeError),
    /// The file holds no VP8 picture to show.
    NoPicture,
    /// Decoding the stream holds more memory than the worker may map.
    TooLarge,
    /// A frame shown before the one ahead of it on its track.
    OutOfOrder,
    /// The request needs more of the file at once than the worker holds.
    WorkingSetExceeded,
    /// The worker's own buffers could not be had.
    OutOfMemory,
    /// A picture larger than a slot could not be scaled into one.
    Undrawable,
    /// A request with no movie open.
    NotOpen,
    /// A frame the protocol does not admit at that point. Whatever was open is
    /// closed.
    MalformedRequest,
    /// The worker holds no ring the size the open states.
    NoRing,
}

impl MovieRefusal {
    fn to_wire(self) -> (u8, u8, u32) {
        match self {
            Self::Demux(err) => (REFUSED_DEMUX, demux_to_wire(err), 0),
            Self::Picture(err) => (REFUSED_PICTURE, picture_to_wire(err), 0),
            Self::Sound(err) => {
                let (code, detail) = decode_to_wire(err);
                (REFUSED_SOUND, code, detail)
            }
            Self::NoPicture => (REFUSED_NO_PICTURE, 0, 0),
            Self::TooLarge => (REFUSED_TOO_LARGE, 0, 0),
            Self::OutOfOrder => (REFUSED_OUT_OF_ORDER, 0, 0),
            Self::WorkingSetExceeded => (REFUSED_WORKING_SET, 0, 0),
            Self::OutOfMemory => (REFUSED_OUT_OF_MEMORY, 0, 0),
            Self::Undrawable => (REFUSED_UNDRAWABLE, 0, 0),
            Self::NotOpen => (REFUSED_NOT_OPEN, 0, 0),
            Self::MalformedRequest => (REFUSED_MALFORMED, 0, 0),
            Self::NoRing => (REFUSED_NO_RING, 0, 0),
        }
    }

    fn from_wire(kind: u8, code: u8, detail: u32) -> Option<Self> {
        let bare = match kind {
            REFUSED_DEMUX => {
                return demux_from_wire(code)
                    .filter(|_| detail == 0)
                    .map(Self::Demux)
            }
            REFUSED_PICTURE => {
                return picture_from_wire(code)
                    .filter(|_| detail == 0)
                    .map(Self::Picture)
            }
            REFUSED_SOUND => return decode_from_wire(code, detail).map(Self::Sound),
            REFUSED_NO_PICTURE => Self::NoPicture,
            REFUSED_TOO_LARGE => Self::TooLarge,
            REFUSED_OUT_OF_ORDER => Self::OutOfOrder,
            REFUSED_WORKING_SET => Self::WorkingSetExceeded,
            REFUSED_OUT_OF_MEMORY => Self::OutOfMemory,
            REFUSED_UNDRAWABLE => Self::Undrawable,
            REFUSED_NOT_OPEN => Self::NotOpen,
            REFUSED_MALFORMED => Self::MalformedRequest,
            REFUSED_NO_RING => Self::NoRing,
            _ => return None,
        };
        (code == 0 && detail == 0).then_some(bare)
    }
}

impl core::fmt::Display for MovieRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Demux(err) => write!(f, "{err}"),
            Self::Picture(err) => f.write_str(picture_reason(*err)),
            Self::Sound(err) => write!(f, "the sound would not decode: {err}"),
            Self::NoPicture => f.write_str("the file holds no picture to show"),
            Self::TooLarge => {
                f.write_str("the movie needs more memory to decode than the decoder may use")
            }
            Self::OutOfOrder => f.write_str("a frame is shown before the one ahead of it"),
            Self::WorkingSetExceeded => {
                f.write_str("the file needs more of itself at once than the decoder holds")
            }
            Self::OutOfMemory => f.write_str("the decoder ran out of memory"),
            Self::Undrawable => {
                f.write_str("a picture could not be scaled to fit the decoder's ring")
            }
            Self::NotOpen => f.write_str("no movie is open"),
            Self::MalformedRequest => f.write_str("malformed movie request"),
            Self::NoRing => f.write_str("the decoder holds no ring of the size asked for"),
        }
    }
}

/// What a picture's refusal tells the user.
const fn picture_reason(err: Vp8Error) -> &'static str {
    match err {
        Vp8Error::Truncated => "a picture ends before what it declares",
        Vp8Error::Interframe => "a picture predicts from one the stream never had",
        Vp8Error::UnsupportedProfile => "a picture is of a VP8 version that does not exist",
        Vp8Error::BadStartCode => "a keyframe lacks its start code",
        Vp8Error::InvalidGeometry => "a picture declares no size",
        Vp8Error::ReservedColourSpace => "a picture names a reserved colour space",
        Vp8Error::ReservedBufferCopy => "a picture copies a reference the format reserves",
        Vp8Error::InvalidPartitions => "a picture's partitions do not fit it",
        Vp8Error::DimensionsOverflow => "a picture's size overflows",
        Vp8Error::OutOfMemory => "the decoder ran out of memory for a picture",
    }
}

/// One table for both directions of a refusal's code, so a code cannot mean
/// one thing to the worker and another to the owner.
macro_rules! codes {
    ($kind:ident, $to:ident, $from:ident { $($code:literal => $variant:ident,)+ }) => {
        const fn $to(value: $kind) -> u8 {
            match value {
                $($kind::$variant => $code,)+
            }
        }

        const fn $from(code: u8) -> Option<$kind> {
            match code {
                $($code => Some($kind::$variant),)+
                _ => None,
            }
        }
    };
}

codes!(DemuxError, demux_to_wire, demux_from_wire {
    1 => InputUnavailable,
    2 => InputFailed,
    3 => OutOfMemory,
    4 => NotWebm,
    5 => UnsupportedVersion,
    6 => Truncated,
    7 => BadId,
    8 => BadSize,
    9 => UnknownSize,
    10 => Overrun,
    11 => TooLarge,
    12 => TooMany,
    13 => BadValue,
    14 => Missing,
    15 => Duplicated,
    16 => Misplaced,
    17 => Untimed,
    18 => BadTrack,
    19 => UndeclaredTrack,
    20 => BadLacing,
    21 => TimestampOutOfRange,
    22 => BadIndex,
});

codes!(Vp8Error, picture_to_wire, picture_from_wire {
    1 => Truncated,
    2 => Interframe,
    3 => UnsupportedProfile,
    4 => BadStartCode,
    5 => InvalidGeometry,
    6 => ReservedColourSpace,
    7 => ReservedBufferCopy,
    8 => InvalidPartitions,
    9 => DimensionsOverflow,
    10 => OutOfMemory,
});

/// A ring's slots: [`RING_SLOTS`] of them, each the straight-alpha RGBA8
/// picture of a box, the largest picture its owner shows.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RingLayout {
    width: u32,
    height: u32,
}

impl RingLayout {
    /// Slots for pictures up to `width`×`height`, or `None` for an empty box,
    /// one past [`MAX_RING_SIDE`], or a ring this target cannot address.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Option<Self> {
        let sides = 1..=MAX_RING_SIDE;
        let ring = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4 * RING_SLOTS));
        (sides.contains(&width) && sides.contains(&height) && ring.is_some())
            .then_some(Self { width, height })
    }

    /// The largest picture a slot holds.
    #[must_use]
    pub const fn size(self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Bytes one slot holds.
    #[must_use]
    pub const fn slot_bytes(self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    /// Bytes the whole ring holds.
    #[must_use]
    pub const fn ring_bytes(self) -> usize {
        self.slot_bytes() * RING_SLOTS
    }

    /// Where slot `slot` starts in the ring.
    #[must_use]
    pub const fn slot_at(self, slot: u8) -> usize {
        slot as usize * self.slot_bytes()
    }

    /// The size a `width`×`height` picture is drawn at in a slot: its own, or
    /// scaled down, keeping its aspect, to the largest that fits.
    #[must_use]
    pub fn fit(self, width: u32, height: u32) -> (u32, u32) {
        if width <= self.width && height <= self.height {
            return (width, height);
        }
        let (width, height) = (u64::from(width.max(1)), u64::from(height.max(1)));
        let (most_w, most_h) = (u64::from(self.width), u64::from(self.height));
        // The tighter of the two ratios sets the scale.
        let (fitted_w, fitted_h) = if most_w * height <= most_h * width {
            (most_w, (height * most_w / width).max(1))
        } else {
            ((width * most_h / height).max(1), most_h)
        };
        (
            u32::try_from(fitted_w).unwrap_or(self.width),
            u32::try_from(fitted_h).unwrap_or(self.height),
        )
    }
}

/// What an open movie is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MovieInfo {
    /// How long it is, where the file states it: a recording its writer never
    /// finished states none.
    pub duration_ns: Option<u64>,
    /// Whether a seek is served from the file's own index rather than by
    /// walking it.
    pub indexed: bool,
    /// Its pictures' width, as the file declares it.
    pub width: u32,
    /// Its pictures' height.
    pub height: u32,
    /// Its sound's channels, at [`OpusPackets::RATE`], when it has sound.
    pub sound: Option<ChannelMap>,
}

/// A picture's head: when it is shown, whether it is a keyframe, and the slot
/// it is drawn in, at what size.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PictureHead {
    /// When the picture is shown, in nanoseconds of the movie.
    pub at_ns: u64,
    /// Whether it decodes without the pictures before it.
    pub keyframe: bool,
    /// The slot it is drawn in, the owner's until it hands it back.
    pub slot: u8,
    /// Its width as drawn: the stream's own, or scaled down to fit a slot.
    pub width: u32,
    /// Its height as drawn.
    pub height: u32,
}

impl PictureHead {
    /// The bytes its straight-alpha RGBA8 pixels take in its slot.
    #[must_use]
    pub const fn pixel_len(self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

/// What the worker is asked to do that may need the file's bytes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Request {
    Open,
    Next { skip_before_ns: u64, free: u8 },
    Sound,
    Seek { target_ns: u64 },
}

/// How an attempt at a request stopped short of its answer.
enum Fault {
    /// It met bytes the cache does not hold.
    Unavailable,
    /// It was refused.
    Refused(MovieRefusal),
}

impl From<MovieRefusal> for Fault {
    fn from(refusal: MovieRefusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<DemuxError> for Fault {
    fn from(err: DemuxError) -> Self {
        match err {
            DemuxError::InputUnavailable => Self::Unavailable,
            err => Self::Refused(MovieRefusal::Demux(err)),
        }
    }
}

impl From<InputError> for Fault {
    fn from(err: InputError) -> Self {
        match err {
            InputError::Unavailable => Self::Unavailable,
            InputError::Failed => Self::Refused(MovieRefusal::Demux(DemuxError::InputFailed)),
        }
    }
}

impl From<Vp8Error> for Fault {
    fn from(err: Vp8Error) -> Self {
        Self::Refused(MovieRefusal::Picture(err))
    }
}

impl From<DecodeError> for Fault {
    fn from(err: DecodeError) -> Self {
        Self::Refused(MovieRefusal::Sound(err))
    }
}

/// The picture stream.
struct Video {
    track: u64,
    /// The size its latest keyframe declares.
    size: (u32, u32),
    decoder: Decoder,
    /// When the latest picture decoded is shown.
    last_ns: Option<u64>,
    /// A picture larger than a slot, drawn here at its own size before it is
    /// scaled into one.
    staging: Vec<u8>,
    scratch: ResampleScratch,
}

/// The sound stream.
struct Sound {
    track: u64,
    packets: OpusPackets,
    channels: usize,
    /// What the container's codec delay discards from the decode's start.
    delay_ns: u64,
    pcm: Vec<f32>,
    /// Where a block's answer is built.
    block: Vec<u8>,
    last_ns: Option<u64>,
}

impl Sound {
    /// The sound `track` carries, its buffers reserved for its widest packet.
    fn open(track: &Track) -> Result<Self, Fault> {
        let packets = OpusPackets::new(&track.codec_private, &SOUND_LIMITS)?;
        let channels = usize::from(packets.channels().channels());
        let samples = MAX_SOUND_FRAMES * channels;
        let pcm = fallible::filled(samples, 0.0f32).ok_or(MovieRefusal::OutOfMemory)?;
        let mut block = Vec::new();
        if !fallible::reserve(&mut block, SOUND_HEADER_LEN + samples * 4) {
            return Err(MovieRefusal::OutOfMemory.into());
        }
        Ok(Self {
            track: track.number,
            packets,
            channels,
            delay_ns: track.codec_delay_ns,
            pcm,
            block,
            last_ns: None,
        })
    }
}

/// Where a walk through the file is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Step {
    /// Finding the next frame.
    Locate,
    /// Reading `frame`'s payload, `filled` bytes of it held.
    Read { frame: Frame, filled: usize },
}

/// What a walk met next.
enum Met {
    /// A frame it takes, its payload read.
    Frame(Frame),
    /// The end of the file's frames, and how it ended.
    End(Ending),
}

/// One stream's walk through the file's frames, from a place of its own.
struct Walk {
    demuxer: Demuxer,
    step: Step,
    /// A frame's payload, as far as it has been read.
    payload: Vec<u8>,
}

impl Walk {
    fn open(pages: &mut Pages) -> Result<Self, Fault> {
        Ok(Self {
            demuxer: Demuxer::open(pages)?,
            step: Step::Locate,
            payload: Vec::new(),
        })
    }

    /// The next frame `wants` takes, its payload read into [`Self::payload`],
    /// or the end.
    fn next(&mut self, pages: &mut Pages, wants: impl Fn(&Frame) -> bool) -> Result<Met, Fault> {
        loop {
            match self.step {
                Step::Locate => {
                    let Some(frame) = self.demuxer.next_frame(pages)? else {
                        return Ok(Met::End(self.demuxer.ending().unwrap_or(Ending::Complete)));
                    };
                    // A frame placed is kept, so the pages it read may go.
                    pages.next_request();
                    if wants(&frame) {
                        self.step = Step::Read { frame, filled: 0 };
                    }
                }
                Step::Read { frame, filled } => {
                    self.read_payload(pages, frame, filled)?;
                    self.step = Step::Locate;
                    return Ok(Met::Frame(frame));
                }
            }
        }
    }

    /// Read `frame`'s payload from `filled` on, a chunk at a time, each chunk
    /// kept as it lands so a retry resumes where the last read missed.
    fn read_payload(
        &mut self,
        pages: &mut Pages,
        frame: Frame,
        mut filled: usize,
    ) -> Result<(), Fault> {
        let len = frame.len as usize;
        if filled == 0 {
            self.payload.clear();
            if !fallible::grow_to(&mut self.payload, len, 0) {
                return Err(MovieRefusal::OutOfMemory.into());
            }
        }
        while filled < len {
            let chunk = (len - filled).min(MAX_READ);
            let target = self
                .payload
                .get_mut(filled..filled + chunk)
                .ok_or(MovieRefusal::OutOfMemory)?;
            let read = pages.read_at(frame.offset + filled as u64, target)?;
            if read != chunk {
                return Err(MovieRefusal::Demux(DemuxError::InputFailed).into());
            }
            filled += chunk;
            self.step = Step::Read { frame, filled };
            pages.next_request();
        }
        Ok(())
    }

    /// Move to the latest keyframe the file places at or before `target_ns`.
    fn seek(&mut self, pages: &mut Pages, target_ns: u64) -> Result<(), Fault> {
        // An index found wanting is dropped, so asking again walks the file.
        let mut landed = self.demuxer.seek(pages, target_ns);
        if landed == Err(DemuxError::BadIndex) {
            landed = self.demuxer.seek(pages, target_ns);
        }
        landed?;
        self.step = Step::Locate;
        Ok(())
    }
}

/// The movie the worker has open.
struct OpenMovie {
    pages: Pages,
    /// The most memory the worker may map, as its owner stated it.
    ceiling: u64,
    layout: RingLayout,
    video: Option<Video>,
    sound: Option<Sound>,
    /// The pictures' walk, and the sound's.
    pictures: Option<Walk>,
    sounds: Option<Walk>,
    /// Whether a keyframe has decoded since the open or the last seek.
    keyed: bool,
}

impl OpenMovie {
    fn open(&mut self) -> Result<Answer, Fault> {
        let pictures = Walk::open(&mut self.pages)?;
        let tracks = pictures.demuxer.tracks();
        let video = picture_track(tracks).ok_or(MovieRefusal::NoPicture)?;
        let sound = sound_track(tracks).map(Sound::open).transpose()?;
        let channels = sound.as_ref().map_or(0, |sound| sound.channels);
        if !fits(video.size, channels, self.layout, self.ceiling) {
            return Err(MovieRefusal::TooLarge.into());
        }
        let sounds = match sound {
            Some(_) => Some(Walk::open(&mut self.pages)?),
            None => None,
        };
        self.pictures = Some(pictures);
        self.sounds = sounds;
        self.video = Some(video);
        self.sound = sound;
        self.keyed = false;
        Ok(Answer::Opened)
    }

    /// The pictures' next unit: one drawn into the lowest slot `free` names,
    /// unless it is due before `skip_before_ns`.
    fn next_picture<W: ShareWrite>(
        &mut self,
        skip_before_ns: u64,
        free: u8,
        ring: &mut W,
    ) -> Result<Answer, Fault> {
        loop {
            let track = self.video.as_ref().ok_or(MovieRefusal::NotOpen)?.track;
            let keyed = self.keyed;
            let walk = self.pictures.as_mut().ok_or(MovieRefusal::NotOpen)?;
            // Pictures before a keyframe predict from frames the walk never had.
            let wanted = |frame: &Frame| frame.track == track && (frame.keyframe || keyed);
            let frame = match walk.next(&mut self.pages, wanted)? {
                Met::Frame(frame) => frame,
                Met::End(ending) => return Ok(Answer::Ended(ending)),
            };
            if let Some(answer) = self.picture(&frame, skip_before_ns, free, ring)? {
                return Ok(answer);
            }
        }
    }

    /// Decode `frame`, whose payload the pictures' walk holds: the unit to
    /// answer, or `None` for a picture not shown.
    fn picture<W: ShareWrite>(
        &mut self,
        frame: &Frame,
        skip_before_ns: u64,
        free: u8,
        ring: &mut W,
    ) -> Result<Option<Answer>, Fault> {
        let channels = self.sound.as_ref().map_or(0, |sound| sound.channels);
        let (Some(video), Some(walk)) = (self.video.as_mut(), self.pictures.as_ref()) else {
            return Ok(None);
        };
        // A keyframe may change the stream's size, which is weighed before
        // the decoder reserves anything for it.
        if let Ok(size) = vp8::probe(&walk.payload) {
            if size != video.size {
                if !fits(size, channels, self.layout, self.ceiling) {
                    return Err(MovieRefusal::TooLarge.into());
                }
                video.size = size;
            }
        }
        if video.last_ns.is_some_and(|last| frame.timestamp_ns < last) {
            return Err(MovieRefusal::OutOfOrder.into());
        }
        let decoded = video.decoder.decode(&walk.payload)?;
        let keyframe = decoded.keyframe();
        self.keyed |= keyframe;
        if !self.keyed || !decoded.shown() {
            return Ok(None);
        }
        video.last_ns = Some(frame.timestamp_ns);
        if frame.timestamp_ns < skip_before_ns {
            return Ok(Some(Answer::Skipped {
                at_ns: frame.timestamp_ns,
                keyframe,
            }));
        }
        let slot = lowest_slot(free).ok_or(MovieRefusal::MalformedRequest)?;
        let (width, height) = self.layout.fit(decoded.width(), decoded.height());
        let staging = (&mut video.staging, &mut video.scratch);
        let at = self.layout.slot_at(slot);
        draw_into(&decoded, (width, height), staging, at, ring)?;
        Ok(Some(Answer::Picture(PictureHead {
            at_ns: frame.timestamp_ns,
            keyframe,
            slot,
            width,
            height,
        })))
    }

    /// The sound's next block, or the end; a movie with no sound has none.
    fn next_sound(&mut self) -> Result<Answer, Fault> {
        loop {
            let Some(track) = self.sound.as_ref().map(|sound| sound.track) else {
                return Ok(Answer::Ended(Ending::Complete));
            };
            let walk = self.sounds.as_mut().ok_or(MovieRefusal::NotOpen)?;
            let frame = match walk.next(&mut self.pages, |frame| frame.track == track)? {
                Met::Frame(frame) => frame,
                Met::End(ending) => return Ok(Answer::Ended(ending)),
            };
            if let Some(answer) = self.sound_block(&frame)? {
                return Ok(answer);
            }
        }
    }

    /// Start the walks a refused `request` left part-way at their next frame.
    fn restep(&mut self, request: Request) {
        let walks = match request {
            Request::Next { .. } => [self.pictures.as_mut(), None],
            Request::Sound => [self.sounds.as_mut(), None],
            Request::Open | Request::Seek { .. } => [self.pictures.as_mut(), self.sounds.as_mut()],
        };
        for walk in walks.into_iter().flatten() {
            walk.step = Step::Locate;
        }
    }

    /// Decode `frame` as a packet of sound into the block buffer: the samples
    /// past the codec delay and short of the padding it discards.
    fn sound_block(&mut self, frame: &Frame) -> Result<Option<Answer>, Fault> {
        let (Some(sound), Some(walk)) = (self.sound.as_mut(), self.sounds.as_ref()) else {
            return Ok(None);
        };
        let samples = sound.packets.decode(&walk.payload, &mut sound.pcm)?;
        let Some((first, count, at_ns)) = kept(frame, samples, sound.delay_ns) else {
            return Ok(None);
        };
        if sound.last_ns.is_some_and(|last| at_ns < last) {
            return Err(MovieRefusal::OutOfOrder.into());
        }
        sound.last_ns = Some(at_ns);
        let values = sound
            .pcm
            .get(first * sound.channels..(first + count) * sound.channels)
            .ok_or(MovieRefusal::OutOfMemory)?;
        sound.block.clear();
        sound.block.push(FROM_SOUND);
        sound.block.extend_from_slice(&at_ns.to_le_bytes());
        let frames = u32::try_from(count).map_err(|_| MovieRefusal::OutOfMemory)?;
        sound.block.extend_from_slice(&frames.to_le_bytes());
        for value in values {
            sound.block.extend_from_slice(&value.to_le_bytes());
        }
        Ok(Some(Answer::Sound(sound.block.len())))
    }

    fn seek(&mut self, target_ns: u64) -> Result<Answer, Fault> {
        let walk = self.pictures.as_mut().ok_or(MovieRefusal::NotOpen)?;
        walk.seek(&mut self.pages, target_ns)?;
        if let Some(walk) = self.sounds.as_mut() {
            walk.seek(&mut self.pages, target_ns)?;
        }
        self.keyed = false;
        if let Some(video) = self.video.as_mut() {
            video.last_ns = None;
        }
        if let Some(sound) = self.sound.as_mut() {
            sound.packets.reset();
            sound.last_ns = None;
        }
        Ok(Answer::Sought)
    }
}

/// The lowest slot the mask `free` names.
fn lowest_slot(free: u8) -> Option<u8> {
    let free = free & ALL_SLOTS;
    if free == 0 {
        return None;
    }
    u8::try_from(free.trailing_zeros()).ok()
}

/// Draw `decoded` at `size` into the slot at `at` of `ring`: straight in at
/// its own size, or drawn into `staging` first and scaled into the slot.
fn draw_into<W: ShareWrite>(
    decoded: &Decoded<'_>,
    (width, height): (u32, u32),
    (staging, scratch): (&mut Vec<u8>, &mut ResampleScratch),
    at: usize,
    ring: &mut W,
) -> Result<(), Fault> {
    let (own_width, own_height) = (decoded.width(), decoded.height());
    let len = picture_len(width, height).ok_or(MovieRefusal::OutOfMemory)?;
    let end = at.checked_add(len).ok_or(MovieRefusal::Undrawable)?;
    let mut failed = None;
    let drawn = if (width, height) == (own_width, own_height) {
        ring.write_with(&mut |bytes| {
            let Some(slot) = bytes.get_mut(at..end) else {
                return false;
            };
            let drawn = decoded.rgba_rows(0..own_height as usize, slot, own_width as usize * 4);
            failed = drawn.err().map(Fault::from);
            failed.is_none()
        })
    } else {
        let own = picture_len(own_width, own_height).ok_or(MovieRefusal::OutOfMemory)?;
        staging.truncate(own);
        if !fallible::grow_to(staging, own, 0) {
            return Err(MovieRefusal::OutOfMemory.into());
        }
        decoded.rgba_rows(0..own_height as usize, staging, own_width as usize * 4)?;
        let image = Rgba8Image::new(own_width, own_height, staging)
            .map_err(|_| MovieRefusal::Undrawable)?;
        let window = Region {
            x: 0,
            y: 0,
            width,
            height,
        };
        ring.write_with(&mut |bytes| {
            bytes.get_mut(at..end).is_some_and(|slot| {
                let into = (window, slot);
                resample_window_in(&image, image.whole(), (width, height), into, scratch).is_ok()
            })
        })
    };
    match (drawn, failed) {
        (true, _) => Ok(()),
        (false, Some(fault)) => Err(fault),
        (false, None) => Err(MovieRefusal::Undrawable.into()),
    }
}

/// The first VP8 track a file declares, with the size it declares.
fn picture_track(tracks: &[Track]) -> Option<Video> {
    tracks
        .iter()
        .find_map(|track| match (track.codec, track.media) {
            (Codec::Vp8, Media::Video { width, height }) if width > 0 && height > 0 => {
                Some(Video {
                    track: track.number,
                    size: (width, height),
                    decoder: Decoder::new(),
                    last_ns: None,
                    staging: Vec::new(),
                    scratch: ResampleScratch::new(),
                })
            }
            _ => None,
        })
}

/// The first Opus track a file declares.
fn sound_track(tracks: &[Track]) -> Option<&Track> {
    tracks
        .iter()
        .find(|track| track.codec == Codec::Opus && matches!(track.media, Media::Audio { .. }))
}

/// Whether decoding a stream of `size` with `channels` of sound into a ring
/// of `layout` fits within `ceiling`.
fn fits(size: (u32, u32), channels: usize, layout: RingLayout, ceiling: u64) -> bool {
    movie_bytes(size.0, size.1, channels, layout).is_some_and(|needed| needed <= ceiling)
}

/// Every byte a worker decoding a `width`×`height` picture stream with
/// `channels` channels of sound into a ring of `layout` holds at once — the
/// decoder at its peak, the ring, a picture larger than a slot drawn at its
/// own size before it is scaled, each walk's frame payload, the page cache,
/// the sound's buffers, a request, and the worker's own baseline — or `None`
/// for a size no stream declares. The ring is mapped in its owner too.
#[must_use]
pub fn movie_bytes(width: u32, height: u32, channels: usize, layout: RingLayout) -> Option<u64> {
    let decoder = stream_peak_bytes(width, height)?;
    let staging = if layout.fit(width, height) == (width, height) {
        0
    } else {
        u64::from(width)
            .checked_mul(u64::from(height))?
            .checked_mul(4)?
    };
    let sound = u64::try_from(MAX_SOUND_FRAMES.checked_mul(channels)?.checked_mul(8)?).ok()?;
    let ring = u64::try_from(layout.ring_bytes()).ok()?;
    let fixed = [
        WORKER_BASELINE_BYTES,
        2 * u64::from(MAX_FRAME_BYTES),
        (CACHE_PAGES * PAGE_BYTES) as u64,
        MAX_TO_WORKER as u64,
    ];
    let varying = decoder
        .checked_add(staging)?
        .checked_add(sound)?
        .checked_add(ring)?;
    fixed.into_iter().try_fold(varying, u64::checked_add)
}

/// The bytes a `width`×`height` RGBA8 picture takes.
fn picture_len(width: u32, height: u32) -> Option<usize> {
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)
}

/// Which samples of a packet decoded to `samples` a channel are sound — past
/// the codec delay and short of the padding `frame` discards — as its first
/// and how many, and when the first plays; `None` where none are.
fn kept(frame: &Frame, samples: usize, delay_ns: u64) -> Option<(usize, usize, u64)> {
    let start = i128::from(frame.timestamp_ns) - i128::from(delay_ns);
    let lead = u128::try_from(-start).unwrap_or(0);
    let mut first = frames_at_least(lead);
    let mut end = samples;
    let padding = frame.discard_padding_ns;
    match padding.cmp(&0) {
        Ordering::Greater => end = end.saturating_sub(frames_nearest(padding.unsigned_abs())),
        Ordering::Less => first = first.max(frames_nearest(padding.unsigned_abs())),
        Ordering::Equal => {}
    }
    if first >= end {
        return None;
    }
    let at = start + i128::try_from(ns_of(first)).ok()?;
    Some((first, end - first, u64::try_from(at.max(0)).ok()?))
}

/// The fewest samples at the decode's rate that span `ns` nanoseconds.
fn frames_at_least(ns: u128) -> usize {
    let rate = u128::from(OpusPackets::RATE.hz());
    usize::try_from((ns * rate).div_ceil(NS_PER_SECOND)).unwrap_or(usize::MAX)
}

/// The samples at the decode's rate nearest `ns` nanoseconds.
fn frames_nearest(ns: u64) -> usize {
    let rate = u128::from(OpusPackets::RATE.hz());
    let frames = (u128::from(ns) * rate + NS_PER_SECOND / 2) / NS_PER_SECOND;
    usize::try_from(frames).unwrap_or(usize::MAX)
}

/// The nanoseconds `frames` samples at the decode's rate span, rounded down.
fn ns_of(frames: usize) -> u128 {
    let rate = u128::from(OpusPackets::RATE.hz());
    (frames as u128) * NS_PER_SECOND / rate
}

/// The worker side of a sandboxed movie decode: one file at a time, held in
/// the pages its owner supplies, its pictures drawn into the ring its owner
/// shares with it.
pub struct MovieDecodeService<W> {
    movie: Option<OpenMovie>,
    pending: Option<(Request, Need)>,
    /// Where every answer but a block of sound is built.
    reply: Vec<u8>,
    ring: Option<W>,
}

/// What the worker answers a frame with.
#[derive(Copy, Clone)]
enum Answer {
    Need((u64, usize)),
    Opened,
    /// A picture drawn into its slot.
    Picture(PictureHead),
    Skipped {
        at_ns: u64,
        keyframe: bool,
    },
    /// A block of sound of this many bytes, built in the sound's block buffer.
    Sound(usize),
    Ended(Ending),
    Sought,
    Refused(MovieRefusal),
}

impl<W: ShareWrite> MovieDecodeService<W> {
    /// A worker with nothing open, drawing into `ring`: its spawn share, or
    /// `None` for a worker handed none, which refuses every open.
    #[must_use]
    pub const fn new(ring: Option<W>) -> Self {
        Self {
            movie: None,
            pending: None,
            reply: Vec::new(),
            ring,
        }
    }

    fn close(&mut self) {
        self.movie = None;
        self.pending = None;
    }

    fn open(&mut self, seed: HashSeed, len: u64, ceiling: u64, layout: RingLayout) -> Answer {
        self.close();
        let held = self.ring.as_ref().map_or(0, ShareWrite::byte_len);
        if held < layout.ring_bytes() {
            return Answer::Refused(MovieRefusal::NoRing);
        }
        let Some(pages) = Pages::new(len, seed, CACHE_PAGES) else {
            return Answer::Refused(MovieRefusal::OutOfMemory);
        };
        self.movie = Some(OpenMovie {
            pages,
            ceiling,
            layout,
            video: None,
            sound: None,
            pictures: None,
            sounds: None,
            keyed: false,
        });
        self.begin(Request::Open)
    }

    fn begin(&mut self, request: Request) -> Answer {
        if self.pending.is_some() {
            self.close();
            return Answer::Refused(MovieRefusal::MalformedRequest);
        }
        let Some(movie) = self.movie.as_mut() else {
            return Answer::Refused(MovieRefusal::NotOpen);
        };
        movie.pages.next_request();
        self.attempt(request)
    }

    fn attempt(&mut self, request: Request) -> Answer {
        let Some(movie) = self.movie.as_mut() else {
            return Answer::Refused(MovieRefusal::NotOpen);
        };
        movie.pages.next_attempt();
        let outcome = match request {
            Request::Open => movie.open(),
            Request::Next {
                skip_before_ns,
                free,
            } => match self.ring.as_mut() {
                Some(ring) => movie.next_picture(skip_before_ns, free, ring),
                None => Err(MovieRefusal::NoRing.into()),
            },
            Request::Sound => movie.next_sound(),
            Request::Seek { target_ns } => movie.seek(target_ns),
        };
        match outcome {
            Ok(answer) => answer,
            Err(Fault::Unavailable) => match movie.pages.need() {
                Ok(need) => {
                    self.pending = Some((request, need));
                    Answer::Need(movie.pages.need_bytes(need))
                }
                Err(unasked) => self.refuse(request, unasked_refusal(unasked)),
            },
            Err(Fault::Refused(refusal)) => self.refuse(request, refusal),
        }
    }

    /// A refusal of `request`: a movie whose open was refused is not open, and
    /// a walk refused part-way starts at the next frame.
    fn refuse(&mut self, request: Request, refusal: MovieRefusal) -> Answer {
        if request == Request::Open {
            self.close();
        } else if let Some(movie) = self.movie.as_mut() {
            movie.restep(request);
        }
        Answer::Refused(refusal)
    }

    /// The outstanding request and its need, taken, if `answers` the bytes it
    /// asked for; otherwise the owner has broken the protocol.
    fn answered(&mut self, answers: impl FnOnce(u64, usize) -> bool) -> Option<(Request, Need)> {
        let (request, need) = self.pending.take()?;
        let (offset, len) = self.movie.as_ref()?.pages.need_bytes(need);
        answers(offset, len).then_some((request, need))
    }

    fn supply(&mut self, offset: u64, bytes: &[u8]) -> Answer {
        let answered = self.answered(|asked, len| (asked, len) == (offset, bytes.len()));
        let (Some((request, need)), Some(movie)) = (answered, self.movie.as_mut()) else {
            self.close();
            return Answer::Refused(MovieRefusal::MalformedRequest);
        };
        match movie.pages.take(need, bytes) {
            Ok(()) => self.attempt(request),
            Err(Shortfall::WorkingSet) => self.refuse(request, MovieRefusal::WorkingSetExceeded),
            Err(Shortfall::Memory) => self.refuse(request, MovieRefusal::OutOfMemory),
        }
    }

    fn unreadable(&mut self, offset: u64) -> Answer {
        if let Some((request, _)) = self.answered(|asked, _| asked == offset) {
            self.refuse(request, MovieRefusal::Demux(DemuxError::InputFailed))
        } else {
            self.close();
            Answer::Refused(MovieRefusal::MalformedRequest)
        }
    }

    fn serve(&mut self, frame: &[u8]) -> Answer {
        match Inbound::read(frame) {
            Ok(Inbound::Open(seed, len, ceiling, layout)) => self.open(seed, len, ceiling, layout),
            Ok(Inbound::Next(skip_before_ns, held)) => self.begin(Request::Next {
                skip_before_ns,
                free: ALL_SLOTS & !held,
            }),
            Ok(Inbound::Sound) => self.begin(Request::Sound),
            Ok(Inbound::Seek(target_ns)) => self.begin(Request::Seek { target_ns }),
            Ok(Inbound::Supply(offset, bytes)) => self.supply(offset, bytes),
            Ok(Inbound::Unreadable(offset)) => self.unreadable(offset),
            Err(_) => {
                self.close();
                Answer::Refused(MovieRefusal::MalformedRequest)
            }
        }
    }

    /// Encode `answer` into the reply buffer, or lend the block it names.
    fn encoded(&mut self, answer: Answer) -> Option<&[u8]> {
        let mut out = Writer::reusing(core::mem::take(&mut self.reply));
        match answer {
            Answer::Sound(len) => {
                self.reply = out.finish();
                return self.movie.as_ref()?.sound.as_ref()?.block.get(..len);
            }
            Answer::Need((offset, len)) => {
                out.u8(FROM_NEED);
                out.u64(offset);
                out.u32(u32::try_from(len).unwrap_or(u32::MAX));
            }
            Answer::Opened => encode_opened(&mut out, self.movie.as_ref()?)?,
            Answer::Picture(head) => encode_picture(&mut out, head),
            Answer::Skipped { at_ns, keyframe } => {
                out.u8(FROM_SKIPPED);
                out.u64(at_ns);
                out.u8(u8::from(keyframe));
            }
            Answer::Ended(ending) => {
                out.u8(FROM_ENDED);
                out.u8(match ending {
                    Ending::Complete => ENDED_COMPLETE,
                    Ending::Truncated => ENDED_TRUNCATED,
                });
            }
            Answer::Sought => out.u8(FROM_SOUGHT),
            Answer::Refused(refusal) => {
                let (kind, code, detail) = refusal.to_wire();
                out.u8(FROM_REFUSED);
                out.u8(kind);
                out.u8(code);
                out.u32(detail);
            }
        }
        self.reply = out.finish();
        Some(&self.reply)
    }

    /// Send `answer` as one frame; one that cannot be built is refused as the
    /// worker's want of memory, closing what was open.
    fn send(&mut self, answer: Answer, out: &mut dyn FrameOut) -> Result<(), ProtoError> {
        if let Some(frame) = self.encoded(answer) {
            return out.frame(frame);
        }
        self.close();
        let frame = self
            .encoded(Answer::Refused(MovieRefusal::OutOfMemory))
            .ok_or(ProtoError::OutOfMemory)?;
        out.frame(frame)
    }
}

impl<W: ShareWrite> SessionService for MovieDecodeService<W> {
    fn handle(&mut self, request: &[u8], out: &mut dyn FrameOut) -> SessionStep {
        let answer = self.serve(request);
        if self.send(answer, out).is_ok() {
            SessionStep::Continue
        } else {
            SessionStep::Finished
        }
    }
}

/// What a miss no need answers is refused as.
const fn unasked_refusal(unasked: Unasked) -> MovieRefusal {
    match unasked {
        Unasked::NoMiss => MovieRefusal::Demux(DemuxError::InputFailed),
        Unasked::TooWide => MovieRefusal::WorkingSetExceeded,
    }
}

/// A frame from the owner, read.
enum Inbound<'a> {
    Open(HashSeed, u64, u64, RingLayout),
    /// The next picture, and the slots the owner holds.
    Next(u64, u8),
    Sound,
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
                let (len, ceiling) = (reader.u64()?, reader.u64()?);
                let layout = RingLayout::new(reader.u32()?, reader.u32()?);
                Ok(Self::Open(
                    seed,
                    len,
                    ceiling,
                    layout.ok_or(WireError::Malformed)?,
                ))
            }
            (TO_NEXT, NEXT_LEN) => {
                let skip_before_ns = reader.u64()?;
                let held = reader.u8()?;
                // A next with every slot held could draw nowhere.
                if held & !ALL_SLOTS != 0 || held == ALL_SLOTS {
                    return Err(WireError::Malformed);
                }
                Ok(Self::Next(skip_before_ns, held))
            }
            (TO_SOUND, SOUND_REQUEST_LEN) => Ok(Self::Sound),
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

fn encode_picture(out: &mut Writer, head: PictureHead) {
    out.u8(FROM_PICTURE);
    out.u64(head.at_ns);
    out.u8(u8::from(head.keyframe));
    out.u8(head.slot);
    out.u32(head.width);
    out.u32(head.height);
}

fn decode_picture(reader: &mut Reader<'_>) -> Result<PictureHead, WireError> {
    Ok(PictureHead {
        at_ns: reader.u64()?,
        keyframe: flag(reader)?,
        slot: reader.u8()?,
        width: reader.u32()?,
        height: reader.u32()?,
    })
}

/// Encode what an open movie is; `None` before its open has completed.
fn encode_opened(out: &mut Writer, movie: &OpenMovie) -> Option<()> {
    let demuxer = &movie.pictures.as_ref()?.demuxer;
    let video = movie.video.as_ref()?;
    let duration = demuxer.duration_ns().filter(|&duration| duration > 0);
    out.u8(FROM_OPENED);
    out.u8(u8::from(duration.is_some()));
    out.u64(duration.unwrap_or(0));
    out.u8(u8::from(demuxer.has_index()));
    out.u32(video.size.0);
    out.u32(video.size.1);
    if let Some(sound) = &movie.sound {
        out.u8(1);
        out.raw(&sound.packets.channels().to_wire());
    } else {
        out.u8(0);
        out.raw(&[0; CHANNEL_MAP_WIRE_LEN]);
    }
    Some(())
}

fn decode_opened(reader: &mut Reader<'_>) -> Result<MovieInfo, WireError> {
    let stated = flag(reader)?;
    let duration = reader.u64()?;
    let indexed = flag(reader)?;
    let (width, height) = (reader.u32()?, reader.u32()?);
    let sounded = flag(reader)?;
    let channels = reader.take(CHANNEL_MAP_WIRE_LEN)?;
    let duration_ns = match (stated, duration) {
        (true, 0) | (false, 1..) => return Err(WireError::Malformed),
        (true, duration) => Some(duration),
        (false, 0) => None,
    };
    let sound = if sounded {
        let map = ChannelMap::from_wire(channels).map_err(|_| WireError::Malformed)?;
        Some(map).filter(|map| map.channels() > 0)
    } else if channels.iter().all(|&byte| byte == 0) {
        None
    } else {
        return Err(WireError::Malformed);
    };
    if (sounded && sound.is_none()) || stream_peak_bytes(width, height).is_none() {
        return Err(WireError::Malformed);
    }
    Ok(MovieInfo {
        duration_ns,
        indexed,
        width,
        height,
        sound,
    })
}

/// What a reply asked of the owner, or told it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MovieEvent<'a> {
    /// Read the `len` bytes at `offset` and hand them to
    /// [`MovieDecodeClient::supply`], or report the read failed through
    /// [`MovieDecodeClient::unreadable`]. It is file I/O, so an interactive
    /// owner reads off its event loop.
    Need {
        /// Where the bytes start.
        offset: u64,
        /// How many there are.
        len: usize,
    },
    /// The movie is open: [`MovieDecodeClient::info`] describes it.
    Opened,
    /// A picture drawn into its slot, the owner's until it hands it back
    /// ([`MovieDecodeClient::release`]).
    Picture(PictureHead),
    /// A picture decoded and not drawn: it was due before the instant asked.
    Skipped {
        /// When it was due.
        at_ns: u64,
    },
    /// Sound from `at_ns`, interleaved `F32` at [`OpusPackets::RATE`].
    Sound {
        /// When its first frame plays.
        at_ns: u64,
        /// The frames.
        pcm: &'a [u8],
    },
    /// The walk asked holds no frame past those answered.
    Ended(Ending),
    /// The next unit starts at the latest keyframe at or before the target.
    Sought,
    /// The worker refused the request. A movie whose open was refused is not
    /// open; a refused next or seek left the walk at the next frame.
    Refused(MovieRefusal),
    /// A replacement worker is back at the latest keyframe the stream passed,
    /// and the units from there come again.
    Resumed,
    /// A step of bringing a replacement back: the next frame to send is
    /// waiting ([`MovieDecodeClient::outgoing`]).
    Resuming,
}

/// Why a [`MovieDecodeClient`] call failed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MovieDecodeError {
    /// Not a call this point admits: a request is outstanding, nothing is
    /// open, every slot is held, the movie has no sound, or nothing was
    /// asked for.
    OutOfTurn,
    /// Bytes of another length than the need asked for.
    WrongLength,
    /// The worker's frame broke the protocol or stated the impossible. It
    /// cannot be believed: condemn the worker and
    /// [`restart`](MovieDecodeClient::restart).
    Unbelievable,
    /// A replacement could not be brought back: the file now reads as another
    /// movie, or the worker failed again where it last did.
    CannotResume,
    /// The request could not be built for want of memory.
    OutOfMemory,
}

impl Unbelieved for MovieDecodeError {
    fn unbelieved(&self) -> bool {
        *self == Self::Unbelievable
    }
}

impl core::fmt::Display for MovieDecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::OutOfTurn => "a movie request was made out of turn",
            Self::WrongLength => "the bytes handed to the decoder were not those it asked for",
            Self::Unbelievable => "the decoder's answer could not be believed",
            Self::CannotResume => "the decoder could not be brought back to the movie",
            Self::OutOfMemory => "there is not enough memory to ask the decoder",
        })
    }
}

/// What the client is waiting for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Awaiting {
    /// Nothing is open.
    Closed,
    /// Open, and no request outstanding.
    Idle,
    Open,
    /// A picture, into one of the slots `free` names.
    Next {
        skip_before_ns: u64,
        free: u8,
    },
    Sound,
    Seek,
    /// A replacement's open of the same file.
    Reopen,
    /// A replacement's seek back to the latest keyframe passed.
    Reposition,
}

/// The owner's side of one sandboxed movie decode.
///
/// Every request is encoded into [`Self::outgoing`] for the owner to send;
/// every frame the worker sends goes through [`Self::on_frame`], which checks
/// it against what was asked before anything in it is used.
pub struct MovieDecodeClient {
    len: u64,
    ceiling: u64,
    info: Option<MovieInfo>,
    awaiting: Awaiting,
    need: Option<(u64, usize)>,
    /// Needs the outstanding request has made: at most every page of the file
    /// once, and the cache's worth more.
    needs: usize,
    outgoing: Option<Vec<u8>>,
    /// Where a frame is built, kept from one request to the next.
    spare: Vec<u8>,
    /// The ring the worker draws into, as the open states it.
    layout: Option<RingLayout>,
    /// The slots of the ring the owner holds, as a mask.
    held: u8,
    /// When the latest picture and the latest sound play, which each later one
    /// of its kind is held to.
    last_picture_ns: Option<u64>,
    last_sound_ns: Option<u64>,
    /// Where a replacement is brought back to: the latest keyframe passed, or
    /// the target of the latest seek.
    resume_ns: u64,
    /// Where the stream was when a worker last failed, so a movie that fails
    /// its worker at one place is brought back once.
    failed_at: Option<u64>,
}

impl Default for MovieDecodeClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MovieDecodeClient {
    /// A client with nothing open.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            len: 0,
            ceiling: 0,
            info: None,
            awaiting: Awaiting::Closed,
            need: None,
            needs: 0,
            outgoing: None,
            spare: Vec::new(),
            layout: None,
            held: 0,
            last_picture_ns: None,
            last_sound_ns: None,
            resume_ns: 0,
            failed_at: None,
        }
    }

    /// What the open movie is, once open.
    #[must_use]
    pub const fn info(&self) -> Option<MovieInfo> {
        self.info
    }

    /// Whether a movie is open and no request is outstanding.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.awaiting == Awaiting::Idle && self.outgoing.is_none()
    }

    /// Whether the client is free for a request: none outstanding or waiting
    /// to be sent. An open needs no more; a next, draw or seek needs a movie
    /// open too ([`Self::is_idle`]).
    #[must_use]
    pub fn takes_request(&self) -> bool {
        matches!(self.awaiting, Awaiting::Closed | Awaiting::Idle) && self.outgoing.is_none()
    }

    /// Whether a slot is free for the next picture.
    #[must_use]
    pub const fn has_free_slot(&self) -> bool {
        self.held != ALL_SLOTS
    }

    /// Hand slot `slot` back: the owner is done with the picture drawn in it,
    /// and the worker is told so with the next picture asked for.
    ///
    /// # Errors
    ///
    /// [`MovieDecodeError::OutOfTurn`] for a slot the owner does not hold.
    pub fn release(&mut self, slot: u8) -> Result<(), MovieDecodeError> {
        let bit = slot_bit(slot)
            .filter(|bit| self.held & bit != 0)
            .ok_or(MovieDecodeError::OutOfTurn)?;
        self.held &= !bit;
        Ok(())
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

    /// Open a movie of `len` bytes in a worker that may map `ceiling` bytes
    /// and draws into a ring of `layout`, its cache keyed by `seed` — a fresh
    /// draw from the owner's random source, so a file cannot choose which of
    /// its pages collide.
    ///
    /// # Errors
    ///
    /// [`MovieDecodeError::OutOfTurn`] while a request is outstanding.
    pub fn open(
        &mut self,
        len: u64,
        ceiling: u64,
        layout: RingLayout,
        seed: HashSeed,
    ) -> Result<(), MovieDecodeError> {
        if !self.takes_request() {
            return Err(MovieDecodeError::OutOfTurn);
        }
        self.len = len;
        self.ceiling = ceiling;
        self.layout = Some(layout);
        self.info = None;
        self.last_picture_ns = None;
        self.last_sound_ns = None;
        self.resume_ns = 0;
        self.failed_at = None;
        self.send_open(seed)?;
        self.awaiting = Awaiting::Open;
        Ok(())
    }

    /// Ask for the pictures' next unit, drawn into a slot the owner does not
    /// hold unless it is due before `skip_before_ns`. The request states the
    /// slots held, which hands back every one released since the last.
    ///
    /// # Errors
    ///
    /// [`MovieDecodeError::OutOfTurn`] unless open and idle with a slot free.
    pub fn next(&mut self, skip_before_ns: u64) -> Result<(), MovieDecodeError> {
        if !self.is_idle() || !self.has_free_slot() {
            return Err(MovieDecodeError::OutOfTurn);
        }
        let mut out = self.writer(NEXT_LEN)?;
        out.u8(TO_NEXT);
        out.u64(skip_before_ns);
        out.u8(self.held);
        self.request(out);
        self.awaiting = Awaiting::Next {
            skip_before_ns,
            free: ALL_SLOTS & !self.held,
        };
        Ok(())
    }

    /// Ask for the sound's next block.
    ///
    /// # Errors
    ///
    /// [`MovieDecodeError::OutOfTurn`] unless open and idle, with sound.
    pub fn sound(&mut self) -> Result<(), MovieDecodeError> {
        if !self.is_idle() || self.info.is_none_or(|info| info.sound.is_none()) {
            return Err(MovieDecodeError::OutOfTurn);
        }
        let mut out = self.writer(SOUND_REQUEST_LEN)?;
        out.u8(TO_SOUND);
        self.request(out);
        self.awaiting = Awaiting::Sound;
        Ok(())
    }

    /// Ask for the next unit to start at the latest keyframe at or before
    /// `target_ns`.
    ///
    /// # Errors
    ///
    /// [`MovieDecodeError::OutOfTurn`] unless open and idle.
    pub fn seek(&mut self, target_ns: u64) -> Result<(), MovieDecodeError> {
        if !self.is_idle() {
            return Err(MovieDecodeError::OutOfTurn);
        }
        self.send_seek(target_ns)?;
        self.resume_ns = target_ns;
        self.awaiting = Awaiting::Seek;
        Ok(())
    }

    /// Hand over the bytes the outstanding need asked for.
    ///
    /// # Errors
    ///
    /// [`MovieDecodeError::OutOfTurn`] with no need outstanding,
    /// [`MovieDecodeError::WrongLength`], or [`MovieDecodeError::OutOfMemory`].
    pub fn supply(&mut self, bytes: &[u8]) -> Result<(), MovieDecodeError> {
        let (offset, len) = self.need.ok_or(MovieDecodeError::OutOfTurn)?;
        if bytes.len() != len {
            return Err(MovieDecodeError::WrongLength);
        }
        let mut out = self.writer(SUPPLY_HEADER_LEN + len)?;
        out.u8(TO_SUPPLY);
        out.u64(offset);
        out.raw(bytes);
        self.outgoing = Some(out.finish());
        self.need = None;
        Ok(())
    }

    /// Report that the bytes the outstanding need asked for could not be read;
    /// the worker refuses the request it was serving.
    ///
    /// # Errors
    ///
    /// [`MovieDecodeError::OutOfTurn`] with no need outstanding.
    pub fn unreadable(&mut self) -> Result<(), MovieDecodeError> {
        let (offset, _) = self.need.ok_or(MovieDecodeError::OutOfTurn)?;
        let mut out = self.writer(UNREADABLE_LEN)?;
        out.u8(TO_UNREADABLE);
        out.u64(offset);
        self.outgoing = Some(out.finish());
        self.need = None;
        Ok(())
    }

    /// A new worker has replaced the one that failed: open the movie in it
    /// again, `seed` a fresh draw, and bring it back to the latest keyframe the
    /// stream passed. Whatever was outstanding is gone; once
    /// [`MovieEvent::Resumed`] arrives the owner asks again.
    ///
    /// # Errors
    ///
    /// [`MovieDecodeError::CannotResume`] when the movie failed a worker at
    /// this place before: it is closed.
    pub fn restart(&mut self, seed: HashSeed) -> Result<(), MovieDecodeError> {
        self.need = None;
        // The replacement draws into a ring of its own, nothing of which is held.
        self.held = 0;
        if let Some(frame) = self.outgoing.take() {
            self.spare = frame;
        }
        if self.awaiting == Awaiting::Closed {
            return Ok(());
        }
        if self.failed_at == Some(self.resume_ns) {
            self.close();
            return Err(MovieDecodeError::CannotResume);
        }
        self.failed_at = Some(self.resume_ns);
        self.send_open(seed)?;
        self.awaiting = if self.info.is_some() {
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
    /// [`MovieDecodeError::Unbelievable`] for a frame that is not an honest
    /// answer to what was asked, and [`MovieDecodeError::CannotResume`] when a
    /// replacement finds a different movie.
    pub fn on_frame<'a>(&mut self, frame: &'a [u8]) -> Result<MovieEvent<'a>, MovieDecodeError> {
        let unbelievable = MovieDecodeError::Unbelievable;
        if self.need.is_some()
            || self.outgoing.is_some()
            || matches!(self.awaiting, Awaiting::Closed | Awaiting::Idle)
        {
            return Err(unbelievable);
        }
        let mut reader = Reader::new(frame);
        let op = reader.u8().map_err(|_| unbelievable)?;
        match op {
            FROM_NEED if frame.len() == NEED_LEN => self.on_need(&mut reader),
            FROM_OPENED if frame.len() == OPENED_LEN => self.on_opened(&mut reader),
            FROM_PICTURE if frame.len() == PICTURE_LEN => self.on_picture(&mut reader),
            FROM_SKIPPED if frame.len() == SKIPPED_LEN => self.on_skipped(&mut reader),
            FROM_SOUND if frame.len() >= SOUND_HEADER_LEN => self.on_sound(&mut reader, frame),
            FROM_ENDED if frame.len() == ENDED_LEN => self.on_ended(&mut reader),
            FROM_SOUGHT if frame.len() == SOUGHT_LEN => self.on_sought(),
            FROM_REFUSED if frame.len() == REFUSED_LEN => self.on_refused(&mut reader),
            _ => Err(unbelievable),
        }
    }

    fn on_need<'a>(&mut self, reader: &mut Reader<'_>) -> Result<MovieEvent<'a>, MovieDecodeError> {
        let (offset, len) = fields(reader, |reader| Ok((reader.u64()?, reader.u32()? as usize)))?;
        self.needs += 1;
        let pages = usize::try_from(self.len.div_ceil(PAGE)).unwrap_or(usize::MAX);
        if !shaped_need(offset, len, self.len) || self.needs > pages.saturating_add(CACHE_PAGES + 1)
        {
            return Err(MovieDecodeError::Unbelievable);
        }
        self.need = Some((offset, len));
        Ok(MovieEvent::Need { offset, len })
    }

    fn on_opened<'a>(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<MovieEvent<'a>, MovieDecodeError> {
        let info = fields(reader, decode_opened)?;
        match self.awaiting {
            Awaiting::Open => {
                self.info = Some(info);
                self.awaiting = Awaiting::Idle;
                Ok(MovieEvent::Opened)
            }
            Awaiting::Reopen if self.info == Some(info) => {
                if self.resume_ns == 0 {
                    return Ok(self.resumed());
                }
                self.send_seek(self.resume_ns)?;
                self.awaiting = Awaiting::Reposition;
                Ok(MovieEvent::Resuming)
            }
            Awaiting::Reopen => {
                self.close();
                Err(MovieDecodeError::CannotResume)
            }
            _ => Err(MovieDecodeError::Unbelievable),
        }
    }

    fn on_picture<'a>(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<MovieEvent<'a>, MovieDecodeError> {
        let head = fields(reader, decode_picture)?;
        let Awaiting::Next {
            skip_before_ns,
            free,
        } = self.awaiting
        else {
            return Err(MovieDecodeError::Unbelievable);
        };
        // Every field is held to what was asked before a pixel is read: a slot
        // of the ring the request left free, and a size a slot holds.
        let sized = self.layout.is_some_and(|layout| {
            let (width, height) = layout.size();
            (1..=width).contains(&head.width) && (1..=height).contains(&head.height)
        });
        let honest = sized
            && head.at_ns >= skip_before_ns
            && self.last_picture_ns.is_none_or(|last| head.at_ns >= last);
        let bit = slot_bit(head.slot).filter(|bit| free & bit != 0 && honest);
        let Some(bit) = bit else {
            return Err(MovieDecodeError::Unbelievable);
        };
        self.held |= bit;
        self.last_picture_ns = Some(head.at_ns);
        if head.keyframe {
            self.resume_ns = head.at_ns;
        }
        self.awaiting = Awaiting::Idle;
        Ok(MovieEvent::Picture(head))
    }

    fn on_skipped<'a>(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<MovieEvent<'a>, MovieDecodeError> {
        let (at_ns, keyframe) = fields(reader, |reader| Ok((reader.u64()?, flag(reader)?)))?;
        let Awaiting::Next { skip_before_ns, .. } = self.awaiting else {
            return Err(MovieDecodeError::Unbelievable);
        };
        if at_ns >= skip_before_ns || self.last_picture_ns.is_some_and(|last| at_ns < last) {
            return Err(MovieDecodeError::Unbelievable);
        }
        self.last_picture_ns = Some(at_ns);
        if keyframe {
            self.resume_ns = at_ns;
        }
        self.awaiting = Awaiting::Idle;
        Ok(MovieEvent::Skipped { at_ns })
    }

    fn on_sound<'a>(
        &mut self,
        reader: &mut Reader<'_>,
        frame: &'a [u8],
    ) -> Result<MovieEvent<'a>, MovieDecodeError> {
        let unbelievable = MovieDecodeError::Unbelievable;
        let (at_ns, frames) = (
            reader.u64().map_err(|_| unbelievable)?,
            reader.u32().map_err(|_| unbelievable)? as usize,
        );
        let pcm = frame.get(SOUND_HEADER_LEN..).ok_or(unbelievable)?;
        let channels = self
            .info
            .and_then(|info| info.sound)
            .map_or(0, |map| usize::from(map.channels()));
        let honest = self.awaiting == Awaiting::Sound
            && channels > 0
            && (1..=MAX_SOUND_FRAMES).contains(&frames)
            && pcm.len() == frames * channels * 4
            && self.last_sound_ns.is_none_or(|last| at_ns >= last);
        if !honest {
            return Err(unbelievable);
        }
        self.last_sound_ns = Some(at_ns);
        self.awaiting = Awaiting::Idle;
        Ok(MovieEvent::Sound { at_ns, pcm })
    }

    fn on_ended<'a>(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<MovieEvent<'a>, MovieDecodeError> {
        let ending = fields(reader, |reader| match reader.u8()? {
            ENDED_COMPLETE => Ok(Ending::Complete),
            ENDED_TRUNCATED => Ok(Ending::Truncated),
            _ => Err(WireError::Malformed),
        })?;
        if !matches!(self.awaiting, Awaiting::Next { .. } | Awaiting::Sound) {
            return Err(MovieDecodeError::Unbelievable);
        }
        self.awaiting = Awaiting::Idle;
        Ok(MovieEvent::Ended(ending))
    }

    fn on_sought<'a>(&mut self) -> Result<MovieEvent<'a>, MovieDecodeError> {
        match self.awaiting {
            Awaiting::Seek => {
                self.forget_walk();
                self.awaiting = Awaiting::Idle;
                Ok(MovieEvent::Sought)
            }
            Awaiting::Reposition => Ok(self.resumed()),
            _ => Err(MovieDecodeError::Unbelievable),
        }
    }

    fn on_refused<'a>(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<MovieEvent<'a>, MovieDecodeError> {
        let refusal = fields(reader, |reader| {
            MovieRefusal::from_wire(reader.u8()?, reader.u8()?, reader.u32()?)
                .ok_or(WireError::Malformed)
        })?;
        // The client never asks what an honest worker refuses so, and a missing
        // page is asked for rather than refused.
        if matches!(
            refusal,
            MovieRefusal::NotOpen
                | MovieRefusal::MalformedRequest
                | MovieRefusal::Demux(DemuxError::InputUnavailable)
        ) {
            return Err(MovieDecodeError::Unbelievable);
        }
        match self.awaiting {
            Awaiting::Open => {
                self.close();
                Ok(MovieEvent::Refused(refusal))
            }
            Awaiting::Next { .. } | Awaiting::Sound | Awaiting::Seek => {
                self.awaiting = Awaiting::Idle;
                Ok(MovieEvent::Refused(refusal))
            }
            _ => {
                self.close();
                Err(MovieDecodeError::CannotResume)
            }
        }
    }

    /// The replacement is back at the latest keyframe passed: the units after
    /// it come again, so none is held to those already met.
    fn resumed<'a>(&mut self) -> MovieEvent<'a> {
        self.forget_walk();
        self.awaiting = Awaiting::Idle;
        MovieEvent::Resumed
    }

    /// Forget where the walks were: the units that follow are held to nothing
    /// before them.
    fn forget_walk(&mut self) {
        self.last_picture_ns = None;
        self.last_sound_ns = None;
    }

    fn close(&mut self) {
        self.info = None;
        self.need = None;
        self.awaiting = Awaiting::Closed;
    }

    /// Queue `frame` as a new request.
    fn request(&mut self, frame: Writer) {
        self.outgoing = Some(frame.finish());
        self.needs = 0;
    }

    fn writer(&mut self, len: usize) -> Result<Writer, MovieDecodeError> {
        let mut buffer = core::mem::take(&mut self.spare);
        buffer.clear();
        buffer
            .try_reserve(len)
            .map_err(|_| MovieDecodeError::OutOfMemory)?;
        Ok(Writer::reusing(buffer))
    }

    fn send_open(&mut self, seed: HashSeed) -> Result<(), MovieDecodeError> {
        let (width, height) = self.layout.ok_or(MovieDecodeError::OutOfTurn)?.size();
        let mut out = self.writer(OPEN_LEN)?;
        out.u8(TO_OPEN);
        let (k0, k1) = seed.words();
        out.u64(k0);
        out.u64(k1);
        out.u64(self.len);
        out.u64(self.ceiling);
        out.u32(width);
        out.u32(height);
        self.request(out);
        Ok(())
    }

    fn send_seek(&mut self, target_ns: u64) -> Result<(), MovieDecodeError> {
        let mut out = self.writer(SEEK_LEN)?;
        out.u8(TO_SEEK);
        out.u64(target_ns);
        self.request(out);
        Ok(())
    }
}

impl Requests for MovieDecodeClient {
    fn outgoing(&self) -> Option<&[u8]> {
        Self::outgoing(self)
    }

    fn sent(&mut self) {
        Self::sent(self);
    }
}

/// The mask naming slot `slot`, or `None` for one past the ring.
fn slot_bit(slot: u8) -> Option<u8> {
    (usize::from(slot) < RING_SLOTS).then(|| 1 << slot)
}

/// Read a whole reply's fields with `read`, refusing one with bytes left
/// over.
fn fields<'r, T>(
    reader: &mut Reader<'r>,
    read: impl FnOnce(&mut Reader<'r>) -> Result<T, WireError>,
) -> Result<T, MovieDecodeError> {
    reader
        .whole(read)
        .map_err(|_| MovieDecodeError::Unbelievable)
}

#[cfg(any(test, feature = "fixture"))]
pub mod fixture;

#[cfg(test)]
#[path = "moviedecode_tests.rs"]
mod tests;
