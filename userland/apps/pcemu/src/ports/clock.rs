//! The clock card: an OKI MSM6242B real-time clock, its sixteen four-bit
//! registers at `2C0h` on the low data lines.
//!
//! It counts in BCD from seconds to a two-digit year, with a day-of-week
//! counter whose meaning is the software's, and takes every year divisible
//! by four for a leap year. HOLD holds one carry and latches BUSY, set when
//! HOLD came within a carry's 190 µs; STOP freezes the divider below a
//! second and RESET clears it, its release taking up the 24/12 bit; 30-second
//! adjust rounds to the nearest minute. The fixed-period flag rises every
//! 1/64 s, second, minute or hour, for 7.8125 ms or until cleared. A counter
//! written out of range counts on as if it held its last value. Epson's
//! RTC-62421 manual, the chip's second source, describes all of it.

use crate::time::Nanos;
use tairix_abi::time::{days_from_civil, days_in_month, weekday_from_days, CivilTime};

/// The card's first port.
pub const PORT: u16 = 0x2C0;
/// The ports it answers.
pub const PORTS: u16 = 16;
/// The years its two digits stand for: those below 80 are this century's.
pub const YEARS: core::ops::RangeInclusive<i64> = 1980..=2079;

const SECOND: Nanos = 1_000_000_000;
const MINUTE: Nanos = 60 * SECOND;
/// The divider's 64 Hz step, and the fixed-period pulse: half of it.
const TICK: Nanos = SECOND / 64;
const PULSE: Nanos = TICK / 2;
/// How long a carry keeps the counter busy, and the 30-second adjustment.
const CARRYING: Nanos = 190_000;
const ADJUSTING: Nanos = 125_000;
/// Days in four years of the chip's calendar.
const FOUR_YEARS: u64 = 4 * 365 + 1;

/// The control registers, from the card's base.
pub mod reg {
    /// Control register D: hold, busy, the fixed-period flag, adjust.
    pub const CD: u16 = 0x0D;
    /// Control register E: the fixed period's setup.
    pub const CE: u16 = 0x0E;
    /// Control register F: reset, stop, 24-hour time, test.
    pub const CF: u16 = 0x0F;
}

/// Control register D's bits.
pub mod cd {
    /// Holds the counter's carry so it can be read or written.
    pub const HOLD: u8 = 0x01;
    /// A carry is under way: the counters may not be read.
    pub const BUSY: u8 = 0x02;
    /// The fixed-period flag, which writing 0 clears; written as 1 otherwise.
    pub const IRQ: u8 = 0x04;
    /// 30-second adjust.
    pub const ADJUST: u8 = 0x08;
}

mod ce {
    pub const MASK: u8 = 0x01;
    pub const INTERRUPT: u8 = 0x02;
}

/// Control register F's bits.
pub mod cf {
    /// Holds the divider below a second cleared.
    pub const RESET: u8 = 0x01;
    /// Stops the divider.
    pub const STOP: u8 = 0x02;
    /// 24-hour time, taken up when RESET is released.
    pub const HOURS_24: u8 = 0x04;
}

/// Each counter's units register, its tens at the next.
pub mod digit {
    /// Seconds.
    pub const SECONDS: usize = 0;
    /// Minutes.
    pub const MINUTES: usize = 2;
    /// Hours, the tens register carrying the PM bit.
    pub const HOURS: usize = 4;
    /// The day of the month.
    pub const DAY: usize = 6;
    /// The month.
    pub const MONTH: usize = 8;
    /// The year's last two digits.
    pub const YEAR: usize = 10;
    /// The day of the week alone, from 0 to 6.
    pub const WEEKDAY: usize = 12;
}

/// The bits each counter register holds; the rest read zero.
const DIGIT_BITS: [u8; 13] = [
    0xF, 0x7, 0xF, 0x7, 0xF, 0x7, 0xF, 0x3, 0xF, 0x1, 0xF, 0xF, 0x7,
];
/// The tens of hours' PM bit, which only 12-hour time keeps.
pub const PM: u8 = 0x04;

