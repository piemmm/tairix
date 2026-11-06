//! Reading a `WebM` file a frame at a time.

mod index;
mod opening;

use alloc::vec::Vec;

use tairix_inline::ArrayString;
use tairix_sound::{InputError, SoundInput, MAX_READ};
use tairix_util::fallible;

use self::index::Index;
use super::ebml::{self, Field, Size, MAX_HEAD_LEN, MAX_ID_LEN, MAX_SIZE_LEN};
use super::id;
use super::{KEYFRAME_FLAG, MAX_CODEC_ID, MAX_FRAME_BYTES};

/// Most children a block group holds.
const MAX_GROUP_CHILDREN: usize = 64;

/// Most bytes a laced block's frame sizes take.
const MAX_LACE_HEAD: usize = 8 * 1024;

/// Most bytes of a block's track, timestamp, flags and lace count.
const MAX_BLOCK_HEAD: usize = MAX_SIZE_LEN + 4;

/// The flag bits a `SimpleBlock` reserves.
const SIMPLE_RESERVED: u8 = 0x70;

/// The flag bits a block group's block reserves: it has no keyframe flag.
const GROUP_RESERVED: u8 = 0xF0;

/// The lacing a block's flags name: Xiph's, fixed sizes, or EBML's.
const XIPH_LACING: u8 = 1;
const FIXED_LACING: u8 = 2;

/// What a track's frames are coded as.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Codec {
    /// VP8 video (`V_VP8`).
    Vp8,
    /// Opus audio (`A_OPUS`), its identification header the track's codec
    /// private data.
    Opus,
    /// Any other codec, or frames the file compresses or encrypts: named by
    /// [`Track::codec_id`], and not decoded here.
    Other,
}

/// What a track carries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Media {
    /// Pictures.
    Video {
        /// Their width in pixels.
        width: u32,
        /// Their height in pixels.
        height: u32,
    },
    /// Sound.
    Audio {
        /// Samples a second.
        rate: u32,
        /// Channels.
        channels: u8,
    },
    /// A track of another Matroska type: subtitles, buttons, metadata.
    Other(u8),
}

/// A track the file declares.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Track {
    /// The number its blocks name it by.
    pub number: u64,
    /// What its frames are coded as.
    pub codec: Codec,
    /// The codec identifier the file states, its first [`MAX_CODEC_ID`] bytes.
    pub codec_id: ArrayString<MAX_CODEC_ID>,
    /// What it carries.
    pub media: Media,
    /// The codec's own setup: an Opus track's identification header.
    pub codec_private: Vec<u8>,
    /// Nanoseconds the decoded output is delayed by, discarded from its start.
    pub codec_delay_ns: u64,
    /// Nanoseconds before a seek's target a decode starts, to converge on it.
    pub seek_pre_roll_ns: u64,
}

/// One coded frame, and where its payload is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Frame {
    /// The track it belongs to.
    pub track: u64,
    /// When it is presented, in nanoseconds. A laced block's frames all carry
    /// the first's, the rest following it in decode order.
    pub timestamp_ns: u64,
    /// Whether it decodes without any frame before it.
    pub keyframe: bool,
    /// Where its payload starts in the file.
    pub offset: u64,
    /// Bytes its payload holds.
    pub len: u32,
    /// Nanoseconds of decoded output to discard from its end, or from its
    /// start where negative.
    pub discard_padding_ns: i64,
}

/// How a walk through a file's frames ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ending {
    /// At the end of its Segment.
    Complete,
    /// Short of what it declares, or inside an element: a file cut off.
    Truncated,
}

/// Why a file could not be read. Every refusal is fail-closed: no malformed,
/// truncated or adversarial file panics or yields a frame it does not hold.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DemuxError {
    /// The input does not hold the bytes yet; asked again once it does, the
    /// call answers as it would have.
    InputUnavailable,
    /// The input failed.
    InputFailed,
    /// A buffer the read needs was refused by the allocator: a property of the
    /// machine, not of the file.
    OutOfMemory,
    /// No EBML header, or one naming a document other than `WebM`.
    NotWebm,
    /// An EBML or Matroska version this reader predates.
    UnsupportedVersion,
    /// The file ends inside the elements ahead of its first Cluster.
    Truncated,
    /// An element ID EBML forbids.
    BadId,
    /// An element size EBML forbids, or one wider than the header allows.
    BadSize,
    /// An unknown size on an element other than the Segment or a Cluster.
    UnknownSize,
    /// An element running past the element holding it.
    Overrun,
    /// An element larger than this reader holds.
    TooLarge,
    /// More elements ahead of the first Cluster, tracks, or children of a block
    /// group than this reader holds.
    TooMany,
    /// A number, float, string or flag that does not hold together.
    BadValue,
    /// An element the format requires is missing: Info, Tracks, a track's
    /// number, type or codec, a block group's block, or a cue point's time or
    /// position.
    Missing,
    /// An element the format allows once appears twice.
    Duplicated,
    /// An element where the format allows none: a Segment's element inside a
    /// Cluster of known size, or a new document inside a Segment of known
    /// size.
    Misplaced,
    /// A block ahead of its Cluster's timestamp.
    Untimed,
    /// A track whose codec its type cannot carry, or an Opus track without its
    /// identification header.
    BadTrack,
    /// A block naming a track the file never declares.
    UndeclaredTrack,
    /// A block whose lacing does not account for its bytes.
    BadLacing,
    /// A timestamp before the timeline's start, or past what nanoseconds hold.
    TimestampOutOfRange,
    /// A seek index that does not hold together. It is dropped, and the next
    /// seek walks the file instead.
    BadIndex,
}

