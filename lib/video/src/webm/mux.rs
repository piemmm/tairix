//! Writing a recording as `WebM`, as it is made.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_fscopy::Target;
use tairix_util::fallible;

use super::ebml::{self, Written, MAX_ID_LEN, MAX_SIZE_LEN, UNKNOWN_SIZE};
use super::id;
use super::{
    AUDIO_TYPE, DEFAULT_TIMESTAMP_SCALE, DOC_TYPE, KEYFRAME_FLAG, MAX_FRAME_BYTES,
    MAX_INDEX_POINTS, OPUS_CODEC, VIDEO_TYPE, VP8_CODEC,
};

/// The application every file names as its muxer and its writer.
const APP: &str = "TAIRiX";

/// The Matroska version the files are written to: `CodecDelay` and
/// `SeekPreRoll` are version 4 elements.
const DOC_TYPE_VERSION: u64 = 4;

/// The Matroska version a reader needs: `SimpleBlock` is a version 2 element.
const DOC_TYPE_READ_VERSION: u64 = 2;

/// The video track's number, which its blocks name it by.
const VIDEO_TRACK: u8 = 1;

/// The audio track's number.
const AUDIO_TRACK: u8 = 2;

/// Opus decodes at 48 kHz, whatever rate the sound was made at.
const OPUS_RATE: u32 = 48_000;

/// The Opus mapping's seek pre-roll: 80 ms.
const SEEK_PRE_ROLL_NS: u64 = 80_000_000;

const NS_PER_SECOND: u64 = 1_000_000_000;

/// How far a held frame may fall behind the newest before it is written
/// whatever the other track does: past a second of skew, order gives way to
/// bounded memory.
const INTERLEAVE_WINDOW_NS: u64 = NS_PER_SECOND;

/// Most frames held back for interleaving at once.
const MAX_HELD: usize = 512;

/// Bytes of index laid out before they are written.
const CUE_BATCH_BYTES: usize = 4096;

/// Room the scratch holds from the start, so no header, block framing or index
/// batch grows it once frames flow.
const SCRATCH_BYTES: usize = CUE_BATCH_BYTES + 256;

/// Zeros the reservation is written with, a page at a time.
static ZEROS: [u8; 4096] = [0; 4096];

/// The channels of an Opus track: what RFC 7845's channel mapping family 0
/// states with no table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpusChannels {
    /// One channel.
    Mono,
    /// Two, left then right.
    Stereo,
}

impl OpusChannels {
    const fn count(self) -> u8 {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
        }
    }
}

/// A recording's Opus track, as its identification header (RFC 7845,
/// section 5.1) states it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpusTrack {
    /// Its channels.
    pub channels: OpusChannels,
    /// Samples at 48 kHz the decoder's output starts with that are not sound.
    pub pre_skip: u16,
    /// The rate the sound was captured at, which decoding does not use.
    pub input_rate: u32,
    /// The gain decoding applies, in Q7.8 dB.
    pub output_gain: i16,
}

/// What a recording holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Layout {
    /// The VP8 video's width in pixels.
    pub width: u32,
    /// Its height in pixels.
    pub height: u32,
    /// The Opus track, for a recording with sound.
    pub audio: Option<OpusTrack>,
    /// The most keyframes the index reserved at the start holds, at most
    /// [`MAX_INDEX_POINTS`]: the longest run expected over the keyframe
    /// interval.
    pub indexed_keyframes: u32,
}

/// Where a finished file's seek index went.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Placement {
    /// Whole, in the space reserved at the start.
    Reserved,
    /// Whole, after the last Cluster: it outgrew the reservation.
    End,
    /// Every `stride`-th keyframe, in the reservation: the whole index fitted
    /// neither there nor after the last Cluster, or outgrew what a reader
    /// keeps.
    Thinned {
        /// Keyframes to each point kept.
        stride: u32,
    },
    /// Nowhere: the recording holds no keyframe, or no room was reserved and
    /// none was left.
    Omitted,
}

/// A finished file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Finished {
    /// Bytes the file holds.
    pub len: u64,
    /// The duration it states, in nanoseconds of its millisecond timeline.
    pub duration_ns: u64,
    /// Where its seek index went.
    pub index: Placement,
    /// Whether a refused write cut the recording short: the file then ends at
    /// the last block written whole.
    pub cut_short: bool,
}

/// Why a muxer refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MuxError {
    /// The file refused a write or a cut. The muxer takes no more frames, and
    /// [`Muxer::finish`] ends the file at the last block written whole.
    Sink(Errno),
    /// An earlier write was refused; only [`Muxer::finish`] is left.
    Stopped,
    /// A video width or height of zero.
    VideoSize,
    /// More keyframes to reserve index room for than [`MAX_INDEX_POINTS`].
    IndexTooLarge,
    /// A sound frame for a recording without sound.
    NoAudioTrack,
    /// A first video frame that is not a keyframe: nothing before it could
    /// decode it.
    NoKeyframe,
    /// A frame earlier than its track's last.
    OutOfOrder,
    /// A frame of no bytes.
    EmptyFrame,
    /// A frame larger than [`MAX_FRAME_BYTES`], or an element larger than
    /// EBML states.
    TooLarge,
    /// A buffer the muxer needs was refused by the allocator; the frame was
    /// not taken.
    OutOfMemory,
}

