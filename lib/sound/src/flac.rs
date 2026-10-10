//! FLAC (RFC 9639): the `fLaC` marker, the metadata blocks, then the audio
//! frames, decoded a frame at a time.
//!
//! [`Decoder`] turns frames into PCM for both carriers: the native stream
//! here and the Ogg mapping in `ogg`. A frame is checked against both of its
//! CRCs and its place in the sequence, and a stream decoded whole from its
//! first sample is checked against the digest its `STREAMINFO` carries.
//!
//! The native stream seeks by bisecting its own frame headers, narrowed first
//! by the seek table where there is one: a frame states its first sample, so
//! no index needs building and a stream without a table seeks as well as one
//! with.

use tairix_abi::driver::audio::{ChannelMap, ChannelPosition, Rate, SampleFormat};

use crate::comment;
use crate::flac_frame::{
    self, FrameError, FrameHeader, Samples, Stream, MAX_CHANNELS, MAX_HEADER_LEN,
};
use crate::input::{self, Fields, Region, SoundInput, Span};
use crate::md5::Md5;
use crate::meta::{Collector, CoverRange, Cue};
use crate::{speaker_map, DecodeError, DecodeLimits, Encoding, SoundFormat, SoundInfo};

/// The stream marker.
pub(crate) const MARKER: &[u8; 4] = b"fLaC";

const STREAMINFO: u8 = 0;
const APPLICATION: u8 = 2;
const SEEKTABLE: u8 = 3;
const VORBIS_COMMENT: u8 = 4;
const CUESHEET: u8 = 5;
const PICTURE: u8 = 6;
const FORBIDDEN: u8 = 127;

/// Bytes of a metadata block header.
pub(crate) const BLOCK_HEADER_LEN: usize = 4;

/// Bytes of the `STREAMINFO` block.
pub(crate) const STREAMINFO_LEN: usize = 34;

const SEEK_POINT_LEN: u64 = 18;

const PLACEHOLDER: u64 = u64::MAX;

/// The least block size `STREAMINFO` may state.
const MIN_BLOCK: u32 = 16;

/// The least sample width.
const MIN_BITS: u8 = 4;

/// Bytes of a frame read first where `STREAMINFO` states no largest frame;
/// a larger frame is read again whole.
const FIRST_READ: usize = 16 * 1024;

/// Bytes a seek's frame search reads at a time.
const SCAN_LEN: usize = 4096;

/// A seek bracket this narrow, or two of the largest frames where that is
/// known, is searched frame by frame rather than bisected again.
const LINEAR_SPAN: u64 = 64 * 1024;

/// Whether `bytes` open with the FLAC marker.
pub(crate) fn has_signature(bytes: &[u8]) -> bool {
    bytes.starts_with(MARKER)
}

/// What `STREAMINFO` states.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct StreamInfo {
    pub(crate) min_block: u32,
    pub(crate) max_block: u32,
    /// Bytes of the largest frame; zero for unknown.
    pub(crate) max_frame: u32,
    pub(crate) rate: u32,
    pub(crate) channels: u8,
    pub(crate) bits: u8,
    /// Samples a channel holds; zero for unknown.
    pub(crate) total: u64,
    /// The MD5 of the samples; zeros for unknown.
    pub(crate) digest: [u8; 16],
}

impl StreamInfo {
    /// Read the block, refusing one the format forbids.
    pub(crate) fn parse(bytes: &[u8; STREAMINFO_LEN]) -> Result<Self, DecodeError> {
        let u16_at = |at: usize| u32::from(u16::from_be_bytes([bytes[at], bytes[at + 1]]));
        let u24_at = |at: usize| u32::from_be_bytes([0, bytes[at], bytes[at + 1], bytes[at + 2]]);
        let mut packed = [0u8; 8];
        packed.copy_from_slice(&bytes[10..18]);
        let packed = u64::from_be_bytes(packed);
        let mut digest = [0u8; 16];
        digest.copy_from_slice(&bytes[18..]);
        let field = |shift: u32, mask: u64| packed >> shift & mask;
        let info = Self {
            min_block: u16_at(0),
            max_block: u16_at(2),
            max_frame: u24_at(7),
            rate: u32::try_from(field(44, 0xF_FFFF)).map_err(|_| DecodeError::FlacBadStreamInfo)?,
            channels: u8::try_from(field(41, 0x7) + 1)
                .map_err(|_| DecodeError::FlacBadStreamInfo)?,
            bits: u8::try_from(field(36, 0x1F) + 1).map_err(|_| DecodeError::FlacBadStreamInfo)?,
            total: field(0, 0xF_FFFF_FFFF),
            digest,
        };
        let blocks = MIN_BLOCK <= info.min_block && info.min_block <= info.max_block;
        if !blocks || info.bits < MIN_BITS {
            return Err(DecodeError::FlacBadStreamInfo);
        }
        Ok(info)
    }

