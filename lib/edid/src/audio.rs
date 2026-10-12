//! What a sink can play: its short audio descriptors and the speakers it
//! has, in the CTA-861 encoding both an EDID and an ELD carry.

use tairix_inline::ArrayVec;

/// Bytes of one short audio descriptor.
pub(crate) const SAD_LEN: usize = 3;

/// The most short audio descriptors a description is read with: a CTA data
/// block holds ten, and a defence bounds how many blocks may add more.
pub const MAX_AUDIO_DESCRIPTORS: usize = 32;

/// An audio coding a sink decodes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AudioFormat {
    /// Linear PCM.
    Lpcm,
    /// AC-3.
    Ac3,
    /// MPEG-1 layers 1 and 2.
    Mpeg1,
    /// MPEG-1 layer 3.
    Mp3,
    /// MPEG-2 multichannel.
    Mpeg2,
    /// AAC LC.
    AacLc,
    /// DTS.
    Dts,
    /// ATRAC.
    Atrac,
    /// One-bit audio.
    OneBit,
    /// Enhanced AC-3.
    EnhancedAc3,
    /// DTS-HD.
    DtsHd,
    /// MAT, which carries MLP and Dolby `TrueHD`.
    Mat,
    /// DST.
    Dst,
    /// WMA Pro.
    WmaPro,
    /// A format the extended type code names.
    Extended(u8),
}

impl AudioFormat {
    fn decode(code: u8, detail: u8) -> Option<Self> {
        Some(match code {
            1 => Self::Lpcm,
            2 => Self::Ac3,
            3 => Self::Mpeg1,
            4 => Self::Mp3,
            5 => Self::Mpeg2,
            6 => Self::AacLc,
            7 => Self::Dts,
            8 => Self::Atrac,
            9 => Self::OneBit,
            10 => Self::EnhancedAc3,
            11 => Self::DtsHd,
            12 => Self::Mat,
            13 => Self::Dst,
            14 => Self::WmaPro,
            15 => Self::Extended(detail >> 3),
            _ => return None,
        })
    }
}

/// The sample rates a descriptor names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SampleRates(u8);

impl SampleRates {
    /// Each rate a bit names, from bit 0.
    const RATES: [u32; 7] = [32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

    /// No rate.
    pub const NONE: Self = Self(0);
    /// Basic audio's: 32, 44.1 and 48 kHz.
    pub const BASIC: Self = Self(0b111);

    /// The rates either names.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether it names no rate.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether `hz` is among them.
    #[must_use]
    pub fn contains(self, hz: u32) -> bool {
        self.iter().any(|rate| rate == hz)
    }

    /// Each rate, ascending.
    pub fn iter(self) -> impl Iterator<Item = u32> {
        Self::RATES
            .into_iter()
            .enumerate()
            .filter(move |&(bit, _)| self.0 & (1 << bit) != 0)
            .map(|(_, rate)| rate)
    }
}

/// The sample sizes a linear PCM descriptor names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SampleSizes(u8);

impl SampleSizes {
    /// Each size a bit names, from bit 0.
    const BITS: [u8; 3] = [16, 20, 24];

    /// Whether `bits` is among them.
    #[must_use]
    pub fn contains(self, bits: u8) -> bool {
        self.iter().any(|size| size == bits)
    }

    /// Each size, ascending.
    pub fn iter(self) -> impl Iterator<Item = u8> {
        Self::BITS
            .into_iter()
            .enumerate()
            .filter(move |&(bit, _)| self.0 & (1 << bit) != 0)
            .map(|(_, size)| size)
    }
}

/// One coding a sink decodes, and the most channels and the rates it takes
/// it at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ShortAudioDescriptor {
    /// The coding.
    pub format: AudioFormat,
    /// The most channels, 1 to 8.
    pub channels: u8,
    /// The rates.
    pub rates: SampleRates,
    /// The format's own third byte: the sample sizes for linear PCM, the
    /// most bit rate for the older compressed codings.
    pub detail: u8,
}

impl ShortAudioDescriptor {
    /// The descriptor `bytes` hold, or `None` for a reserved coding or one
    /// naming no rate.
    pub(crate) fn decode(bytes: [u8; SAD_LEN]) -> Option<Self> {
        let rates = SampleRates(bytes[1] & 0x7F);
        if rates.0 == 0 {
            return None;
        }
        Some(Self {
            format: AudioFormat::decode((bytes[0] >> 3) & 0xF, bytes[2])?,
            channels: (bytes[0] & 0x7) + 1,
            rates,
            detail: bytes[2],
        })
    }

    /// The sample sizes, for a linear PCM descriptor.
    #[must_use]
    pub const fn lpcm_sizes(&self) -> Option<SampleSizes> {
        match self.format {
            AudioFormat::Lpcm => Some(SampleSizes(self.detail & 0x7)),
            _ => None,
        }
    }
}

/// The speakers a sink has, as the CTA-861 speaker allocation's 24 bits
/// name them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Speakers(u32);

impl Speakers {
    /// Front left and right.
    pub const FRONT: u32 = 1 << 0;
    /// The low-frequency effects channel.
    pub const LFE: u32 = 1 << 1;
    /// Front centre.
    pub const FRONT_CENTRE: u32 = 1 << 2;
    /// Rear left and right.
    pub const REAR: u32 = 1 << 3;
    /// Rear centre.
    pub const REAR_CENTRE: u32 = 1 << 4;
    /// Front left and right of centre.
    pub const FRONT_CENTRE_PAIR: u32 = 1 << 5;
    /// Rear left and right of centre.
    pub const REAR_CENTRE_PAIR: u32 = 1 << 6;

    pub(crate) const fn decode(bytes: [u8; 3]) -> Self {
        Self(bytes[0] as u32 | (bytes[1] as u32) << 8 | (bytes[2] as u32) << 16)
    }

    /// The allocation's bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
}

/// What a sink states about the audio it plays.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Audio {
    /// Whether it plays basic audio: two-channel 16-bit linear PCM at the
    /// [`SampleRates::BASIC`] rates, whatever its descriptors say.
    pub basic: bool,
    /// Each coding it decodes, as it listed them.
    pub descriptors: ArrayVec<ShortAudioDescriptor, MAX_AUDIO_DESCRIPTORS>,
    /// The speakers it has, when it said.
    pub speakers: Option<Speakers>,
}

impl Audio {
    /// Add each descriptor `bytes` holds, keeping as many as fit.
    pub(crate) fn add_descriptors(&mut self, bytes: &[u8]) {
        let (descriptors, _) = bytes.as_chunks::<SAD_LEN>();
        for descriptor in descriptors
            .iter()
            .filter_map(|sad| ShortAudioDescriptor::decode(*sad))
        {
            if self.descriptors.try_push(descriptor).is_err() {
                return;
            }
        }
    }
}
