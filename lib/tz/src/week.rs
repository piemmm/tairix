//! The days of the week, and which of them a region's calendar begins on.

use tairix_abi::time::weekday_from_days;

/// A day of the week.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Weekday {
    /// Monday, the day ISO 8601 numbers 1.
    Monday,
    /// Tuesday.
    Tuesday,
    /// Wednesday.
    Wednesday,
    /// Thursday.
    Thursday,
    /// Friday.
    Friday,
    /// Saturday.
    Saturday,
    /// Sunday, the day ISO 8601 numbers 7.
    Sunday,
}

impl Weekday {
    /// Every day, Monday first, as ISO 8601 numbers them.
    pub const ALL: [Self; 7] = [
        Self::Monday,
        Self::Tuesday,
        Self::Wednesday,
        Self::Thursday,
        Self::Friday,
        Self::Saturday,
        Self::Sunday,
    ];

    /// The day ISO 8601 numbers `number`, Monday being 1, or `None` outside
    /// `1..=7`.
    #[must_use]
    pub const fn from_iso(number: u32) -> Option<Self> {
        match number {
            1 => Some(Self::Monday),
            2 => Some(Self::Tuesday),
            3 => Some(Self::Wednesday),
            4 => Some(Self::Thursday),
            5 => Some(Self::Friday),
            6 => Some(Self::Saturday),
            7 => Some(Self::Sunday),
            _ => None,
        }
    }

    /// Its ISO 8601 number, Monday being 1.
    #[must_use]
    pub const fn iso(self) -> u32 {
        match self {
            Self::Monday => 1,
            Self::Tuesday => 2,
            Self::Wednesday => 3,
            Self::Thursday => 4,
            Self::Friday => 5,
            Self::Saturday => 6,
            Self::Sunday => 7,
        }
    }

    /// The day `days` days after 1970-01-01 falls on.
    #[must_use]
    pub fn of_days(days: i64) -> Self {
        Self::from_iso(weekday_from_days(days)).unwrap_or(Self::Monday)
    }

    /// How many days after `start` it falls, `0..=6`: its column in a week
    /// that begins on `start`.
    #[must_use]
    pub const fn days_after(self, start: Self) -> u32 {
        (self.iso() + 7 - start.iso()) % 7
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_abi::time::days_from_civil;

    #[test]
    fn the_iso_numbers_round_trip_and_nothing_else_names_a_day() {
        for (index, day) in Weekday::ALL.into_iter().enumerate() {
            let number = u32::try_from(index + 1).expect("seven");
            assert_eq!(day.iso(), number);
            assert_eq!(Weekday::from_iso(number), Some(day));
        }
        assert_eq!(Weekday::from_iso(0), None);
        assert_eq!(Weekday::from_iso(8), None);
    }

    #[test]
    fn a_date_falls_on_its_own_day() {
        let day = |y, m, d| Weekday::of_days(days_from_civil(y, m, d).expect("a date"));
        assert_eq!(day(1970, 1, 1), Weekday::Thursday);
        assert_eq!(day(2026, 10, 25), Weekday::Sunday);
        assert_eq!(day(2000, 2, 29), Weekday::Tuesday);
        assert_eq!(day(1969, 12, 31), Weekday::Wednesday);
    }

    #[test]
    fn a_day_counts_from_the_start_of_a_week() {
        assert_eq!(Weekday::Monday.days_after(Weekday::Monday), 0);
        assert_eq!(Weekday::Sunday.days_after(Weekday::Monday), 6);
        assert_eq!(Weekday::Monday.days_after(Weekday::Sunday), 1);
        assert_eq!(Weekday::Friday.days_after(Weekday::Saturday), 6);
        assert_eq!(Weekday::Saturday.days_after(Weekday::Saturday), 0);
    }
}