    pub(crate) const fn stream(&self) -> Stream {
        Stream {
            rate: self.rate,
            bits: self.bits,
            channels: self.channels,
            max_block: self.max_block,
        }
    }

    pub(crate) const fn total(&self) -> Option<u64> {
        if self.total == 0 {
            None
        } else {
            Some(self.total)
        }
    }

    fn digest(&self) -> Option<[u8; 16]> {
        (self.digest != [0; 16]).then_some(self.digest)
    }
}

/// The channels of a stream of `count` that states no mask (RFC 9639,
/// section 9.1.3); seven channels place a back centre the PCM vocabulary
/// has no position for.
fn default_channels(count: u8) -> Option<ChannelMap> {
    use ChannelPosition::{
        FrontCentre, FrontLeft, FrontRight, LowFrequency, Mono, RearLeft, RearRight, SideLeft,
        SideRight,
    };
    let positions: &[ChannelPosition] = match count {
        1 => &[Mono],
        2 => &[FrontLeft, FrontRight],
        3 => &[FrontLeft, FrontRight, FrontCentre],
        4 => &[FrontLeft, FrontRight, RearLeft, RearRight],
        5 => &[FrontLeft, FrontRight, FrontCentre, RearLeft, RearRight],
        6 => &[
            FrontLeft,
            FrontRight,
            FrontCentre,
            LowFrequency,
            RearLeft,
            RearRight,
        ],
        8 => &[
            FrontLeft,
            FrontRight,
            FrontCentre,
            LowFrequency,
            RearLeft,
            RearRight,
            SideLeft,
            SideRight,
        ],
        _ => return None,
    };
    ChannelMap::new(positions).ok()
}

/// What the metadata blocks after `STREAMINFO` state.
#[derive(Default)]
pub(crate) struct Blocks {
    /// The seek table's offset in the file and its point count.
    seek_table: Option<(u64, u64)>,
    seen_table: bool,
    seen_comments: bool,
    channel_mask: Option<u32>,
}

impl Blocks {
    /// Take the block of `kind` `region` holds, which starts at `offset` in
    /// the file where the stream is native.
    pub(crate) fn take(
        &mut self,
        kind: u8,
        region: &mut impl Region,
        offset: Option<u64>,
        info: &StreamInfo,
        collector: &mut Collector,
    ) -> Result<(), DecodeError> {
        match kind {
            STREAMINFO => Err(DecodeError::FlacDuplicateBlock),
            APPLICATION if region.len() < 4 => Err(DecodeError::FlacBadApplication),
            SEEKTABLE => {
                if core::mem::replace(&mut self.seen_table, true) {
                    return Err(DecodeError::FlacDuplicateBlock);
                }
                if !region.len().is_multiple_of(SEEK_POINT_LEN) {
                    return Err(DecodeError::FlacBadSeekTable);
                }
                self.seek_table = offset.map(|offset| (offset, region.len() / SEEK_POINT_LEN));
                Ok(())
            }
            VORBIS_COMMENT => {
                if core::mem::replace(&mut self.seen_comments, true) {
                    return Err(DecodeError::FlacDuplicateBlock);
                }
                let comments = comment::read(region, collector, DecodeError::FlacBadComment)?;
                self.channel_mask = comments.channel_mask;
                Ok(())
            }
            CUESHEET => cuesheet(region, info, collector),
            PICTURE => picture(region, offset, collector),
            FORBIDDEN => Err(DecodeError::FlacForbiddenBlock),
            // Padding, an application's data and the reserved types are
            // stepped over.
            _ => Ok(()),
        }
    }

    /// The channel map of a stream `info` describes.
    pub(crate) fn channels(
        &self,
        info: &StreamInfo,
        limits: &DecodeLimits,
    ) -> Result<ChannelMap, DecodeError> {
        if info.channels > limits.max_channels() {
            return Err(DecodeError::ChannelsExceedLimit);
        }
        match self.channel_mask {
            Some(mask) => speaker_map(info.channels, mask).ok_or(DecodeError::FlacChannelMask),
            None => default_channels(info.channels).ok_or(DecodeError::ChannelLayoutUnsupported),
        }
    }
}

/// The track a cuesheet ends with, on a CD and off one.
const LEAD_OUT_CD: u8 = 170;
const LEAD_OUT: u8 = 255;

/// A CD's tracks and index points.
const MAX_CD_ENTRIES: u8 = 100;

/// Samples a CD frame holds.
const CD_FRAME: u64 = 588;