impl core::fmt::Display for DemuxError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::InputUnavailable => "the input does not hold the bytes yet",
            Self::InputFailed => "the input failed",
            Self::OutOfMemory => "out of memory",
            Self::NotWebm => "not a WebM file",
            Self::UnsupportedVersion => "a WebM version this reader predates",
            Self::Truncated => "the file ends inside its header",
            Self::BadId => "an element ID EBML forbids",
            Self::BadSize => "an element size EBML forbids",
            Self::UnknownSize => "an unknown size where the format requires one",
            Self::Overrun => "an element running past the one holding it",
            Self::TooLarge => "an element larger than this reader holds",
            Self::TooMany => "more elements than this reader holds",
            Self::BadValue => "a value that does not hold together",
            Self::Missing => "an element the format requires is missing",
            Self::Duplicated => "an element the format allows once appears twice",
            Self::Misplaced => "an element where the format allows none",
            Self::Untimed => "a block ahead of its cluster's timestamp",
            Self::BadTrack => "a track whose codec its type cannot carry",
            Self::UndeclaredTrack => "a block for a track the file never declares",
            Self::BadLacing => "a block whose lacing does not account for its bytes",
            Self::TimestampOutOfRange => "a timestamp out of range",
            Self::BadIndex => "a seek index that does not hold together",
        })
    }
}

impl From<InputError> for DemuxError {
    fn from(error: InputError) -> Self {
        match error {
            InputError::Unavailable => Self::InputUnavailable,
            InputError::Failed => Self::InputFailed,
        }
    }
}

/// A `WebM` file being read a frame at a time.
///
/// The demuxer holds no input: each call that reads names the one it was
/// opened over, so a caller that met [`DemuxError::InputUnavailable`] can
/// supply the bytes and call again.
pub struct Demuxer {
    file: Opened,
    walk: Walk,
    lace: Lace,
    ending: Option<Ending>,
    index: Index,
    scratch: Vec<u8>,
}

impl Demuxer {
    /// Open the file `input` holds: its header, its tracks and the elements
    /// ahead of its first Cluster.
    ///
    /// # Errors
    ///
    /// [`DemuxError::InputUnavailable`] with nothing kept, or the refusal of a
    /// file that does not hold together.
    pub fn open(input: &mut (impl SoundInput + ?Sized)) -> Result<Self, DemuxError> {
        let mut scratch = Vec::new();
        let (file, cues) = opening::open(input, &mut scratch)?;
        let walk = Walk::at(file.body);
        let index = Index::new(&file, cues);
        Ok(Self {
            file,
            walk,
            lace: Lace::default(),
            ending: None,
            index,
            scratch,
        })
    }

    /// The tracks the file declares.
    #[must_use]
    pub fn tracks(&self) -> &[Track] {
        &self.file.tracks
    }

    /// The duration the file states, in nanoseconds.
    #[must_use]
    pub const fn duration_ns(&self) -> Option<u64> {
        self.file.duration_ns
    }

    /// How the walk through the frames ended, once it has.
    #[must_use]
    pub const fn ending(&self) -> Option<Ending> {
        self.ending
    }

    /// Whether seeks are served from the file's own index rather than by
    /// walking its Clusters: an index named but not yet read counts, and one
    /// found wanting does not.
    #[must_use]
    pub const fn has_index(&self) -> bool {
        self.index.read_from_cues()
    }

