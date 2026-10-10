//! First-party TAIRiX sound-file decoding (`lib/sound`).
//!
//! A player reads a file's samples through this crate, inside the parser
//! sandbox: complete, `no_std` + `alloc`, `unsafe`-free decoders that turn an
//! untrusted file into interleaved PCM a block at a time, or a typed refusal —
//! never a panic, and never more memory than the caller allows. Moving and
//! mixing samples is `lib/audio`'s; this crate knows files and nothing of
//! devices.
//!
//! [`PcmSource::open`] dispatches on the format [`sniff`] recognises — AU
//! ([`SoundFormat::Au`]), WAV ([`SoundFormat::Wav`]) or FLAC
//! ([`SoundFormat::Flac`]) — each read by its own private module; [`probe`]
//! reads only what the header states. Every format claimed is claimed
//! completely, and a variant that would be half-read is refused by name. An
//! `ID3v2` tag other software put ahead of a FLAC stream is stepped over.
//!
//! # Streaming
//!
//! A decoder reads its file through [`SoundInput`], at offsets of its own
//! choosing and never more than [`MAX_READ`] bytes at once, and holds at most
//! one block's bytes, so a four-hour recording decodes in the memory of one
//! block. An input whose bytes are not at hand answers
//! [`InputError::Unavailable`], and the call that met it changes nothing the
//! caller can see, so its caller supplies the bytes and asks again: that is
//! how the sandbox's worker decodes a file it never holds whole.
//! [`max_working_set`] states the most of the file one call reads.
//!
//! # Bounds and fail-closed policy
//!
//! [`DecodeLimits`] bounds what a file can make a decoder keep: channels,
//! metadata text and markers. Every size a file declares is weighed against
//! the bytes it holds or the limits before it is used, and all arithmetic over
//! a declared value is checked, so a malformed file is refused with a
//! [`DecodeError`] and never panics.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::vec::Vec;

use tairix_abi::driver::audio::{ChannelMap, Rate, SampleFormat};

mod au;
mod bits;
mod comment;
mod crc;
mod flac;
#[cfg(any(test, feature = "encode"))]
pub mod flac_encode;
mod flac_frame;
mod g711;
mod g722;
mod g72x;
mod id3;
mod ima;
mod input;
mod md5;
mod meta;
mod msadpcm;
mod ogg;
mod pcm;
mod wav;

pub use input::{InputError, SoundInput, MAX_READ};
pub use meta::{CoverRange, Cue, Loop, LoopKind, Metadata, Tag, TagKey, TagKind, TAG_KEY_MAX};

/// A sound-file format this crate decodes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SoundFormat {
    /// Sun/NeXT audio (`.au`, `.snd`).
    Au,
    /// RIFF WAVE, and its RF64 and BW64 forms (`.wav`).
    Wav,
    /// Native FLAC (`.flac`).
    Flac,
    /// Ogg (`.ogg`, `.oga`), whose logical streams carry the codec.
    Ogg,
}

impl SoundFormat {
    /// The stable machine word naming the format, for a record a tool reads.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Au => "au",
            Self::Wav => "wav",
            Self::Flac => "flac",
            Self::Ogg => "ogg",
        }
    }

    /// Whether other software may put an `ID3v2` tag ahead of the format.
    const fn follows_id3(self) -> bool {
        matches!(self, Self::Flac)
    }
}

impl core::fmt::Display for SoundFormat {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Au => "AU",
            Self::Wav => "WAV",
            Self::Flac => "FLAC",
            Self::Ogg => "Ogg",
        })
    }
}

/// Bytes [`sniff`] needs to recognise every format.
pub const SNIFF_LEN: usize = 12;

/// The format `bytes`, a file's first [`SNIFF_LEN`] bytes or more, opens
/// with.
#[must_use]
pub fn sniff(bytes: &[u8]) -> Option<SoundFormat> {
    if au::has_signature(bytes) {
        Some(SoundFormat::Au)
    } else if wav::has_signature(bytes) {
        Some(SoundFormat::Wav)
    } else if flac::has_signature(bytes) {
        Some(SoundFormat::Flac)
    } else if ogg::has_signature(bytes) {
        Some(SoundFormat::Ogg)
    } else {
        None
    }
}