/// Read a cuesheet, keeping a cue a track at its first sample of programme:
/// its index 1, or its first index point where it has none.
fn cuesheet(
    region: &mut impl Region,
    info: &StreamInfo,
    collector: &mut Collector,
) -> Result<(), DecodeError> {
    let mut fields = Fields::new(region, DecodeError::FlacBadCuesheet);
    fields.skip(128 + 8)?;
    let cd = fields.array::<1>()?[0] & 0x80 != 0;
    fields.skip(258)?;
    let tracks = fields.array::<1>()?[0];
    let lead_out = if cd { LEAD_OUT_CD } else { LEAD_OUT };
    if tracks == 0 || (cd && tracks > MAX_CD_ENTRIES) {
        return Err(DecodeError::FlacBadCuesheet);
    }
    let mut seen = [false; 256];
    for track in 1..=tracks {
        let offset = u64::from_be_bytes(fields.array()?);
        let number = fields.array::<1>()?[0];
        fields.skip(12 + 1 + 13)?;
        let indices = fields.array::<1>()?[0];
        let last = track == tracks;
        let misplaced = (number == lead_out) != last || number == 0;
        let repeated = core::mem::replace(&mut seen[usize::from(number)], true);
        let counted = if last {
            indices == 0
        } else {
            indices > 0 && !(cd && indices > MAX_CD_ENTRIES)
        };
        let beyond = info.total().is_some_and(|total| offset > total);
        if misplaced || repeated || !counted || beyond || (cd && offset % CD_FRAME != 0) {
            return Err(DecodeError::FlacBadCuesheet);
        }
        let (mut previous, mut first, mut programme) = (None::<u8>, None, None);
        for _ in 0..indices {
            let relative = u64::from_be_bytes(fields.array()?);
            let point = fields.array::<1>()?[0];
            fields.skip(3)?;
            let in_step = previous.map_or(point <= 1, |previous| {
                previous.checked_add(1) == Some(point)
            });
            if !in_step || (cd && relative % CD_FRAME != 0) {
                return Err(DecodeError::FlacBadCuesheet);
            }
            let at = offset
                .checked_add(relative)
                .ok_or(DecodeError::FlacBadCuesheet)?;
            first.get_or_insert(at);
            if point == 1 {
                programme = Some(at);
            }
            previous = Some(point);
        }
        if let Some(frame) = programme.or(first).filter(|_| !last) {
            collector.cue(Cue {
                id: u32::from(number),
                frame,
            })?;
        }
    }
    if fields.remaining() != 0 {
        return Err(DecodeError::FlacBadCuesheet);
    }
    Ok(())
}

/// Check a picture block holds together, and offer its image as the cover
/// where the block lies whole in the file from `offset`. Its picture is not
/// sound, and this decoder reads none of it.
fn picture(
    region: &mut impl Region,
    offset: Option<u64>,
    collector: &mut Collector,
) -> Result<(), DecodeError> {
    let mut fields = Fields::new(region, DecodeError::FlacBadPicture);
    let kind = u32::from_be_bytes(fields.array()?);
    let mime = u64::from(u32::from_be_bytes(fields.array()?));
    // "-->" names a link, not an image.
    let link = if mime == LINK_MIME.len() as u64 {
        fields.array::<3>()? == *LINK_MIME
    } else {
        fields.skip(mime)?;
        false
    };
    let description = u64::from(u32::from_be_bytes(fields.array()?));
    fields.skip(description)?;
    fields.skip(16)?;
    let data = u64::from(u32::from_be_bytes(fields.array()?));
    let start = fields.position();
    fields.skip(data)?;
    if let Some(offset) = offset.filter(|_| data != 0 && !link) {
        let offset = offset
            .checked_add(start)
            .ok_or(DecodeError::FlacBadPicture)?;
        collector.picture(CoverRange { offset, len: data }, kind == FRONT_COVER);
    }
    Ok(())
}

/// A picture block's MIME type for a link rather than an image.
const LINK_MIME: &[u8; 3] = b"-->";

/// The picture type of a front cover.
const FRONT_COVER: u32 = 3;

/// The sample format a stream of `bits` is written in, and how a sample is
/// placed in it: shifted to the container's top bit, as WAV places it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Output {
    format: SampleFormat,
    bytes: usize,
    shift: u32,
}

impl Output {
    fn for_bits(bits: u8) -> Self {
        let (format, bytes) = match bits {
            0..=8 => (SampleFormat::U8, 1),
            9..=16 => (SampleFormat::S16, 2),
            17..=24 => (SampleFormat::S24, 3),
            _ => (SampleFormat::S32, 4),
        };
        let container = u32::try_from(bytes * 8).unwrap_or(32);
        Self {
            format,
            bytes,
            shift: container - u32::from(bits),
        }
    }
}