    /// The next frame in file order, `None` once the walk has ended.
    ///
    /// # Errors
    ///
    /// [`DemuxError::InputUnavailable`], answered as it would have been when
    /// asked again once the bytes are there, or the refusal of an element that
    /// does not hold together.
    pub fn next_frame(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
    ) -> Result<Option<Frame>, DemuxError> {
        if let Some(frame) = self.lace.next_laced() {
            return Ok(Some(frame));
        }
        while self.ending.is_none() {
            match self.walk.step(input, &self.file)? {
                Step::Block { element, tick } => {
                    let frame = self.block(input, element, tick)?;
                    self.walk.at = element.end;
                    return Ok(Some(frame));
                }
                Step::Ended(ending) => self.ending = Some(ending),
                Step::Entered | Step::Timed { .. } | Step::Skipped | Step::Left => {}
            }
        }
        Ok(None)
    }

    /// Move to the latest Cluster the index places at or before `target_ns`,
    /// so the frames that follow lead up to it: in a file that indexes its
    /// video, a keyframe's. A file without an index is walked a Cluster at a
    /// time as far as the target, and what the walk finds serves later seeks.
    ///
    /// # Errors
    ///
    /// [`DemuxError::InputUnavailable`], keeping what was read;
    /// [`DemuxError::BadIndex`] for an index that does not hold together,
    /// which is dropped so the next seek walks the file; or the refusal of an
    /// element the walk meets.
    pub fn seek(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        target_ns: u64,
    ) -> Result<(), DemuxError> {
        let target = target_ns / self.file.scale;
        self.index
            .fill(input, &self.file, target, &mut self.scratch)?;
        let landing = self.index.landing(target).unwrap_or(self.file.body);
        if landing != self.file.body && !lands(input, &self.file, landing)? {
            self.index.drop_cues(&self.file);
            return Err(DemuxError::BadIndex);
        }
        self.walk = Walk::at(landing);
        self.lace.pending = None;
        self.ending = None;
        Ok(())
    }

    /// The first frame of the block `element` holds; the rest of a lace wait
    /// for the calls after.
    fn block(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        element: Element,
        tick: u64,
    ) -> Result<Frame, DemuxError> {
        let span = Span {
            at: element.data,
            end: element.end,
        };
        let work = &mut self.lace.work;
        let (head, discard) = if element.id == id::SIMPLE_BLOCK {
            (read_block(input, &self.file, span, tick, true, work)?, 0)
        } else {
            read_group(input, &self.file, span, tick, work)?
        };
        let len = *work.sizes.first().ok_or(DemuxError::BadLacing)?;
        let only = work.sizes.len() == 1;
        let first = Frame {
            track: head.track,
            timestamp_ns: head.timestamp_ns,
            keyframe: head.keyframe,
            offset: head.payload,
            len,
            discard_padding_ns: if only { discard } else { 0 },
        };
        if !only {
            self.lace.pending = Some(Pending {
                template: first,
                next: 1,
                offset: head.payload + u64::from(len),
                discard,
            });
        }
        Ok(first)
    }
}

/// The widest element ID and size the file's header allows.
#[derive(Clone, Copy, Debug)]
struct Widths {
    id: usize,
    size: usize,
}

impl Widths {
    const WIDEST: Self = Self {
        id: MAX_ID_LEN,
        size: MAX_SIZE_LEN,
    };
}

/// Where the Segment's data starts, and where it ends where its size is
/// known.
#[derive(Clone, Copy, Debug)]
struct Segment {
    data: u64,
    end: Option<u64>,
}

impl Segment {
    /// Where reading the Segment stops in a file `file_len` bytes long.
    fn limit(&self, file_len: u64) -> u64 {
        self.end.map_or(file_len, |end| end.min(file_len))
    }

    /// Whether the file stops short of the Segment's declared end.
    fn cut_short(&self, file_len: u64) -> bool {
        self.end.is_some_and(|end| end > file_len)
    }
}

/// What opening a file read: everything a later call needs.
struct Opened {
    widths: Widths,
    segment: Segment,
    /// Nanoseconds a tick of the timeline stands for.
    scale: u64,
    duration_ns: Option<u64>,
    tracks: Vec<Track>,
    /// Where the walk through the Clusters starts: the first Cluster, or where
    /// the elements ahead of one ran out.
    body: u64,
    /// The track a seek lands on a point of: the first video track, else the
    /// first.
    seek_track: u64,
}

impl Opened {
    /// Nanoseconds from the timeline's start of a block `relative` ticks from
    /// its Cluster's `tick`.
    fn timestamp_ns(&self, tick: u64, relative: i16) -> Result<u64, DemuxError> {
        let tick = i128::from(tick) + i128::from(relative);
        u64::try_from(tick)
            .ok()
            .and_then(|tick| tick.checked_mul(self.scale))
            .ok_or(DemuxError::TimestampOutOfRange)
    }