impl core::fmt::Display for MuxError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let message = match self {
            Self::Sink(errno) => return write!(f, "the file refused a write: {errno}"),
            Self::Stopped => "a write was refused earlier",
            Self::VideoSize => "a video size of zero",
            Self::IndexTooLarge => "more keyframes to index than a file holds",
            Self::NoAudioTrack => "a sound frame for a recording without sound",
            Self::NoKeyframe => "a first video frame that is not a keyframe",
            Self::OutOfOrder => "a frame earlier than its track's last",
            Self::EmptyFrame => "a frame of no bytes",
            Self::TooLarge => "a frame larger than a file holds",
            Self::OutOfMemory => "out of memory",
        };
        f.write_str(message)
    }
}

/// A frame held back until the other track reaches it.
struct Held {
    timestamp_ns: u64,
    keyframe: bool,
    payload: Vec<u8>,
}

/// The Cluster being written.
#[derive(Clone, Copy)]
struct Open {
    start: u64,
    size_at: u64,
    data: u64,
    tick: u64,
}

/// A keyframe the index names: when, the Cluster it opens, and where its
/// block ends.
#[derive(Clone, Copy)]
struct Cue {
    tick: u64,
    cluster: u64,
    end: u64,
}

/// Where the fields finishing patches are, from the file's start.
#[derive(Clone, Copy)]
struct Marks {
    segment_size_at: u64,
    segment_data: u64,
    duration_at: u64,
    cue_seek_at: u64,
}

/// The space at the start the index is written into.
#[derive(Clone, Copy)]
struct Reserve {
    at: u64,
    len: u64,
}

/// An element to write, children and all: one definition of its bytes serves
/// for measuring it and for writing it.
#[derive(Clone, Copy)]
enum Node<'a> {
    /// An unsigned integer in the fewest bytes.
    Uint(u32, u64),
    /// An unsigned integer in eight bytes, so a later write can replace it in
    /// place.
    Wide(u32, u64),
    /// An eight-byte float.
    Float(u32, f64),
    /// Bytes as they are, a string's among them.
    Bytes(u32, &'a [u8]),
    /// An element ID as data, as a seek entry names its target.
    Id(u32, u32),
    /// A master element and its children.
    Master(u32, &'a [Node<'a>]),
    /// A `Void` element this many bytes long in all, zeros within: room a
    /// later write fills.
    Void(u64),
}

impl Node<'_> {
    /// Bytes of the element's data.
    fn data_len(&self) -> u64 {
        match *self {
            Self::Uint(_, value) => ebml::uint_len(value) as u64,
            Self::Wide(..) | Self::Float(..) => 8,
            Self::Bytes(_, bytes) => bytes.len() as u64,
            Self::Id(_, target) => ebml::id_len(target) as u64,
            Self::Master(_, children) => children.iter().map(Node::len).sum(),
            Self::Void(len) => void_size(len).map_or(0, |(_, data)| data),
        }
    }

    /// Bytes of the whole element.
    fn len(&self) -> u64 {
        let element = match *self {
            Self::Void(len) => return len,
            Self::Uint(element, _)
            | Self::Wide(element, _)
            | Self::Float(element, _)
            | Self::Bytes(element, _)
            | Self::Id(element, _)
            | Self::Master(element, _) => element,
        };
        let data = self.data_len();
        ebml::id_len(element) as u64 + ebml::size_len(data).unwrap_or(MAX_SIZE_LEN) as u64 + data
    }

    /// Append the element to `out`.
    fn put(&self, out: &mut Vec<u8>) -> Result<(), MuxError> {
        match *self {
            Self::Uint(element, value) => {
                put_element(out, element, ebml::uint_field(value).as_slice())
            }
            Self::Wide(element, value) => put_element(out, element, &value.to_be_bytes()),
            Self::Float(element, value) => put_element(out, element, &value.to_be_bytes()),
            Self::Bytes(element, bytes) => put_element(out, element, bytes),
            Self::Id(element, target) => {
                put_element(out, element, ebml::id_field(target).as_slice())
            }
            Self::Master(element, children) => {
                put_head(out, element, self.data_len())?;
                children.iter().try_for_each(|child| child.put(out))
            }
            Self::Void(len) => put_void(out, len, true),
        }
    }
}

/// A `WebM` file being written as its recording is made.
///
/// Every call names the file it writes to; the muxer never reads it back.
pub struct Muxer {
    audio: bool,
    marks: Marks,
    reserve: Reserve,
    /// The file's length as written.
    end: u64,
    /// Where the last block written whole ends.
    committed: u64,
    cluster: Option<Open>,
    cues: Vec<Cue>,
    /// Video keyframes written, and every how many of them the index names.
    keyframes: u64,
    cue_stride: u64,
    video: VecDeque<Held>,
    sound: VecDeque<Held>,
    last_video_ns: Option<u64>,
    last_audio_ns: Option<u64>,
    last_tick: Option<u64>,
    /// A write was refused: only finishing is left.
    stopped: bool,
    scratch: Vec<u8>,
}