/// The frame a decoder holds, by its first sample from the stream's start.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Held {
    first: u64,
    len: u64,
}

/// A digest running over the samples from the stream's first on.
struct Running {
    md5: Md5,
    next: u64,
}

/// How the stream numbers its frames, learnt from its first.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Numbering {
    variable: bool,
    fixed_block: u32,
    /// The first frame's first sample, from which positions count.
    base: u64,
}

impl Numbering {
    /// The numbering a stream whose first frame has `header` uses.
    pub(crate) fn of(info: &StreamInfo, header: &FrameHeader) -> Self {
        let fixed_block = if info.min_block == info.max_block {
            info.max_block
        } else {
            header.block_size
        };
        let mut numbering = Self {
            variable: header.variable,
            fixed_block,
            base: 0,
        };
        numbering.base = header.first_sample(fixed_block);
        numbering
    }

    /// The numbering of a stream that holds no frames.
    pub(crate) const fn unframed(info: &StreamInfo) -> Self {
        Self {
            variable: false,
            fixed_block: info.max_block,
            base: 0,
        }
    }

    /// The position of the frame `header` opens, or `None` for a frame no
    /// stream numbered so could hold.
    pub(crate) fn position(&self, header: &FrameHeader) -> Option<u64> {
        if header.variable != self.variable {
            return None;
        }
        header.first_sample(self.fixed_block).checked_sub(self.base)
    }
}

/// Frames into PCM: the frame held, its samples, and the digest of all the
/// stream's samples so far while they have been decoded in order.
pub(crate) struct Decoder {
    stream: Stream,
    total: Option<u64>,
    digest: Option<[u8; 16]>,
    output: Output,
    samples: Samples,
    held: Option<Held>,
    running: Option<Running>,
    verdict: Option<bool>,
}

impl Decoder {
    pub(crate) fn new(info: &StreamInfo) -> Self {
        let digest = info.digest();
        Self {
            stream: info.stream(),
            total: info.total(),
            digest,
            output: Output::for_bits(info.bits),
            samples: Samples::default(),
            held: None,
            running: digest.map(|_| Running {
                md5: Md5::new(),
                next: 0,
            }),
            verdict: None,
        }
    }

    pub(crate) const fn stream(&self) -> &Stream {
        &self.stream
    }

    pub(crate) const fn sample_format(&self) -> SampleFormat {
        self.output.format
    }

    pub(crate) fn frame_bytes(&self) -> usize {
        self.output.bytes * usize::from(self.stream.channels)
    }

    /// Samples a channel of the held frame holds.
    pub(crate) fn held_len(&self) -> u64 {
        self.held.map_or(0, |held| held.len)
    }

    /// Whether the held frame holds `position`.
    pub(crate) fn holds(&self, position: u64) -> bool {
        self.held
            .is_some_and(|held| (held.first..held.first + held.len).contains(&position))
    }

    /// Whether `position` is past the samples the stream states.
    pub(crate) fn is_past_end(&self, position: u64) -> bool {
        self.total.is_some_and(|total| position >= total)
    }

    /// Decode the frame `bytes` opens with as the frame at `position`, or the
    /// frame its own header places where `position` is `None`; answer the
    /// header, the position and the bytes the frame took.
    pub(crate) fn decode(
        &mut self,
        bytes: &[u8],
        numbering: &Numbering,
        position: Option<u64>,
    ) -> Result<(FrameHeader, u64, usize), FrameError> {
        self.held = None;
        let (header, len) = flac_frame::decode(bytes, &self.stream, &mut self.samples)?;
        let at = numbering.position(&header).ok_or(FrameError::OutOfOrder)?;
        if position.is_some_and(|position| position != at) {
            return Err(FrameError::OutOfOrder);
        }
        let held = Held {
            first: at,
            len: u64::from(header.block_size),
        };
        self.held = Some(held);
        self.digest_frame(held);
        Ok((header, at, len))
    }

