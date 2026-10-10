//! RIFF WAVE, with its RF64 and BW64 forms: a list of chunks, in any order,
//! the format in `fmt ` and the samples in `data`, and what the file says of
//! them in `fact`, `cue `, `smpl` and `LIST` `INFO`. RF64 and BW64 carry the
//! sizes a 32-bit field cannot in their leading `ds64` chunk, so a file over
//! four gibibytes is read rather than truncated.

use alloc::vec::Vec;

use tairix_abi::driver::audio::{ChannelMap, Rate, SampleFormat};

use crate::g711::{ALAW, ULAW};
use crate::input::{self, SoundInput};
use crate::meta::{Collector, Cue, Loop, LoopKind, TagKey, TagKind};
use crate::{
    conventional_channels, ima, msadpcm, pcm, speaker_map, DataLength, DecodeError, DecodeLimits,
    Encoding, SoundFormat, SoundInfo,
};

const WAVE: &[u8; 4] = b"WAVE";

/// The chunk headers a file is walked through, data and metadata alike: a
/// fixed containment bound, far past any real file's handful.
const MAX_CHUNKS: u32 = 4096;

/// A 32-bit size an RF64 file states in its `ds64` chunk instead.
const SIZE_IN_DS64: u32 = u32::MAX;

/// Bytes of the `fmt ` chunk read: its fixed part, then the most an
/// extension's 16-bit size can declare.
const MAX_FORMAT_BYTES: usize = 18 + u16::MAX as usize;

/// Entries of the `ds64` table of further chunk sizes kept.
const MAX_DS64_ENTRIES: usize = 16;

const TAG_PCM: u16 = 0x0001;
const TAG_MS_ADPCM: u16 = 0x0002;
const TAG_FLOAT: u16 = 0x0003;
const TAG_ALAW: u16 = 0x0006;
const TAG_MULAW: u16 = 0x0007;
const TAG_IMA_ADPCM: u16 = 0x0011;
const TAG_GSM610: u16 = 0x0031;
const TAG_MPEG: u16 = 0x0050;
const TAG_MPEG_LAYER3: u16 = 0x0055;
const TAG_EXTENSIBLE: u16 = 0xFFFE;

/// Every extensible subformat's GUID after its first two bytes, which carry
/// the format tag it stands for.
const SUBFORMAT_SUFFIX: [u8; 14] = [
    0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];

/// Whether `bytes` open as a WAVE file of any of its three forms.
pub(crate) fn has_signature(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && matches!(&bytes[..4], b"RIFF" | b"RF64" | b"BW64") && &bytes[8..12] == WAVE
}

/// How the stream's samples lie.
enum Codec {
    /// Linear PCM in `width`-byte containers, little-endian, unsigned at one
    /// byte: already the vocabulary's own form.
    Pcm {
        width: usize,
    },
    Float32,
    Float64,
    Law(&'static [i16; 256]),
    Ima {
        block: usize,
    },
    Ms {
        block: usize,
        coefficients: Vec<[i16; 2]>,
    },
}

/// A WAVE stream past its chunk list.
pub(crate) struct Wav {
    data_start: u64,
    data_end: u64,
    channels: usize,
    codec: Codec,
    frames: u64,
}

fn le_u16(bytes: &[u8], at: usize) -> Option<u16> {
    bytes
        .get(at..)
        .and_then(<[u8]>::first_chunk::<2>)
        .map(|field| u16::from_le_bytes(*field))
}

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..)
        .and_then(<[u8]>::first_chunk::<4>)
        .map(|field| u32::from_le_bytes(*field))
}

fn le_u64(bytes: &[u8], at: usize) -> Option<u64> {
    bytes
        .get(at..)
        .and_then(<[u8]>::first_chunk::<8>)
        .map(|field| u64::from_le_bytes(*field))
}

/// The sizes an RF64 file's `ds64` chunk states.
#[derive(Default)]
struct Ds64 {
    data: u64,
    frames: u64,
    table: Vec<([u8; 4], u64)>,
}

