//! MPEG audio (ISO/IEC 11172-3; ISO/IEC 13818-3 and MPEG-2.5 for the lower
//! rates): Layers I, II and III, decoded to interleaved 32-bit floats.
//!
//! A stream's first frame is believed only once it and the two after it
//! agree — the same family, layer, rate and channel count, each starting
//! where the last ends — never on the first byte pair that looks like a
//! sync word. A frame that carries an encoder's tag (Xing's, with LAME's
//! extension, or VBRI's) in place of audio states the frame count and,
//! from LAME, the silence the encoder added before and after the sound,
//! which is trimmed so an album plays gapless. ID3 and APE tags around the
//! stream are stepped over.
//!
//! Each frame is read whole before any state moves, so an input whose bytes
//! are not at hand changes nothing. A seek is exact: frame headers are
//! walked from the nearest frame already passed, four bytes a frame, and
//! enough frames before the target are decoded and dropped to refill Layer
//! III's reservoir and the filterbank's history.

mod frame;
mod huffman;
mod info;
mod layer12;
mod layer3;
mod synth;
mod tables;

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_abi::driver::audio::{ChannelMap, Rate, SampleFormat};

use self::frame::{Header, Layer, HEADER_LEN, MAX_FRAME_LEN};
use self::layer12::{Slots, LAYER2_SLOTS};
use self::layer3::{Layer3, RESERVOIR};
use self::synth::{Synthesis, SynthesisTables, SUBBANDS};
use crate::input::{self, SoundInput};
use crate::{id3, DecodeError, DecodeLimits, Encoding, SoundFormat, SoundInfo};

/// Headers that must agree before a stream's first frame is believed.
const AGREEING: usize = 3;

/// Bytes past where a stream should start that are searched for its first
/// frame.
const SEARCH: u64 = 64 * 1024;

/// Frames between the positions remembered for seeking.
const INDEX_STRIDE: u64 = 32;

/// Frames a seek walks back over at most to refill the reservoir: every
/// frame holds at least one byte of main data, and a frame reaches back
/// [`RESERVOIR`] bytes.
const PREROLL_LIMIT: usize = RESERVOIR + 1;

/// Bytes a frame's header is read in.
const HEADER_READ: usize = HEADER_LEN;

/// The most bytes one read takes from the input: the window a lost sync is
/// searched again in.
pub(crate) const MAX_WORKING_SET: u64 = SEARCH + (AGREEING * MAX_FRAME_LEN) as u64;

/// Whether `bytes` open with a frame header.
pub(crate) fn has_signature(bytes: &[u8]) -> bool {
    Header::parse(bytes).is_some()
}

/// The stream's shape, as its first audio frame states it.
#[derive(Copy, Clone)]
struct Shape {
    header: Header,
    /// A free-format stream's frame length without padding.
    free_unpadded: Option<usize>,
}

impl Shape {
    /// The length of the frame `header` opens, in this stream, where it is
    /// one a frame can have.
    fn len_of(&self, header: Header) -> Option<usize> {
        let len = match header.bitrate() {
            Some(bitrate) => header.len_at(bitrate),
            None => self.free_unpadded? + slot(header) * usize::from(header.padding),
        };
        (header.data_start()..=MAX_FRAME_LEN)
            .contains(&len)
            .then_some(len)
    }

    /// The bitrate of the frame `header` opens, `len` bytes long.
    fn bitrate_of(header: Header, len: usize) -> u32 {
        header.bitrate().unwrap_or_else(|| header.bitrate_of(len))
    }
}

/// Where the next frame is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Cursor {
    offset: u64,
    /// Audio frames before it.
    frame: u64,
}

/// One frame's decoding, its synthesis, and the samples it made.
struct Decoder {
    channels: usize,
    layer3: Option<Layer3>,
    tables: SynthesisTables,
    synthesis: [Synthesis; 2],
    slots: Slots,
    /// The last frame's samples, interleaved.
    pcm: Vec<f32>,
    /// The raw sample the held frame starts at, and its samples per channel.
    held_start: u64,
    held_len: usize,
}

impl Decoder {
    fn reset(&mut self) {
        if let Some(layer3) = &mut self.layer3 {
            layer3.reset();
        }
        for synthesis in &mut self.synthesis {
            synthesis.reset();
        }
        self.held_len = 0;
    }