    /// Fold a frame just decoded into the running digest where it continues
    /// it; a frame from the stream's start begins it again.
    fn digest_frame(&mut self, held: Held) {
        if self.digest.is_none() {
            return;
        }
        if held.first == 0 {
            self.running = Some(Running {
                md5: Md5::new(),
                next: 0,
            });
        }
        let Some(running) = self
            .running
            .as_mut()
            .filter(|running| running.next == held.first)
        else {
            self.running = None;
            return;
        };
        let count = self.total.map_or(held.len, |total| {
            held.len.min(total.saturating_sub(held.first))
        });
        let count = usize::try_from(count).unwrap_or(0);
        let width = usize::from(self.stream.bits).div_ceil(8);
        let channels = usize::from(self.stream.channels);
        let mut slices: [&[i64]; MAX_CHANNELS as usize] = [&[]; MAX_CHANNELS as usize];
        for (channel, slice) in slices.iter_mut().enumerate().take(channels) {
            *slice = &self.samples.channel(channel)[..count];
        }
        let slices = &slices[..channels];
        let mut chunk = [0u8; 4096];
        let per_chunk = chunk.len() / (width * channels);
        let mut at = 0;
        while at < count {
            let take = per_chunk.min(count - at);
            let out = match width {
                1 => interleave::<1>(slices, at, take, &mut chunk),
                2 => interleave::<2>(slices, at, take, &mut chunk),
                3 => interleave::<3>(slices, at, take, &mut chunk),
                _ => interleave::<4>(slices, at, take, &mut chunk),
            };
            running.md5.update(&chunk[..out]);
            at += take;
        }
        running.next = held.first + held.len;
    }

    /// Write the held frame's samples from `position` into `out`, as many
    /// whole frames as both hold, answering how many.
    pub(crate) fn emit(&self, position: u64, out: &mut [u8]) -> usize {
        let Some(held) = self.held.filter(|_| self.holds(position)) else {
            return 0;
        };
        let end = self.total.map_or(held.first + held.len, |total| {
            total.min(held.first + held.len)
        });
        let frame_bytes = self.frame_bytes();
        let left = usize::try_from(end.saturating_sub(position)).unwrap_or(usize::MAX);
        let count = left.min(out.len() / frame_bytes);
        if count == 0 {
            return 0;
        }
        let from = usize::try_from(position - held.first).unwrap_or(0);
        let Output { bytes, shift, .. } = self.output;
        let flip = if self.output.format == SampleFormat::U8 {
            0x80
        } else {
            0
        };
        for channel in 0..usize::from(self.stream.channels) {
            let samples = &self.samples.channel(channel)[from..from + count];
            let out = &mut out[channel * bytes..count * frame_bytes];
            match bytes {
                1 => place::<1>(samples, out, frame_bytes, shift, flip),
                2 => place::<2>(samples, out, frame_bytes, shift, 0),
                3 => place::<3>(samples, out, frame_bytes, shift, 0),
                _ => place::<4>(samples, out, frame_bytes, shift, 0),
            }
        }
        count
    }

    /// The stream has ended at `position`: refuse one that ends short of the
    /// samples it states, or whose samples, decoded whole and in order,
    /// disagree with its digest.
    pub(crate) fn finish(&mut self, position: u64) -> Result<(), DecodeError> {
        if self.total.is_some_and(|total| position < total) {
            return Err(DecodeError::FlacTruncated);
        }
        if self.verdict.is_none() {
            let whole = self
                .running
                .take()
                .filter(|running| running.next >= position);
            if let (Some(running), Some(digest)) = (whole, self.digest) {
                self.verdict = Some(running.md5.finish() == digest);
            }
        }
        match self.verdict {
            Some(false) => Err(DecodeError::FlacDigestMismatch),
            _ => Ok(()),
        }
    }
}

/// Write each of `samples`, shifted up by `shift` and its top byte flipped
/// by `flip`, as the low `N` bytes of a slot every `stride` bytes of `out`.
fn place<const N: usize>(samples: &[i64], out: &mut [u8], stride: usize, shift: u32, flip: u8) {
    for (slot, &sample) in out.chunks_mut(stride).zip(samples) {
        let placed = (sample << shift).to_le_bytes();
        if let Some(slot) = slot.first_chunk_mut::<N>() {
            *slot = *placed.first_chunk::<N>().unwrap_or(&[0; N]);
            slot[N - 1] ^= flip;
        }
    }
}

/// Append each frame of `slices`' samples at `at..at + count`, interleaved
/// as the low `N` bytes of each, to `chunk` from `out`, answering the end.
fn interleave<const N: usize>(
    slices: &[&[i64]],
    at: usize,
    count: usize,
    chunk: &mut [u8],
) -> usize {
    let mut out = 0;
    for index in at..at + count {
        for slice in slices {
            let sample = slice[index].to_le_bytes();
            if let (Some(slot), Some(low)) = (
                chunk
                    .get_mut(out..out + N)
                    .and_then(|slot| slot.first_chunk_mut::<N>()),
                sample.first_chunk::<N>(),
            ) {
                *slot = *low;
            }
            out += N;
        }
    }
    out
}