/// How a file stores its samples.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Encoding {
    /// Linear PCM of `bits` bits a sample.
    Linear {
        /// Bits a sample holds.
        bits: u8,
    },
    /// Fixed-point fractions of `bits` bits, which read as linear PCM.
    Fixed {
        /// Bits a sample holds.
        bits: u8,
    },
    /// IEEE 754 floating point of `bits` bits.
    Float {
        /// Bits a sample holds.
        bits: u8,
    },
    /// ITU-T G.711 μ-law.
    MuLaw,
    /// ITU-T G.711 A-law.
    ALaw,
    /// ITU-T G.721 ADPCM at 32 kbit/s.
    G721,
    /// ITU-T G.722 sub-band ADPCM at 64 kbit/s.
    G722,
    /// ITU-T G.723 ADPCM at 24 kbit/s.
    G723Kbit24,
    /// ITU-T G.723 ADPCM at 40 kbit/s.
    G723Kbit40,
    /// Microsoft ADPCM.
    MsAdpcm,
    /// IMA/DVI ADPCM.
    ImaAdpcm,
    /// FLAC.
    Flac,
}

impl Encoding {
    /// The stable machine word naming the encoding, for a record a tool reads;
    /// a sample's width is [`Self::bits`].
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Linear { .. } => "pcm",
            Self::Fixed { .. } => "fixed",
            Self::Float { .. } => "float",
            Self::MuLaw => "mulaw",
            Self::ALaw => "alaw",
            Self::G721 => "g721",
            Self::G722 => "g722",
            Self::G723Kbit24 => "g723-24",
            Self::G723Kbit40 => "g723-40",
            Self::MsAdpcm => "ms-adpcm",
            Self::ImaAdpcm => "ima-adpcm",
            Self::Flac => "flac",
        }
    }

    /// Bits a sample holds, where the encoding is a width of plain samples.
    #[must_use]
    pub const fn bits(self) -> Option<u8> {
        match self {
            Self::Linear { bits } | Self::Fixed { bits } | Self::Float { bits } => Some(bits),
            _ => None,
        }
    }
}

impl core::fmt::Display for Encoding {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Linear { bits } => write!(f, "{bits}-bit PCM"),
            Self::Fixed { bits } => write!(f, "{bits}-bit fixed point"),
            Self::Float { bits } => write!(f, "{bits}-bit float"),
            Self::MuLaw => f.write_str("μ-law"),
            Self::ALaw => f.write_str("A-law"),
            Self::G721 => f.write_str("G.721 ADPCM"),
            Self::G722 => f.write_str("G.722"),
            Self::G723Kbit24 => f.write_str("G.723 ADPCM, 24 kbit/s"),
            Self::G723Kbit40 => f.write_str("G.723 ADPCM, 40 kbit/s"),
            Self::MsAdpcm => f.write_str("Microsoft ADPCM"),
            Self::ImaAdpcm => f.write_str("IMA ADPCM"),
            Self::Flac => f.write_str("FLAC"),
        }
    }
}

/// A data length the header declares that the file does not hold: the file
/// wins, and this says by how much they disagreed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DataLength {
    /// The bytes of sound the header declared.
    pub declared: u64,
    /// The bytes of sound the file holds.
    pub held: u64,
}

/// What a stream is, as its file states it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SoundInfo {
    /// The file's format.
    pub format: SoundFormat,
    /// How the file stores its samples.
    pub encoding: Encoding,
    /// Frames a second.
    pub rate: Rate,
    /// The channels each frame interleaves, in order.
    pub channels: ChannelMap,
    /// The sample format blocks are written in.
    pub sample: SampleFormat,
    /// Frames the stream holds, where the file says.
    pub frames: Option<u64>,
    /// Whether the stream can be entered at any frame.
    pub seekable: bool,
    /// The disagreement between the data length declared and held, where
    /// there was one.
    pub data_length: Option<DataLength>,
}