impl Muxer {
    /// Start a file holding `layout` in `sink`, which is empty: its header,
    /// and the space its index is written into as it finishes.
    ///
    /// # Errors
    ///
    /// [`MuxError::VideoSize`] or [`MuxError::IndexTooLarge`] for a layout no
    /// file states, [`MuxError::Sink`] for a refused write, and
    /// [`MuxError::OutOfMemory`].
    pub fn begin(sink: &mut (impl Target + ?Sized), layout: &Layout) -> Result<Self, MuxError> {
        if layout.width == 0 || layout.height == 0 {
            return Err(MuxError::VideoSize);
        }
        if layout.indexed_keyframes > MAX_INDEX_POINTS {
            return Err(MuxError::IndexTooLarge);
        }
        let mut scratch = Vec::new();
        if !fallible::reserve(&mut scratch, SCRATCH_BYTES) {
            return Err(MuxError::OutOfMemory);
        }
        let marks = lay_out_head(&mut scratch, layout)?;
        sink.write_all_at(0, &scratch).map_err(MuxError::Sink)?;
        let reserve = Reserve {
            at: scratch.len() as u64,
            len: reserved_len(layout.indexed_keyframes),
        };
        if reserve.len > 0 {
            scratch.clear();
            put_void(&mut scratch, reserve.len, false)?;
            sink.write_all_at(reserve.at, &scratch)
                .map_err(MuxError::Sink)?;
            let head = scratch.len() as u64;
            write_zeros(sink, reserve.at + head, reserve.len - head)?;
        }
        let end = reserve.at + reserve.len;
        Ok(Self {
            audio: layout.audio.is_some(),
            marks,
            reserve,
            end,
            committed: end,
            cluster: None,
            cues: Vec::new(),
            keyframes: 0,
            cue_stride: 1,
            video: VecDeque::new(),
            sound: VecDeque::new(),
            last_video_ns: None,
            last_audio_ns: None,
            last_tick: None,
            stopped: false,
            scratch,
        })
    }

    /// Take a VP8 frame presented at `timestamp_ns`, written once the sound
    /// has reached it.
    ///
    /// # Errors
    ///
    /// [`MuxError::NoKeyframe`], [`MuxError::OutOfOrder`],
    /// [`MuxError::EmptyFrame`], [`MuxError::TooLarge`] or
    /// [`MuxError::OutOfMemory`] for a frame not taken; [`MuxError::Sink`] or
    /// [`MuxError::Stopped`] once the file has refused a write.
    pub fn push_video(
        &mut self,
        sink: &mut (impl Target + ?Sized),
        timestamp_ns: u64,
        keyframe: bool,
        payload: &[u8],
    ) -> Result<(), MuxError> {
        self.admit(self.last_video_ns, timestamp_ns, payload)?;
        if self.last_video_ns.is_none() && !keyframe {
            return Err(MuxError::NoKeyframe);
        }
        self.take(sink, true, timestamp_ns, keyframe, payload)
    }

    /// Take an Opus packet starting at `timestamp_ns`, written once the video
    /// has reached it.
    ///
    /// # Errors
    ///
    /// [`MuxError::NoAudioTrack`] for a recording without sound, and as
    /// [`Self::push_video`] does.
    pub fn push_audio(
        &mut self,
        sink: &mut (impl Target + ?Sized),
        timestamp_ns: u64,
        payload: &[u8],
    ) -> Result<(), MuxError> {
        if !self.audio {
            return Err(MuxError::NoAudioTrack);
        }
        self.admit(self.last_audio_ns, timestamp_ns, payload)?;
        self.take(sink, false, timestamp_ns, true, payload)
    }

    /// End the file: write what is held, then the index, the duration and the
    /// Segment's size. After a refused write the file is first cut back to the
    /// last block written whole, and its index goes into the reserved space,
    /// which needs none of the room the disk lacks.
    ///
    /// # Errors
    ///
    /// [`MuxError::Sink`] where the file refuses a write or the cut; what was
    /// written still reads as a live stream.
    pub fn finish(
        mut self,
        sink: &mut (impl Target + ?Sized),
        ends_at_ns: u64,
    ) -> Result<Finished, MuxError> {
        self.flush(sink);
        let cut_short = self.stopped;
        if cut_short {
            sink.set_len(self.committed).map_err(MuxError::Sink)?;
            self.end = self.committed;
            let committed = self.committed;
            self.cluster = self.cluster.filter(|open| open.start < committed);
        }
        self.close_cluster(sink)?;
        let index = self.write_index(sink, cut_short)?;
        let last = self.last_tick.unwrap_or(0);
        let ticks = if cut_short {
            last
        } else {
            (ends_at_ns / DEFAULT_TIMESTAMP_SCALE).max(last)
        };
        self.write_duration(sink, ticks)?;
        let segment = self.end - self.marks.segment_data;
        let size = ebml::size_field(segment, MAX_SIZE_LEN).ok_or(MuxError::TooLarge)?;
        self.write(sink, self.marks.segment_size_at, size.as_slice())?;
        Ok(Finished {
            len: self.end,
            duration_ns: ticks.saturating_mul(DEFAULT_TIMESTAMP_SCALE),
            index,
            cut_short,
        })
    }

    /// Refuse a frame the muxer cannot take: after a refused write, out of
    /// its track's order, or of a size no block holds.
    fn admit(
        &self,
        last_ns: Option<u64>,
        timestamp_ns: u64,
        payload: &[u8],
    ) -> Result<(), MuxError> {
        if self.stopped {
            return Err(MuxError::Stopped);
        }
        if payload.is_empty() {
            return Err(MuxError::EmptyFrame);
        }
        if payload.len() as u64 > u64::from(MAX_FRAME_BYTES) {
            return Err(MuxError::TooLarge);
        }
        if last_ns.is_some_and(|last| timestamp_ns < last) {
            return Err(MuxError::OutOfOrder);
        }
        Ok(())
    }