impl Ds64 {
    fn read(chunk: &[u8]) -> Result<Self, DecodeError> {
        let (Some(data), Some(frames), Some(entries)) =
            (le_u64(chunk, 8), le_u64(chunk, 16), le_u32(chunk, 24))
        else {
            return Err(DecodeError::WavMissingDs64);
        };
        let mut table = Vec::new();
        let kept = usize::try_from(entries).map_or(MAX_DS64_ENTRIES, |n| n.min(MAX_DS64_ENTRIES));
        if !tairix_util::fallible::reserve(&mut table, kept) {
            return Err(DecodeError::OutOfMemory);
        }
        for entry in chunk
            .get(28..)
            .unwrap_or(&[])
            .as_chunks::<12>()
            .0
            .iter()
            .take(kept)
        {
            let (Some(id), Some(size)) = (entry.first_chunk::<4>(), le_u64(entry, 4)) else {
                break;
            };
            table.push((*id, size));
        }
        Ok(Self {
            data,
            frames,
            table,
        })
    }

    /// The size of the chunk `id` whose 32-bit field defers to this table.
    fn size_of(&self, id: [u8; 4]) -> Option<u64> {
        if &id == b"data" {
            return Some(self.data);
        }
        self.table
            .iter()
            .find(|(entry, _)| *entry == id)
            .map(|&(_, size)| size)
    }
}

/// What the `fmt ` chunk states.
struct Format {
    codec: Codec,
    encoding: Encoding,
    sample: SampleFormat,
    channels: ChannelMap,
    rate: Rate,
    block_align: usize,
}

/// The channel map of `count` channels and an extensible format's `mask`, no
/// mask naming none.
fn channel_map(
    count: u16,
    mask: Option<u32>,
    limits: &DecodeLimits,
) -> Result<ChannelMap, DecodeError> {
    let conventional = conventional_channels(u32::from(count), limits)?;
    match mask.filter(|&mask| mask != 0) {
        None => Ok(conventional),
        Some(mask) => speaker_map(conventional.channels(), mask).ok_or(DecodeError::WavChannelMask),
    }
}

/// The fixed fields of a `fmt ` chunk, and its extension.
struct Fields<'f> {
    tag: u16,
    count: u16,
    rate: u32,
    block_align: usize,
    bits: u16,
    extension: &'f [u8],
}

impl<'f> Fields<'f> {
    fn read(chunk: &'f [u8]) -> Result<Self, DecodeError> {
        let field16 = |at| le_u16(chunk, at).ok_or(DecodeError::WavFormatTruncated);
        let extension = match le_u16(chunk, 16) {
            Some(len) => chunk
                .get(18..18 + usize::from(len))
                .ok_or(DecodeError::WavFormatTruncated)?,
            None => &[],
        };
        Ok(Self {
            tag: field16(0)?,
            count: field16(2)?,
            rate: le_u32(chunk, 4).ok_or(DecodeError::WavFormatTruncated)?,
            block_align: usize::from(field16(12)?),
            bits: field16(14)?,
            extension,
        })
    }

    /// Bytes a frame of `width`-byte samples occupies.
    fn per_frame(&self, width: usize) -> usize {
        width * usize::from(self.count)
    }

    /// The format tag the samples are in, and an extensible format's channel
    /// mask.
    fn subformat(&self) -> Result<(u16, Option<u32>), DecodeError> {
        if self.tag != TAG_EXTENSIBLE {
            return Ok((self.tag, None));
        }
        let extension = self.extension;
        let (Some(valid), Some(mask), Some(guid)) = (
            le_u16(extension, 0),
            le_u32(extension, 2),
            extension.get(6..22),
        ) else {
            return Err(DecodeError::WavBadExtensible);
        };
        if guid[2..] != SUBFORMAT_SUFFIX {
            return Err(DecodeError::WavUnknownSubformat);
        }
        let sub = u16::from_le_bytes([guid[0], guid[1]]);
        if !matches!(sub, TAG_PCM | TAG_FLOAT | TAG_ALAW | TAG_MULAW) {
            return Err(DecodeError::WavUnknownSubformat);
        }
        // The container is whole bytes, and holds the valid bits.
        if !self.bits.is_multiple_of(8) || valid > self.bits {
            return Err(DecodeError::WavBadExtensible);
        }
        Ok((sub, Some(mask)))
    }