impl SoundInfo {
    /// Bytes a written frame occupies.
    #[must_use]
    pub const fn frame_bytes(&self) -> usize {
        self.sample.bytes_per_sample() * self.channels.channels() as usize
    }
}

/// What a file may make a decoder keep.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DecodeLimits {
    channels: u8,
    metadata_bytes: u32,
    markers: u32,
}

impl DecodeLimits {
    /// Limits of `max_channels` channels, `max_metadata_bytes` of kept tags
    /// and `max_markers` cues and loops between them.
    #[must_use]
    pub const fn new(max_channels: u8, max_metadata_bytes: u32, max_markers: u32) -> Self {
        Self {
            channels: max_channels,
            metadata_bytes: max_metadata_bytes,
            markers: max_markers,
        }
    }

    /// Most channels a stream may interleave.
    #[must_use]
    pub const fn max_channels(&self) -> u8 {
        self.channels
    }

    /// Most bytes kept tags may occupy, their text and a fixed charge for
    /// each one's entry; past it, tags are left out.
    #[must_use]
    pub const fn max_metadata_bytes(&self) -> u32 {
        self.metadata_bytes
    }

    /// Most cues and loops kept between them; past it, they are left out.
    #[must_use]
    pub const fn max_markers(&self) -> u32 {
        self.markers
    }
}

/// What a probe keeps: nothing past what the header states.
const PROBE_LIMITS: DecodeLimits = DecodeLimits::new(u8::MAX, 0, 0);

