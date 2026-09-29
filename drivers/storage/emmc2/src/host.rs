//! The host controller's capabilities, and the SD clock it divides from its
//! base clock.
//!
//! Both follow the SD Host Controller Simplified Specification: the
//! capability registers (v3.00 §2.2.26) and the clock divider (§2.2.14),
//! whose encoding changed from a power-of-two field in v2.00 to a 10-bit
//! divisor in v3.00.

/// Specification-version field value for SDHCI 3.00, the first with the
/// 10-bit divided clock, the capabilities' high word, and UHS-I modes.
const SPEC_3_00: u32 = 2;

/// `CAPABILITIES[21]`: the host drives SD High Speed timing.
const CAPS_HIGH_SPEED: u32 = 1 << 21;
/// `CAPABILITIES[19]`: the host has an ADMA2 engine.
const CAPS_ADMA2: u32 = 1 << 19;
/// `CAPABILITIES_1[0]`: UHS-I SDR50.
const CAPS1_SDR50: u32 = 1 << 0;
/// `CAPABILITIES_1[1]`: UHS-I SDR104.
const CAPS1_SDR104: u32 = 1 << 1;
/// `CAPABILITIES_1[2]`: UHS-I DDR50.
const CAPS1_DDR50: u32 = 1 << 2;
/// `CAPABILITIES_1[13]`: SDR50 needs sampling-clock tuning.
const CAPS1_SDR50_TUNING: u32 = 1 << 13;

/// `MAX_CURRENT` counts in steps of this many milliamps.
const MAX_CURRENT_STEP_MA: u32 = 4;

/// Widest 10-bit divided-clock divisor field.
const MAX_DIVIDER: u32 = 0x3FF;

/// Widest power-of-two divisor a v2.00 host can select.
const MAX_POWER_OF_TWO_DIVISOR: u32 = 256;

/// What the controller's registers say it can do.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HostCaps {
    /// SDHCI 3.00 or later: the 10-bit divider and the high word apply.
    v3: bool,
    caps: u32,
    /// The capabilities' high word, zero before 3.00, where it is reserved.
    caps1: u32,
    max_current_330_ma: u32,
}

impl HostCaps {
    /// Decode the host-version (`SLOTISR_VER`), capabilities, and
    /// maximum-current registers.
    #[must_use]
    pub const fn decode(slotisr_ver: u32, caps: u32, caps1: u32, max_current: u32) -> Self {
        let v3 = ((slotisr_ver >> 16) & 0xFF) >= SPEC_3_00;
        Self {
            v3,
            caps,
            caps1: if v3 { caps1 } else { 0 },
            max_current_330_ma: (max_current & 0xFF) * MAX_CURRENT_STEP_MA,
        }
    }

    /// The base clock the capabilities declare, when they declare one.
    #[must_use]
    pub const fn base_clock_hz(&self) -> Option<u32> {
        let mhz = if self.v3 {
            (self.caps >> 8) & 0xFF
        } else {
            (self.caps >> 8) & 0x3F
        };
        if mhz == 0 {
            None
        } else {
            Some(mhz * 1_000_000)
        }
    }

    /// Whether the clock divider is the 3.00 10-bit form.
    #[must_use]
    pub const fn divided_clock(&self) -> bool {
        self.v3
    }

    /// Whether the host drives SD High Speed.
    #[must_use]
    pub const fn high_speed(&self) -> bool {
        self.caps & CAPS_HIGH_SPEED != 0
    }

    /// Whether the host has an ADMA2 engine.
    #[must_use]
    pub const fn adma2(&self) -> bool {
        self.caps & CAPS_ADMA2 != 0
    }

    /// Whether the host signals UHS-I at 1.8 V; any UHS-I mode implies SDR12
    /// and SDR25.
    #[must_use]
    pub const fn uhs(&self) -> bool {
        self.caps1 & (CAPS1_SDR50 | CAPS1_SDR104 | CAPS1_DDR50) != 0
    }

    /// Whether the host drives SDR50 without sampling-clock tuning; SDR104
    /// support implies SDR50's.
    #[must_use]
    pub const fn untuned_sdr50(&self) -> bool {
        self.caps1 & (CAPS1_SDR50 | CAPS1_SDR104) != 0 && self.caps1 & CAPS1_SDR50_TUNING == 0
    }

    /// Whether the host drives DDR50.
    #[must_use]
    pub const fn ddr50(&self) -> bool {
        self.caps1 & CAPS1_DDR50 != 0
    }

    /// The most current the host supplies the card at 3.3 V, in milliamps.
    #[must_use]
    pub const fn max_current_330_ma(&self) -> u32 {
        self.max_current_330_ma
    }
}

/// A programmed SD clock: the Clock Control frequency-select bits
/// (`CONTROL1[15:6]`) and the clock they divide the base clock to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SdClock {
    /// The frequency-select bits, in place.
    pub select: u32,
    /// The clock they yield, in Hz.
    pub hz: u32,
}