    /// Whether the file declares track `number`.
    fn declares(&self, number: u64) -> bool {
        self.tracks.iter().any(|track| track.number == number)
    }
}

/// An element header: its ID, its size, and how many bytes the two take.
#[derive(Clone, Copy, Debug)]
struct Head {
    id: u32,
    size: Size,
    len: u64,
}

/// Fill `buf` from `at`, answering how many bytes the file held there.
fn read(
    input: &mut (impl SoundInput + ?Sized),
    at: u64,
    buf: &mut [u8],
) -> Result<usize, DemuxError> {
    Ok(input.read_at(at, buf)?)
}

/// Fill `buf` from `at` whole: its bytes are already held to the file's
/// length, so a shortfall is the input failing.
fn read_exact(
    input: &mut (impl SoundInput + ?Sized),
    at: u64,
    buf: &mut [u8],
) -> Result<(), DemuxError> {
    if read(input, at, buf)? == buf.len() {
        Ok(())
    } else {
        Err(DemuxError::InputFailed)
    }
}

/// The element header at `at`, read no further than `limit`: `None` where
/// `limit` cuts it short.
fn read_head(
    input: &mut (impl SoundInput + ?Sized),
    at: u64,
    limit: u64,
    widths: Widths,
) -> Result<Option<Head>, DemuxError> {
    let mut buf = [0; MAX_HEAD_LEN];
    let room = usize::try_from(limit.saturating_sub(at))
        .map_or(MAX_HEAD_LEN, |room| room.min(MAX_HEAD_LEN));
    let held = read(input, at, &mut buf[..room])?;
    let bytes = buf.get(..held).ok_or(DemuxError::InputFailed)?;
    let (element, id_len) = match ebml::read_id(bytes, widths.id) {
        Ok(found) => found,
        Err(Field::Short) => return Ok(None),
        Err(Field::Bad) => return Err(DemuxError::BadId),
    };
    let after_id = bytes.get(id_len..).unwrap_or_default();
    let (size, size_len) = match ebml::read_size(after_id, widths.size) {
        Ok(found) => found,
        Err(Field::Short) => return Ok(None),
        Err(Field::Bad) => return Err(DemuxError::BadSize),
    };
    Ok(Some(Head {
        id: element,
        size,
        len: (id_len + size_len) as u64,
    }))
}

/// A size the element must state.
fn known(size: Size) -> Result<u64, DemuxError> {
    match size {
        Size::Known(size) => Ok(size),
        Size::Unknown => Err(DemuxError::UnknownSize),
    }
}

/// The elements a master element's data holds, in order.
struct Children<'a> {
    rest: &'a [u8],
    widths: Widths,
}

impl<'a> Children<'a> {
    const fn new(data: &'a [u8], widths: Widths) -> Self {
        Self { rest: data, widths }
    }
}

impl<'a> Iterator for Children<'a> {
    type Item = Result<(u32, &'a [u8]), DemuxError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let child = split_child(self.rest, self.widths);
        self.rest = match child {
            Ok((_, _, rest)) => rest,
            Err(_) => &[],
        };
        Some(child.map(|(element, data, _)| (element, data)))
    }
}

/// The element opening `bytes`: its ID, its data, and the bytes after it.
fn split_child(bytes: &[u8], widths: Widths) -> Result<(u32, &[u8], &[u8]), DemuxError> {
    let (element, id_len) =
        ebml::read_id(bytes, widths.id).map_err(|field| refusal(field, DemuxError::BadId))?;
    let after_id = bytes.get(id_len..).unwrap_or_default();
    let (size, size_len) = ebml::read_size(after_id, widths.size)
        .map_err(|field| refusal(field, DemuxError::BadSize))?;
    let size = usize::try_from(known(size)?).map_err(|_| DemuxError::Overrun)?;
    let data = after_id.get(size_len..).unwrap_or_default();
    if size > data.len() {
        return Err(DemuxError::Overrun);
    }
    let (body, rest) = data.split_at(size);
    Ok((element, body, rest))
}

/// What a field cut short or malformed inside its parent's bytes is refused
/// as.
const fn refusal(field: Field, bad: DemuxError) -> DemuxError {
    match field {
        Field::Short => DemuxError::Overrun,
        Field::Bad => bad,
    }
}

/// The `size` bytes of an element's data at `data`, read whole into
/// `scratch`.
fn slurp<'s>(
    input: &mut (impl SoundInput + ?Sized),
    data: u64,
    size: u64,
    scratch: &'s mut Vec<u8>,
) -> Result<&'s [u8], DemuxError> {
    let len = usize::try_from(size)
        .ok()
        .filter(|&len| len <= MAX_READ)
        .ok_or(DemuxError::TooLarge)?;
    if data.checked_add(size).is_none_or(|end| end > input.len()) {
        return Err(DemuxError::Truncated);
    }
    scratch.clear();
    if !fallible::grow_to(scratch, len, 0) {
        return Err(DemuxError::OutOfMemory);
    }
    read_exact(input, data, scratch)?;
    Ok(scratch.as_slice())
}