/// What a frame's refusal says of the stream.
pub(crate) const fn frame_error(err: FrameError) -> DecodeError {
    match err {
        FrameError::Truncated => DecodeError::FlacTruncated,
        FrameError::NoSync => DecodeError::FlacNoSync,
        FrameError::HeaderCrc => DecodeError::FlacHeaderCrc,
        FrameError::FrameCrc => DecodeError::FlacFrameCrc,
        FrameError::Reserved => DecodeError::FlacReserved,
        FrameError::Invalid => DecodeError::FlacFrameInvalid,
        FrameError::Mismatch => DecodeError::FlacFrameMismatch,
        FrameError::OutOfOrder => DecodeError::FlacFrameOutOfOrder,
        FrameError::OutOfMemory => DecodeError::OutOfMemory,
    }
}

/// A frame the stream is known to start at, by its offset and position.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Cursor {
    offset: u64,
    position: u64,
    /// Whether a decoded frame or the stream's own structure put it there,
    /// rather than a search that could have met a sync in a frame's data.
    vouched: bool,
}

/// The file's bytes from `start`, held across frames.
#[derive(Default)]
struct Window {
    start: u64,
    bytes: alloc::vec::Vec<u8>,
}

impl Window {
    /// The bytes from `offset`, at least `want` of them where the file holds
    /// them before `end`.
    fn load(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        offset: u64,
        want: usize,
        end: u64,
    ) -> Result<&[u8], DecodeError> {
        let held_end = self.start + self.bytes.len() as u64;
        if offset < self.start || offset > held_end {
            self.bytes.clear();
            self.start = offset;
        } else if offset > self.start {
            let drop =
                usize::try_from(offset - self.start).map_err(|_| DecodeError::InputFailed)?;
            self.bytes.drain(..drop);
            self.start = offset;
        }
        let available = usize::try_from(end - offset).unwrap_or(usize::MAX);
        let want = want.min(available);
        let held = self.bytes.len();
        if held < want {
            if !tairix_util::fallible::grow_to(&mut self.bytes, want, 0u8) {
                return Err(DecodeError::OutOfMemory);
            }
            let from = offset + held as u64;
            if let Err(err) = input::read_exact(
                input,
                from,
                &mut self.bytes[held..want],
                DecodeError::InputFailed,
            ) {
                self.bytes.truncate(held);
                return Err(err);
            }
        }
        Ok(&self.bytes[..want])
    }
}

/// A native FLAC stream past its metadata.
pub(crate) struct Flac {
    decoder: Decoder,
    numbering: Numbering,
    first_frame: u64,
    end: u64,
    /// The bytes of the largest frame, where `STREAMINFO` states it.
    max_frame: usize,
    seek_table: Option<(u64, u64)>,
    window: Window,
    /// The frame after the last decoded.
    next: Cursor,
}

/// Read the metadata of the native stream `input` holds from `start`.
pub(crate) fn open(
    input: &mut (impl SoundInput + ?Sized),
    start: u64,
    limits: &DecodeLimits,
    collector: &mut Collector,
) -> Result<(SoundInfo, Flac), DecodeError> {
    let len = input.len();
    let mut marker = [0u8; 4];
    input::read_exact(input, start, &mut marker, DecodeError::FlacBadMarker)?;
    if !has_signature(&marker) {
        return Err(DecodeError::FlacBadMarker);
    }
    let mut at = start + 4;
    let (last, kind, size) = block_header(input, at)?;
    if kind != STREAMINFO || size != STREAMINFO_LEN as u64 {
        return Err(DecodeError::FlacMissingStreamInfo);
    }
    let mut body = [0u8; STREAMINFO_LEN];
    input::read_exact(input, at + 4, &mut body, DecodeError::FlacMetadataTruncated)?;
    let info = StreamInfo::parse(&body)?;
    let rate = Rate::new(info.rate).map_err(|_| DecodeError::RateOutOfRange)?;
    at += 4 + size;
    let mut blocks = Blocks::default();
    let mut done = last;
    while !done {
        let (last, kind, size) = block_header(input, at)?;
        let data = at + 4;
        if data.checked_add(size).is_none_or(|end| end > len) {
            return Err(DecodeError::FlacMetadataTruncated);
        }
        let mut region = Span {
            input: &mut *input,
            start: data,
            len: size,
        };
        blocks.take(kind, &mut region, Some(data), &info, collector)?;
        at = data + size;
        done = last;
    }
    let channels = blocks.channels(&info, limits)?;
    let end = len - crate::id3::trailing_len(input, at, len)?;
    let decoder = Decoder::new(&info);
    let mut window = Window::default();
    let numbering = if at < end {
        let bytes = window.load(input, at, MAX_HEADER_LEN, end)?;
        let header = flac_frame::header(bytes, decoder.stream()).map_err(frame_error)?;
        Numbering::of(&info, &header)
    } else {
        Numbering::unframed(&info)
    };
    let flac = Flac {
        decoder,
        numbering,
        first_frame: at,
        end,
        max_frame: usize::try_from(info.max_frame).unwrap_or(0),
        seek_table: blocks.seek_table,
        window,
        next: Cursor {
            offset: at,
            position: 0,
            vouched: true,
        },
    };
    let sound = SoundInfo {
        format: SoundFormat::Flac,
        encoding: Encoding::Flac,
        rate,
        channels,
        sample: flac.decoder.sample_format(),
        frames: info.total(),
        seekable: true,
        data_length: None,
    };
    Ok((sound, flac))
}

