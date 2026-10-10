//! The clock manager's registers: the generators the served clocks run on and
//! the PLL channels they divide.
//!
//! A generator divides one of its sources by a 12.12 fixed-point divisor. A
//! divisor with a fractional part runs through first-order MASH, so every
//! output period is a whole number of source periods within one of the exact
//! division, and the rate averages it. MASH may never make a period shorter
//! than [`MASH_MAX_HZ`]'s (BCM2711 ARM Peripherals, 5.4), so a fractional
//! divisor is used only where its whole part keeps within that. Every write
//! carries the manager's password, and a generator's source and divisor
//! change only while it is stopped.

use core::cmp::Ordering;

use tairix_abi::{DriverError, RegisterBlock};

/// The top byte every write carries; the manager ignores a write without it.
const PASSWORD: u32 = 0x5A << 24;

const CTL_SOURCE: u32 = 0xF;
const CTL_ENABLE: u32 = 1 << 4;
const CTL_KILL: u32 = 1 << 5;
const CTL_BUSY: u32 = 1 << 7;
const CTL_MASH: u32 = 0b11 << 9;
const MASH_FIRST_ORDER: u32 = 1 << 9;

const FRACTION_BITS: u32 = 12;
const FRACTION: u32 = (1 << FRACTION_BITS) - 1;
const DIVISOR: u32 = 0x00FF_FFFF;

/// The smallest divisor a MASH generator takes.
pub const MIN_DIVISOR: u32 = 2 << FRACTION_BITS;

/// The largest whole divisor the 12-bit integer field holds.
pub const MAX_DIVISOR: u32 = 0xFFF << FRACTION_BITS;

/// The fastest a generator may be clocked while MASH spreads its periods.
pub const MASH_MAX_HZ: u64 = 25_000_000;

/// Status reads a stopping generator is given to finish its last period. The
/// longest a served clock can have is the oscillator's 54 MHz divided by
/// 4095, 76 µs, which the budget outlasts at any read faster than 0.7 ns.
const STOP_BUDGET: u32 = 100_000;

/// A generator's source.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Source {
    /// Held low: the generator makes nothing.
    Ground,
    /// The crystal oscillator, at the rate the tree states.
    Oscillator,
    /// PLLA's peripheral channel.
    PllaPer,
    /// PLLC's peripheral channel, which the firmware retunes with the core
    /// clock.
    PllcPer,
    /// PLLD's peripheral channel.
    PlldPer,
}

impl Source {
    const fn field(self) -> u32 {
        match self {
            Self::Ground => 0,
            Self::Oscillator => 1,
            Self::PllaPer => 4,
            Self::PllcPer => 5,
            Self::PlldPer => 6,
        }
    }

    /// The source a generator's field selects; [`None`] for the two test
    /// inputs, PLLH, which this part lacks, and the reserved values.
    const fn from_field(field: u32) -> Option<Self> {
        match field {
            0 => Some(Self::Ground),
            1 => Some(Self::Oscillator),
            4 => Some(Self::PllaPer),
            5 => Some(Self::PllcPer),
            6 => Some(Self::PlldPer),
            _ => None,
        }
    }
}

/// The sources a served clock is run from: the two the firmware leaves fixed.
/// PLLC follows the core clock, and PLLA feeds the display.
const RUN_SOURCES: [Source; 2] = [Source::Oscillator, Source::PlldPer];

/// One PLL's registers.
struct Pll {
    control: usize,
    fraction: usize,
    /// The analogue register holding the feedback pre-divider flag.
    analogue: usize,
    /// The peripheral channel a generator source names.
    channel: usize,
}

const PLLA: Pll = Pll {
    control: 0x1100,
    fraction: 0x1200,
    analogue: 0x1014,
    channel: 0x1500,
};
const PLLC: Pll = Pll {
    control: 0x1120,
    fraction: 0x1220,
    analogue: 0x1034,
    channel: 0x1520,
};
const PLLD: Pll = Pll {
    control: 0x1140,
    fraction: 0x1240,
    analogue: 0x1054,
    channel: 0x1540,
};

/// Bytes of window the registers the driver reaches lie within.
pub const WINDOW_LEN: usize = 0x1544;

const PLL_NDIV: u32 = 0x3FF;
const PLL_PDIV_SHIFT: u32 = 12;
const PLL_PDIV: u32 = 0x7;
const PLL_POWER_DOWN: u32 = 1 << 16;
const PLL_OUT_OF_RESET: u32 = 1 << 17;
const PLL_FRACTION_BITS: u32 = 20;
const PLL_FRACTION: u32 = (1 << PLL_FRACTION_BITS) - 1;
/// Doubles the feedback multiplier.
const PLL_FEEDBACK_PREDIV: u32 = 1 << 14;
const CHANNEL_DIVIDER: u32 = 0xFF;
const CHANNEL_DISABLED: u32 = 1 << 8;

/// A clock generator's two registers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Generator {
    control: usize,
    divisor: usize,
}

/// The PWM blocks' generator.
pub const PWM: Generator = Generator {
    control: 0xA0,
    divisor: 0xA4,
};