    /// The bit depth as the encoding states it.
    fn depth(&self) -> Result<u8, DecodeError> {
        u8::try_from(self.bits).map_err(|_| DecodeError::WavBadBitDepth)
    }

    fn pcm(&self) -> Result<(Codec, Encoding, SampleFormat), DecodeError> {
        let width = usize::from(self.bits.div_ceil(8));
        let sample = match (self.bits, width) {
            (1..=8, 1) => SampleFormat::U8,
            (9..=16, 2) => SampleFormat::S16,
            (17..=24, 3) => SampleFormat::S24,
            (25..=32, 4) => SampleFormat::S32,
            _ => return Err(DecodeError::WavBadBitDepth),
        };
        if self.block_align != self.per_frame(width) {
            return Err(DecodeError::WavBadBlockAlign);
        }
        let encoding = Encoding::Linear {
            bits: self.depth()?,
        };
        Ok((Codec::Pcm { width }, encoding, sample))
    }

    fn float(&self) -> Result<(Codec, Encoding, SampleFormat), DecodeError> {
        let (codec, width) = match self.bits {
            32 => (Codec::Float32, 4),
            64 => (Codec::Float64, 8),
            _ => return Err(DecodeError::WavBadBitDepth),
        };
        if self.block_align != self.per_frame(width) {
            return Err(DecodeError::WavBadBlockAlign);
        }
        let encoding = Encoding::Float {
            bits: self.depth()?,
        };
        Ok((codec, encoding, SampleFormat::F32))
    }

    fn law(&self, tag: u16) -> Result<(Codec, Encoding, SampleFormat), DecodeError> {
        if self.bits != 8 {
            return Err(DecodeError::WavBadBitDepth);
        }
        if self.block_align != self.per_frame(1) {
            return Err(DecodeError::WavBadBlockAlign);
        }
        let (table, encoding) = if tag == TAG_ALAW {
            (&ALAW, Encoding::ALaw)
        } else {
            (&ULAW, Encoding::MuLaw)
        };
        Ok((Codec::Law(table), encoding, SampleFormat::S16))
    }

    fn ima(&self) -> Result<(Codec, Encoding, SampleFormat), DecodeError> {
        let stated = le_u16(self.extension, 0).ok_or(DecodeError::WavBadAdpcmFormat)?;
        if self.bits != 4 {
            return Err(DecodeError::WavBadBitDepth);
        }
        if ima::frames_in(self.block_align, usize::from(self.count)) != Some(usize::from(stated)) {
            return Err(DecodeError::WavBadAdpcmFormat);
        }
        let codec = Codec::Ima {
            block: self.block_align,
        };
        Ok((codec, Encoding::ImaAdpcm, SampleFormat::S16))
    }

    fn ms(&self) -> Result<(Codec, Encoding, SampleFormat), DecodeError> {
        let (Some(stated), Some(pairs)) = (le_u16(self.extension, 0), le_u16(self.extension, 2))
        else {
            return Err(DecodeError::WavBadAdpcmFormat);
        };
        if self.bits != 4 {
            return Err(DecodeError::WavBadBitDepth);
        }
        let table = self
            .extension
            .get(4..4 + 4 * usize::from(pairs))
            .filter(|table| !table.is_empty())
            .ok_or(DecodeError::WavBadAdpcmFormat)?;
        if msadpcm::frames_in(self.block_align, usize::from(self.count))
            != Some(usize::from(stated))
        {
            return Err(DecodeError::WavBadAdpcmFormat);
        }
        let coefficients = tairix_util::fallible::collected(
            usize::from(pairs),
            table.as_chunks::<4>().0.iter().map(|pair| {
                [
                    i16::from_le_bytes([pair[0], pair[1]]),
                    i16::from_le_bytes([pair[2], pair[3]]),
                ]
            }),
        )
        .ok_or(DecodeError::OutOfMemory)?;
        let codec = Codec::Ms {
            block: self.block_align,
            coefficients,
        };
        Ok((codec, Encoding::MsAdpcm, SampleFormat::S16))
    }
}