    /// Write the frame at once where nothing still to come can precede it,
    /// else hold a copy of it back; then write whatever it lets through.
    fn take(
        &mut self,
        sink: &mut (impl Target + ?Sized),
        video: bool,
        timestamp_ns: u64,
        keyframe: bool,
        payload: &[u8],
    ) -> Result<(), MuxError> {
        let (own, other) = if video {
            (&self.video, &self.sound)
        } else {
            (&self.sound, &self.video)
        };
        let direct = own.is_empty()
            && match other.front() {
                Some(head) if video => head.timestamp_ns >= timestamp_ns,
                Some(head) => head.timestamp_ns > timestamp_ns,
                None => self.alone_ready(video, timestamp_ns),
            };
        if direct {
            self.write_block(sink, video, timestamp_ns, keyframe, payload)?;
        } else {
            let frame = held(timestamp_ns, keyframe, payload)?;
            let queue = if video {
                &mut self.video
            } else {
                &mut self.sound
            };
            queue.try_reserve(1).map_err(|_| MuxError::OutOfMemory)?;
            queue.push_back(frame);
        }
        if video {
            self.last_video_ns = Some(timestamp_ns);
        } else {
            self.last_audio_ns = Some(timestamp_ns);
        }
        self.release(sink)
    }

    /// Whether the head of the only track holding frames, at `timestamp_ns`,
    /// may be written: the other track has reached it, it has waited the
    /// window, or too many are held. A video frame goes before sound at its
    /// own timestamp, so sound waits for video to pass it.
    fn alone_ready(&self, video: bool, timestamp_ns: u64) -> bool {
        let reached = if video {
            !self.audio || self.last_audio_ns.is_some_and(|last| last >= timestamp_ns)
        } else {
            self.last_video_ns.is_some_and(|last| last > timestamp_ns)
        };
        let newest = self
            .last_video_ns
            .max(self.last_audio_ns)
            .map_or(timestamp_ns, |newest| newest.max(timestamp_ns));
        reached
            || newest - timestamp_ns >= INTERLEAVE_WINDOW_NS
            || self.video.len() + self.sound.len() >= MAX_HELD
    }

    /// The track whose held head is earliest, video winning a tie, and that
    /// head's timestamp: `true` for video.
    fn earliest(&self) -> Option<(bool, u64)> {
        match (self.video.front(), self.sound.front()) {
            (Some(video), Some(sound)) if sound.timestamp_ns < video.timestamp_ns => {
                Some((false, sound.timestamp_ns))
            }
            (Some(video), _) => Some((true, video.timestamp_ns)),
            (None, Some(sound)) => Some((false, sound.timestamp_ns)),
            (None, None) => None,
        }
    }

    /// The track whose held head may be written now: the earlier head while
    /// both tracks hold frames, else the only head once it is ready.
    fn next_ready(&self) -> Option<bool> {
        let (video, timestamp_ns) = self.earliest()?;
        let both = !self.video.is_empty() && !self.sound.is_empty();
        (both || self.alone_ready(video, timestamp_ns)).then_some(video)
    }

    /// Write every held frame that may be written now.
    fn release(&mut self, sink: &mut (impl Target + ?Sized)) -> Result<(), MuxError> {
        while let Some(video) = self.next_ready() {
            self.write_held(sink, video)?;
        }
        Ok(())
    }

    /// Write every held frame in timestamp order, stopping at a refusal.
    fn flush(&mut self, sink: &mut (impl Target + ?Sized)) {
        while !self.stopped {
            let Some((video, _)) = self.earliest() else {
                return;
            };
            if self.write_held(sink, video).is_err() {
                self.stopped = true;
            }
        }
    }

    /// Write the head held for the video track, or the audio track.
    fn write_held(
        &mut self,
        sink: &mut (impl Target + ?Sized),
        video: bool,
    ) -> Result<(), MuxError> {
        let queue = if video {
            &mut self.video
        } else {
            &mut self.sound
        };
        let Some(frame) = queue.pop_front() else {
            return Ok(());
        };
        self.write_block(
            sink,
            video,
            frame.timestamp_ns,
            frame.keyframe,
            &frame.payload,
        )
    }