/// The PCM block's generator.
pub const PCM: Generator = Generator {
    control: 0x98,
    divisor: 0x9C,
};

/// A source and divisor a generator runs at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Setting {
    source: Source,
    /// 12.12 fixed point.
    divisor: u32,
}

impl Setting {
    /// The source.
    #[must_use]
    pub const fn source(self) -> Source {
        self.source
    }

    /// The divisor, in 12.12 fixed point.
    #[must_use]
    pub const fn divisor(self) -> u32 {
        self.divisor
    }

    const fn is_fractional(self) -> bool {
        self.divisor & FRACTION != 0
    }

    /// Whether the setting may run from a source at `parent`: a fractional
    /// divisor's shortest period, its whole part's, keeps within
    /// [`MASH_MAX_HZ`].
    fn admits(self, parent: u64) -> bool {
        !self.is_fractional()
            || u128::from(parent)
                <= u128::from(MASH_MAX_HZ) * u128::from(self.divisor >> FRACTION_BITS)
    }
}

/// How a stopped generator ended its last period.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Halted {
    /// It finished it.
    Finished,
    /// It did not, so it was reset mid-period, which may have glitched it.
    Killed,
}

/// The rate a source at `parent` divided by `divisor` averages, rounded.
fn divided(parent: u64, divisor: u32) -> u64 {
    let divisor = u128::from(divisor);
    let made = (u128::from(parent) << FRACTION_BITS) + divisor / 2;
    // At most `parent`: the divisor is at least one whole.
    u64::try_from(made / divisor).unwrap_or(parent)
}

/// A setting under consideration, with its source's rate and its distance
/// from the rate asked for, scaled by the divisor:
/// `|parent * 4096 - hz * divisor|`.
struct Candidate {
    setting: Setting,
    parent: u64,
    distance: u128,
}

impl Candidate {
    fn new(source: Source, parent: u64, divisor: u32, hz: u64) -> Self {
        let made = u128::from(parent) << FRACTION_BITS;
        let asked = u128::from(hz) * u128::from(divisor);
        Self {
            setting: Setting { source, divisor },
            parent,
            distance: made.abs_diff(asked),
        }
    }

    /// Whether this is the better setting: the nearer average rate; at an
    /// equal distance a whole divisor, which has no MASH jitter, then the
    /// larger divisor, whose jitter is the smaller part of a period.
    fn beats(&self, other: &Self) -> bool {
        let ours = self.distance * u128::from(other.setting.divisor);
        let theirs = other.distance * u128::from(self.setting.divisor);
        ours.cmp(&theirs)
            .then_with(|| {
                self.setting
                    .is_fractional()
                    .cmp(&other.setting.is_fractional())
            })
            .then_with(|| other.setting.divisor.cmp(&self.setting.divisor))
            == Ordering::Less
    }
}

/// The divisors either side of the exact division of `parent` to `hz`, and
/// the whole ones either side of it, which need no MASH, within the
/// generators' range.
fn divisors_near(parent: u64, hz: u64) -> [u32; 4] {
    let exact = (u128::from(parent) << FRACTION_BITS) / u128::from(hz);
    let whole = exact & !u128::from(FRACTION);
    let clamp = |divisor: u128| {
        u32::try_from(divisor)
            .unwrap_or(MAX_DIVISOR)
            .clamp(MIN_DIVISOR, MAX_DIVISOR)
    };
    [
        clamp(exact),
        clamp(exact + 1),
        clamp(whole),
        clamp(whole + (1 << FRACTION_BITS)),
    ]
}

/// The clock manager, reached through its register window.
pub struct Cprman<'r, R: RegisterBlock + ?Sized> {
    regs: &'r R,
    oscillator: u64,
}

impl<'r, R: RegisterBlock + ?Sized> Cprman<'r, R> {
    /// The manager behind `regs`, its oscillator running at `oscillator` Hz.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for a window short of the PLL
    /// registers, [`DriverError::OutOfRange`] for an oscillator at no rate.
    pub fn new(regs: &'r R, oscillator: u64) -> Result<Self, DriverError> {
        if regs.block_len() < WINDOW_LEN {
            return Err(DriverError::LengthOutOfRange);
        }
        if oscillator == 0 {
            return Err(DriverError::OutOfRange);
        }
        Ok(Self { regs, oscillator })
    }

    /// The rate `source` runs at now; zero while it is stopped.
    ///
    /// # Errors
    ///
    /// A register read's failure.
    pub fn source_rate(&self, source: Source) -> Result<u64, DriverError> {
        match source {
            Source::Ground => Ok(0),
            Source::Oscillator => Ok(self.oscillator),
            Source::PllaPer => self.channel_rate(&PLLA),
            Source::PllcPer => self.channel_rate(&PLLC),
            Source::PlldPer => self.channel_rate(&PLLD),
        }
    }