/// Read the `fmt ` chunk's bytes.
fn format(chunk: &[u8], limits: &DecodeLimits) -> Result<Format, DecodeError> {
    let fields = Fields::read(chunk)?;
    let (tag, mask) = fields.subformat()?;
    let channels = channel_map(fields.count, mask, limits)?;
    let rate = Rate::new(fields.rate).map_err(|_| DecodeError::RateOutOfRange)?;
    let (codec, encoding, sample) = match tag {
        TAG_PCM => fields.pcm()?,
        TAG_FLOAT => fields.float()?,
        TAG_ALAW | TAG_MULAW => fields.law(tag)?,
        TAG_IMA_ADPCM => fields.ima()?,
        TAG_MS_ADPCM => fields.ms()?,
        TAG_MPEG | TAG_MPEG_LAYER3 => return Err(DecodeError::WavMpegAudio),
        TAG_GSM610 => return Err(DecodeError::WavGsm610),
        other => return Err(DecodeError::WavUnknownFormatTag(other)),
    };
    Ok(Format {
        codec,
        encoding,
        sample,
        channels,
        rate,
        block_align: fields.block_align,
    })
}

/// What a tag's INFO id names; none for an id no key can spell.
fn info_kind(id: [u8; 4]) -> Option<TagKind> {
    Some(match &id {
        b"INAM" => TagKind::Title,
        b"IART" => TagKind::Artist,
        b"IPRD" => TagKind::Album,
        b"ICMT" => TagKind::Comment,
        b"ICRD" => TagKind::Date,
        b"IGNR" => TagKind::Genre,
        b"ICOP" => TagKind::Copyright,
        b"ISFT" => TagKind::Software,
        b"ITRK" | b"IPRT" => TagKind::Track,
        _ => TagKind::Other(TagKey::new(&id)?),
    })
}

/// The chunk walk's state.
struct Walk<'c> {
    collector: &'c mut Collector,
    chunks: u32,
    /// RF64 or BW64, whose first chunk is `ds64`.
    sixty_four: bool,
    ds64: Option<Ds64>,
    format: Option<Format>,
    /// The data chunk's start and its declared size.
    data: Option<(u64, u64)>,
    fact: Option<u64>,
    /// Where a tag's text is read, reused from tag to tag.
    text: Vec<u8>,
}