/// Why decoding a sound failed. Every variant is a fail-closed refusal: no
/// malformed, truncated or adversarial file ever panics or yields samples it
/// does not hold.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
    /// [`sniff`] recognised no format.
    UnknownFormat,
    /// The input does not hold the bytes yet; nothing changed, and the call
    /// may be made again once it does.
    InputUnavailable,
    /// The input failed.
    InputFailed,
    /// A buffer the decode needs was refused by the allocator: a property of
    /// the machine, not of the file.
    OutOfMemory,
    /// The stream interleaves more channels than the limits allow.
    ChannelsExceedLimit,
    /// The stream declares no channels.
    NoChannels,
    /// The stream's channels have no layout the PCM vocabulary states.
    ChannelLayoutUnsupported,
    /// The stream's rate is outside the PCM vocabulary's.
    RateOutOfRange,
    /// The caller's block holds not one whole frame.
    BufferTooSmall,
    /// The stream cannot be entered except at its start.
    SeekUnsupported,
    /// The frame sought lies past the stream's end.
    SeekPastEnd,

    /// The file does not open with `.snd`.
    AuBadMagic,
    /// The file ends inside the AU header.
    AuHeaderTruncated,
    /// The data offset lies inside the header or past the file.
    AuDataOffsetBad,
    /// Fragmented sample data: pointers to samples elsewhere, not samples.
    AuFragmentedData,
    /// A nested sound structure, not samples.
    AuNestedSound,
    /// A DSP program, not samples.
    AuDspProgram,
    /// Display data, not samples.
    AuDisplayData,
    /// Music-kit DSP commands, not samples.
    AuDspCommands,
    /// A squelched, emphasised or compressed variant whose processing the
    /// format never specifies.
    AuUnspecifiedEncoding,
    /// An encoding the format does not define.
    AuUnknownEncoding(u32),
    /// ADPCM in more than one channel, whose interleave the format never
    /// specifies.
    AuAdpcmChannels,
    /// G.722 at a rate other than the 16 kHz it decodes to.
    AuG722Rate,

    /// The file is not RIFF, RF64 or BW64 holding `WAVE`.
    WavBadMagic,
    /// A chunk header or a chunk the decode reads runs past the file.
    WavChunkTruncated,
    /// More chunks than a WAVE file is read through.
    WavTooManyChunks,
    /// RF64 or BW64 without its `ds64` chunk first.
    WavMissingDs64,
    /// No `fmt ` chunk precedes the `data` chunk.
    WavMissingFormat,
    /// No `data` chunk.
    WavMissingData,
    /// A second `fmt ` chunk.
    WavDuplicateFormat,
    /// A second `data` chunk.
    WavDuplicateData,
    /// The `fmt ` chunk is shorter than its format needs.
    WavFormatTruncated,
    /// MPEG audio in a WAVE container: that codec's decoder's to read.
    WavMpegAudio,
    /// GSM 6.10 in a WAVE container: that codec's decoder's to read.
    WavGsm610,
    /// A format tag with no decoder here.
    WavUnknownFormatTag(u16),
    /// A block alignment that disagrees with the format.
    WavBadBlockAlign,
    /// A bit depth the format does not take.
    WavBadBitDepth,
    /// A `WAVE_FORMAT_EXTENSIBLE` extension that does not hold together.
    WavBadExtensible,
    /// An extensible format's subformat with no decoder here.
    WavUnknownSubformat,
    /// A channel mask naming a position the PCM vocabulary lacks, or other
    /// than as many positions as channels.
    WavChannelMask,
    /// An ADPCM format whose parameters do not hold together.
    WavBadAdpcmFormat,
    /// An ADPCM block whose header is out of range.
    WavAdpcmBlockCorrupt,

    /// The stream does not open with `fLaC`.
    FlacBadMarker,
    /// No `STREAMINFO` block first.
    FlacMissingStreamInfo,
    /// A `STREAMINFO` block stating what the format forbids: a block size
    /// under 16, a minimum over the maximum, samples narrower than 4 bits.
    FlacBadStreamInfo,
    /// A second `STREAMINFO`, seek table or Vorbis comment block.
    FlacDuplicateBlock,
    /// A metadata block of the forbidden type 127.
    FlacForbiddenBlock,
    /// A metadata block runs past the file.
    FlacMetadataTruncated,
    /// A seek table that is no whole number of points.
    FlacBadSeekTable,
    /// A Vorbis comment block that does not hold together.
    FlacBadComment,
    /// A cuesheet that does not hold together.
    FlacBadCuesheet,
    /// A picture block that does not hold together.
    FlacBadPicture,
    /// An application block shorter than its id.
    FlacBadApplication,
    /// A channel mask naming a position the PCM vocabulary lacks, or other
    /// than as many positions as channels.
    FlacChannelMask,
    /// No frame where one should start.
    FlacNoSync,
    /// A frame header that fails its CRC-8.
    FlacHeaderCrc,
    /// A frame that fails its CRC-16.
    FlacFrameCrc,
    /// A reserved or forbidden code in a frame.
    FlacReserved,
    /// A frame whose values cannot hold: a sample past its width, a
    /// partition shorter than its predictor, a block past the stream's.
    FlacFrameInvalid,
    /// A frame whose rate, width or channels differ from `STREAMINFO`'s.
    FlacFrameMismatch,
    /// A frame that does not continue the stream where it should.
    FlacFrameOutOfOrder,
    /// A frame larger than twice its samples stored verbatim: more unary
    /// padding than sound, which no encoder needs to write.
    FlacFrameTooLarge,
    /// The stream ends inside a frame, or before the samples it states.
    FlacTruncated,
    /// The samples, decoded whole, disagree with the stream's own digest.
    FlacDigestMismatch,

    /// No page capture pattern where a page should start.
    OggNoCapture,
    /// A page of a stream structure version other than zero.
    OggBadPage,
    /// A page runs past the file.
    OggPageTruncated,
    /// A page that fails its CRC.
    OggPageCrc,
    /// A page missing from the stream's sequence.
    OggPageLost,
    /// A packet continued where none was begun, or begun where one was
    /// continued, or a frame packet holding other than one frame.
    OggBadPacket,
    /// A packet longer than anything it may hold.
    OggPacketTooLarge,
    /// No logical stream holding a codec decoded here.
    OggNoFlacStream,
    /// A FLAC mapping header of another version or form.
    OggBadFlacMapping,
    /// The stream ends within its headers.
    OggNoAudio,
    /// A stream chained after the one decoded, which this decoder does not
    /// read on into.
    OggChained,
}

