//! The SD bus timings the driver negotiates, how it picks one, and what it
//! reports having picked.

use crate::host::HostCaps;
use crate::BringUpFault;

/// A bus timing the card and host both drive.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BusMode {
    /// Default Speed: 3.3 V, up to 25 MHz.
    DefaultSpeed,
    /// High Speed: 3.3 V, up to 50 MHz.
    HighSpeed,
    /// UHS-I SDR12: 1.8 V, up to 25 MHz — where a card lands after the
    /// voltage switch.
    Sdr12,
    /// UHS-I SDR25: 1.8 V, up to 50 MHz.
    Sdr25,
    /// UHS-I SDR50: 1.8 V, up to 100 MHz.
    Sdr50,
    /// UHS-I DDR50: 1.8 V, both clock edges, up to 50 MHz.
    Ddr50,
}

impl BusMode {
    /// The fastest SD clock the mode allows, in Hz.
    #[must_use]
    pub const fn max_clock_hz(self) -> u32 {
        match self {
            Self::DefaultSpeed | Self::Sdr12 => 25_000_000,
            Self::HighSpeed | Self::Sdr25 | Self::Ddr50 => 50_000_000,
            Self::Sdr50 => 100_000_000,
        }
    }

    /// Whether the mode signals at 1.8 V.
    #[must_use]
    pub const fn signals_at_1v8(self) -> bool {
        !matches!(self, Self::DefaultSpeed | Self::HighSpeed)
    }

    /// The access-mode function (`CMD6` group 1) that selects the mode.
    #[must_use]
    pub(crate) const fn function(self) -> u8 {
        match self {
            Self::DefaultSpeed | Self::Sdr12 => 0,
            Self::HighSpeed | Self::Sdr25 => 1,
            Self::Sdr50 => 2,
            Self::Ddr50 => 4,
        }
    }

    /// The host's UHS Mode Select value (`CONTROL2[18:16]`); the 3.3 V modes
    /// take SDR12's encoding, which the host ignores without 1.8 V signalling.
    #[must_use]
    pub(crate) const fn uhs_select(self) -> u32 {
        match self {
            Self::DefaultSpeed | Self::HighSpeed | Self::Sdr12 => 0b000,
            Self::Sdr25 => 0b001,
            Self::Sdr50 => 0b010,
            Self::Ddr50 => 0b100,
        }
    }

    /// Whether the host drives the mode with its High Speed output timing:
    /// every mode faster than 25 MHz does.
    #[must_use]
    pub(crate) const fn high_speed_timing(self) -> bool {
        self.max_clock_hz() > 25_000_000
    }

    /// A stable, terse name for the mode, for the operator-facing link record.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DefaultSpeed => "default speed",
            Self::HighSpeed => "high speed",
            Self::Sdr12 => "UHS-I SDR12",
            Self::Sdr25 => "UHS-I SDR25",
            Self::Sdr50 => "UHS-I SDR50",
            Self::Ddr50 => "UHS-I DDR50",
        }
    }

    const fn offered_by(self, access_modes: u16) -> bool {
        access_modes & (1 << self.function()) != 0
    }
}

/// A rung of the bring-up's negotiation ladder: the fastest family of
/// timings one attempt may reach.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Rung {
    /// UHS-I, at 1.8 V signalling.
    Uhs,
    /// High Speed, at 3.3 V.
    HighSpeed,
    /// Default Speed, at 3.3 V.
    DefaultSpeed,
}

impl Rung {
    /// The next rung down, or `None` from the bottom one.
    pub(crate) const fn below(self) -> Option<Self> {
        match self {
            Self::Uhs => Some(Self::HighSpeed),
            Self::HighSpeed => Some(Self::DefaultSpeed),
            Self::DefaultSpeed => None,
        }
    }

    /// A stable, terse name for the rung, for the operator-facing trace.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uhs => "UHS-I",
            Self::HighSpeed => "high speed",
            Self::DefaultSpeed => "default speed",
        }
    }
}

/// The fastest mode `host` and a card offering `access_modes` both drive at
/// the card's signalling voltage.
///
/// At 1.8 V DDR50 comes first: it moves SDR50's 50 MB/s at half the clock
/// and needs no sampling-clock tuning, which this driver does not perform, so
/// SDR50 is taken only from a host that runs it untuned.
#[must_use]
pub(crate) fn fastest(host: &HostCaps, access_modes: u16, signalling_1v8: bool) -> BusMode {
    if signalling_1v8 {
        if host.ddr50() && BusMode::Ddr50.offered_by(access_modes) {
            BusMode::Ddr50
        } else if host.untuned_sdr50() && BusMode::Sdr50.offered_by(access_modes) {
            BusMode::Sdr50
        } else if BusMode::Sdr25.offered_by(access_modes) {
            BusMode::Sdr25
        } else {
            BusMode::Sdr12
        }
    } else if host.high_speed() && BusMode::HighSpeed.offered_by(access_modes) {
        BusMode::HighSpeed
    } else {
        BusMode::DefaultSpeed
    }
}