impl Walk<'_> {
    fn count_chunk(&mut self) -> Result<(), DecodeError> {
        self.chunks += 1;
        if self.chunks > MAX_CHUNKS {
            return Err(DecodeError::WavTooManyChunks);
        }
        Ok(())
    }

    /// Read a chunk's first `len` bytes, fallibly sized.
    fn read_chunk(
        input: &mut (impl SoundInput + ?Sized),
        start: u64,
        len: usize,
    ) -> Result<Vec<u8>, DecodeError> {
        let mut bytes = tairix_util::fallible::filled(len, 0u8).ok_or(DecodeError::OutOfMemory)?;
        input::read_exact(input, start, &mut bytes, DecodeError::WavChunkTruncated)?;
        Ok(bytes)
    }

    fn info(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        mut at: u64,
        end: u64,
    ) -> Result<(), DecodeError> {
        while at + 8 <= end {
            self.count_chunk()?;
            let mut header = [0u8; 8];
            input::read_exact(input, at, &mut header, DecodeError::WavChunkTruncated)?;
            let (Some(id), Some(size)) = (header.first_chunk::<4>(), le_u32(&header, 4)) else {
                break;
            };
            let size = u64::from(size);
            let text_end = at + 8 + size;
            if text_end > end {
                return Err(DecodeError::WavChunkTruncated);
            }
            let kind = info_kind(*id);
            if let Some(kind) = kind.filter(|_| self.collector.has_room(size)) {
                let len = usize::try_from(size).map_err(|_| DecodeError::OutOfMemory)?;
                if !tairix_util::fallible::grow_to(&mut self.text, len, 0u8) {
                    return Err(DecodeError::OutOfMemory);
                }
                let text = &mut self.text[..len];
                input::read_exact(input, at + 8, text, DecodeError::WavChunkTruncated)?;
                self.collector.tag(kind, text)?;
            }
            at = text_end + (size & 1);
        }
        Ok(())
    }

    fn cues(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        start: u64,
        size: u64,
    ) -> Result<(), DecodeError> {
        let mut count = [0u8; 4];
        input::read_exact(input, start, &mut count, DecodeError::WavChunkTruncated)?;
        let points = u64::from(u32::from_le_bytes(count)).min(size.saturating_sub(4) / 24);
        for point in 0..points {
            let mut entry = [0u8; 24];
            let at = start + 4 + point * 24;
            input::read_exact(input, at, &mut entry, DecodeError::WavChunkTruncated)?;
            let (Some(id), Some(frame)) = (le_u32(&entry, 0), le_u32(&entry, 20)) else {
                break;
            };
            let cue = Cue {
                id,
                frame: u64::from(frame),
            };
            if !self.collector.cue(cue)? {
                break;
            }
        }
        Ok(())
    }

    fn sampler(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        start: u64,
        size: u64,
    ) -> Result<(), DecodeError> {
        let mut header = [0u8; 36];
        input::read_exact(input, start, &mut header, DecodeError::WavChunkTruncated)?;
        let (Some(note), Some(count)) = (le_u32(&header, 12), le_u32(&header, 28)) else {
            return Ok(());
        };
        self.collector.unity_note(note);
        let loops = u64::from(count).min(size.saturating_sub(36) / 24);
        for index in 0..loops {
            let mut entry = [0u8; 24];
            let at = start + 36 + index * 24;
            input::read_exact(input, at, &mut entry, DecodeError::WavChunkTruncated)?;
            let field = |at| u64::from(le_u32(&entry, at).unwrap_or(0));
            let kind = match le_u32(&entry, 4).unwrap_or(0) {
                0 => LoopKind::Forward,
                1 => LoopKind::Alternating,
                2 => LoopKind::Backward,
                other => LoopKind::Other(other),
            };
            let sampler_loop = Loop {
                start: field(8),
                end: field(12),
                kind,
                count: le_u32(&entry, 20).unwrap_or(0),
            };
            if !self.collector.sampler_loop(sampler_loop)? {
                break;
            }
        }
        Ok(())
    }
}

/// Read the chunk list of the WAVE file `input` holds.
pub(crate) fn open(
    input: &mut (impl SoundInput + ?Sized),
    limits: &DecodeLimits,
    collector: &mut Collector,
) -> Result<(SoundInfo, Wav), DecodeError> {
    let mut signature = [0u8; 12];
    input::read_exact(input, 0, &mut signature, DecodeError::WavBadMagic)?;
    if !has_signature(&signature) {
        return Err(DecodeError::WavBadMagic);
    }
    let len = input.len();
    let mut walk = Walk {
        collector,
        chunks: 0,
        sixty_four: &signature[..4] != b"RIFF",
        ds64: None,
        format: None,
        data: None,
        fact: None,
        text: Vec::new(),
    };
    let mut at: u64 = 12;
    while at.checked_add(8).is_some_and(|end| end <= len) {
        let Some(next) = walk.chunk(input, at, len, limits)? else {
            break;
        };
        at = next;
    }
    walk.finish(len)
}