impl DecodeError {
    const fn message(self) -> &'static str {
        match self {
            Self::UnknownFormat => "no sound format recognised",
            Self::InputUnavailable => "the input does not hold the bytes yet",
            Self::InputFailed => "the input failed",
            Self::OutOfMemory => "out of memory",
            Self::ChannelsExceedLimit => "more channels than the limits allow",
            Self::NoChannels => "no channels",
            Self::ChannelLayoutUnsupported => "a channel layout with no reading",
            Self::RateOutOfRange => "a rate out of range",
            Self::BufferTooSmall => "a block too small for one frame",
            Self::SeekUnsupported => "the stream cannot be entered past its start",
            Self::SeekPastEnd => "a frame past the end",
            Self::AuBadMagic => "AU: no .snd signature",
            Self::AuHeaderTruncated => "AU: the header is truncated",
            Self::AuDataOffsetBad => "AU: the data offset is out of range",
            Self::AuFragmentedData => "AU: fragmented sample data",
            Self::AuNestedSound => "AU: a nested sound",
            Self::AuDspProgram => "AU: a DSP program",
            Self::AuDisplayData => "AU: display data",
            Self::AuDspCommands => "AU: DSP commands",
            Self::AuUnspecifiedEncoding => "AU: an encoding the format never specifies",
            Self::AuUnknownEncoding(_) => "AU: an unknown encoding",
            Self::AuAdpcmChannels => "AU: ADPCM in more than one channel",
            Self::AuG722Rate => "AU: G.722 at a rate other than 16 kHz",
            Self::WavBadMagic => "WAV: no RIFF WAVE signature",
            Self::WavChunkTruncated => "WAV: a chunk runs past the file",
            Self::WavTooManyChunks => "WAV: too many chunks",
            Self::WavMissingDs64 => "WAV: RF64 without ds64",
            Self::WavMissingFormat => "WAV: no fmt chunk before the data",
            Self::WavMissingData => "WAV: no data chunk",
            Self::WavDuplicateFormat => "WAV: a second fmt chunk",
            Self::WavDuplicateData => "WAV: a second data chunk",
            Self::WavFormatTruncated => "WAV: the fmt chunk is truncated",
            Self::WavMpegAudio => "WAV: MPEG audio inside",
            Self::WavGsm610 => "WAV: GSM 6.10 inside",
            Self::WavUnknownFormatTag(_) => "WAV: an unknown format tag",
            Self::WavBadBlockAlign => "WAV: a block alignment out of step",
            Self::WavBadBitDepth => "WAV: an unsupported bit depth",
            Self::WavBadExtensible => "WAV: a malformed extensible format",
            Self::WavUnknownSubformat => "WAV: an unknown subformat",
            Self::WavChannelMask => "WAV: a channel mask with no reading",
            Self::WavBadAdpcmFormat => "WAV: inconsistent ADPCM parameters",
            Self::WavAdpcmBlockCorrupt => "WAV: a corrupt ADPCM block",
            Self::FlacBadMarker => "FLAC: no fLaC marker",
            Self::FlacMissingStreamInfo => "FLAC: no STREAMINFO first",
            Self::FlacBadStreamInfo => "FLAC: STREAMINFO states what the format forbids",
            Self::FlacDuplicateBlock => "FLAC: a second STREAMINFO, seek table or comment block",
            Self::FlacForbiddenBlock => "FLAC: a metadata block of the forbidden type",
            Self::FlacMetadataTruncated => "FLAC: a metadata block runs past the file",
            Self::FlacBadSeekTable => "FLAC: a malformed seek table",
            Self::FlacBadComment => "FLAC: a malformed Vorbis comment",
            Self::FlacBadCuesheet => "FLAC: a malformed cuesheet",
            Self::FlacBadPicture => "FLAC: a malformed picture block",
            Self::FlacBadApplication => "FLAC: an application block shorter than its id",
            Self::FlacChannelMask => "FLAC: a channel mask with no reading",
            Self::FlacNoSync => "FLAC: no frame where one should start",
            Self::FlacHeaderCrc => "FLAC: a frame header fails its CRC",
            Self::FlacFrameCrc => "FLAC: a frame fails its CRC",
            Self::FlacReserved => "FLAC: a reserved code in a frame",
            Self::FlacFrameInvalid => "FLAC: a frame whose values cannot hold",
            Self::FlacFrameMismatch => "FLAC: a frame disagrees with STREAMINFO",
            Self::FlacFrameOutOfOrder => "FLAC: a frame out of sequence",
            Self::FlacFrameTooLarge => "FLAC: a frame larger than twice its samples verbatim",
            Self::FlacTruncated => "FLAC: the stream ends before its samples",
            Self::FlacDigestMismatch => {
                "FLAC: the decoded samples disagree with the stream's digest"
            }
            Self::OggNoCapture => "Ogg: no page where one should start",
            Self::OggBadPage => "Ogg: a page of an unknown version",
            Self::OggPageTruncated => "Ogg: a page runs past the file",
            Self::OggPageCrc => "Ogg: a page fails its CRC",
            Self::OggPageLost => "Ogg: a page is missing",
            Self::OggBadPacket => "Ogg: a malformed packet",
            Self::OggPacketTooLarge => "Ogg: a packet larger than it may be",
            Self::OggNoFlacStream => "Ogg: no stream decoded here",
            Self::OggBadFlacMapping => "Ogg: an unknown FLAC mapping",
            Self::OggNoAudio => "Ogg: the stream ends within its headers",
            Self::OggChained => "Ogg: a chained stream follows, which is not read on into",
        }
    }
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.message())
    }
}

