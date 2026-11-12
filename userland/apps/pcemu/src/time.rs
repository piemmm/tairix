//! Emulated time and the clock domains that count it.
//!
//! Emulated time is a count of nanoseconds since power-on. Every clock — the
//! CPU, the PIT, a CRTC, the FDC — converts from it by an exact rational over
//! `u128`, so no domain drifts against another however long a machine runs.

/// Nanoseconds since power-on.
pub type Nanos = u64;

/// Nanoseconds in a second.
pub const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Nanoseconds in a millisecond.
pub const NANOS_PER_MILLISECOND: u64 = NANOS_PER_SECOND / 1000;

/// The largest denominator a rate is held with: any tick count times it
/// and a second's nanoseconds stays within `u128`, so every conversion is
/// exact.
const MOST_DENOMINATOR: u64 = 1 << 32;

/// A clock's frequency as an exact rational number of hertz.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hertz {
    num: u64,
    den: u64,
}

impl Hertz {
    /// `num / den` hertz. A zero numerator or denominator is refused, and
    /// so is a denominator over 2³².
    #[must_use]
    pub const fn ratio(num: u64, den: u64) -> Option<Self> {
        if num == 0 || den == 0 || den > MOST_DENOMINATOR {
            None
        } else {
            Some(Self { num, den })
        }
    }

    /// A whole number of hertz.
    #[must_use]
    pub const fn whole(hz: u64) -> Option<Self> {
        Self::ratio(hz, 1)
    }

    /// The numerator and denominator the frequency is `num / den` hertz by.
    #[must_use]
    pub const fn parts(self) -> (u64, u64) {
        (self.num, self.den)
    }

    /// The PC's master crystal, the NTSC colour-burst frequency times four:
    /// 315/22 MHz.
    pub const CRYSTAL: Self = Self {
        num: 315_000_000,
        den: 22,
    };

    /// The 8253's input clock, the crystal over twelve: 1.193181… MHz.
    pub const PIT: Self = Self {
        num: 315_000_000,
        den: 264,
    };

    /// The MDA's and Hercules card's dot clock, and the EGA's for 350
    /// lines: 16.257 MHz.
    pub const MONO_DOTS: Self = Self {
        num: 16_257_000,
        den: 1,
    };

    /// The VGA's 640-dot clock: 25.175 MHz.
    pub const VGA_25: Self = Self {
        num: 25_175_000,
        den: 1,
    };

    /// The VGA's 720-dot clock: 28.322 MHz.
    pub const VGA_28: Self = Self {
        num: 28_322_000,
        den: 1,
    };

    /// The Disney Sound Source's own clock: 7 kHz.
    pub const SOUND_SOURCE: Self = Self { num: 7_000, den: 1 };

    /// A serial adapter's crystal: 1.8432 MHz, which the UART's baud
    /// generator divides by its divisor latch and then by 16 for each bit.
    pub const UART: Self = Self {
        num: 1_843_200,
        den: 1,
    };

    /// Whole ticks of this clock elapsed by `t`.
    #[must_use]
    pub fn ticks_at(self, t: Nanos) -> u64 {
        let ticks = u128::from(t) * u128::from(self.num)
            / (u128::from(self.den) * u128::from(NANOS_PER_SECOND));
        u64::try_from(ticks).unwrap_or(u64::MAX)
    }

    /// The instant tick `n` of this clock begins, rounded up to the
    /// nanosecond, so `ticks_at(time_of(n)) >= n`.
    #[must_use]
    pub fn time_of(self, ticks: u64) -> Nanos {
        let scaled = u128::from(ticks) * u128::from(self.den) * u128::from(NANOS_PER_SECOND);
        let t = scaled.div_ceil(u128::from(self.num));
        u64::try_from(t).unwrap_or(u64::MAX)
    }

    /// This frequency divided by `divisor`; none for a zero divisor, or a
    /// denominator past what [`Self::ratio`] takes.
    #[must_use]
    pub const fn divided(self, divisor: u64) -> Option<Self> {
        match self.den.checked_mul(divisor) {
            Some(den) if divisor != 0 => Self::ratio(self.num, den),
            _ => None,
        }
    }

    /// The frequency in millihertz, rounded down, for display.
    #[must_use]
    pub fn millihertz(self) -> u64 {
        let mhz = u128::from(self.num) * 1000 / u128::from(self.den);
        u64::try_from(mhz).unwrap_or(u64::MAX)
    }
}

/// A rate a processor is clocked at: from 1 MHz to 25 MHz, the fastest part
/// of the 80186 family, with a denominator of at most 16 bits, as a
/// crystal's ratio has, so a rate divided from it for a peripheral is exact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuClock(Hertz);

/// The rates a processor is clocked at, in millihertz.
const CPU_RATES: core::ops::RangeInclusive<u64> = 1_000_000_000..=25_000_000_000;