/// An unsigned integer element's value.
fn uint(data: &[u8]) -> Result<u64, DemuxError> {
    ebml::read_uint(data).ok_or(DemuxError::BadValue)
}

/// Fill `slot` with `value`, refusing a second.
fn once<T>(slot: &mut Option<T>, value: T) -> Result<(), DemuxError> {
    if slot.replace(value).is_some() {
        Err(DemuxError::Duplicated)
    } else {
        Ok(())
    }
}

/// The data of a number element at `at`, eight bytes at most, and its length.
fn number_bytes(
    input: &mut (impl SoundInput + ?Sized),
    at: u64,
    size: u64,
) -> Result<([u8; 8], usize), DemuxError> {
    let mut bytes = [0; 8];
    let len = usize::try_from(size)
        .ok()
        .filter(|&len| len <= bytes.len())
        .ok_or(DemuxError::BadValue)?;
    read_exact(input, at, &mut bytes[..len])?;
    Ok((bytes, len))
}

/// Whether a walk may start at `at`: a Cluster's header begins there, or the
/// file ends inside the header that would.
fn lands(
    input: &mut (impl SoundInput + ?Sized),
    file: &Opened,
    at: u64,
) -> Result<bool, DemuxError> {
    let file_len = input.len();
    let limit = file.segment.limit(file_len);
    match read_head(input, at, limit, file.widths) {
        Ok(Some(head)) => Ok(head.id == id::CLUSTER),
        Ok(None) => Ok(limit >= file_len),
        Err(DemuxError::BadId | DemuxError::BadSize) => Ok(false),
        Err(refused) => Err(refused),
    }
}

/// Where a walk through the Segment's elements is.
#[derive(Clone, Copy, Debug)]
struct Walk {
    /// The next element header to read.
    at: u64,
    /// The Cluster being walked, if one is.
    cluster: Option<Cluster>,
}

/// A Cluster being walked.
#[derive(Clone, Copy, Debug)]
struct Cluster {
    start: u64,
    /// Where it ends, where its size is known.
    end: Option<u64>,
    /// Its timestamp in ticks, once read.
    tick: Option<u64>,
}

/// A block-holding element: a `SimpleBlock` or a block group.
#[derive(Clone, Copy, Debug)]
struct Element {
    id: u32,
    data: u64,
    end: u64,
}

/// A run of the file's bytes.
#[derive(Clone, Copy, Debug)]
struct Span {
    at: u64,
    end: u64,
}

impl Span {
    /// Bytes the span holds, `cap` at most.
    fn len_within(self, cap: usize) -> usize {
        usize::try_from(self.end.saturating_sub(self.at)).map_or(cap, |len| len.min(cap))
    }
}

/// What one step of a walk met.
#[derive(Clone, Copy, Debug)]
enum Step {
    /// A Cluster's start.
    Entered,
    /// The timestamp of the Cluster at `start`, which ends at `end` where its
    /// size is known.
    Timed {
        start: u64,
        tick: u64,
        end: Option<u64>,
    },
    /// A block-holding element of a Cluster timed at `tick`, not yet stepped
    /// past.
    Block { element: Element, tick: u64 },
    /// An element stepped over.
    Skipped,
    /// The end of a Cluster.
    Left,
    /// The end of the walk.
    Ended(Ending),
}

impl Walk {
    const fn at(at: u64) -> Self {
        Self { at, cluster: None }
    }

    /// Read the next element and step past it, but for a block-holding one,
    /// which the caller steps past once it has read it.
    fn step(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        file: &Opened,
    ) -> Result<Step, DemuxError> {
        let file_len = input.len();
        match self.cluster {
            Some(cluster) => self.in_cluster(input, file, cluster, file_len),
            None => self.in_segment(input, file, file_len),
        }
    }