/// The channel map of `count` channels within `limits`, in the conventional
/// order a format that states no positions has.
pub(crate) fn conventional_channels(
    count: u32,
    limits: &DecodeLimits,
) -> Result<ChannelMap, DecodeError> {
    if count == 0 {
        return Err(DecodeError::NoChannels);
    }
    let count = u8::try_from(count).map_err(|_| DecodeError::ChannelsExceedLimit)?;
    if count > limits.max_channels() {
        return Err(DecodeError::ChannelsExceedLimit);
    }
    ChannelMap::conventional(count).ok_or(DecodeError::ChannelLayoutUnsupported)
}

/// The channel map a WAVE speaker mask states for `count` channels: the
/// positions its set bits name, in bit order. `None` for a mask naming other
/// than `count` positions, or one the PCM vocabulary lacks.
pub(crate) fn speaker_map(count: u8, mask: u32) -> Option<ChannelMap> {
    use tairix_abi::driver::audio::{ChannelPosition, MAX_CHANNELS};
    if mask.count_ones() != u32::from(count) {
        return None;
    }
    if count == 1 {
        return Some(ChannelMap::MONO);
    }
    let mut positions = [ChannelPosition::Mono; MAX_CHANNELS];
    for (slot, bit) in positions
        .iter_mut()
        .zip((0..32).filter(|bit| mask & (1 << bit) != 0))
    {
        *slot = match 1u32 << bit {
            0x1 => ChannelPosition::FrontLeft,
            0x2 => ChannelPosition::FrontRight,
            0x4 => ChannelPosition::FrontCentre,
            0x8 => ChannelPosition::LowFrequency,
            0x10 => ChannelPosition::RearLeft,
            0x20 => ChannelPosition::RearRight,
            0x200 => ChannelPosition::SideLeft,
            0x400 => ChannelPosition::SideRight,
            _ => return None,
        };
    }
    ChannelMap::new(positions.get(..usize::from(count))?).ok()
}