/// The fixed periods, as `t1` and `t0` number them.
mod period {
    pub const SECOND: usize = 1;
    pub const MINUTE: usize = 2;
    pub const HOUR: usize = 3;
}

/// HOLD, and while it is set, what it latched and what it held back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hold {
    Off,
    /// `busy` is BUSY as HOLD latched it; `held`, a carry it held back.
    On {
        busy: bool,
        held: bool,
    },
}

/// The divider below a second.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Divider {
    /// Counting since `since`; the next second carries at `next`.
    Running { next: Nanos, since: Nanos },
    /// Stopped with `left` to go before the next carry.
    Stopped { left: Nanos },
}

/// The card.
#[derive(Clone, Debug)]
pub struct ClockCard {
    digits: [u8; 13],
    hold: Hold,
    adjusting_until: Nanos,
    ce: u8,
    cf: u8,
    /// The timer mode in effect, which a RESET's release takes up.
    hours_24: bool,
    divider: Divider,
    /// When each fixed period last came.
    periods: [Option<Nanos>; 4],
    /// When the fixed-period flag was last cleared.
    cleared: Nanos,
    /// When the counter last counted.
    counted: Option<Nanos>,
    /// The guest wrote the time or date, or adjusted it, since the card was
    /// last asked.
    set_by_guest: bool,
}

impl ClockCard {
    /// A card keeping 24-hour time from midnight on 1 January 1980, its
    /// divider starting at `t` and its fixed-period output masked.
    #[must_use]
    pub fn new(t: Nanos) -> Self {
        let mut card = Self {
            digits: [0; 13],
            hold: Hold::Off,
            adjusting_until: 0,
            ce: ce::MASK,
            cf: cf::HOURS_24,
            hours_24: true,
            divider: Divider::Running {
                next: t + SECOND,
                since: t,
            },
            periods: [None; 4],
            cleared: 0,
            counted: None,
            set_by_guest: false,
        };
        card.set_date(1980, 1, 1);
        card
    }

    /// Sets the time and date to `now` at `t`, its second beginning then;
    /// refuses a time it cannot hold.
    pub fn set(&mut self, now: &CivilTime, t: Nanos) -> bool {
        if !now.is_valid() || !YEARS.contains(&now.year) {
            return false;
        }
        self.advance(t);
        self.set_field(digit::SECONDS, narrow(u64::from(now.second)));
        self.set_field(digit::MINUTES, narrow(u64::from(now.minute)));
        self.set_hour(narrow(u64::from(now.hour)));
        self.set_date(now.year, now.month, now.day);
        if let Hold::On { held, .. } = &mut self.hold {
            *held = false;
        }
        self.divider = match self.divider {
            Divider::Running { .. } => Divider::Running {
                next: t + SECOND,
                since: t,
            },
            Divider::Stopped { .. } => Divider::Stopped { left: SECOND },
        };
        true
    }

    /// The time and date the card holds at `t`, if they are one.
    pub fn time(&mut self, t: Nanos) -> Option<CivilTime> {
        self.advance(t);
        let bcd = |at: usize, max: u8| {
            let (units, tens) = (self.digits[at], self.digits[at + 1]);
            Some(tens * 10 + units).filter(|&value| units <= 9 && value <= max)
        };
        let (units, tens) = (self.digits[digit::HOURS], self.digits[digit::HOURS + 1]);
        let clock = (tens & 3) * 10 + units;
        let hour = match (self.hours_24, units <= 9) {
            (true, true) if clock <= 23 => clock,
            (false, true) if (1..=12).contains(&clock) => {
                clock % 12 + if tens & PM != 0 { 12 } else { 0 }
            }
            _ => return None,
        };
        let year = bcd(digit::YEAR, 99)?;
        let time = CivilTime {
            year: i64::from(year) + if year < 80 { 2000 } else { 1900 },
            month: u32::from(bcd(digit::MONTH, 12)?),
            day: u32::from(bcd(digit::DAY, 31)?),
            hour: u32::from(hour),
            minute: u32::from(bcd(digit::MINUTES, 59)?),
            second: u32::from(bcd(digit::SECONDS, 59)?),
        };
        time.is_valid().then_some(time)
    }