    /// The held frame's samples from raw sample `raw` on, interleaved, where
    /// it holds `raw`.
    fn held_from(&self, raw: u64) -> Option<&[f32]> {
        let offset = usize::try_from(raw.checked_sub(self.held_start)?).ok()?;
        (offset < self.held_len)
            .then(|| &self.pcm[offset * self.channels..self.held_len * self.channels])
    }

    /// Decode `frame`, whose header is `header`, as raw samples from `start`.
    ///
    /// A stream keeps the channels its first frame states: a mono frame in a
    /// stereo stream drives both channels' filterbanks with its one channel,
    /// so each filterbank's state follows the signal its output carries, and
    /// a stereo frame in a mono stream is refused rather than folded into one.
    fn frame(
        &mut self,
        header: Header,
        bytes: &[u8],
        bitrate: u32,
        start: u64,
    ) -> Result<(), DecodeError> {
        let coded = usize::from(header.channels());
        if coded > self.channels {
            return Err(DecodeError::MpegFrameMismatch);
        }
        let slots = match header.layer {
            Layer::I => {
                layer12::layer1(header, bytes, &mut self.slots)?;
                layer12::LAYER1_SLOTS
            }
            Layer::II => {
                layer12::layer2(header, bytes, bitrate, &mut self.slots)?;
                LAYER2_SLOTS
            }
            Layer::III => {
                let layer3 = self.layer3.as_mut().ok_or(DecodeError::MpegFrameMismatch)?;
                let filled = layer3.frame(header, bytes, self.channels, &mut self.slots)?;
                let slots = header.samples() as usize / SUBBANDS;
                if filled == 0 {
                    for channel in &mut self.slots {
                        channel[..slots].fill([0.0; SUBBANDS]);
                    }
                }
                slots
            }
        };
        let channels = self.channels;
        if coded < channels && header.layer != Layer::III {
            let [left, right] = &mut self.slots;
            right[..slots].copy_from_slice(&left[..slots]);
        }
        for slot in 0..slots {
            for channel in 0..channels {
                let at = slot * SUBBANDS * channels + channel;
                self.synthesis[channel].slot(
                    &mut self.tables,
                    &self.slots[channel][slot],
                    &mut self.pcm[at..],
                    channels,
                );
            }
        }
        self.held_start = start;
        self.held_len = slots * SUBBANDS;
        Ok(())
    }
}

/// An MPEG audio stream being read.
pub(crate) struct Mpeg {
    shape: Shape,
    /// Where the stream's frames end.
    end: u64,
    /// Audio frames the stream's tag states.
    stated_frames: Option<u64>,
    /// Raw samples dropped before the sound, and the sound's length.
    skip: u64,
    total: Option<u64>,
    next: Cursor,
    /// Every [`INDEX_STRIDE`]th frame's offset, as far as frames have been
    /// passed.
    index: Vec<u64>,
    decoder: Decoder,
    frame: Vec<u8>,
}

/// Bytes a padded frame of `header`'s layer adds.
fn slot(header: Header) -> usize {
    if header.layer == Layer::I {
        4
    } else {
        1
    }
}

/// Find the first frame at or past `from` whose headers agree for
/// [`AGREEING`] frames, or end the stream at `end`, answering where it
/// starts and the stream's shape.
fn find_stream(
    input: &mut (impl SoundInput + ?Sized),
    from: u64,
    end: u64,
) -> Result<(u64, Shape), DecodeError> {
    if from >= end {
        return Err(DecodeError::MpegNoSync);
    }
    let window = (end - from).min(SEARCH + (AGREEING * MAX_FRAME_LEN) as u64);
    let len = usize::try_from(window).map_err(|_| DecodeError::OutOfMemory)?;
    let mut bytes = tairix_util::fallible::filled(len, 0u8).ok_or(DecodeError::OutOfMemory)?;
    input::read_exact(input, from, &mut bytes, DecodeError::InputFailed)?;
    let tail_is_end = from + window == end;
    let mut at = 0;
    while at + HEADER_LEN <= bytes.len() && (at as u64) <= SEARCH {
        if let Some(shape) = agree(&bytes, at, tail_is_end) {
            return Ok((from + at as u64, shape));
        }
        at += 1;
    }
    Err(DecodeError::MpegNoSync)
}