/// The most bytes one [`PcmSource::next_block`] call reads from its input
/// under `limits`, writing at most `block_frames` frames a block: what an
/// input served from a bounded cache must hold at once for any stream within
/// them. A seek may read a search's worth more, a probe at a time.
#[must_use]
pub const fn max_working_set(limits: &DecodeLimits, block_frames: u32) -> u64 {
    let channels = limits.max_channels() as u64;
    let sampled = block_frames as u64 * channels * 8;
    let adpcm = block_frames as u64 * channels + 2 * u16::MAX as u64;
    let flac = ogg::max_working_set(limits);
    let pcm = if sampled > adpcm { sampled } else { adpcm };
    if pcm > flac {
        pcm
    } else {
        flac
    }
}

/// A format's reader, once its header has been read.
#[allow(
    clippy::large_enum_variant,
    reason = "one a stream, so its size buys nothing to box away"
)]
enum Body {
    Au(au::Au),
    Wav(wav::Wav),
    Flac(flac::Flac),
    Ogg(ogg::OggFlac),
}

/// The PCM a sound file holds, read a block at a time.
///
/// The source holds no input: each call that reads names the one it was
/// opened over, so a caller that met [`DecodeError::InputUnavailable`] can
/// supply the bytes and call again.
pub struct PcmSource {
    info: SoundInfo,
    metadata: Metadata,
    body: Body,
    position: u64,
    scratch: Vec<u8>,
}

impl PcmSource {
    /// Open the file `input` holds, in the format [`sniff`] recognises.
    ///
    /// # Errors
    ///
    /// [`DecodeError::UnknownFormat`], or the format's refusal.
    pub fn open(
        input: &mut (impl SoundInput + ?Sized),
        limits: &DecodeLimits,
    ) -> Result<Self, DecodeError> {
        let mut signature = [0u8; SNIFF_LEN];
        let held = input::read(input, 0, &mut signature)?;
        let format = if let Some(format) = sniff(&signature[..held]) {
            format
        } else {
            let start = id3::id3v2_len(&signature[..held]).ok_or(DecodeError::UnknownFormat)?;
            let held = input::read(input, start, &mut signature)?;
            sniff(&signature[..held])
                .filter(|format| format.follows_id3())
                .ok_or(DecodeError::UnknownFormat)?
        };
        Self::open_as(format, input, limits)
    }

    /// Open the file `input` holds as `format`, whose own reader still
    /// checks it.
    ///
    /// # Errors
    ///
    /// The format's refusal.
    pub fn open_as(
        format: SoundFormat,
        input: &mut (impl SoundInput + ?Sized),
        limits: &DecodeLimits,
    ) -> Result<Self, DecodeError> {
        let mut collector = meta::Collector::new(limits);
        let (info, body) = match format {
            SoundFormat::Au => {
                let (info, au) = au::open(input, limits, &mut collector)?;
                (info, Body::Au(au))
            }
            SoundFormat::Wav => {
                let (info, wav) = wav::open(input, limits, &mut collector)?;
                (info, Body::Wav(wav))
            }
            SoundFormat::Flac => {
                let start = Self::past_id3(input)?;
                let (info, flac) = flac::open(input, start, limits, &mut collector)?;
                (info, Body::Flac(flac))
            }
            SoundFormat::Ogg => {
                let (info, ogg) = ogg::open(input, limits, &mut collector)?;
                (info, Body::Ogg(ogg))
            }
        };
        Ok(Self {
            info,
            metadata: collector.finish(),
            body,
            position: 0,
            scratch: Vec::new(),
        })
    }

    /// Where a format other software may tag ahead starts: past an `ID3v2`
    /// tag, where one opens the file.
    fn past_id3(input: &mut (impl SoundInput + ?Sized)) -> Result<u64, DecodeError> {
        let mut header = [0u8; id3::ID3V2_HEADER_LEN];
        let held = input::read(input, 0, &mut header)?;
        Ok(id3::id3v2_len(&header[..held]).unwrap_or(0))
    }

    /// What the stream is.
    #[must_use]
    pub const fn info(&self) -> &SoundInfo {
        &self.info
    }

