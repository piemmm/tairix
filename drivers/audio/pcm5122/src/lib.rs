//! TI PCM5122 DAC over I2C (`ti,pcm5122`).
//!
//! The part follows its digital audio interface's clocks and makes its own
//! system clock from the bit clock with its PLL, its dividers set
//! automatically from the rates it detects; the driver programs the framing
//! and the word length the link and the stream state, and serves the part's
//! digital volume, 24 dB to −103 dB in half-decibel steps, as the stream's
//! gain. Its whole authority over the part is the transfer endpoint its node's
//! grant names, so it reaches no other device on the bus.
//!
//! References: TI SLASE91, the `PCM512x` datasheet; Linux
//! `sound/soc/codecs/pcm512x.c` for the consumer-mode sequence.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use tairix_abi::driver::audio::{GainRange, Rate, RateSet, RateSupport};
use tairix_abi::driver::codec::{Codec, CodecFacts, DaiFormat, DaiFormats, DaiLink, SampleWidths};
use tairix_abi::driver::i2c::I2cPort;
use tairix_abi::{
    CapabilityId, Delay, DriverBindKey, DriverError, DriverHandle, DriverHost, HwMatchKey,
};
use tairix_i2c::Device;

/// The capabilities the driver runs with, which its signed manifest requests:
/// the transfer endpoint its node's grant names, the codec's endpoint, and
/// the log.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::IPC_ENDPOINT,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::LOG_EMIT,
];

/// Device-tree `compatible` string of the part.
pub const PCM5122_COMPATIBLE: &[u8] = b"ti,pcm5122";

/// The bind priority [`BIND_KEYS`] carries: an exact `compatible` match.
const BIND_PRIORITY: u16 = 10;

/// The driver's canonical bind table — the single source both the installed
/// bundle's signed manifest and the autoload match are built from.
pub const BIND_KEYS: &[DriverBindKey] = &[DriverBindKey::new(
    BIND_PRIORITY,
    match HwMatchKey::compatible(PCM5122_COMPATIBLE) {
        Ok(key) => key,
        // A literal too long for the key would fail const evaluation here,
        // never at run time.
        Err(_) => panic!("compatible string fits HW_COMPATIBLE_MAX"),
    },
)];

const PAGE: u8 = 0;
const RESET: u8 = 1;
const POWER: u8 = 2;
const MUTE: u8 = 3;
const BCLK_LRCLK_CFG: u8 = 9;
const MASTER_MODE: u8 = 12;
const PLL_REF: u8 = 13;
const ERROR_DETECT: u8 = 37;
const I2S_1: u8 = 40;
const I2S_2: u8 = 41;
const VOLUME_LEFT: u8 = 61;
const VOLUME_RIGHT: u8 = 62;
const ANALOG_MUTE_DET: u8 = 108;

const RSTR: u8 = 1 << 0;
const RSTM: u8 = 1 << 4;
const RQPD: u8 = 1 << 0;
const RQST: u8 = 1 << 4;
const RQMR: u8 = 1 << 0;
const RQML: u8 = 1 << 4;
const LRKO: u8 = 1 << 0;
const BCKO: u8 = 1 << 4;
const BCKP: u8 = 1 << 5;
const RLRK: u8 = 1 << 0;
const RBCK: u8 = 1 << 1;
const SREF: u8 = 7 << 4;
const SREF_BCK: u8 = 1 << 4;
const DCAS: u8 = 1 << 1;
const IDCH: u8 = 1 << 3;
const AFMT: u8 = 3 << 4;
const ALEN: u8 = 3;
/// Both channels' analogue outputs are live.
const OUTPUTS_LIVE: u8 = 0b11;

/// Register value of 0 dB; each step below it is half a decibel quieter.
const VOLUME_UNITY: i32 = 48;
/// The quietest step that still sounds; the next is mute.
const VOLUME_QUIETEST: i32 = 254;
/// Hundredths of a decibel in a volume step.
const STEP_MILLIBEL: i32 = 50;

/// Reads of the analogue mute a stop waits through, and the pause between
/// them: 10 ms in all, the time Linux allows the soft-mute ramp.
const MUTE_SETTLE_READS: u32 = 50;
const MUTE_SETTLE_US: u32 = 200;

/// The rates the part's automatic clocking derives from a bit clock.
const RATES: RateSupport = RateSupport::Discrete(
    match RateSet::new(&[
        rate(8_000),
        rate(11_025),
        rate(16_000),
        rate(22_050),
        rate(32_000),
        rate(44_100),
        rate(48_000),
        rate(64_000),
        rate(88_200),
        rate(96_000),
        rate(176_400),
        rate(192_000),
        rate(384_000),
    ]) {
        Ok(set) => set,
        Err(_) => panic!("thirteen ascending rates fit a set"),
    },
);

const fn rate(hz: u32) -> Rate {
    match Rate::new(hz) {
        Ok(rate) => rate,
        Err(_) => panic!("a rate in the vocabulary"),
    }
}

/// The sample widths it takes in a slot.
const WIDTHS: SampleWidths = match SampleWidths::of(&[16, 20, 24, 32]) {
    Some(widths) => widths,
    None => panic!("every width a slot carries"),
};

/// Its digital volume: 24 dB down to −103 dB.
const GAIN: GainRange = match GainRange::new(
    (VOLUME_UNITY - VOLUME_QUIETEST) * STEP_MILLIBEL,
    VOLUME_UNITY * STEP_MILLIBEL,
    STEP_MILLIBEL.unsigned_abs(),
) {
    Ok(range) => range,
    Err(_) => panic!("an ordered range"),
};

/// The PCM5122, reached through `P`.
pub struct Pcm5122<P: I2cPort, D: Delay> {
    part: Device<P>,
    delay: D,
    muted: bool,
    running: bool,
}