    /// Reads register `offset` at `t`; the high data lines float.
    pub fn input(&mut self, offset: u16, t: Nanos) -> u8 {
        self.advance(t);
        let value = match offset {
            reg::CD => {
                let (hold, busy) = match self.hold {
                    Hold::Off => (0, cd::BUSY),
                    Hold::On { busy, .. } => (cd::HOLD, if busy { cd::BUSY } else { 0 }),
                };
                hold | busy
                    | if self.flag(t) { cd::IRQ } else { 0 }
                    | if t < self.adjusting_until {
                        cd::ADJUST
                    } else {
                        0
                    }
            }
            reg::CE => self.ce,
            reg::CF => self.cf,
            n => self.digits.get(usize::from(n)).copied().unwrap_or(0),
        };
        0xF0 | value
    }

    /// Writes register `offset` at `t`.
    pub fn output(&mut self, offset: u16, value: u8, t: Nanos) {
        self.advance(t);
        let value = value & 0x0F;
        match offset {
            reg::CD => self.write_cd(value, t),
            reg::CE => {
                // A changed setup starts the fixed period afresh.
                if value != self.ce {
                    self.cleared = t;
                }
                self.ce = value;
            }
            reg::CF => self.write_cf(value, t),
            n => {
                let n = usize::from(n);
                let pm_dropped = n == digit::HOURS + 1 && self.hours_24;
                if let (Some(slot), Some(&bits)) = (self.digits.get_mut(n), DIGIT_BITS.get(n)) {
                    *slot = value & if pm_dropped { bits & !PM } else { bits };
                    self.set_by_guest = true;
                }
            }
        }
    }

    /// The time the guest set, once it has set one and let the card run on
    /// with it: taken once a setting.
    pub fn take_guest_setting(&mut self, t: Nanos) -> Option<CivilTime> {
        let running =
            matches!(self.divider, Divider::Running { .. }) && matches!(self.hold, Hold::Off);
        if !self.set_by_guest || !running {
            return None;
        }
        self.set_by_guest = false;
        self.time(t)
    }

    fn write_cd(&mut self, value: u8, t: Nanos) {
        if value & cd::IRQ == 0 {
            self.cleared = t;
        }
        self.hold = match (self.hold, value & cd::HOLD != 0) {
            (Hold::Off, true) => Hold::On {
                busy: self.counted.is_some_and(|at| t < at + CARRYING),
                held: false,
            },
            (Hold::On { held, .. }, false) => {
                if held {
                    self.count(1, t);
                }
                Hold::Off
            }
            (hold, _) => hold,
        };
        if value & cd::ADJUST != 0 && t >= self.adjusting_until {
            self.adjust(t);
            self.set_by_guest = true;
        }
    }

    fn write_cf(&mut self, value: u8, t: Nanos) {
        let reset = value & cf::RESET != 0;
        if self.cf & cf::RESET != 0 && !reset {
            self.hours_24 = value & cf::HOURS_24 != 0;
        }
        self.cf = value;
        let running = value & (cf::STOP | cf::RESET) == 0;
        self.divider = match (self.divider, running) {
            (Divider::Running { .. }, true) => self.divider,
            (Divider::Running { next, .. }, false) => Divider::Stopped {
                left: if reset { SECOND } else { next - t },
            },
            (Divider::Stopped { left }, true) => Divider::Running {
                next: t + left,
                since: t,
            },
            (Divider::Stopped { left }, false) => Divider::Stopped {
                left: if reset { SECOND } else { left },
            },
        };
    }

