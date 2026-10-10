//! The stream format word a stream descriptor and its converters share
//! (Intel High Definition Audio Specification 1.0a, section 3.7.1), and the
//! PCM support a converter states (section 7.3.4.7).

use tairix_abi::driver::audio::{Rate, RateSet, SampleFormat, SampleFormats, MAX_DEVICE_RATES};
use tairix_abi::DriverError;

/// The rates `PCM` support bits 0 to 11 name.
const PCM_RATES: [u32; 12] = [
    8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000,
    384_000,
];

/// `PCM` support: sample sizes in bits 16 to 20.
const SIZE_16: u32 = 1 << 17;
const SIZE_20: u32 = 1 << 18;
const SIZE_24: u32 = 1 << 19;
const SIZE_32: u32 = 1 << 20;

/// `STREAM_FORMATS`: the converter carries PCM.
const PCM_STREAMS: u32 = 1;

/// The format word's base-rate bit: 44.1 kHz rather than 48 kHz.
const BASE_44K1: u16 = 1 << 14;

/// The sample format bits a 32-bit container carries, by valid sample size.
const BITS_16: u16 = 0b001 << 4;
const BITS_20: u16 = 0b010 << 4;
const BITS_24: u16 = 0b011 << 4;
const BITS_32: u16 = 0b100 << 4;

/// What a converter's `PCM` and `STREAM_FORMATS` parameters say it carries.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PcmSupport {
    pcm: u32,
    streams: u32,
}

impl PcmSupport {
    /// From the two parameters' answers.
    #[must_use]
    pub const fn new(pcm: u32, streams: u32) -> Self {
        Self { pcm, streams }
    }

    /// Support common to both.
    #[must_use]
    pub const fn and(self, other: Self) -> Self {
        Self {
            pcm: self.pcm & other.pcm,
            streams: self.streams & other.streams,
        }
    }

    /// The sample formats the converter takes without conversion: 16-bit
    /// samples as they are, and 20-, 24- or 32-bit samples most-significant
    /// first in a 32-bit container, which is a signed 32-bit sample the
    /// converter keeps the top of.
    #[must_use]
    pub const fn formats(self) -> SampleFormats {
        let mut formats = SampleFormats::EMPTY;
        if self.streams & PCM_STREAMS == 0 {
            return formats;
        }
        if self.pcm & SIZE_16 != 0 {
            formats = formats.with(SampleFormat::S16);
        }
        if self.pcm & (SIZE_20 | SIZE_24 | SIZE_32) != 0 {
            formats = formats.with(SampleFormat::S32);
        }
        formats
    }

    /// The rates the converter runs at, ascending.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] for a converter stating none.
    pub fn rates(self) -> Result<RateSet, DriverError> {
        let mut rates = [Rate::HZ_48000; MAX_DEVICE_RATES];
        let mut count = 0;
        for (bit, &hz) in PCM_RATES.iter().enumerate() {
            if self.pcm & (1 << bit) != 0 && count < rates.len() {
                rates[count] = Rate::new(hz).map_err(|_| DriverError::Unsupported)?;
                count += 1;
            }
        }
        RateSet::new(&rates[..count]).map_err(|_| DriverError::Unsupported)
    }

    /// Whether the converter runs at `hz`.
    #[must_use]
    pub fn runs_at(self, hz: u32) -> bool {
        PCM_RATES
            .iter()
            .position(|&rate| rate == hz)
            .is_some_and(|bit| self.pcm & (1 << bit) != 0)
    }

    /// The format word's sample bits for `format`.
    fn sample_bits(self, format: SampleFormat) -> Result<u16, DriverError> {
        match format {
            SampleFormat::S16 if self.pcm & SIZE_16 != 0 => Ok(BITS_16),
            SampleFormat::S32 if self.pcm & SIZE_32 != 0 => Ok(BITS_32),
            SampleFormat::S32 if self.pcm & SIZE_24 != 0 => Ok(BITS_24),
            SampleFormat::S32 if self.pcm & SIZE_20 != 0 => Ok(BITS_20),
            _ => Err(DriverError::Unsupported),
        }
    }
}

/// The format word for `channels` channels of `format` at `hz`.
///
/// # Errors
///
/// [`DriverError::Unsupported`] for a rate no base, multiplier and divisor
/// make, a sample format the converter does not take, or a channel count
/// outside one to sixteen.
pub fn stream_format(
    support: PcmSupport,
    hz: u32,
    format: SampleFormat,
    channels: u8,
) -> Result<u16, DriverError> {
    if !(1..=16).contains(&channels) {
        return Err(DriverError::Unsupported);
    }
    Ok(rate_bits(hz)? | support.sample_bits(format)? | u16::from(channels - 1))
}