    /// The rate `pll`'s peripheral channel runs at: the oscillator multiplied
    /// by the PLL's fractional feedback over its pre-divider, then divided by
    /// the channel's own divider.
    fn channel_rate(&self, pll: &Pll) -> Result<u64, DriverError> {
        let control = self.regs.read32(pll.control)?;
        if control & PLL_OUT_OF_RESET == 0 || control & PLL_POWER_DOWN != 0 {
            return Ok(0);
        }
        let channel = self.regs.read32(pll.channel)?;
        let pdiv = (control >> PLL_PDIV_SHIFT) & PLL_PDIV;
        if channel & CHANNEL_DISABLED != 0 || pdiv == 0 {
            return Ok(0);
        }
        let mut multiplier = u128::from(control & PLL_NDIV) << PLL_FRACTION_BITS
            | u128::from(self.regs.read32(pll.fraction)? & PLL_FRACTION);
        if self.regs.read32(pll.analogue)? & PLL_FEEDBACK_PREDIV != 0 {
            multiplier *= 2;
        }
        // A field of zero divides by its whole span.
        let divider = match channel & CHANNEL_DIVIDER {
            0 => CHANNEL_DIVIDER + 1,
            divider => divider,
        };
        let rate = u128::from(self.oscillator) * multiplier
            / (u128::from(pdiv) << PLL_FRACTION_BITS)
            / u128::from(divider);
        u64::try_from(rate).map_err(|_| DriverError::OutOfRange)
    }

    /// The setting nearest `hz` from the run sources, among those MASH
    /// allows, and the rate it makes.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] for no rate at all, or a register read's
    /// failure.
    pub fn nearest(&self, hz: u64) -> Result<(Setting, u64), DriverError> {
        if hz == 0 {
            return Err(DriverError::OutOfRange);
        }
        let mut best: Option<Candidate> = None;
        for source in RUN_SOURCES {
            let parent = self.source_rate(source)?;
            if parent == 0 {
                continue;
            }
            for divisor in divisors_near(parent, hz) {
                let candidate = Candidate::new(source, parent, divisor, hz);
                if !candidate.setting.admits(parent) {
                    continue;
                }
                if best.as_ref().is_none_or(|best| candidate.beats(best)) {
                    best = Some(candidate);
                }
            }
        }
        // The oscillator always runs, and a whole divisor needs no MASH, so a
        // candidate always exists.
        let best = best.ok_or(DriverError::DeviceFault)?;
        Ok((best.setting, divided(best.parent, best.setting.divisor)))
    }

    /// The rate `generator` runs at now; zero while it is stopped.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] for a generator left on a source this part
    /// cannot state the rate of or on a zero divisor, or a register read's
    /// failure.
    pub fn rate(&self, generator: Generator) -> Result<u64, DriverError> {
        let control = self.regs.read32(generator.control)?;
        if control & CTL_ENABLE == 0 {
            return Ok(0);
        }
        let source = Source::from_field(control & CTL_SOURCE).ok_or(DriverError::Unsupported)?;
        let mut divisor = self.regs.read32(generator.divisor)? & DIVISOR;
        // Without MASH the fraction is ignored.
        if control & CTL_MASH == 0 {
            divisor &= !FRACTION;
        }
        if divisor == 0 {
            return Err(DriverError::Unsupported);
        }
        Ok(divided(self.source_rate(source)?, divisor))
    }

    /// Stop `generator`, then run it at `setting`.
    ///
    /// # Errors
    ///
    /// As [`stop`](Self::stop), the setting left unwritten; or a register
    /// write's failure.
    pub fn run(&self, generator: Generator, setting: Setting) -> Result<Halted, DriverError> {
        let halted = self.stop(generator)?;
        self.regs
            .write32(generator.divisor, PASSWORD | setting.divisor)?;
        let mash = if setting.is_fractional() {
            MASH_FIRST_ORDER
        } else {
            0
        };
        let control = PASSWORD | mash | setting.source.field();
        // Enabling in the same write as the source change can glitch it.
        self.regs.write32(generator.control, control)?;
        self.regs.write32(generator.control, control | CTL_ENABLE)?;
        Ok(halted)
    }

    /// Stop `generator`, letting it finish its period, or resetting it when
    /// it does not: one whose source has stopped never would.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] for a generator still running after the
    /// reset, which must then not be reconfigured; or a register access's
    /// failure.
    pub fn stop(&self, generator: Generator) -> Result<Halted, DriverError> {
        let control = self.regs.read32(generator.control)?;
        if control & (CTL_ENABLE | CTL_BUSY) == 0 {
            return Ok(Halted::Finished);
        }
        let kept = PASSWORD | (control & (CTL_SOURCE | CTL_MASH));
        self.regs.write32(generator.control, kept)?;
        if self.settled(generator)? {
            return Ok(Halted::Finished);
        }
        self.regs.write32(generator.control, kept | CTL_KILL)?;
        let settled = self.settled(generator)?;
        self.regs.write32(generator.control, kept)?;
        if settled {
            Ok(Halted::Killed)
        } else {
            Err(DriverError::DeviceFault)
        }
    }

    /// Whether `generator` stops running within the budget.
    fn settled(&self, generator: Generator) -> Result<bool, DriverError> {
        for _ in 0..STOP_BUDGET {
            if self.regs.read32(generator.control)? & CTL_BUSY == 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