/// The shape the frames from `at` in `bytes` agree on, where they do. A
/// free-format stream's length is the first distance to a header that the
/// frames after it agree with.
fn agree(bytes: &[u8], at: usize, tail_is_end: bool) -> Option<Shape> {
    let header = Header::parse(&bytes[at..])?;
    if header.bitrate().is_some() {
        let shape = Shape {
            header,
            free_unpadded: None,
        };
        return agrees(bytes, at, tail_is_end, shape).then_some(shape);
    }
    let first = at + header.data_start() + 1;
    let last = (at + MAX_FRAME_LEN).min(bytes.len().saturating_sub(HEADER_LEN));
    (first..=last)
        .filter(|&next| Header::parse(&bytes[next..]).is_some_and(|other| header.continues(other)))
        .map(|next| Shape {
            header,
            free_unpadded: Some(next - at - slot(header) * usize::from(header.padding)),
        })
        .find(|&shape| agrees(bytes, at, tail_is_end, shape))
}

/// Whether [`AGREEING`] frames of `shape` follow one another from `at`, or
/// fewer end the stream exactly.
fn agrees(bytes: &[u8], at: usize, tail_is_end: bool, shape: Shape) -> bool {
    let mut here = at;
    for _ in 0..AGREEING {
        let Some(current) = bytes.get(here..).and_then(Header::parse) else {
            return false;
        };
        if !shape.header.continues(current) {
            return false;
        }
        let Some(len) = shape.len_of(current) else {
            return false;
        };
        here += len;
        if here == bytes.len() && tail_is_end {
            return true;
        }
        if here > bytes.len() {
            return false;
        }
    }
    true
}

/// Open the stream `input` holds from `start`.
///
/// # Errors
///
/// [`DecodeError::MpegNoSync`] where no frames agree, a limit's refusal, or
/// [`DecodeError::OutOfMemory`].
pub(crate) fn open(
    input: &mut (impl SoundInput + ?Sized),
    start: u64,
    limits: &DecodeLimits,
) -> Result<(SoundInfo, Mpeg), DecodeError> {
    let len = input.len();
    if start >= len {
        return Err(DecodeError::MpegNoSync);
    }
    let end = len - id3::trailing_len(input, start, len)?;
    let (first, shape) = find_stream(input, start, end)?;
    let header = shape.header;
    if header.channels() > limits.max_channels() {
        return Err(DecodeError::ChannelsExceedLimit);
    }
    let rate = Rate::new(header.rate()).map_err(|_| DecodeError::RateOutOfRange)?;
    let channels = if header.channels() == 1 {
        ChannelMap::MONO
    } else {
        ChannelMap::STEREO
    };
    let mut frame =
        tairix_util::fallible::filled(MAX_FRAME_LEN, 0u8).ok_or(DecodeError::OutOfMemory)?;
    let first_len = shape.len_of(header).ok_or(DecodeError::MpegNoSync)?;
    input::read_exact(
        input,
        first,
        &mut frame[..first_len],
        DecodeError::InputFailed,
    )?;
    let tag = info::parse(header, &frame[..first_len]);
    let audio_start = if tag.is_some() {
        first + first_len as u64
    } else {
        first
    };
    let tag = tag.unwrap_or_default();
    let samples = u64::from(header.samples());
    let (skip, total) = match (tag.frames, tag.gapless) {
        (Some(frames), Some((delay, padding))) if header.layer == Layer::III => {
            let raw = frames * samples;
            let skip = delay + info::DECODER_DELAY;
            // A padding shorter than the decoder's delay cannot be trimmed
            // past what the frames decode to.
            let total = raw
                .saturating_sub(delay + padding)
                .min(raw.saturating_sub(skip));
            (skip, Some(total))
        }
        (Some(frames), _) => (0, Some(frames * samples)),
        (None, _) => (0, None),
    };
    let layer3 = if header.layer == Layer::III {
        Some(Layer3::new()?)
    } else {
        None
    };
    let pcm = tairix_util::fallible::filled(1152 * 2, 0f32).ok_or(DecodeError::OutOfMemory)?;
    let mut index = Vec::new();
    if !tairix_util::fallible::reserve(&mut index, 1) {
        return Err(DecodeError::OutOfMemory);
    }
    index.push(audio_start);
    let info = SoundInfo {
        format: SoundFormat::Mpeg,
        encoding: match header.layer {
            Layer::I => Encoding::MpegLayer1,
            Layer::II => Encoding::MpegLayer2,
            Layer::III => Encoding::MpegLayer3,
        },
        rate,
        channels,
        sample: SampleFormat::F32,
        frames: total,
        seekable: true,
        data_length: None,
    };
    let mpeg = Mpeg {
        shape,
        end,
        stated_frames: tag.frames,
        skip,
        total,
        next: Cursor {
            offset: audio_start,
            frame: 0,
        },
        index,
        decoder: Decoder {
            channels: usize::from(header.channels()),
            layer3,
            tables: SynthesisTables::build()?,
            synthesis: [Synthesis::new(), Synthesis::new()],
            slots: [[[0.0; SUBBANDS]; LAYER2_SLOTS]; 2],
            pcm,
            held_start: 0,
            held_len: 0,
        },
        frame,
    };
    Ok((info, mpeg))
}