impl Walk<'_> {
    /// Read the chunk at `at`, answering where the next begins; [`None`]
    /// for one the file cannot hold, which is the last.
    fn chunk(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        at: u64,
        len: u64,
        limits: &DecodeLimits,
    ) -> Result<Option<u64>, DecodeError> {
        self.count_chunk()?;
        let mut header = [0u8; 8];
        input::read_exact(input, at, &mut header, DecodeError::WavChunkTruncated)?;
        let mut id = [0u8; 4];
        id.copy_from_slice(&header[..4]);
        let field = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        let start = at + 8;
        if self.sixty_four && self.chunks == 1 {
            if &id != b"ds64" {
                return Err(DecodeError::WavMissingDs64);
            }
            let size = usize::try_from(field).map_err(|_| DecodeError::OutOfMemory)?;
            let chunk = Self::read_chunk(input, start, size.min(28 + 12 * MAX_DS64_ENTRIES))?;
            self.ds64 = Some(Ds64::read(&chunk)?);
        }
        let size = match (&self.ds64, field) {
            (Some(ds64), SIZE_IN_DS64) => ds64.size_of(id).ok_or(DecodeError::WavMissingDs64)?,
            _ => u64::from(field),
        };
        let held = len - start;
        let whole = size <= held;
        match &id {
            b"fmt " => {
                if self.format.is_some() {
                    return Err(DecodeError::WavDuplicateFormat);
                }
                let read = usize::try_from(size.min(held))
                    .map_or(MAX_FORMAT_BYTES, |n| n.min(MAX_FORMAT_BYTES));
                let chunk = Self::read_chunk(input, start, read)?;
                self.format = Some(format(&chunk, limits)?);
            }
            b"data" => {
                if self.data.is_some() {
                    return Err(DecodeError::WavDuplicateData);
                }
                self.data = Some((start, size));
            }
            b"fact" if size >= 4 && whole => {
                let mut frames = [0u8; 4];
                input::read_exact(input, start, &mut frames, DecodeError::WavChunkTruncated)?;
                let stated = u32::from_le_bytes(frames);
                self.fact = Some(match (&self.ds64, stated) {
                    (Some(ds64), SIZE_IN_DS64) => ds64.frames,
                    _ => u64::from(stated),
                });
            }
            b"cue " if size >= 4 && whole => self.cues(input, start, size)?,
            b"smpl" if size >= 36 && whole => self.sampler(input, start, size)?,
            b"LIST" if size >= 4 && whole => {
                let mut kind = [0u8; 4];
                input::read_exact(input, start, &mut kind, DecodeError::WavChunkTruncated)?;
                if &kind == b"INFO" {
                    self.info(input, start + 4, start + size)?;
                }
            }
            _ => {}
        }
        Ok(start
            .checked_add(size)
            .and_then(|end| end.checked_add(size & 1)))
    }

    /// The stream the walk found: its format over its data.
    fn finish(self, len: u64) -> Result<(SoundInfo, Wav), DecodeError> {
        let format = self.format.ok_or(DecodeError::WavMissingFormat)?;
        let (data_start, declared) = self.data.ok_or(DecodeError::WavMissingData)?;
        let held = len - data_start;
        let (data_end, data_length) = if declared > held {
            (len, Some(DataLength { declared, held }))
        } else {
            (data_start + declared, None)
        };
        let channels = usize::from(format.channels.channels());
        let bytes = data_end - data_start;
        let align = u64::try_from(format.block_align).map_err(|_| DecodeError::OutOfMemory)?;
        let frames = match &format.codec {
            Codec::Ima { block } | Codec::Ms { block, .. } => {
                let (whole, partial) = frames_in_blocks(&format.codec, *block, channels);
                let rest = usize::try_from(bytes % align).unwrap_or(0);
                let computed = bytes / align * whole + partial(rest);
                self.fact.map_or(computed, |fact| fact.min(computed))
            }
            _ => bytes / align,
        };
        let info = SoundInfo {
            format: SoundFormat::Wav,
            encoding: format.encoding,
            rate: format.rate,
            channels: format.channels,
            sample: format.sample,
            frames: Some(frames),
            seekable: true,
            data_length,
        };
        let wav = Wav {
            data_start,
            data_end,
            channels,
            codec: format.codec,
            frames,
        };
        Ok((info, wav))
    }
}

/// Frames a whole block of an ADPCM codec holds, and how many a final
/// partial block of so many bytes does.
fn frames_in_blocks(codec: &Codec, block: usize, channels: usize) -> (u64, impl Fn(usize) -> u64) {
    let ms = matches!(codec, Codec::Ms { .. });
    let frames_in = move |bytes: usize| -> u64 {
        let frames = if ms {
            msadpcm::frames_in(bytes, channels)
        } else {
            // A partial IMA block keeps its whole words.
            bytes
                .checked_sub(ima::HEADER * channels)
                .map(|data| 1 + data / (4 * channels) * 8)
        };
        frames
            .and_then(|frames| u64::try_from(frames).ok())
            .unwrap_or(0)
    };
    (frames_in(block), frames_in)
}