    fn in_segment(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        file: &Opened,
        file_len: u64,
    ) -> Result<Step, DemuxError> {
        let limit = file.segment.limit(file_len);
        if self.at >= limit {
            let ending = if file.segment.cut_short(file_len) {
                Ending::Truncated
            } else {
                Ending::Complete
            };
            return Ok(Step::Ended(ending));
        }
        let Some(head) = read_head(input, self.at, limit, file.widths)? else {
            return cut_at(limit, file_len);
        };
        if matches!(head.id, id::EBML | id::SEGMENT) {
            return if file.segment.end.is_some() {
                Err(DemuxError::Misplaced)
            } else {
                Ok(Step::Ended(Ending::Complete))
            };
        }
        let data = self.at.checked_add(head.len).ok_or(DemuxError::Overrun)?;
        if head.id == id::CLUSTER {
            let end = match head.size {
                Size::Known(size) => Some(within(file, data, size, None)?),
                Size::Unknown => None,
            };
            self.cluster = Some(Cluster {
                start: self.at,
                end,
                tick: None,
            });
            self.at = data;
            return Ok(Step::Entered);
        }
        let end = within(file, data, known(head.size)?, None)?;
        if end > file_len {
            return Ok(Step::Ended(Ending::Truncated));
        }
        self.at = end;
        Ok(Step::Skipped)
    }

    fn in_cluster(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        file: &Opened,
        cluster: Cluster,
        file_len: u64,
    ) -> Result<Step, DemuxError> {
        let segment_limit = file.segment.limit(file_len);
        let limit = cluster
            .end
            .map_or(segment_limit, |end| end.min(segment_limit));
        if self.at >= limit {
            if cluster.end.is_some_and(|end| end > file_len) {
                return Ok(Step::Ended(Ending::Truncated));
            }
            self.cluster = None;
            return Ok(Step::Left);
        }
        let Some(head) = read_head(input, self.at, limit, file.widths)? else {
            return cut_at(limit, file_len);
        };
        if ends_cluster(head.id) {
            if cluster.end.is_some() {
                return Err(DemuxError::Misplaced);
            }
            self.cluster = None;
            return Ok(Step::Left);
        }
        let data = self.at.checked_add(head.len).ok_or(DemuxError::Overrun)?;
        let end = within(file, data, known(head.size)?, cluster.end)?;
        if end > file_len {
            return Ok(Step::Ended(Ending::Truncated));
        }
        match head.id {
            id::CLUSTER_TIMESTAMP => {
                if cluster.tick.is_some() {
                    return Err(DemuxError::Duplicated);
                }
                let (bytes, len) = number_bytes(input, data, end - data)?;
                let tick = uint(&bytes[..len])?;
                self.cluster = Some(Cluster {
                    tick: Some(tick),
                    ..cluster
                });
                self.at = end;
                Ok(Step::Timed {
                    start: cluster.start,
                    tick,
                    end: cluster.end,
                })
            }
            id::SIMPLE_BLOCK | id::BLOCK_GROUP => {
                let tick = cluster.tick.ok_or(DemuxError::Untimed)?;
                let element = Element {
                    id: head.id,
                    data,
                    end,
                };
                Ok(Step::Block { element, tick })
            }
            _ => {
                self.at = end;
                Ok(Step::Skipped)
            }
        }
    }
}

/// The step where `limit` cuts an element header short: the file's end
/// truncates it, and any other end it overruns.
fn cut_at(limit: u64, file_len: u64) -> Result<Step, DemuxError> {
    if limit >= file_len {
        Ok(Step::Ended(Ending::Truncated))
    } else {
        Err(DemuxError::Overrun)
    }
}

/// Where an element of `size` bytes from `data` ends, refused where that is
/// past its Cluster's end or the Segment's.
fn within(
    file: &Opened,
    data: u64,
    size: u64,
    cluster_end: Option<u64>,
) -> Result<u64, DemuxError> {
    let end = data.checked_add(size).ok_or(DemuxError::Overrun)?;
    let past = |parent: Option<u64>| parent.is_some_and(|parent| end > parent);
    if past(cluster_end) || past(file.segment.end) {
        return Err(DemuxError::Overrun);
    }
    Ok(end)
}

/// Whether `element` ends a Cluster of unknown size: an element of the
/// Segment's, or a new document.
const fn ends_cluster(element: u32) -> bool {
    matches!(
        element,
        id::CLUSTER
            | id::CUES
            | id::SEEK_HEAD
            | id::INFO
            | id::TRACKS
            | id::TAGS
            | id::CHAPTERS
            | id::ATTACHMENTS
            | id::EBML
            | id::SEGMENT
    )
}

/// The frames of a laced block still to answer, and the buffers a block is
/// read with.
#[derive(Default)]
struct Lace {
    pending: Option<Pending>,
    work: Work,
}

/// What reading a block needs: its frames' sizes, and its lacing's bytes.
#[derive(Default)]
struct Work {
    sizes: Vec<u32>,
    bytes: Vec<u8>,
}

/// The rest of a laced block: the frame its others copy, which of its sizes
/// comes next and where its frame starts, and the padding the last discards.
#[derive(Clone, Copy, Debug)]
struct Pending {
    template: Frame,
    next: usize,
    offset: u64,
    discard: i64,
}