    /// Write one frame as a `SimpleBlock`, opening a Cluster first for a video
    /// keyframe or a timestamp too far from the open Cluster's.
    fn write_block(
        &mut self,
        sink: &mut (impl Target + ?Sized),
        video: bool,
        timestamp_ns: u64,
        keyframe: bool,
        payload: &[u8],
    ) -> Result<(), MuxError> {
        let tick = timestamp_ns / DEFAULT_TIMESTAMP_SCALE;
        let cue = video && keyframe;
        let within = self
            .cluster
            .filter(|_| !cue)
            .and_then(|open| offset(open.tick, tick));
        let relative = if let Some(relative) = within {
            relative
        } else {
            self.open_cluster(sink, tick)?;
            0
        };
        let track = if video { VIDEO_TRACK } else { AUDIO_TRACK };
        let flags = if keyframe { KEYFRAME_FLAG } else { 0 };
        self.scratch.clear();
        put(
            &mut self.scratch,
            ebml::id_field(id::SIMPLE_BLOCK).as_slice(),
        )?;
        put(
            &mut self.scratch,
            sized(payload.len() as u64 + 4)?.as_slice(),
        )?;
        put(&mut self.scratch, sized(u64::from(track))?.as_slice())?;
        put(&mut self.scratch, &relative.to_be_bytes())?;
        put(&mut self.scratch, &[flags])?;
        self.write_scratch(sink, None)?;
        self.append(sink, payload)?;
        self.committed = self.end;
        self.last_tick = Some(self.last_tick.map_or(tick, |last| last.max(tick)));
        if let Some(open) = self.cluster.filter(|_| cue) {
            self.note_cue(Cue {
                tick,
                cluster: open.start,
                end: self.end,
            });
        }
        Ok(())
    }

    /// Close the open Cluster and open one at `tick`, its size unknown until
    /// it closes.
    fn open_cluster(
        &mut self,
        sink: &mut (impl Target + ?Sized),
        tick: u64,
    ) -> Result<(), MuxError> {
        self.close_cluster(sink)?;
        self.scratch.clear();
        put(&mut self.scratch, ebml::id_field(id::CLUSTER).as_slice())?;
        put(&mut self.scratch, &UNKNOWN_SIZE)?;
        Node::Uint(id::CLUSTER_TIMESTAMP, tick).put(&mut self.scratch)?;
        let start = self.end;
        self.write_scratch(sink, None)?;
        let size_at = start + ebml::id_len(id::CLUSTER) as u64;
        self.cluster = Some(Open {
            start,
            size_at,
            data: size_at + UNKNOWN_SIZE.len() as u64,
            tick,
        });
        Ok(())
    }

    /// Patch the open Cluster's size to end at the last block written whole.
    fn close_cluster(&mut self, sink: &mut (impl Target + ?Sized)) -> Result<(), MuxError> {
        let Some(open) = self.cluster else {
            return Ok(());
        };
        let size = ebml::size_field(self.committed.saturating_sub(open.data), MAX_SIZE_LEN)
            .ok_or(MuxError::TooLarge)?;
        self.write(sink, open.size_at, size.as_slice())?;
        self.cluster = None;
        Ok(())
    }

    /// Name the keyframe `cue` opens in the index, which keeps every
    /// `cue_stride`-th: the stride doubles whenever the index would outgrow
    /// what a reader keeps or what memory grants, so the index still spans
    /// the whole recording.
    fn note_cue(&mut self, cue: Cue) {
        let ordinal = self.keyframes;
        self.keyframes += 1;
        if !ordinal.is_multiple_of(self.cue_stride) {
            return;
        }
        let full = self.cues.len() as u64 >= u64::from(MAX_INDEX_POINTS);
        if full || self.cues.try_reserve(1).is_err() {
            self.thin_cues();
            if !ordinal.is_multiple_of(self.cue_stride) {
                return;
            }
        }
        if self.cues.len() < self.cues.capacity() {
            self.cues.push(cue);
        }
    }

    /// Keep every other point of the index, and every other keyframe from now.
    fn thin_cues(&mut self) {
        let mut ordinal = 0usize;
        self.cues.retain(|_| {
            ordinal += 1;
            ordinal % 2 == 1
        });
        self.cue_stride = self.cue_stride.saturating_mul(2);
    }

    /// Write `bytes` at the file's end.
    fn append(&mut self, sink: &mut (impl Target + ?Sized), bytes: &[u8]) -> Result<(), MuxError> {
        let at = self.end;
        self.write(sink, at, bytes)?;
        self.end = at + bytes.len() as u64;
        Ok(())
    }

    /// Write `bytes` at `at`; a refusal stops the muxer.
    fn write(
        &mut self,
        sink: &mut (impl Target + ?Sized),
        at: u64,
        bytes: &[u8],
    ) -> Result<(), MuxError> {
        sink.write_all_at(at, bytes).map_err(|errno| {
            self.stopped = true;
            MuxError::Sink(errno)
        })
    }

    /// Write the scratch at `at`, or at the file's end.
    fn write_scratch(
        &mut self,
        sink: &mut (impl Target + ?Sized),
        at: Option<u64>,
    ) -> Result<(), MuxError> {
        let bytes = core::mem::take(&mut self.scratch);
        let written = match at {
            Some(at) => self.write(sink, at, &bytes),
            None => self.append(sink, &bytes),
        };
        self.scratch = bytes;
        written
    }