    /// The fixed-period flag at `t`: a pulse after each period, or a latch.
    fn flag(&self, t: Nanos) -> bool {
        if self.ce & ce::MASK != 0 {
            return false;
        }
        let latched = self.ce & ce::INTERRUPT != 0;
        self.periods[usize::from(self.ce >> 2)]
            .is_some_and(|at| at > self.cleared && (latched || t < at + PULSE))
    }

    /// Runs the divider through `t`, counting each second it carries.
    fn advance(&mut self, t: Nanos) {
        let Divider::Running { mut next, since } = self.divider else {
            return;
        };
        if t >= next {
            let carries = 1 + (t - next) / SECOND;
            let last = next + (carries - 1) * SECOND;
            next += carries * SECOND;
            match &mut self.hold {
                Hold::On { held, .. } => *held = true,
                Hold::Off => self.count(carries, last),
            }
        }
        let tick = next - (next - t).div_ceil(TICK) * TICK;
        if tick > since {
            self.periods[0] = Some(tick);
        }
        self.divider = Divider::Running { next, since };
    }

    /// Counts `n` seconds, a second apart, the last at `last`.
    fn count(&mut self, n: u64, last: Nanos) {
        let (second, minutes) = roll(self.field(digit::SECONDS), 0, 59, n);
        self.set_field(digit::SECONDS, second);
        self.periods[period::SECOND] = Some(last);
        self.counted = Some(last);
        self.carry_minutes(minutes, last.saturating_sub(u64::from(second) * SECOND));
    }

    /// Carries `n` minutes in, a minute apart, the last at `at`.
    fn carry_minutes(&mut self, n: u64, at: Nanos) {
        if n == 0 {
            return;
        }
        self.periods[period::MINUTE] = Some(at);
        let (minute, hours) = roll(self.field(digit::MINUTES), 0, 59, n);
        self.set_field(digit::MINUTES, minute);
        if hours == 0 {
            return;
        }
        self.periods[period::HOUR] = Some(at.saturating_sub(u64::from(minute) * MINUTE));
        let (hour, days) = roll(self.hour(), 0, 23, hours);
        self.set_hour(hour);
        self.carry_days(days);
    }

    /// Carries `n` days into the weekday and the date. Four years of the
    /// chip's calendar always hold the same days, so whole ones are skipped.
    fn carry_days(&mut self, n: u64) {
        if n == 0 {
            return;
        }
        let weekday = u64::from(self.digits[digit::WEEKDAY].min(6));
        self.digits[digit::WEEKDAY] = narrow((weekday + n) % 7);
        let mut year = self.field(digit::YEAR).min(99);
        let mut month = match self.field(digit::MONTH) {
            month @ 1..=12 => month,
            _ => 12,
        };
        let mut day = match self.field(digit::DAY) {
            day if (1..=month_days(year, month)).contains(&day) => day,
            _ => month_days(year, month),
        };
        let skipped = n / FOUR_YEARS;
        let (mut months, mut years) = (skipped > 0, skipped > 0);
        year = narrow((u64::from(year) + 4 * skipped) % 100);
        let mut left = n % FOUR_YEARS;
        while left > 0 {
            let rest = u64::from(month_days(year, month) - day);
            if left <= rest {
                day += narrow(left);
                left = 0;
            } else {
                left -= rest + 1;
                day = 1;
                months = true;
                if month == 12 {
                    month = 1;
                    year = (year + 1) % 100;
                    years = true;
                } else {
                    month += 1;
                }
            }
        }
        self.set_field(digit::DAY, day);
        if months {
            self.set_field(digit::MONTH, month);
        }
        if years {
            self.set_field(digit::YEAR, year);
        }
    }