/// The fastest SD clock not above `target_hz` that `base_hz` divides to, or
/// `None` when even the widest divisor is too fast (or either rate is zero).
///
/// `divided` selects the 3.00 10-bit divisor (`base / 2N`, `N = 0` the base
/// itself); otherwise the v2.00 power-of-two divisor applies.
#[must_use]
pub fn sd_clock(base_hz: u32, target_hz: u32, divided: bool) -> Option<SdClock> {
    if base_hz == 0 || target_hz == 0 {
        return None;
    }
    if base_hz <= target_hz {
        return Some(SdClock {
            select: 0,
            hz: base_hz,
        });
    }
    let base = u64::from(base_hz);
    let target = u64::from(target_hz);
    if divided {
        let n = base.div_ceil(2 * target);
        let n = u32::try_from(n).ok().filter(|&n| n <= MAX_DIVIDER)?;
        Some(SdClock {
            select: ((n & 0xFF) << 8) | ((n >> 8) << 6),
            hz: base_hz / (2 * n),
        })
    } else {
        let mut divisor = 2;
        while divisor <= MAX_POWER_OF_TWO_DIVISOR {
            if base <= target * u64::from(divisor) {
                return Some(SdClock {
                    select: (divisor / 2) << 8,
                    hz: base_hz / divisor,
                });
            }
            divisor *= 2;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The BCM2711 EMMC2 as Linux dumps it: SDHCI 3.00, a 100 MHz base,
    /// ADMA2, High Speed, DDR50, and SDR50 only with tuning.
    fn bcm2711() -> HostCaps {
        HostCaps::decode(0x1002_0000, 0x45ee_6432, 0x0000_a525, 0x0008_0008)
    }

    #[test]
    fn the_bcm2711_capabilities_decode_to_ddr50_without_tuning() {
        let caps = bcm2711();
        assert!(caps.divided_clock());
        assert_eq!(caps.base_clock_hz(), Some(100_000_000));
        assert!(caps.high_speed() && caps.adma2() && caps.uhs() && caps.ddr50());
        assert!(!caps.untuned_sdr50(), "its SDR50 needs tuning");
        assert_eq!(caps.max_current_330_ma(), 32);
    }

    #[test]
    fn a_pre_3_00_host_has_no_uhs_and_a_six_bit_base_clock() {
        let caps = HostCaps::decode(0x0001_0000, 0x0000_ff00, u32::MAX, 0);
        assert!(!caps.divided_clock());
        assert_eq!(caps.base_clock_hz(), Some(63_000_000));
        assert!(!caps.uhs() && !caps.ddr50() && !caps.untuned_sdr50());
    }

    #[test]
    fn a_zero_base_clock_field_declares_none() {
        let caps = HostCaps::decode(0x0002_0000, 0, 0, 0);
        assert_eq!(caps.base_clock_hz(), None);
    }

    #[test]
    fn the_divided_clock_is_the_fastest_not_above_the_target() {
        let cases = [
            (400_000, 0x7d << 8, 400_000),
            (25_000_000, 0x02 << 8, 25_000_000),
            (50_000_000, 0x01 << 8, 50_000_000),
            (100_000_000, 0, 100_000_000),
            (208_000_000, 0, 100_000_000),
        ];
        for (target, select, hz) in cases {
            assert_eq!(
                sd_clock(100_000_000, target, true),
                Some(SdClock { select, hz }),
                "target {target}"
            );
        }
    }

    #[test]
    fn a_divisor_past_eight_bits_carries_its_high_bits_in_place() {
        // 250 MHz / (2 * 313) = 399.4 kHz: N = 0x139.
        let clock = sd_clock(250_000_000, 400_000, true).expect("reachable");
        assert_eq!(clock.select, (0x39 << 8) | (0x1 << 6));
        assert!(clock.hz <= 400_000);
    }

    #[test]
    fn no_divided_clock_is_offered_above_the_target() {
        for base in [1_000_000, 41_666_667, 100_000_001, 200_000_000, 255_000_000] {
            for target in [400_000, 25_000_000, 50_000_000] {
                let clock = sd_clock(base, target, true).expect("reachable");
                assert!(
                    u64::from(clock.hz) <= u64::from(target),
                    "{base} → {target}"
                );
                let n = (clock.select >> 8) | ((clock.select >> 6 & 0b11) << 8);
                let exact = if n == 0 {
                    u64::from(base)
                } else {
                    u64::from(base) / (2 * u64::from(n))
                };
                assert_eq!(u64::from(clock.hz), exact);
                assert!(n == 0 || u64::from(base) <= u64::from(target) * 2 * u64::from(n));
            }
        }
    }

    #[test]
    fn a_power_of_two_host_picks_its_smallest_sufficient_divisor() {
        assert_eq!(
            sd_clock(63_000_000, 25_000_000, false),
            Some(SdClock {
                select: 0x02 << 8,
                hz: 15_750_000
            })
        );
        assert_eq!(
            sd_clock(63_000_000, 400_000, false),
            Some(SdClock {
                select: 0x80 << 8,
                hz: 246_093
            })
        );
    }

    #[test]
    fn an_unreachable_or_degenerate_clock_is_refused() {
        assert_eq!(sd_clock(900_000_000, 400_000, true), None);
        assert_eq!(sd_clock(200_000_000, 400_000, false), None);
        assert_eq!(sd_clock(0, 400_000, true), None);
        assert_eq!(sd_clock(100_000_000, 0, true), None);
    }
}