/// The base, multiplier and divisor bits that make `hz` exactly.
fn rate_bits(hz: u32) -> Result<u16, DriverError> {
    for (base, base_bit) in [(48_000u32, 0u16), (44_100, BASE_44K1)] {
        for multiplier in 1..=4u16 {
            for divisor in 1..=8u16 {
                if base * u32::from(multiplier) == hz * u32::from(divisor) {
                    return Ok(base_bit | ((multiplier - 1) << 11) | ((divisor - 1) << 8));
                }
            }
        }
    }
    Err(DriverError::Unsupported)
}

/// Bytes one frame of `channels` channels of `format` takes in memory.
#[must_use]
pub fn frame_bytes(format: SampleFormat, channels: u8) -> u32 {
    u32::try_from(format.bytes_per_sample()).unwrap_or(u32::MAX) * u32::from(channels)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{frame_bytes, stream_format, PcmSupport};
    use tairix_abi::driver::audio::SampleFormat;
    use tairix_abi::DriverError;

    /// What QEMU's codecs state: 16-bit samples at 16 to 96 kHz.
    const QEMU: PcmSupport = PcmSupport::new((1 << 17) | 0x1FC, 1);

    #[test]
    fn common_rates_encode_as_the_specification_tabulates() {
        assert_eq!(
            stream_format(QEMU, 48_000, SampleFormat::S16, 2),
            Ok(0x0011)
        );
        assert_eq!(
            stream_format(QEMU, 44_100, SampleFormat::S16, 2),
            Ok(0x4011)
        );
        assert_eq!(
            stream_format(QEMU, 96_000, SampleFormat::S16, 2),
            Ok(0x0811)
        );
        assert_eq!(
            stream_format(QEMU, 16_000, SampleFormat::S16, 1),
            Ok(0x0210)
        );
        assert_eq!(
            stream_format(QEMU, 22_050, SampleFormat::S16, 2),
            Ok(0x4111)
        );
        assert_eq!(stream_format(QEMU, 8_000, SampleFormat::S16, 2), Ok(0x0511));
        assert_eq!(
            stream_format(QEMU, 192_000, SampleFormat::S16, 8),
            Ok(0x1817)
        );
    }

    #[test]
    fn a_wide_sample_takes_the_widest_size_the_converter_states() {
        let to_24 = PcmSupport::new((1 << 17) | (1 << 19) | 0x40, 1);
        assert_eq!(
            stream_format(to_24, 48_000, SampleFormat::S32, 2),
            Ok(0x0031)
        );
        let to_32 = PcmSupport::new((1 << 19) | (1 << 20) | 0x40, 1);
        assert_eq!(
            stream_format(to_32, 48_000, SampleFormat::S32, 2),
            Ok(0x0041)
        );
        assert_eq!(
            stream_format(QEMU, 48_000, SampleFormat::S32, 2),
            Err(DriverError::Unsupported)
        );
        assert_eq!(frame_bytes(SampleFormat::S32, 6), 24);
    }

    #[test]
    fn what_no_format_word_spells_is_refused() {
        assert_eq!(
            stream_format(QEMU, 12_345, SampleFormat::S16, 2),
            Err(DriverError::Unsupported)
        );
        assert_eq!(
            stream_format(QEMU, 48_000, SampleFormat::S16, 0),
            Err(DriverError::Unsupported)
        );
        assert_eq!(
            stream_format(QEMU, 48_000, SampleFormat::S16, 17),
            Err(DriverError::Unsupported)
        );
        assert_eq!(
            stream_format(QEMU, 48_000, SampleFormat::F32, 2),
            Err(DriverError::Unsupported)
        );
    }

    #[test]
    fn the_rates_and_formats_a_converter_states_are_what_it_reports() {
        let rates = QEMU.rates().expect("QEMU states rates");
        let hz: std::vec::Vec<u32> = rates.rates().iter().map(|rate| rate.hz()).collect();
        assert_eq!(hz, [16_000, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000]);
        assert!(QEMU.runs_at(48_000) && !QEMU.runs_at(8_000));
        assert!(QEMU.formats().contains(SampleFormat::S16));
        assert!(!QEMU.formats().contains(SampleFormat::S32));
        assert!(PcmSupport::new(QEMU.pcm, 0).formats().is_empty());
    }
}
