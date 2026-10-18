//! The annual rules a zone's offset changes by, as the IANA source states
//! them, and the one arithmetic that turns a rule and a year into an instant.
//!
//! The compiler builds every zone's transitions with it and the reader extends
//! a zone past its last transition with it, so the two cannot disagree about
//! when a rule fires. It follows `zic`'s own arithmetic (`rpytime`).

use tairix_abi::time::{days_from_civil, days_in_month, weekday_from_days, SECS_PER_DAY};

/// The furthest year from the epoch an `i64` count of seconds reaches: past
/// it no instant exists to name, and the calendar arithmetic would overflow.
const YEAR_LIMIT: i64 = 292_277_026_596;

/// A leap year, whose months are the longest any year has.
const LEAP_YEAR: i64 = 2000;

/// The days `month` has in a leap year — the most a rule's day may name —
/// or 0 outside `1..=12`.
#[must_use]
pub fn leap_month_length(month: u8) -> u8 {
    u8::try_from(days_in_month(LEAP_YEAR, u32::from(month))).unwrap_or(0)
}

/// Which day of its month a rule names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DayRule {
    /// That day of the month.
    Of(u8),
    /// The first `weekday` (`0` = Sunday) on or after `day`.
    OnOrAfter {
        /// The weekday, `0` = Sunday.
        weekday: u8,
        /// The day of the month it may not fall before.
        day: u8,
    },
    /// The last `weekday` (`0` = Sunday) on or before `day`: a month's last
    /// such day names the month's length in a leap year.
    OnOrBefore {
        /// The weekday, `0` = Sunday.
        weekday: u8,
        /// The day of the month it may not fall after.
        day: u8,
    },
}

/// The clock a rule's time of day is read on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TimeBase {
    /// The local wall clock, saving included.
    Wall,
    /// Local standard time.
    Standard,
    /// Universal time.
    Universal,
}

/// When in each year a rule takes effect.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Moment {
    /// The month, `1..=12`.
    pub month: u8,
    /// The day of it.
    pub day: DayRule,
    /// Seconds after that day's midnight, possibly past one day either way.
    pub at: i32,
    /// The clock `at` is read on.
    pub base: TimeBase,
}

impl Moment {
    /// Whether the moment names a day every year has: a month `1..=12`, a
    /// weekday `0..=6`, and a day the month can hold, with the 29th of February
    /// allowed only as a bound a weekday is sought on or before.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        let leap_length = leap_month_length(self.month);
        let day_fits = |day: u8| day >= 1 && day <= leap_length;
        let feb_29 = |day: u8| self.month == 2 && day == 29;
        (1..=12).contains(&self.month)
            && match self.day {
                DayRule::Of(day) => day_fits(day) && !feb_29(day),
                DayRule::OnOrAfter { weekday, day } => {
                    weekday <= 6 && day_fits(day) && !feb_29(day)
                }
                DayRule::OnOrBefore { weekday, day } => weekday <= 6 && day_fits(day),
            }
    }

    /// The moment's local date and time in `year` as seconds from the epoch,
    /// before any offset applies. `None` where the year lacks the day — the
    /// 29th of February outside a leap year, for a rule that does not seek a
    /// weekday before it — where a weekday sought is not `0..=6`, or where no
    /// `i64` count of seconds reaches it.
    #[must_use]
    pub fn local_seconds(&self, year: i64) -> Option<i64> {
        if !(-YEAR_LIMIT..=YEAR_LIMIT).contains(&year) {
            return None;
        }
        let month = u32::from(self.month);
        let length = days_in_month(year, month);
        let (bound, seek) = match self.day {
            DayRule::Of(day) => (u32::from(day), None),
            DayRule::OnOrAfter { weekday, day } => (u32::from(day), Some((weekday, true))),
            DayRule::OnOrBefore { weekday, day } => {
                (u32::from(day).min(length), Some((weekday, false)))
            }
        };
        if bound == 0 || bound > length || seek.is_some_and(|(weekday, _)| weekday > 6) {
            return None;
        }
        let mut days = days_from_civil(year, month, bound)?;
        if let Some((weekday, forward)) = seek {
            let current = weekday_from_days(days) % 7;
            let target = u32::from(weekday);
            days += if forward {
                i64::from((target + 7 - current) % 7)
            } else {
                -i64::from((current + 7 - target) % 7)
            };
        }
        days.checked_mul(SECS_PER_DAY)?
            .checked_add(i64::from(self.at))
    }

    /// The UT instant the moment names in `year`, in a zone `stdoff` seconds
    /// east of UT with `save` seconds of saving in force just before it.
    #[must_use]
    pub fn instant(&self, year: i64, stdoff: i32, save: i32) -> Option<i64> {
        let local = self.local_seconds(year)?;
        let shift = match self.base {
            TimeBase::Wall => i64::from(stdoff) + i64::from(save),
            TimeBase::Standard => i64::from(stdoff),
            TimeBase::Universal => 0,
        };
        local.checked_sub(shift)
    }
}

#[cfg(test)]
#[path = "rule_tests.rs"]
mod tests;