    /// What the file says about it.
    #[must_use]
    pub const fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// The frame the next block starts at.
    #[must_use]
    pub const fn position(&self) -> u64 {
        self.position
    }

    /// Write whole frames into `out` from the position on, answering how
    /// many: as many as it holds, unless the stream ends first or a format
    /// that reads its file a frame at a time meets bytes not yet at hand once
    /// it has written some; none once the stream has ended.
    ///
    /// # Errors
    ///
    /// [`DecodeError::BufferTooSmall`] for a block short of one frame,
    /// [`DecodeError::InputUnavailable`] with nothing changed, or the
    /// format's refusal of what it met.
    pub fn next_block(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        out: &mut [u8],
    ) -> Result<usize, DecodeError> {
        let frame_bytes = self.info.frame_bytes();
        let fit = out.len() / frame_bytes;
        if fit == 0 {
            return Err(DecodeError::BufferTooSmall);
        }
        let out = &mut out[..fit * frame_bytes];
        let written = match &mut self.body {
            Body::Au(au) => au.read(input, self.position, out, &mut self.scratch)?,
            Body::Wav(wav) => wav.read(input, self.position, out, &mut self.scratch)?,
            Body::Flac(flac) => flac.read(input, self.position, out)?,
            Body::Ogg(ogg) => ogg.read(input, self.position, out)?,
        };
        self.position += u64::try_from(written).map_err(|_| DecodeError::OutOfMemory)?;
        Ok(written)
    }

    /// Move to `frame`, so the next block starts there.
    ///
    /// # Errors
    ///
    /// [`DecodeError::SeekUnsupported`] for a stream that can only be read
    /// from its start, or [`DecodeError::SeekPastEnd`].
    pub fn seek(&mut self, frame: u64) -> Result<(), DecodeError> {
        if !self.info.seekable {
            return Err(DecodeError::SeekUnsupported);
        }
        if self.info.frames.is_some_and(|frames| frame > frames) {
            return Err(DecodeError::SeekPastEnd);
        }
        self.position = frame;
        Ok(())
    }
}

/// What the file `input` holds states about its stream, read from its header
/// alone and keeping none of its metadata.
///
/// # Errors
///
/// [`DecodeError::UnknownFormat`], or the format's refusal.
pub fn probe(input: &mut (impl SoundInput + ?Sized)) -> Result<SoundInfo, DecodeError> {
    PcmSource::open(input, &PROBE_LIMITS).map(|source| source.info)
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::vec::Vec;

    use super::{Encoding, SoundFormat};

    const ENCODINGS: [Encoding; 12] = [
        Encoding::Linear { bits: 16 },
        Encoding::Fixed { bits: 24 },
        Encoding::Float { bits: 32 },
        Encoding::MuLaw,
        Encoding::ALaw,
        Encoding::G721,
        Encoding::G722,
        Encoding::G723Kbit24,
        Encoding::G723Kbit40,
        Encoding::MsAdpcm,
        Encoding::ImaAdpcm,
        Encoding::Flac,
    ];

    /// A tool tells encodings apart by the token alone, so no two share one.
    #[test]
    fn every_encoding_has_its_own_token_and_a_width_only_where_it_has_one() {
        let mut tokens: Vec<&str> = ENCODINGS.iter().map(|encoding| encoding.token()).collect();
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(tokens.len(), ENCODINGS.len());
        assert_eq!(Encoding::Linear { bits: 16 }.bits(), Some(16));
        assert_eq!(Encoding::ImaAdpcm.bits(), None);
        assert_eq!(format!("{}", Encoding::Linear { bits: 24 }), "24-bit PCM");
    }

    #[test]
    fn a_format_is_named_for_people_and_for_tools() {
        assert_eq!(format!("{}", SoundFormat::Wav), "WAV");
        assert_eq!(SoundFormat::Wav.token(), "wav");
        assert_eq!(SoundFormat::Au.token(), "au");
    }
}