impl Wav {
    const fn frame_width(&self) -> usize {
        match self.codec {
            Codec::Pcm { width } => width,
            Codec::Float32 | Codec::Float64 => 4,
            Codec::Law(_) | Codec::Ima { .. } | Codec::Ms { .. } => 2,
        }
    }

    /// Write the frames from `position` that `out` has room for.
    pub(crate) fn read(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        position: u64,
        out: &mut [u8],
        scratch: &mut Vec<u8>,
    ) -> Result<usize, DecodeError> {
        let frame_bytes = self.frame_width() * self.channels;
        let left = self.frames.saturating_sub(position);
        let room = out.len() / frame_bytes;
        let frames = usize::try_from(left).map_or(room, |left| left.min(room));
        if frames == 0 {
            return Ok(0);
        }
        let out = &mut out[..frames * frame_bytes];
        let stored = |width: usize| -> Result<u64, DecodeError> {
            let frame =
                u64::try_from(width * self.channels).map_err(|_| DecodeError::OutOfMemory)?;
            Ok(self.data_start + position * frame)
        };
        match &self.codec {
            Codec::Pcm { width } => {
                input::read_exact(input, stored(*width)?, out, DecodeError::InputFailed)?;
            }
            Codec::Float32 => {
                input::read_exact(input, stored(4)?, out, DecodeError::InputFailed)?;
                pcm::finite_each(out);
            }
            Codec::Float64 => {
                if !tairix_util::fallible::grow_to(scratch, out.len() * 2, 0u8) {
                    return Err(DecodeError::OutOfMemory);
                }
                let wide = &mut scratch[..out.len() * 2];
                input::read_exact(input, stored(8)?, wide, DecodeError::InputFailed)?;
                pcm::narrow_doubles(wide, false, out);
            }
            Codec::Law(table) => {
                let codes = out.len() / 2;
                input::read_exact(
                    input,
                    stored(1)?,
                    &mut out[codes..],
                    DecodeError::InputFailed,
                )?;
                pcm::expand_codes_in_place(out, table);
            }
            Codec::Ima { block } | Codec::Ms { block, .. } => {
                self.read_blocks(input, position, *block, out, scratch)?;
            }
        }
        Ok(frames)
    }

    /// Decode the ADPCM blocks covering the frames `out` holds from
    /// `position`, read whole in one go before any is decoded.
    fn read_blocks(
        &self,
        input: &mut (impl SoundInput + ?Sized),
        position: u64,
        block: usize,
        out: &mut [u8],
        scratch: &mut Vec<u8>,
    ) -> Result<(), DecodeError> {
        let (per_block, _) = frames_in_blocks(&self.codec, block, self.channels);
        let frames =
            u64::try_from(out.len() / (2 * self.channels)).map_err(|_| DecodeError::OutOfMemory)?;
        let first = position / per_block;
        let last = (position + frames - 1) / per_block;
        let align = u64::try_from(block).map_err(|_| DecodeError::OutOfMemory)?;
        let start = self.data_start + first * align;
        let end = (self.data_start + (last + 1) * align).min(self.data_end);
        let bytes = usize::try_from(end - start).map_err(|_| DecodeError::OutOfMemory)?;
        if !tairix_util::fallible::grow_to(scratch, bytes, 0u8) {
            return Err(DecodeError::OutOfMemory);
        }
        let blocks = &mut scratch[..bytes];
        input::read_exact(input, start, blocks, DecodeError::InputFailed)?;
        let mut skip = usize::try_from(position % per_block).unwrap_or(0);
        let mut rest = out;
        for bytes in blocks.chunks(block) {
            if rest.is_empty() {
                break;
            }
            let written = match &self.codec {
                Codec::Ms { coefficients, .. } => {
                    msadpcm::decode_block(bytes, self.channels, coefficients, skip, rest)?
                }
                _ => ima::decode_block(bytes, self.channels, skip, rest)?,
            };
            if written == 0 {
                return Err(DecodeError::WavAdpcmBlockCorrupt);
            }
            rest = &mut rest[written * 2 * self.channels..];
            skip = 0;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "wav_tests.rs"]
mod tests;
