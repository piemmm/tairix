//! TI PCM5102A DAC (`ti,pcm5102a`).
//!
//! The part has no control port: its framing and its de-emphasis, filter and
//! mute are strapped by pins, and with no system clock it derives its own
//! from the bit clock. Its driver therefore binds with nothing but the codec
//! duty its node carries, and serves `codec-v1` with what the part accepts,
//! so the digital audio interface it is linked to sends it what it can play.
//!
//! Reference: TI SLAS859, the `PCM510xA` datasheet.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use tairix_abi::driver::audio::{Rate, RateSupport};
use tairix_abi::driver::codec::{
    ClockInversion, Codec, CodecFacts, DaiFormat, DaiFormats, DaiLink, SampleWidths,
};
use tairix_abi::{CapabilityId, DriverBindKey, DriverError, DriverHandle, DriverHost, HwMatchKey};

/// The capabilities the driver runs with, which its signed manifest requests:
/// the codec's endpoint, and the log.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] =
    &[CapabilityId::IPC_BIND_PRIVILEGED, CapabilityId::LOG_EMIT];

/// Device-tree `compatible` string of the part.
pub const PCM5102A_COMPATIBLE: &[u8] = b"ti,pcm5102a";

/// The bind priority [`BIND_KEYS`] carries: an exact `compatible` match.
const BIND_PRIORITY: u16 = 10;

/// The driver's canonical bind table — the single source both the installed
/// bundle's signed manifest and the autoload match are built from.
pub const BIND_KEYS: &[DriverBindKey] = &[DriverBindKey::new(
    BIND_PRIORITY,
    match HwMatchKey::compatible(PCM5102A_COMPATIBLE) {
        Ok(key) => key,
        // A literal too long for the key would fail const evaluation here,
        // never at run time.
        Err(_) => panic!("compatible string fits HW_COMPATIBLE_MAX"),
    },
)];

/// The rates the part converts: 8 kHz to 384 kHz.
const RATES: RateSupport = RateSupport::Continuous {
    min: match Rate::new(8_000) {
        Ok(rate) => rate,
        Err(_) => panic!("8 kHz is in the vocabulary"),
    },
    max: match Rate::new(384_000) {
        Ok(rate) => rate,
        Err(_) => panic!("384 kHz is in the vocabulary"),
    },
};

/// The sample widths it takes in a slot.
const WIDTHS: SampleWidths = match SampleWidths::of(&[16, 24, 32]) {
    Some(widths) => widths,
    None => panic!("every width a slot carries"),
};

/// The PCM5102A.
#[derive(Copy, Clone, Debug, Default)]
pub struct Pcm5102a;

impl Codec for Pcm5102a {
    fn facts(&self) -> CodecFacts {
        CodecFacts {
            rates: RATES,
            widths: WIDTHS,
            // The format pin picks between the two; the board's link states
            // which it strapped.
            formats: DaiFormats::EMPTY
                .with(DaiFormat::I2s)
                .with(DaiFormat::LeftJustified),
            drives_clocks: false,
            gain: None,
        }
    }

    fn configure(&mut self, link: &DaiLink, rate: Rate, width: u8) -> Result<(), DriverError> {
        let facts = self.facts();
        // It only ever follows the interface's clocks, in their normal
        // sense: no control port could tell it otherwise.
        if link.codec_drives_bit_clock
            || link.codec_drives_frame_clock
            || link.inversion != ClockInversion::Normal
            || !facts.formats.contains(link.format)
            || !facts.widths.contains(width)
            || !facts.rates.admits(rate)
        {
            return Err(DriverError::Unsupported);
        }
        Ok(())
    }

    fn set_gain(&mut self, _millibel: i32, _mute: bool) -> Result<i32, DriverError> {
        Err(DriverError::NotImplemented)
    }

    fn start(&mut self) -> Result<(), DriverError> {
        Ok(())
    }

    fn stop(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// Handle marker [`register`] returns; the host re-issues its own. `"P02A"`.
const REGISTER_HANDLE_MARKER: u64 = 0x5030_3241_0000_0001;

/// Driver entry point.
///
/// # Errors
///
/// [`DriverError::PermissionDenied`] if the host did not grant
/// [`CapabilityId::DRV_LOAD`].
///
/// # Capabilities
///
/// Requires [`CapabilityId::DRV_LOAD`]. Serving the codec additionally needs
/// its node's codec `LinkDuty`.
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}

#[cfg(test)]
mod tests {
    use tairix_abi::driver::audio::Rate;
    use tairix_abi::driver::codec::{ClockInversion, Codec, DaiFormat, DaiLink};
    use tairix_abi::DriverError;

    use super::Pcm5102a;

    fn link(format: DaiFormat, codec_clocks: bool) -> DaiLink {
        DaiLink {
            format,
            codec_drives_bit_clock: codec_clocks,
            codec_drives_frame_clock: codec_clocks,
            inversion: ClockInversion::Normal,
            cpu_dai: 0,
            codec_dai: 0,
        }
    }

    fn rate(hz: u32) -> Rate {
        Rate::new(hz).expect("a rate")
    }

    #[test]
    fn it_takes_what_its_strapping_allows_and_owns_no_gain() {
        let mut part = Pcm5102a;
        let facts = part.facts();
        assert!(facts.widths.contains(32) && !facts.widths.contains(20));
        assert!(facts.gain.is_none());
        assert!(!facts.drives_clocks);
        assert_eq!(
            part.configure(&link(DaiFormat::I2s, false), rate(48_000), 32),
            Ok(())
        );
        assert_eq!(
            part.configure(&link(DaiFormat::LeftJustified, false), rate(384_000), 16),
            Ok(())
        );
        assert_eq!(part.set_gain(0, false), Err(DriverError::NotImplemented));
    }

    #[test]
    fn a_link_asking_it_to_drive_the_clocks_or_frame_otherwise_is_refused() {
        let mut part = Pcm5102a;
        for (link, rate, width) in [
            (link(DaiFormat::I2s, true), rate(48_000), 32),
            (
                DaiLink {
                    inversion: ClockInversion::BitClock,
                    ..link(DaiFormat::I2s, false)
                },
                rate(48_000),
                32,
            ),
            (link(DaiFormat::DspA, false), rate(48_000), 32),
            (link(DaiFormat::I2s, false), rate(48_000), 20),
            (link(DaiFormat::I2s, false), rate(768_000), 32),
            (link(DaiFormat::I2s, false), rate(4_000), 32),
        ] {
            assert_eq!(
                part.configure(&link, rate, width),
                Err(DriverError::Unsupported),
                "{link:?} {rate:?} {width}"
            );
        }
    }
}