impl Lace {
    /// The next frame of the lace being answered.
    fn next_laced(&mut self) -> Option<Frame> {
        let mut pending = self.pending.take()?;
        let len = *self.work.sizes.get(pending.next)?;
        pending.next += 1;
        let last = pending.next == self.work.sizes.len();
        let frame = Frame {
            offset: pending.offset,
            len,
            discard_padding_ns: if last { pending.discard } else { 0 },
            ..pending.template
        };
        pending.offset += u64::from(len);
        if !last {
            self.pending = Some(pending);
        }
        Some(frame)
    }
}

/// A block's header: whose frames, when, whether flagged a keyframe, and
/// where its first frame starts.
#[derive(Clone, Copy, Debug)]
struct BlockHead {
    track: u64,
    timestamp_ns: u64,
    keyframe: bool,
    payload: u64,
}

/// Read the block whose bytes `span` holds, laying its frames' sizes out in
/// `work`.
fn read_block(
    input: &mut (impl SoundInput + ?Sized),
    file: &Opened,
    span: Span,
    tick: u64,
    simple: bool,
    work: &mut Work,
) -> Result<BlockHead, DemuxError> {
    let mut fixed = [0; MAX_BLOCK_HEAD];
    let held = read(
        input,
        span.at,
        &mut fixed[..span.len_within(MAX_BLOCK_HEAD)],
    )?;
    let bytes = fixed.get(..held).ok_or(DemuxError::InputFailed)?;
    let (track, track_len) = match ebml::read_vint(bytes, MAX_SIZE_LEN) {
        Ok((Some(track), len)) => (track, len),
        Ok((None, _)) | Err(Field::Bad) => return Err(DemuxError::BadValue),
        Err(Field::Short) => return Err(DemuxError::Overrun),
    };
    let fields = bytes
        .get(track_len..track_len + 3)
        .ok_or(DemuxError::Overrun)?;
    let [high, low, flags] = <[u8; 3]>::try_from(fields).map_err(|_| DemuxError::Overrun)?;
    let reserved = if simple {
        SIMPLE_RESERVED
    } else {
        GROUP_RESERVED
    };
    if flags & reserved != 0 {
        return Err(DemuxError::BadValue);
    }
    if !file.declares(track) {
        return Err(DemuxError::UndeclaredTrack);
    }
    let timestamp_ns = file.timestamp_ns(tick, i16::from_be_bytes([high, low]))?;
    let frames = Span {
        at: span.at + track_len as u64 + 3,
        end: span.end,
    };
    let payload = lay_out_lace(input, frames, (flags >> 1) & 3, work)?;
    Ok(BlockHead {
        track,
        timestamp_ns,
        keyframe: simple && flags & KEYFRAME_FLAG != 0,
        payload,
    })
}

/// Lay the sizes of the frames `span` holds out in `work` as `lacing` states
/// them, answering where the first frame starts.
fn lay_out_lace(
    input: &mut (impl SoundInput + ?Sized),
    span: Span,
    lacing: u8,
    work: &mut Work,
) -> Result<u64, DemuxError> {
    work.sizes.clear();
    if lacing == 0 {
        push_size(&mut work.sizes, span.end.saturating_sub(span.at))?;
        return Ok(span.at);
    }
    if span.at >= span.end {
        return Err(DemuxError::BadLacing);
    }
    let mut count = [0u8];
    read_exact(input, span.at, &mut count)?;
    let frames = usize::from(count[0]) + 1;
    let sizes = Span {
        at: span.at + 1,
        end: span.end,
    };
    let head_len = if lacing == FIXED_LACING {
        0
    } else {
        laced_sizes(input, sizes, lacing, frames, work)?
    };
    let payload = sizes.at + head_len as u64;
    let total = span.end.checked_sub(payload).ok_or(DemuxError::BadLacing)?;
    if lacing == FIXED_LACING {
        let frames = frames as u64;
        if !total.is_multiple_of(frames) {
            return Err(DemuxError::BadLacing);
        }
        for _ in 0..frames {
            push_size(&mut work.sizes, total / frames)?;
        }
    } else {
        let laced: u64 = work.sizes.iter().map(|&size| u64::from(size)).sum();
        let last = total.checked_sub(laced).ok_or(DemuxError::BadLacing)?;
        push_size(&mut work.sizes, last)?;
    }
    Ok(payload)
}