/// The current limit (`CMD6` group 4 function) to raise the card to for
/// `mode`, or `None` to keep its 200 mA default: the highest the host can
/// supply at 3.3 V that the card offers, for the modes that define one.
#[must_use]
pub(crate) fn current_limit(host_ma: u32, current_limits: u16, mode: BusMode) -> Option<u8> {
    if !matches!(mode, BusMode::Sdr50 | BusMode::Ddr50) {
        return None;
    }
    [(800, 3), (600, 2), (400, 1)]
        .into_iter()
        .find(|&(ma, function)| host_ma >= ma && current_limits & (1 << function) != 0)
        .map(|(_, function)| function)
}

/// The bus the bring-up left the card on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Link {
    /// The negotiated timing.
    pub mode: BusMode,
    /// The SD clock it runs at, in Hz.
    pub clock_hz: u32,
    /// The base clock that was divided, in Hz.
    pub base_clock_hz: u32,
    /// Multi-block transfers announce their length (`CMD23`) rather than
    /// being stopped (`CMD12`).
    pub counted_transfers: bool,
    /// Transfers move by ADMA2 rather than the buffer data port.
    pub dma: bool,
    /// The failure that made the bring-up settle for a slower mode, if any.
    pub fallback: Option<BringUpFault>,
    /// The failure that left transfers on the data port despite granted
    /// staging, if any.
    pub dma_fallback: Option<BringUpFault>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY_MODE: u16 = 0x1F;

    fn host(caps1: u32, max_current: u32) -> HostCaps {
        HostCaps::decode(0x0002_0000, 0x0028_6400, caps1, max_current)
    }

    #[test]
    fn the_bcm2711_runs_ddr50_at_1v8_and_high_speed_at_3v3() {
        let bcm2711 = host(0xa525, 0x0008_0008);
        assert_eq!(fastest(&bcm2711, EVERY_MODE, true), BusMode::Ddr50);
        assert_eq!(fastest(&bcm2711, EVERY_MODE, false), BusMode::HighSpeed);
    }

    #[test]
    fn a_card_without_ddr50_on_a_host_whose_sdr50_needs_tuning_runs_sdr25() {
        let bcm2711 = host(0xa525, 0);
        assert_eq!(fastest(&bcm2711, 0b0_0111, true), BusMode::Sdr25);
        let untuned = host(0x0001, 0);
        assert_eq!(fastest(&untuned, 0b0_0111, true), BusMode::Sdr50);
    }

    #[test]
    fn a_card_offering_nothing_stays_at_each_voltages_default() {
        let any = host(0x0005, 0);
        assert_eq!(fastest(&any, 0b1, true), BusMode::Sdr12);
        assert_eq!(fastest(&any, 0b1, false), BusMode::DefaultSpeed);
    }

    #[test]
    fn the_current_limit_is_raised_only_as_far_as_both_ends_allow() {
        assert_eq!(
            current_limit(32, 0xF, BusMode::Ddr50),
            None,
            "the Pi 4's 32 mA"
        );
        assert_eq!(current_limit(600, 0xF, BusMode::Ddr50), Some(2));
        assert_eq!(current_limit(800, 0b0011, BusMode::Sdr50), Some(1));
        assert_eq!(current_limit(800, 0xF, BusMode::Sdr25), None);
    }

    #[test]
    fn each_mode_selects_its_own_function_and_host_encoding() {
        let modes = [
            (BusMode::DefaultSpeed, 0, 0b000, false, false),
            (BusMode::HighSpeed, 1, 0b000, true, false),
            (BusMode::Sdr12, 0, 0b000, false, true),
            (BusMode::Sdr25, 1, 0b001, true, true),
            (BusMode::Sdr50, 2, 0b010, true, true),
            (BusMode::Ddr50, 4, 0b100, true, true),
        ];
        for (mode, function, select, high_speed, uhs) in modes {
            assert_eq!(mode.function(), function, "{mode:?}");
            assert_eq!(mode.uhs_select(), select, "{mode:?}");
            assert_eq!(mode.high_speed_timing(), high_speed, "{mode:?}");
            assert_eq!(mode.signals_at_1v8(), uhs, "{mode:?}");
        }
    }
}