    /// 30-second adjust: the seconds to zero, a minute carried from 30 on,
    /// and the divider below a second cleared.
    fn adjust(&mut self, t: Nanos) {
        let carry = self.field(digit::SECONDS) >= 30;
        self.set_field(digit::SECONDS, 0);
        if carry {
            self.periods[period::SECOND] = Some(t);
            self.carry_minutes(1, t);
        }
        self.adjusting_until = t + ADJUSTING;
        self.divider = match self.divider {
            Divider::Running { .. } => Divider::Running {
                next: t + SECOND,
                since: t,
            },
            Divider::Stopped { .. } => Divider::Stopped { left: SECOND },
        };
    }

    /// A counter's value as its digits hold it, in or out of range.
    fn field(&self, at: usize) -> u8 {
        self.digits[at + 1] * 10 + self.digits[at]
    }

    fn set_field(&mut self, at: usize, value: u8) {
        self.digits[at] = value % 10;
        self.digits[at + 1] = value / 10;
    }

    /// The hour from 0 to 23 in the mode in effect, an hour out of range
    /// taken as the last of its half day.
    fn hour(&self) -> u8 {
        let tens = self.digits[digit::HOURS + 1];
        let clock = (tens & 3) * 10 + self.digits[digit::HOURS];
        if self.hours_24 {
            return clock.min(23);
        }
        let pm = if tens & PM != 0 { 12 } else { 0 };
        match clock {
            1..=12 => clock % 12 + pm,
            _ => 11 + pm,
        }
    }

    fn set_hour(&mut self, hour: u8) {
        let (clock, pm) = match (self.hours_24, hour % 12) {
            (true, _) => (hour, 0),
            (false, 0) => (12, if hour >= 12 { PM } else { 0 }),
            (false, h) => (h, if hour >= 12 { PM } else { 0 }),
        };
        self.digits[digit::HOURS] = clock % 10;
        self.digits[digit::HOURS + 1] = (clock / 10) | pm;
    }

    /// The date and its weekday, Sunday being 0.
    fn set_date(&mut self, year: i64, month: u32, day: u32) {
        let weekday = days_from_civil(year, month, day).map_or(0, weekday_from_days) % 7;
        self.digits[digit::WEEKDAY] = narrow(u64::from(weekday));
        self.set_field(digit::YEAR, narrow(year.rem_euclid(100).unsigned_abs()));
        self.set_field(digit::MONTH, narrow(u64::from(month)));
        self.set_field(digit::DAY, narrow(u64::from(day)));
    }
}

/// A counter from `first` to `last` holding `value`, counted on `n` times:
/// its value, and how many times it carried.
fn roll(value: u8, first: u8, last: u8, n: u64) -> (u8, u64) {
    let value = if (first..=last).contains(&value) {
        value
    } else {
        last
    };
    let span = u64::from(last - first) + 1;
    let total = u64::from(value - first) + n;
    (first + narrow(total % span), total / span)
}

/// The days in `month` of two-digit `year`: for 0 to 99 the Gregorian rule
/// is the chip's, every fourth year a leap year.
fn month_days(year: u8, month: u8) -> u8 {
    u8::try_from(days_in_month(i64::from(year), u32::from(month))).unwrap_or(31)
}

/// A value its caller has bounded below 256.
fn narrow(value: u64) -> u8 {
    u8::try_from(value).unwrap_or(u8::MAX)
}

crate::snapshot::state_enum!(Hold {
    0 => Off,
    1 => On { busy, held },
});

crate::snapshot::state_enum!(Divider {
    0 => Running { next, since },
    1 => Stopped { left },
});

crate::snapshot::state!(
    ClockCard {
        digits,
        hold,
        adjusting_until,
        ce,
        cf,
        hours_24,
        divider,
        periods,
        cleared,
        counted,
        set_by_guest,
    }
    check |card: &ClockCard| card.digits.iter().all(|&digit| digit <= 0x0F)
);

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