/// The metadata block header at `at`: whether it is the last, its kind and
/// the bytes it holds.
fn block_header(
    input: &mut (impl SoundInput + ?Sized),
    at: u64,
) -> Result<(bool, u8, u64), DecodeError> {
    let mut header = [0u8; BLOCK_HEADER_LEN];
    input::read_exact(input, at, &mut header, DecodeError::FlacMetadataTruncated)?;
    let size = u32::from_be_bytes([0, header[1], header[2], header[3]]);
    Ok((header[0] & 0x80 != 0, header[0] & 0x7F, u64::from(size)))
}

impl Flac {
    /// Write the frames from `position` that `out` has room for. A frame
    /// whose bytes are not at hand ends the block early once one frame is
    /// written, so no call ever needs more than one frame read at once.
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

    /// Decode the frame holding `position`, answering `false` where the
    /// stream's frames end first.
    fn advance_to(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        position: u64,
    ) -> Result<bool, DecodeError> {
        let ahead = position.checked_sub(self.next.position);
        let near = u64::from(self.decoder.stream().max_block) * 2;
        if ahead.is_none_or(|ahead| ahead > near) {
            self.locate(input, position)?;
        }
        loop {
            let cursor = self.next;
            if cursor.offset >= self.end {
                return Ok(false);
            }
            let expected = cursor.vouched.then_some(cursor.position);
            match self.frame_at(input, cursor.offset, expected) {
                Ok((at, len)) => {
                    self.next = Cursor {
                        offset: cursor.offset + len,
                        position: at + self.decoder.held_len(),
                        vouched: true,
                    };
                    if self.decoder.holds(position) {
                        return Ok(true);
                    }
                    if at > position {
                        return Err(DecodeError::FlacFrameInvalid);
                    }
                }
                Err(err) if !cursor.vouched && is_corrupt(err) => {
                    match self.scan(input, cursor.offset + 1, self.end)? {
                        Some(found) => self.next = found,
                        None => return Ok(false),
                    }
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// Decode the frame at `offset`, answering its position and length.
    fn frame_at(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        offset: u64,
        expected: Option<u64>,
    ) -> Result<(u64, u64), DecodeError> {
        let header = {
            let bytes = self.window.load(input, offset, MAX_HEADER_LEN, self.end)?;
            flac_frame::header(bytes, self.decoder.stream()).map_err(frame_error)?
        };
        let bound = self
            .decoder
            .stream()
            .frame_bound(header.block_size, header.len);
        let first = if self.max_frame > 0 {
            self.max_frame
        } else {
            FIRST_READ
        };
        let mut want = first.min(bound);
        loop {
            let bytes = self.window.load(input, offset, want, self.end)?;
            let held = bytes.len();
            match self.decoder.decode(bytes, &self.numbering, expected) {
                Ok((_, at, len)) => return Ok((at, len as u64)),
                Err(FrameError::Truncated) if held < want => {
                    return Err(DecodeError::FlacTruncated)
                }
                Err(FrameError::Truncated) if want >= bound => {
                    return Err(DecodeError::FlacFrameTooLarge)
                }
                Err(FrameError::Truncated) => want = want.saturating_mul(2).min(bound),
                Err(err) => return Err(frame_error(err)),
            }
        }
    }

    /// Set the cursor at a frame starting at or before `position` and near
    /// it: the seek table's nearest point, then bisection over the frames'
    /// own headers.
    fn locate(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        position: u64,
    ) -> Result<(), DecodeError> {
        let mut lo = Cursor {
            offset: self.first_frame,
            position: 0,
            vouched: true,
        };
        if self.next.position <= position && self.next.position > lo.position {
            lo = self.next;
        }
        let mut hi = self.end;
        if let Some((point, bound)) = self.table_point(input, position)? {
            if point.position >= lo.position {
                lo = point;
            }
            if let Some(bound) = bound.filter(|&bound| bound > lo.offset) {
                hi = hi.min(bound);
            }
        }
        let span = if self.max_frame > 0 {
            (2 * self.max_frame as u64).max(LINEAR_SPAN)
        } else {
            LINEAR_SPAN
        };
        while hi - lo.offset > span {
            let mid = lo.offset + (hi - lo.offset) / 2;
            match self.scan(input, mid, hi)? {
                Some(found) if found.position <= position => lo = found,
                Some(found) => hi = found.offset,
                None => hi = mid,
            }
        }
        self.next = lo;
        Ok(())
    }

    /// The seek table's last point at or before `position`, checked against
    /// the frame it names, and the offset of the point after it.
    fn table_point(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        position: u64,
    ) -> Result<Option<(Cursor, Option<u64>)>, DecodeError> {
        let Some((offset, points)) = self.seek_table else {
            return Ok(None);
        };
        let target = self.numbering.base.saturating_add(position);
        let mut point = |index: u64| -> Result<(u64, u64), DecodeError> {
            let mut bytes = [0u8; 18];
            input::read_exact(
                input,
                offset + index * SEEK_POINT_LEN,
                &mut bytes,
                DecodeError::FlacBadSeekTable,
            )?;
            let mut sample = [0u8; 8];
            sample.copy_from_slice(&bytes[..8]);
            let mut at = [0u8; 8];
            at.copy_from_slice(&bytes[8..16]);
            Ok((u64::from_be_bytes(sample), u64::from_be_bytes(at)))
        };
        let (mut low, mut high) = (0, points);
        while low < high {
            let mid = low + (high - low) / 2;
            let (sample, _) = point(mid)?;
            if sample != PLACEHOLDER && sample <= target {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        let Some(found) = low.checked_sub(1) else {
            return Ok(None);
        };
        let (sample, relative) = point(found)?;
        let bound = if low < points {
            let (after, after_offset) = point(low)?;
            (after != PLACEHOLDER).then(|| self.first_frame.saturating_add(after_offset))
        } else {
            None
        };
        let Some(at) = self
            .first_frame
            .checked_add(relative)
            .filter(|&at| at < self.end)
        else {
            self.seek_table = None;
            return Ok(None);
        };
        let header = {
            let bytes = self.window.load(input, at, MAX_HEADER_LEN, self.end)?;
            flac_frame::header(bytes, self.decoder.stream()).ok()
        };
        let checked = header
            .and_then(|header| self.numbering.position(&header))
            .filter(|&found| self.numbering.base.checked_add(found) == Some(sample));
        let Some(found) = checked else {
            self.seek_table = None;
            return Ok(None);
        };
        Ok(Some((
            Cursor {
                offset: at,
                position: found,
                vouched: true,
            },
            bound,
        )))
    }

    /// The first frame header at or after `from` and before `before` that
    /// the stream could hold.
    fn scan(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        from: u64,
        before: u64,
    ) -> Result<Option<Cursor>, DecodeError> {
        let mut at = from;
        while at < before {
            let want = SCAN_LEN + MAX_HEADER_LEN;
            let bytes = self.window.load(input, at, want, self.end)?;
            let scanned = (SCAN_LEN as u64).min(before - at);
            for skip in 0..usize::try_from(scanned).unwrap_or(SCAN_LEN) {
                let candidate = &bytes[skip..];
                if !flac_frame::has_sync(candidate) {
                    continue;
                }
                let found = flac_frame::header(candidate, self.decoder.stream())
                    .ok()
                    .and_then(|header| self.numbering.position(&header))
                    .filter(|&position| !self.decoder.is_past_end(position));
                if let Some(position) = found {
                    return Ok(Some(Cursor {
                        offset: at + skip as u64,
                        position,
                        vouched: false,
                    }));
                }
            }
            at += scanned;
        }
        Ok(None)
    }
}

/// Whether `err` is what a sync met in a frame's data would give.
const fn is_corrupt(err: DecodeError) -> bool {
    matches!(
        err,
        DecodeError::FlacNoSync
            | DecodeError::FlacHeaderCrc
            | DecodeError::FlacFrameCrc
            | DecodeError::FlacReserved
            | DecodeError::FlacFrameInvalid
            | DecodeError::FlacFrameMismatch
            | DecodeError::FlacFrameOutOfOrder
            | DecodeError::FlacFrameTooLarge
            | DecodeError::FlacTruncated
    )
}

/// The most bytes one call reads for a stream within `limits`: its largest
/// frame stored verbatim, read whole at once, and a search's read past it.
pub(crate) const fn max_working_set(limits: &DecodeLimits) -> u64 {
    let channels = if limits.max_channels() < MAX_CHANNELS {
        limits.max_channels()
    } else {
        MAX_CHANNELS
    };
    let stream = Stream {
        rate: 0,
        bits: flac_frame::MAX_BITS,
        channels,
        max_block: flac_frame::MAX_BLOCK,
    };
    (stream.max_frame_len() + SCAN_LEN + MAX_HEADER_LEN) as u64
}

#[cfg(test)]
#[path = "flac_tests.rs"]
mod tests;