    /// Write the index where it fits: whole into the reservation, else whole
    /// after the last Cluster, else thinned into the reservation.
    fn write_index(
        &mut self,
        sink: &mut (impl Target + ?Sized),
        cut_short: bool,
    ) -> Result<Placement, MuxError> {
        let committed = self.committed;
        self.cues.retain(|cue| cue.end <= committed);
        if self.cues.is_empty() {
            return Ok(Placement::Omitted);
        }
        let reserve = self.reserve;
        if self.cues_len(1) <= reserve.len {
            self.write_cues(sink, reserve.at, 1, Some(reserve.len))?;
            self.write_cue_seek(sink, reserve.at)?;
            return Ok(self.placement(1, Placement::Reserved));
        }
        if !cut_short {
            let at = self.end;
            match self.write_cues(sink, at, 1, None) {
                Ok(len) => {
                    self.end = at + len;
                    self.write_cue_seek(sink, at)?;
                    return Ok(self.placement(1, Placement::End));
                }
                Err(MuxError::Sink(_)) => sink.set_len(at).map_err(MuxError::Sink)?,
                Err(refused) => return Err(refused),
            }
        }
        let capacity = reserve.len.saturating_sub(cues_head_len()) / widest_cue_point();
        if capacity == 0 {
            return Ok(Placement::Omitted);
        }
        let stride = (self.cues.len() as u64).div_ceil(capacity);
        self.write_cues(sink, reserve.at, stride, Some(reserve.len))?;
        self.write_cue_seek(sink, reserve.at)?;
        Ok(self.placement(stride, Placement::Reserved))
    }

    /// Where an index went that keeps every `stride`-th keyframe held:
    /// `whole` unless that stride or the recording thinned it.
    fn placement(&self, stride: u64, whole: Placement) -> Placement {
        match self.cue_stride.saturating_mul(stride) {
            1 => whole,
            total => Placement::Thinned {
                stride: u32::try_from(total).unwrap_or(u32::MAX),
            },
        }
    }

    /// Bytes of the cue points an index keeping every `stride`-th holds.
    fn cue_points_len(&self, stride: usize) -> u64 {
        let origin = self.marks.segment_data;
        self.cues
            .iter()
            .step_by(stride.max(1))
            .map(|cue| with_cue_point(cue, origin, |point| point.len()))
            .sum()
    }

    /// Bytes of the whole index keeping every `stride`-th point.
    fn cues_len(&self, stride: usize) -> u64 {
        let data = self.cue_points_len(stride);
        let width = ebml::size_len(data).unwrap_or(MAX_SIZE_LEN);
        ebml::id_len(id::CUES) as u64 + width as u64 + data
    }

    /// Write an index keeping every `stride`-th keyframe held at `at`, filling
    /// `room` bytes where it is given, and answer how long it is.
    fn write_cues(
        &mut self,
        sink: &mut (impl Target + ?Sized),
        at: u64,
        stride: u64,
        room: Option<u64>,
    ) -> Result<u64, MuxError> {
        let stride = usize::try_from(stride).unwrap_or(usize::MAX).max(1);
        let data = self.cue_points_len(stride);
        let mut width = ebml::size_len(data).ok_or(MuxError::TooLarge)?;
        let mut len = ebml::id_len(id::CUES) as u64 + width as u64 + data;
        if room == Some(len + 1) {
            width += 1;
            len += 1;
        }
        if room.is_some_and(|room| len > room) {
            return Err(MuxError::TooLarge);
        }
        let size = ebml::size_field(data, width).ok_or(MuxError::TooLarge)?;
        self.scratch.clear();
        put(&mut self.scratch, ebml::id_field(id::CUES).as_slice())?;
        put(&mut self.scratch, size.as_slice())?;
        let origin = self.marks.segment_data;
        let mut written = at;
        for index in (0..self.cues.len()).step_by(stride) {
            let Some(&cue) = self.cues.get(index) else {
                break;
            };
            if self.scratch.len() >= CUE_BATCH_BYTES {
                self.write_scratch(sink, Some(written))?;
                written += self.scratch.len() as u64;
                self.scratch.clear();
            }
            with_cue_point(&cue, origin, |point| point.put(&mut self.scratch))?;
        }
        self.write_scratch(sink, Some(written))?;
        written += self.scratch.len() as u64;
        if let Some(gap) = room.map(|room| room - len).filter(|&gap| gap > 0) {
            self.scratch.clear();
            put_void(&mut self.scratch, gap, false)?;
            self.write_scratch(sink, Some(written))?;
        }
        Ok(len)
    }

    /// Point the seek head's reserved entry at the index written at `at`.
    fn write_cue_seek(
        &mut self,
        sink: &mut (impl Target + ?Sized),
        at: u64,
    ) -> Result<(), MuxError> {
        let fields = [
            Node::Id(id::SEEK_ID, id::CUES),
            Node::Wide(id::SEEK_POSITION, at - self.marks.segment_data),
        ];
        self.scratch.clear();
        Node::Master(id::SEEK, &fields).put(&mut self.scratch)?;
        self.write_scratch(sink, Some(self.marks.cue_seek_at))
    }

    /// Write the duration into the room Info keeps for it.
    #[allow(
        clippy::cast_precision_loss,
        reason = "a count of milliseconds is exact as a double below 2^53, some 285,000 years"
    )]
    fn write_duration(
        &mut self,
        sink: &mut (impl Target + ?Sized),
        ticks: u64,
    ) -> Result<(), MuxError> {
        self.scratch.clear();
        Node::Float(id::DURATION, ticks as f64).put(&mut self.scratch)?;
        self.write_scratch(sink, Some(self.marks.duration_at))
    }
}

/// `tick` as an offset from a Cluster's `base`, where a block's signed 16 bits
/// reach it.
fn offset(base: u64, tick: u64) -> Option<i16> {
    i16::try_from(i128::from(tick) - i128::from(base)).ok()
}