/// Read the sizes Xiph or EBML lacing states for all but the last of
/// `frames` frames, answering the bytes they take.
fn laced_sizes(
    input: &mut (impl SoundInput + ?Sized),
    span: Span,
    lacing: u8,
    frames: usize,
    work: &mut Work,
) -> Result<usize, DemuxError> {
    let cap = span.len_within(MAX_LACE_HEAD);
    work.bytes.clear();
    if !fallible::grow_to(&mut work.bytes, cap, 0) {
        return Err(DemuxError::OutOfMemory);
    }
    read_exact(input, span.at, &mut work.bytes)?;
    let parsed = if lacing == XIPH_LACING {
        xiph_sizes(&work.bytes, frames, &mut work.sizes)?
    } else {
        ebml_sizes(&work.bytes, frames, &mut work.sizes)?
    };
    let capped = span.end.saturating_sub(span.at) > cap as u64;
    parsed.ok_or(if capped {
        DemuxError::TooLarge
    } else {
        DemuxError::BadLacing
    })
}

/// Xiph lacing's sizes: each a run of 255s and the byte ending it. `None`
/// where the bytes end first.
fn xiph_sizes(
    bytes: &[u8],
    frames: usize,
    sizes: &mut Vec<u32>,
) -> Result<Option<usize>, DemuxError> {
    let mut at = 0;
    for _ in 1..frames {
        let mut size = 0u64;
        loop {
            let Some(&byte) = bytes.get(at) else {
                return Ok(None);
            };
            at += 1;
            size += u64::from(byte);
            if byte < u8::MAX {
                break;
            }
        }
        push_size(sizes, size)?;
    }
    Ok(Some(at))
}

/// EBML lacing's sizes: the first as a variable-size integer, each after it as
/// a signed difference from the one before. `None` where the bytes end first
/// or a field is malformed.
fn ebml_sizes(
    bytes: &[u8],
    frames: usize,
    sizes: &mut Vec<u32>,
) -> Result<Option<usize>, DemuxError> {
    if frames < 2 {
        return Ok(Some(0));
    }
    let Ok((Some(first), mut at)) = ebml::read_vint(bytes, MAX_SIZE_LEN) else {
        return Ok(None);
    };
    push_size(sizes, first)?;
    let mut size = i64::try_from(first).map_err(|_| DemuxError::TooLarge)?;
    for _ in 2..frames {
        let rest = bytes.get(at..).unwrap_or_default();
        let Ok((difference, len)) = ebml::read_signed_vint(rest) else {
            return Ok(None);
        };
        at += len;
        size = size
            .checked_add(difference)
            .filter(|&size| size >= 0)
            .ok_or(DemuxError::BadLacing)?;
        push_size(sizes, size.unsigned_abs())?;
    }
    Ok(Some(at))
}

/// Append a frame's size, refusing one past [`MAX_FRAME_BYTES`].
fn push_size(sizes: &mut Vec<u32>, size: u64) -> Result<(), DemuxError> {
    let size = u32::try_from(size)
        .ok()
        .filter(|&size| size <= MAX_FRAME_BYTES)
        .ok_or(DemuxError::TooLarge)?;
    if fallible::push(sizes, size) {
        Ok(())
    } else {
        Err(DemuxError::OutOfMemory)
    }
}

/// Read the block group whose bytes `span` holds: its block, a keyframe
/// unless a reference names another, and its discard padding.
fn read_group(
    input: &mut (impl SoundInput + ?Sized),
    file: &Opened,
    span: Span,
    tick: u64,
    work: &mut Work,
) -> Result<(BlockHead, i64), DemuxError> {
    let mut block = None;
    let mut referenced = false;
    let mut discard = None;
    let mut at = span.at;
    for _ in 0..MAX_GROUP_CHILDREN {
        if at >= span.end {
            break;
        }
        let head = read_head(input, at, span.end, file.widths)?.ok_or(DemuxError::Overrun)?;
        let data = at.checked_add(head.len).ok_or(DemuxError::Overrun)?;
        let end = data
            .checked_add(known(head.size)?)
            .filter(|&end| end <= span.end)
            .ok_or(DemuxError::Overrun)?;
        match head.id {
            id::BLOCK => {
                let found = read_block(input, file, Span { at: data, end }, tick, false, work)?;
                once(&mut block, found)?;
            }
            id::REFERENCE_BLOCK => referenced = true,
            id::DISCARD_PADDING => {
                let (bytes, len) = number_bytes(input, data, end - data)?;
                let padding = ebml::read_int(&bytes[..len]).ok_or(DemuxError::BadValue)?;
                once(&mut discard, padding)?;
            }
            _ => {}
        }
        at = end;
    }
    if at < span.end {
        return Err(DemuxError::TooMany);
    }
    let mut block = block.ok_or(DemuxError::Missing)?;
    block.keyframe = !referenced;
    Ok((block, discard.unwrap_or(0)))
}