/// The largest denominator a processor's clock is held with, and the
/// largest divisor a peripheral divides it by: their product is
/// [`MOST_DENOMINATOR`].
const MOST_CPU_DENOMINATOR: u64 = 1 << 16;

impl CpuClock {
    /// The IBM PC's, the crystal over three: 4.772727… MHz.
    pub const PC: Self = Self(Hertz {
        num: 315_000_000,
        den: 66,
    });

    /// A turbo XT's slower setting, the crystal over two: 7.159090… MHz.
    pub const TURBO_XT_SLOW: Self = Self(Hertz {
        num: 315_000_000,
        den: 44,
    });

    /// The 80186-8's: 8 MHz.
    pub const I80186_8: Self = Self(Hertz {
        num: 8_000_000,
        den: 1,
    });

    /// A turbo XT's: 10 MHz.
    pub const TURBO_XT: Self = Self(Hertz {
        num: 10_000_000,
        den: 1,
    });

    /// `rate`, where a processor can be clocked at it.
    #[must_use]
    pub fn new(rate: Hertz) -> Option<Self> {
        let held = rate.den <= MOST_CPU_DENOMINATOR && CPU_RATES.contains(&rate.millihertz());
        held.then_some(Self(rate))
    }

    /// The clock's frequency.
    #[must_use]
    pub const fn rate(self) -> Hertz {
        self.0
    }

    /// The rate of a peripheral that ticks once every `DIVISOR` of this
    /// clock's cycles.
    #[must_use]
    pub const fn divided<const DIVISOR: u64>(self) -> Hertz {
        const {
            assert!(
                DIVISOR > 0 && DIVISOR <= MOST_CPU_DENOMINATOR,
                "a divisor the clock's denominator leaves room for"
            );
        }
        Hertz {
            num: self.0.num,
            den: self.0.den * DIVISOR,
        }
    }
}

/// A clock whose rate can change: its ticks mapped onto emulated time from a
/// base instant, so a change of rate takes effect from the moment it is
/// made. The CPU runs on one, and so does a CRTC, whose character clock
/// follows the card's mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    rate: Hertz,
    base_ticks: u64,
    base_time: Nanos,
}

impl Clock {
    /// A clock at `rate`, at tick 0 at time 0.
    #[must_use]
    pub const fn new(rate: Hertz) -> Self {
        Self {
            rate,
            base_ticks: 0,
            base_time: 0,
        }
    }

    /// The clock's rate.
    #[must_use]
    pub const fn rate(&self) -> Hertz {
        self.rate
    }

    /// The emulated time at which tick `ticks` happens.
    #[must_use]
    pub fn time_of(&self, ticks: u64) -> Nanos {
        let elapsed = ticks.saturating_sub(self.base_ticks);
        self.base_time.saturating_add(self.rate.time_of(elapsed))
    }

    /// The first tick at or after emulated time `t`.
    #[must_use]
    pub fn first_tick_at(&self, t: Nanos) -> u64 {
        let elapsed = t.saturating_sub(self.base_time);
        let ticks = u128::from(elapsed) * u128::from(self.rate.num);
        let per = u128::from(self.rate.den) * u128::from(NANOS_PER_SECOND);
        let ticks = ticks.div_ceil(per);
        self.base_ticks
            .saturating_add(u64::try_from(ticks).unwrap_or(u64::MAX))
    }

    /// Whole ticks elapsed by emulated time `t`.
    #[must_use]
    pub fn ticks_by(&self, t: Nanos) -> u64 {
        self.base_ticks
            .saturating_add(self.rate.ticks_at(t.saturating_sub(self.base_time)))
    }

    /// Changes the rate from tick `ticks` onward; time already elapsed is
    /// unchanged.
    pub fn set_rate(&mut self, rate: Hertz, ticks: u64) {
        self.base_time = self.time_of(ticks);
        self.base_ticks = ticks;
        self.rate = rate;
    }
}

/// The rates a clock on the board can run at, in millihertz: none slower
/// than a hertz, none faster than 200 MHz, which no crystal or divider of a
/// modelled machine comes near.
const CLOCK_RATES: core::ops::RangeInclusive<u64> = 1_000..=200_000_000_000;

crate::snapshot::state!(
    Hertz { num, den }
    check |rate: &Hertz| Hertz::ratio(rate.num, rate.den).is_some()
        && CLOCK_RATES.contains(&rate.millihertz())
);