/// A copy of a frame to hold back.
fn held(timestamp_ns: u64, keyframe: bool, payload: &[u8]) -> Result<Held, MuxError> {
    let mut copy = Vec::new();
    if !fallible::reserve(&mut copy, payload.len()) {
        return Err(MuxError::OutOfMemory);
    }
    copy.extend_from_slice(payload);
    Ok(Held {
        timestamp_ns,
        keyframe,
        payload: copy,
    })
}

/// Append `bytes` to `out`, reporting a growth the allocator refuses.
fn put(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), MuxError> {
    out.try_reserve(bytes.len())
        .map_err(|_| MuxError::OutOfMemory)?;
    out.extend_from_slice(bytes);
    Ok(())
}

/// `value` in the fewest-byte size field.
fn sized(value: u64) -> Result<Written, MuxError> {
    ebml::size_len(value)
        .and_then(|len| ebml::size_field(value, len))
        .ok_or(MuxError::TooLarge)
}

/// Append an element's ID and a size field for `data_len` bytes.
fn put_head(out: &mut Vec<u8>, element: u32, data_len: u64) -> Result<(), MuxError> {
    put(out, ebml::id_field(element).as_slice())?;
    put(out, sized(data_len)?.as_slice())
}

/// Append an element holding `data`.
fn put_element(out: &mut Vec<u8>, element: u32, data: &[u8]) -> Result<(), MuxError> {
    put_head(out, element, data.len() as u64)?;
    put(out, data)
}

/// Append a `Void` element `len` bytes long in all, and its zeros where
/// `body`.
fn put_void(out: &mut Vec<u8>, len: u64, body: bool) -> Result<(), MuxError> {
    let (size, data) = void_size(len).ok_or(MuxError::TooLarge)?;
    put(out, ebml::id_field(id::VOID).as_slice())?;
    put(out, size.as_slice())?;
    if body {
        let zeros = usize::try_from(data).map_err(|_| MuxError::TooLarge)?;
        out.try_reserve(zeros).map_err(|_| MuxError::OutOfMemory)?;
        out.resize(out.len() + zeros, 0);
    }
    Ok(())
}

/// The size field of a `Void` element `len` bytes long in all and the bytes
/// it holds, `None` for a length no `Void` has.
fn void_size(len: u64) -> Option<(Written, u64)> {
    let id_bytes = ebml::id_len(id::VOID) as u64;
    (1..=MAX_SIZE_LEN).find_map(|width| {
        let data = len.checked_sub(id_bytes + width as u64)?;
        ebml::size_field(data, width).map(|field| (field, data))
    })
}

/// Write `count` zero bytes at `at`.
fn write_zeros(sink: &mut (impl Target + ?Sized), at: u64, count: u64) -> Result<(), MuxError> {
    let mut done = 0;
    while done < count {
        let left = usize::try_from(count - done).unwrap_or(usize::MAX);
        let zeros = &ZEROS[..left.min(ZEROS.len())];
        sink.write_all_at(at + done, zeros)
            .map_err(MuxError::Sink)?;
        done += zeros.len() as u64;
    }
    Ok(())
}

/// Bytes of a `Cues` element's header at its widest.
fn cues_head_len() -> u64 {
    (ebml::id_len(id::CUES) + MAX_SIZE_LEN) as u64
}

/// Bytes of a cue point at its widest: an eight-byte time and position.
fn widest_cue_point() -> u64 {
    let widest = Cue {
        tick: u64::MAX,
        cluster: u64::MAX,
        end: 0,
    };
    with_cue_point(&widest, 0, |point| point.len())
}

/// Bytes reserved for an index naming `keyframes`, each at its widest.
fn reserved_len(keyframes: u32) -> u64 {
    if keyframes == 0 {
        return 0;
    }
    cues_head_len() + u64::from(keyframes) * widest_cue_point()
}