impl<P: I2cPort, D: Delay> Pcm5122<P, D> {
    /// The part on `port`, pausing through `delay`.
    pub const fn new(port: P, delay: D) -> Self {
        Self {
            part: Device::new(port),
            delay,
            muted: false,
            running: false,
        }
    }

    /// Reset the part and set it up to follow the interface's clocks, its
    /// PLL fed from the bit clock, muted and in standby.
    ///
    /// # Errors
    ///
    /// The bus's refusal, the part's not answering among them.
    pub fn bring_up(&mut self) -> Result<(), DriverError> {
        self.part.write_one(PAGE, 0)?;
        self.part.write_one(RESET, RSTM | RSTR)?;
        self.part.write_one(RESET, 0)?;
        self.part.update_one(POWER, |power| power | RQST)?;
        self.part
            .update_one(BCLK_LRCLK_CFG, |cfg| cfg & !(BCKP | BCKO | LRKO))?;
        self.part
            .update_one(MASTER_MODE, |mode| mode & !(RLRK | RBCK))?;
        self.part
            .update_one(PLL_REF, |reference| reference & !SREF | SREF_BCK)?;
        // No system clock is wired, so its absence is no error.
        self.part.update_one(ERROR_DETECT, |detect| detect | IDCH)?;
        self.part.update_one(MUTE, |mute| mute | RQML | RQMR)?;
        self.running = false;
        Ok(())
    }

    fn apply_mute(&self) -> Result<(), DriverError> {
        let silent = self.muted || !self.running;
        self.part.update_one(MUTE, |mute| {
            if silent {
                mute | RQML | RQMR
            } else {
                mute & !(RQML | RQMR)
            }
        })
    }

    /// Wait, within its budget, for both analogue outputs to finish their
    /// soft mute, so the interface's clocks can stop under a silent part.
    fn settle_mute(&self) -> Result<(), DriverError> {
        for _ in 0..MUTE_SETTLE_READS {
            if self.part.read_one(ANALOG_MUTE_DET)? & OUTPUTS_LIVE == 0 {
                return Ok(());
            }
            self.delay.delay_us(MUTE_SETTLE_US);
        }
        Ok(())
    }
}

impl<P: I2cPort, D: Delay> Codec for Pcm5122<P, D> {
    fn facts(&self) -> CodecFacts {
        CodecFacts {
            rates: RATES,
            widths: WIDTHS,
            formats: DaiFormats::EMPTY
                .with(DaiFormat::I2s)
                .with(DaiFormat::LeftJustified)
                .with(DaiFormat::RightJustified)
                .with(DaiFormat::DspA)
                .with(DaiFormat::DspB),
            drives_clocks: false,
            gain: Some(GAIN),
        }
    }

    fn configure(&mut self, link: &DaiLink, rate: Rate, width: u8) -> Result<(), DriverError> {
        // Driving the clocks would take a system clock this setup lacks, and
        // the part has no frame clock polarity to invert.
        if link.codec_drives_bit_clock
            || link.codec_drives_frame_clock
            || link.inversion.frame_clock()
            || !RATES.admits(rate)
        {
            return Err(DriverError::Unsupported);
        }
        let length = match width {
            16 => 0,
            20 => 1,
            24 => 2,
            32 => 3,
            _ => return Err(DriverError::Unsupported),
        };
        let (format, offset) = match link.format {
            DaiFormat::I2s => (0, 0),
            DaiFormat::DspA => (1 << 4, 1),
            DaiFormat::DspB => (1 << 4, 0),
            DaiFormat::RightJustified => (2 << 4, 0),
            DaiFormat::LeftJustified => (3 << 4, 0),
        };
        let polarity = if link.inversion.bit_clock() { BCKP } else { 0 };
        self.part
            .update_one(BCLK_LRCLK_CFG, |cfg| cfg & !BCKP | polarity)?;
        self.part
            .update_one(I2S_1, |cfg| cfg & !(AFMT | ALEN) | format | length)?;
        self.part.write_one(I2S_2, offset)?;
        // The dividers follow the detected rates.
        self.part.update_one(ERROR_DETECT, |detect| detect & !DCAS)
    }

    fn set_gain(&mut self, millibel: i32, mute: bool) -> Result<i32, DriverError> {
        // The step at or above the asked-for gain, so the caller never has
        // to make up the difference: the gain rounded up to a whole step.
        let louder_steps = (-i64::from(millibel)).div_euclid(i64::from(STEP_MILLIBEL));
        let step = (i64::from(VOLUME_UNITY) + louder_steps).clamp(0, i64::from(VOLUME_QUIETEST));
        let register = u8::try_from(step).map_err(|_| DriverError::OutOfRange)?;
        // One register a transfer: the part increments its pointer only when
        // asked to.
        self.part.write_one(VOLUME_LEFT, register)?;
        self.part.write_one(VOLUME_RIGHT, register)?;
        self.muted = mute;
        self.apply_mute()?;
        Ok((VOLUME_UNITY - i32::from(register)) * STEP_MILLIBEL)
    }

    fn start(&mut self) -> Result<(), DriverError> {
        self.part
            .update_one(POWER, |power| power & !(RQST | RQPD))?;
        self.running = true;
        self.apply_mute()
    }

    fn stop(&mut self) -> Result<(), DriverError> {
        self.running = false;
        self.apply_mute()?;
        self.settle_mute()?;
        self.part.update_one(POWER, |power| power | RQST)
    }
}

/// Handle marker [`register`] returns; the host re-issues its own. `"P122"`.
const REGISTER_HANDLE_MARKER: u64 = 0x5031_3232_0000_0001;

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
/// the transfer endpoint its node's grant names and its codec `LinkDuty`.
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}

#[cfg(test)]
mod tests;
