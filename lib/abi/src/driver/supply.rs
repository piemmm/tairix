//! The switchable supplies of a device the platform firmware powers: a rail
//! selecting its I/O voltage and a rail switching it on, each driven by one
//! line of the firmware's own GPIO expander.
//!
//! Only discovery states them ([`HwProperty::IoRail`](crate::HwProperty::IoRail),
//! [`HwProperty::PowerRail`](crate::HwProperty::PowerRail)): which line
//! switches which rail is the board's wiring, read from its tree. The node's
//! driver switches them through the firmware channel its host lends it, which
//! answers those lines and no other.
//!
//! Each record is a little-endian word: the line in byte 0, its flags in byte
//! 1, and the rail's timing in the bytes above.

/// A rail selecting a device's I/O signalling voltage.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct IoRail {
    /// The firmware expander line that selects the voltage.
    pub line: u8,
    /// The line's level that selects 1.8 V, or `None` where the rail has no
    /// such state.
    pub level_1v8: Option<bool>,
    /// The line's level that selects 3.3 V, or `None` likewise.
    pub level_3v3: Option<bool>,
    /// How long the rail takes to settle after a change, in microseconds.
    pub settle_us: u32,
}

impl IoRail {
    /// The rail as the value of its property: the two levels in byte 1, two
    /// bits each, and the settling time in bytes 4–7.
    #[must_use]
    pub fn to_word(self) -> u64 {
        let [s0, s1, s2, s3] = self.settle_us.to_le_bytes();
        let levels = level_bits(self.level_1v8) | (level_bits(self.level_3v3) << 2);
        u64::from_le_bytes([self.line, levels, 0, 0, s0, s1, s2, s3])
    }

    /// Inverse of [`Self::to_word`]; `None` for a value it never produces.
    #[must_use]
    pub fn from_word(word: u64) -> Option<Self> {
        let [line, levels, 0, 0, s0, s1, s2, s3] = word.to_le_bytes() else {
            return None;
        };
        if levels >> 4 != 0 {
            return None;
        }
        Some(Self {
            line,
            level_1v8: level_of(levels).ok()?,
            level_3v3: level_of(levels >> 2).ok()?,
            settle_us: u32::from_le_bytes([s0, s1, s2, s3]),
        })
    }
}

/// A rail switching a device's power.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PowerRail {
    /// The firmware expander line that switches the rail.
    pub line: u8,
    /// Whether the line's low level is the rail's on.
    pub active_low: bool,
    /// How long the output takes to rise once on, in microseconds.
    pub startup_us: u32,
    /// The shortest time the output stays off before it may come back on, in
    /// microseconds.
    pub off_on_us: u32,
}

impl PowerRail {
    /// The longest delay the record carries, in microseconds: three bytes,
    /// over sixteen seconds, far past any regulator's. A format bound, not a
    /// capacity.
    pub const MAX_DELAY_US: u32 = 0x00ff_ffff;

    /// The line's level that leaves the rail `on`.
    #[must_use]
    pub const fn level(self, on: bool) -> bool {
        on != self.active_low
    }

    /// The rail as the value of its property, the start-up delay in bytes
    /// 2–4 and the off-on delay in bytes 5–7; `None` for a delay past
    /// [`Self::MAX_DELAY_US`], which no record carries.
    #[must_use]
    pub fn to_word(self) -> Option<u64> {
        let [st0, st1, st2, 0] = self.startup_us.to_le_bytes() else {
            return None;
        };
        let [oo0, oo1, oo2, 0] = self.off_on_us.to_le_bytes() else {
            return None;
        };
        let flags = u8::from(self.active_low);
        Some(u64::from_le_bytes([
            self.line, flags, st0, st1, st2, oo0, oo1, oo2,
        ]))
    }

    /// Inverse of [`Self::to_word`]; `None` for a value it never produces.
    #[must_use]
    pub fn from_word(word: u64) -> Option<Self> {
        let [line, flags, st0, st1, st2, oo0, oo1, oo2] = word.to_le_bytes();
        if flags > 1 {
            return None;
        }
        Some(Self {
            line,
            active_low: flags == 1,
            startup_us: u32::from_le_bytes([st0, st1, st2, 0]),
            off_on_us: u32::from_le_bytes([oo0, oo1, oo2, 0]),
        })
    }
}

/// An optional level as two bits: a present bit, then the level above it.
fn level_bits(level: Option<bool>) -> u8 {
    match level {
        None => 0b00,
        Some(false) => 0b01,
        Some(true) => 0b11,
    }
}

/// A level stated without its present bit.
struct StrayLevel;

/// Inverse of [`level_bits`], reading the low two bits of `bits`.
fn level_of(bits: u8) -> Result<Option<bool>, StrayLevel> {
    match bits & 0b11 {
        0b00 => Ok(None),
        0b01 => Ok(Some(false)),
        0b11 => Ok(Some(true)),
        _ => Err(StrayLevel),
    }
}

#[cfg(test)]
mod tests {
    use super::{IoRail, PowerRail};

    #[test]
    fn an_io_rail_survives_its_record_with_every_level_state() {
        for (level_1v8, level_3v3) in [
            (Some(true), Some(false)),
            (Some(false), Some(true)),
            (None, Some(true)),
            (Some(true), None),
            (None, None),
        ] {
            let rail = IoRail {
                line: 4,
                level_1v8,
                level_3v3,
                settle_us: u32::MAX,
            };
            assert_eq!(IoRail::from_word(rail.to_word()), Some(rail));
        }
    }

    #[test]
    fn an_io_rail_refuses_a_record_it_never_writes() {
        let good = IoRail {
            line: 4,
            level_1v8: Some(true),
            level_3v3: Some(false),
            settle_us: 5000,
        }
        .to_word();
        assert_eq!(IoRail::from_word(good | 1 << 16), None, "a stray byte");
        assert_eq!(IoRail::from_word(good | 1 << 12), None, "a stray level bit");
        assert_eq!(
            IoRail::from_word(good & !(1 << 8)),
            None,
            "a level with no present bit"
        );
    }

    #[test]
    fn a_power_rail_survives_its_record_up_to_its_widest_delay() {
        for active_low in [false, true] {
            let rail = PowerRail {
                line: 6,
                active_low,
                startup_us: PowerRail::MAX_DELAY_US,
                off_on_us: 3000,
            };
            let word = rail.to_word().expect("in range");
            assert_eq!(PowerRail::from_word(word), Some(rail));
        }
    }

    #[test]
    fn a_power_rail_refuses_a_delay_past_its_record_and_a_stray_flag() {
        for (startup_us, off_on_us) in [
            (PowerRail::MAX_DELAY_US + 1, 0),
            (0, PowerRail::MAX_DELAY_US + 1),
        ] {
            let rail = PowerRail {
                line: 6,
                active_low: false,
                startup_us,
                off_on_us,
            };
            assert_eq!(rail.to_word(), None, "{startup_us} {off_on_us}");
        }
        assert_eq!(PowerRail::from_word(2 << 8), None);
    }

    #[test]
    fn a_power_rail_is_on_at_its_active_level() {
        let high = PowerRail {
            line: 6,
            active_low: false,
            startup_us: 0,
            off_on_us: 0,
        };
        assert!(high.level(true));
        assert!(!high.level(false));
        let low = PowerRail {
            active_low: true,
            ..high
        };
        assert!(!low.level(true));
        assert!(low.level(false));
    }
}