crate::snapshot::state!(Clock {
    rate,
    base_ticks,
    base_time,
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pc_clocks_derive_from_the_crystal() {
        assert_eq!(Hertz::CRYSTAL.divided(3), Some(CpuClock::PC.rate()));
        assert_eq!(
            Hertz::CRYSTAL.divided(2),
            Some(CpuClock::TURBO_XT_SLOW.rate())
        );
        assert_eq!(Hertz::CRYSTAL.divided(12), Some(Hertz::PIT));
        assert_eq!(Hertz::PIT.millihertz(), 1_193_181_818);
        assert_eq!(CpuClock::PC.rate().millihertz(), 4_772_727_272);
    }

    #[test]
    fn only_a_processor_s_rates_clock_one() {
        let rate = |num, den| Hertz::ratio(num, den).and_then(CpuClock::new);
        for named in [
            CpuClock::PC,
            CpuClock::TURBO_XT_SLOW,
            CpuClock::I80186_8,
            CpuClock::TURBO_XT,
        ] {
            assert_eq!(CpuClock::new(named.rate()), Some(named));
        }
        assert!(rate(1_000_000, 1).is_some() && rate(25_000_000, 1).is_some());
        assert_eq!(rate(999_999, 1), None, "slower than a megahertz");
        assert_eq!(rate(25_000_001, 1), None, "faster than any part");
        assert_eq!(rate(210_472_505_638, 3), None, "70 GHz");
        assert_eq!(
            rate(10_000_000 << 17, 1 << 17),
            None,
            "10 MHz, but no crystal's ratio"
        );
    }

    #[test]
    fn every_conversion_of_a_rate_held_is_exact() {
        let slowest = Hertz::ratio(1, MOST_DENOMINATOR).unwrap();
        assert_eq!(Hertz::ratio(1, MOST_DENOMINATOR + 1), None);
        assert_eq!(slowest.divided(2), None, "past the denominator held");
        assert_eq!(
            slowest.time_of(u64::MAX),
            u64::MAX,
            "saturates, not overflows"
        );
        let fastest = Hertz::whole(u64::MAX).unwrap();
        assert_eq!(fastest.ticks_at(u64::MAX), u64::MAX);
        assert_eq!(Clock::new(fastest).first_tick_at(u64::MAX), u64::MAX);
        let divided = CpuClock::PC.divided::<MOST_CPU_DENOMINATOR>();
        assert_eq!(divided.time_of(u64::MAX), u64::MAX);
        assert_eq!(
            CpuClock::I80186_8.divided::<4>().millihertz(),
            2_000_000_000
        );
    }

    #[test]
    fn ticks_and_times_round_trip_without_drift() {
        let pit = Hertz::PIT;
        assert_eq!(pit.ticks_at(NANOS_PER_SECOND), 1_193_181);
        // A day of 18.2 Hz timer ticks lands on the same instant however it
        // is reached.
        let day = 86_400 * NANOS_PER_SECOND;
        let ticks = pit.ticks_at(day);
        assert!(pit.time_of(ticks) <= day);
        assert_eq!(pit.ticks_at(pit.time_of(ticks)), ticks);
        for n in [0u64, 1, 2, 3, 65_536, 1 << 40] {
            assert!(pit.ticks_at(pit.time_of(n)) >= n);
            assert!(pit.ticks_at(pit.time_of(n).saturating_sub(1)) < n.max(1));
        }
    }

    #[test]
    fn a_clock_no_board_runs_at_is_refused_from_a_snapshot() {
        use crate::snapshot::{In, Out, State, Torn};
        let restored = |rate: Hertz| {
            let mut out = Out::refusable();
            rate.save(&mut out);
            let bytes = out.finish().unwrap();
            let mut into = Hertz::PIT;
            into.restore(&mut In::new(&bytes)).map(|()| into)
        };
        assert_eq!(restored(Hertz::PIT), Ok(Hertz::PIT));
        for absurd in [
            Hertz {
                num: u64::MAX,
                den: 1,
            },
            Hertz {
                num: 1,
                den: 1 << 40,
            },
            Hertz {
                num: 1 << 63,
                den: 1 << 60,
            },
        ] {
            assert_eq!(restored(absurd), Err(Torn::Malformed), "{absurd:?}");
        }
    }

    #[test]
    fn a_speed_change_keeps_elapsed_time() {
        let mut clock = Clock::new(CpuClock::PC.rate());
        let at = clock.time_of(4_772_727);
        clock.set_rate(Hertz::whole(10_000_000).unwrap(), 4_772_727);
        assert_eq!(clock.time_of(4_772_727), at);
        assert_eq!(clock.time_of(4_772_727 + 10_000_000), at + NANOS_PER_SECOND);
        let second = 4_772_727 + 10_000_000;
        assert_eq!(clock.first_tick_at(at + NANOS_PER_SECOND), second);
        assert_eq!(clock.ticks_by(at + NANOS_PER_SECOND), second);
        assert_eq!(clock.ticks_by(at + NANOS_PER_SECOND - 1), second - 1);
    }

    #[test]
    fn degenerate_rates_are_refused() {
        assert_eq!(Hertz::ratio(0, 1), None);
        assert_eq!(Hertz::ratio(1, 0), None);
        assert_eq!(Hertz::PIT.divided(0), None);
    }
}