/// Hand `use_point` the cue point naming `cue`, its Cluster's position
/// counted from `origin`.
fn with_cue_point<R>(cue: &Cue, origin: u64, use_point: impl FnOnce(Node<'_>) -> R) -> R {
    let positions = [
        Node::Uint(id::CUE_TRACK, u64::from(VIDEO_TRACK)),
        Node::Uint(id::CUE_CLUSTER_POSITION, cue.cluster.saturating_sub(origin)),
    ];
    let point = [
        Node::Uint(id::CUE_TIME, cue.tick),
        Node::Master(id::CUE_TRACK_POSITIONS, &positions),
    ];
    use_point(Node::Master(id::CUE_POINT, &point))
}

/// Bytes of a seek entry, its position written eight bytes wide whatever it
/// holds.
fn seek_entry_len() -> u64 {
    let fields = [
        Node::Id(id::SEEK_ID, id::CUES),
        Node::Wide(id::SEEK_POSITION, 0),
    ];
    Node::Master(id::SEEK, &fields).len()
}

/// Lay the file's header out in `out` (the EBML header, the Segment's
/// opening, its seek head, Info and Tracks), answering where finishing
/// patches it.
fn lay_out_head(out: &mut Vec<u8>, layout: &Layout) -> Result<Marks, MuxError> {
    let header = [
        Node::Uint(id::EBML_VERSION, 1),
        Node::Uint(id::EBML_READ_VERSION, 1),
        Node::Uint(id::EBML_MAX_ID_LENGTH, MAX_ID_LEN as u64),
        Node::Uint(id::EBML_MAX_SIZE_LENGTH, MAX_SIZE_LEN as u64),
        Node::Bytes(id::DOC_TYPE, DOC_TYPE.as_bytes()),
        Node::Uint(id::DOC_TYPE_VERSION, DOC_TYPE_VERSION),
        Node::Uint(id::DOC_TYPE_READ_VERSION, DOC_TYPE_READ_VERSION),
    ];
    Node::Master(id::EBML, &header).put(out)?;
    put(out, ebml::id_field(id::SEGMENT).as_slice())?;
    let segment_size_at = out.len() as u64;
    put(out, &UNKNOWN_SIZE)?;
    let segment_data = out.len() as u64;
    let duration = Node::Float(id::DURATION, 0.0).len();
    let info_fields = [
        Node::Uint(id::TIMESTAMP_SCALE, DEFAULT_TIMESTAMP_SCALE),
        Node::Bytes(id::MUXING_APP, APP.as_bytes()),
        Node::Bytes(id::WRITING_APP, APP.as_bytes()),
        Node::Void(duration),
    ];
    let info = Node::Master(id::INFO, &info_fields);
    let entry = seek_entry_len();
    let seek_head = Node::Master(id::SEEK_HEAD, &[Node::Void(entry); 3]).len();
    let info_seek = [
        Node::Id(id::SEEK_ID, id::INFO),
        Node::Wide(id::SEEK_POSITION, seek_head),
    ];
    let tracks_seek = [
        Node::Id(id::SEEK_ID, id::TRACKS),
        Node::Wide(id::SEEK_POSITION, seek_head + info.len()),
    ];
    let entries = [
        Node::Master(id::SEEK, &info_seek),
        Node::Master(id::SEEK, &tracks_seek),
        Node::Void(entry),
    ];
    Node::Master(id::SEEK_HEAD, &entries).put(out)?;
    info.put(out)?;
    put_tracks(out, layout)?;
    Ok(Marks {
        segment_size_at,
        segment_data,
        duration_at: segment_data + seek_head + info.len() - duration,
        cue_seek_at: segment_data + seek_head - entry,
    })
}

/// Append the Tracks element: the VP8 track, and the Opus track where the
/// recording has sound.
fn put_tracks(out: &mut Vec<u8>, layout: &Layout) -> Result<(), MuxError> {
    let picture = [
        Node::Uint(id::PIXEL_WIDTH, u64::from(layout.width)),
        Node::Uint(id::PIXEL_HEIGHT, u64::from(layout.height)),
    ];
    let video_fields = [
        Node::Uint(id::TRACK_NUMBER, u64::from(VIDEO_TRACK)),
        Node::Uint(id::TRACK_UID, u64::from(VIDEO_TRACK)),
        Node::Uint(id::TRACK_TYPE, u64::from(VIDEO_TYPE)),
        Node::Uint(id::FLAG_LACING, 0),
        Node::Bytes(id::CODEC_ID, VP8_CODEC.as_bytes()),
        Node::Master(id::VIDEO, &picture),
    ];
    let video = Node::Master(id::TRACK_ENTRY, &video_fields);
    let Some(opus) = layout.audio else {
        return Node::Master(id::TRACKS, &[video]).put(out);
    };
    let head = opus_head(opus);
    let sound = [
        Node::Float(id::SAMPLING_FREQUENCY, f64::from(OPUS_RATE)),
        Node::Uint(id::CHANNELS, u64::from(opus.channels.count())),
    ];
    let audio_fields = [
        Node::Uint(id::TRACK_NUMBER, u64::from(AUDIO_TRACK)),
        Node::Uint(id::TRACK_UID, u64::from(AUDIO_TRACK)),
        Node::Uint(id::TRACK_TYPE, u64::from(AUDIO_TYPE)),
        Node::Uint(id::FLAG_LACING, 0),
        Node::Bytes(id::CODEC_ID, OPUS_CODEC.as_bytes()),
        Node::Bytes(id::CODEC_PRIVATE, &head),
        Node::Uint(id::CODEC_DELAY, codec_delay_ns(opus.pre_skip)),
        Node::Uint(id::SEEK_PRE_ROLL, SEEK_PRE_ROLL_NS),
        Node::Master(id::AUDIO, &sound),
    ];
    let audio = Node::Master(id::TRACK_ENTRY, &audio_fields);
    Node::Master(id::TRACKS, &[video, audio]).put(out)
}

/// The identification header (RFC 7845, section 5.1) of `opus`, in channel
/// mapping family 0.
fn opus_head(opus: OpusTrack) -> [u8; 19] {
    let mut head = [0; 19];
    head[..8].copy_from_slice(b"OpusHead");
    head[8] = 1;
    head[9] = opus.channels.count();
    head[10..12].copy_from_slice(&opus.pre_skip.to_le_bytes());
    head[12..16].copy_from_slice(&opus.input_rate.to_le_bytes());
    head[16..18].copy_from_slice(&opus.output_gain.to_le_bytes());
    head
}

/// `pre_skip` samples at 48 kHz in nanoseconds, to the nearest: the Opus
/// mapping's `CodecDelay`.
fn codec_delay_ns(pre_skip: u16) -> u64 {
    let rate = u64::from(OPUS_RATE);
    (u64::from(pre_skip) * NS_PER_SECOND + rate / 2) / rate
}