/// What reading the frame at a cursor found.
enum Found {
    /// A frame of this header and length.
    Frame(Header, usize),
    /// The stream's frames end here.
    End,
}

impl Mpeg {
    /// Write whole frames of samples into `out` from output frame `position`
    /// on, answering how many.
    ///
    /// # Errors
    ///
    /// [`DecodeError::InputUnavailable`] with nothing changed when nothing
    /// was written, or the stream's refusal.
    pub(crate) fn read(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        position: u64,
        out: &mut [u8],
    ) -> Result<usize, DecodeError> {
        let channels = self.decoder.channels;
        let frame_bytes = channels * 4;
        let room = out.len() / frame_bytes;
        let mut written = 0;
        while written < room {
            let at = position + written as u64;
            if self.total.is_some_and(|total| at >= total) {
                break;
            }
            let raw = at + self.skip;
            let Some(held) = self.decoder.held_from(raw) else {
                match self.advance_to(input, raw) {
                    Ok(true) => continue,
                    Ok(false) => break,
                    Err(_) if written > 0 => return Ok(written),
                    Err(err) => return Err(err),
                }
            };
            let left = self.total.map_or(usize::MAX, |total| {
                usize::try_from(total - at).unwrap_or(usize::MAX)
            });
            let count = (held.len() / channels).min(room - written).min(left);
            let to = out[written * frame_bytes..].as_chunks_mut::<4>().0;
            for (sample, bytes) in held[..count * channels].iter().zip(to) {
                *bytes = sample.to_le_bytes();
            }
            written += count;
        }
        Ok(written)
    }

    /// Decode on until the held frame covers raw sample `raw`, answering
    /// `false` where the stream ends first.
    fn advance_to(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        raw: u64,
    ) -> Result<bool, DecodeError> {
        let samples = u64::from(self.shape.header.samples());
        let target = raw / samples;
        let behind = target < self.next.frame;
        if behind || target - self.next.frame > INDEX_STRIDE {
            self.seek_to(input, target)?;
        }
        loop {
            let (cursor, found) = self.locate(input, self.next)?;
            let (header, len) = match found {
                Found::Frame(header, len) => (header, len),
                Found::End => {
                    if self
                        .stated_frames
                        .is_some_and(|stated| cursor.frame < stated)
                    {
                        return Err(DecodeError::MpegTruncated);
                    }
                    return Ok(false);
                }
            };
            input::read_exact(
                input,
                cursor.offset,
                &mut self.frame[..len],
                DecodeError::MpegTruncated,
            )?;
            let bitrate = Shape::bitrate_of(header, len);
            self.decoder
                .frame(header, &self.frame[..len], bitrate, cursor.frame * samples)?;
            self.advance(cursor, len)?;
            if self.decoder.held_from(raw).is_some() {
                return Ok(true);
            }
        }
    }

    /// Move past the frame at `cursor`, `len` bytes.
    fn advance(&mut self, cursor: Cursor, len: usize) -> Result<(), DecodeError> {
        let next = Cursor {
            offset: cursor.offset + len as u64,
            frame: cursor.frame + 1,
        };
        self.remember(next)?;
        self.next = next;
        Ok(())
    }

    /// Remember where frame `cursor` starts, where the index takes it next.
    fn remember(&mut self, cursor: Cursor) -> Result<(), DecodeError> {
        if cursor.frame.is_multiple_of(INDEX_STRIDE)
            && cursor.frame / INDEX_STRIDE == self.index.len() as u64
        {
            if !tairix_util::fallible::reserve(&mut self.index, 1) {
                return Err(DecodeError::OutOfMemory);
            }
            self.index.push(cursor.offset);
        }
        Ok(())
    }

    /// The frame at `cursor`, or where the stream is re-found past bytes
    /// that are not one: its header and length, or the stream's end.
    fn locate(
        &self,
        input: &mut (impl SoundInput + ?Sized),
        cursor: Cursor,
    ) -> Result<(Cursor, Found), DecodeError> {
        if cursor.offset + HEADER_READ as u64 > self.end {
            return Ok((cursor, Found::End));
        }
        let mut bytes = [0u8; HEADER_READ];
        input::read_exact(input, cursor.offset, &mut bytes, DecodeError::InputFailed)?;
        let found = Header::parse(&bytes)
            .filter(|&header| self.shape.header.continues(header))
            .and_then(|header| Some((header, self.shape.len_of(header)?)));
        if let Some((header, len)) = found {
            let fits = cursor.offset + len as u64 <= self.end;
            return Ok((
                cursor,
                if fits {
                    Found::Frame(header, len)
                } else {
                    Found::End
                },
            ));
        }
        let (offset, shape) = match find_stream(input, cursor.offset + 1, self.end) {
            Ok(found) => found,
            Err(DecodeError::MpegNoSync) => return Ok((cursor, Found::End)),
            Err(err) => return Err(err),
        };
        if !self.shape.header.continues(shape.header) {
            return Err(DecodeError::MpegFrameMismatch);
        }
        let found = shape
            .len_of(shape.header)
            .map_or(Found::End, |len| Found::Frame(shape.header, len));
        Ok((Cursor { offset, ..cursor }, found))
    }

    /// Reposition at frame `target`, ready to decode it exactly: decoding
    /// restarts far enough before it that every byte its reservoir reaches
    /// and every slot the filterbank remembers is decoded first. Nothing
    /// moves until the walk there is done, so a walk that meets bytes not
    /// yet at hand can be made again.
    fn seek_to(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        target: u64,
    ) -> Result<(), DecodeError> {
        let mut recent: VecDeque<(Cursor, usize)> = VecDeque::new();
        recent
            .try_reserve_exact(PREROLL_LIMIT)
            .map_err(|_| DecodeError::OutOfMemory)?;
        let back = target.saturating_sub(PREROLL_LIMIT as u64);
        let known = (back / INDEX_STRIDE).min(self.index.len() as u64 - 1);
        let mut cursor = Cursor {
            offset: self.index[known as usize],
            frame: known * INDEX_STRIDE,
        };
        while cursor.frame < target {
            let (at, found) = self.locate(input, cursor)?;
            let Found::Frame(header, len) = found else {
                break;
            };
            if recent.len() == PREROLL_LIMIT {
                recent.pop_front();
            }
            recent.push_back((at, len - header.data_start()));
            cursor = Cursor {
                offset: at.offset + len as u64,
                frame: at.frame + 1,
            };
            self.remember(cursor)?;
        }
        let start = match self.shape.header.layer {
            // The target's filterbank remembers the frame before it, whose
            // first slots at the lower rates — one granule a frame — carry the
            // overlap of the frame before that. Those frames must decode
            // exactly, so decoding starts where the frames ahead of them fill
            // the reservoir, or at the earliest walked where they never do.
            Layer::III => {
                let exact = if self.shape.header.lsf() { 2 } else { 1 };
                let mut main = 0;
                recent
                    .iter()
                    .enumerate()
                    .rev()
                    .skip(exact)
                    .find_map(|(at, &(_, bytes))| {
                        main += bytes;
                        (main >= RESERVOIR).then_some(at)
                    })
                    .unwrap_or(0)
            }
            // Layer I's frame is twelve slots, fewer than the sixteen the
            // filterbank remembers.
            _ => recent.len().saturating_sub(2),
        };
        self.next = recent.get(start).map_or(cursor, |&(at, _)| at);
        self.decoder.reset();
        Ok(())
    }
}

#[cfg(test)]
#[path = "mpeg_tests.rs"]
mod tests;
