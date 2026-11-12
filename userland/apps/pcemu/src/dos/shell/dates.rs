//! Dates and times written as a country writes them, from the information
//! record function `38h` gives: the shell's `DATE`, `TIME` and `DIR`, and the
//! utilities, all write them through here.

use crate::dos::country::{field, INFO};
use crate::dos::message::decimal;
use crate::dos::program::Program;
use alloc::vec::Vec;

/// What an information record says of writing a date or a time. A
/// separator the record leaves zero is written as the United States' is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Writing {
    /// `0` month-day-year, `1` day-month-year, `2` year-month-day.
    pub order: u16,
    pub date_separator: u8,
    pub time_separator: u8,
    pub decimal: u8,
    /// Twenty-four hours, else twelve with `a` or `p` after the minutes.
    pub hours24: bool,
}

impl Writing {
    /// What record `info` says.
    #[must_use]
    pub fn of(info: &[u8; INFO]) -> Self {
        let or = |byte: u8, us: u8| if byte == 0 { us } else { byte };
        Self {
            order: u16::from_le_bytes([info[field::DATE], info[field::DATE + 1]]),
            date_separator: or(info[field::DATE_SEPARATOR], b'-'),
            time_separator: or(info[field::TIME_SEPARATOR], b':'),
            decimal: or(info[field::DECIMAL], b'.'),
            hours24: info[field::CLOCK] & 1 != 0,
        }
    }

    /// What the record at `at` in the program's memory says.
    pub fn read(p: &mut Program<'_, '_>, at: u16) -> Self {
        let mut info = [0u8; INFO];
        p.g.read_bytes(p.own, at, &mut info);
        Self::of(&info)
    }

    /// A date in the country's order: the month and day two digits, the
    /// year its last two or all of them.
    #[must_use]
    pub fn date(self, year: u16, month: u8, day: u8, short_year: bool) -> Vec<u8> {
        let year = if short_year {
            two_digits(year % 100).to_vec()
        } else {
            decimal(u32::from(year))
        };
        let (month, day) = (
            two_digits(u16::from(month)).to_vec(),
            two_digits(u16::from(day)).to_vec(),
        );
        let fields = match self.order {
            1 => [day, month, year],
            2 => [year, month, day],
            _ => [month, day, year],
        };
        fields.join(&self.date_separator)
    }

    /// `hour`, from `0`, as the country's clock shows it, and the letter a
    /// twelve-hour clock writes after the minutes.
    #[must_use]
    pub fn hour(self, hour: u8) -> (u8, Option<u8>) {
        if self.hours24 {
            return (hour, None);
        }
        let shown = u8::try_from((u16::from(hour) + 11) % 12 + 1).unwrap_or(12);
        (shown, Some(if hour < 12 { b'a' } else { b'p' }))
    }
}

/// `n`'s last two decimal digits.
#[must_use]
pub fn two_digits(n: u16) -> [u8; 2] {
    let digit = |d: u16| b'0' + u8::try_from(d % 10).unwrap_or(0);
    [digit(n / 10), digit(n)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dos::country::COUNTRIES;

    fn of(code: u16) -> Writing {
        Writing::of(&crate::dos::country::find(code).unwrap().info())
    }

    #[test]
    fn a_date_is_written_in_its_countrys_order() {
        assert_eq!(of(1).date(1988, 6, 7, false), b"06-07-1988");
        assert_eq!(of(44).date(1988, 6, 7, true), b"07/06/88");
        assert_eq!(of(46).date(2026, 11, 10, false), b"2026-11-10");
        assert_eq!(of(1).date(2107, 12, 31, true), b"12-31-07");
    }

    #[test]
    fn an_hour_is_shown_on_its_countrys_clock() {
        let us = Writing::of(&COUNTRIES[0].info());
        assert_eq!(us.hour(0), (12, Some(b'a')));
        assert_eq!(us.hour(11), (11, Some(b'a')));
        assert_eq!(us.hour(12), (12, Some(b'p')));
        assert_eq!(us.hour(23), (11, Some(b'p')));
        assert_eq!(
            us.hour(31),
            (7, Some(b'p')),
            "a stamp's hour field holds up to 31"
        );
        assert_eq!(of(49).hour(23), (23, None));
    }

    #[test]
    fn a_separator_left_zero_is_the_united_states() {
        let writing = Writing::of(&[0; INFO]);
        assert_eq!(
            (
                writing.date_separator,
                writing.time_separator,
                writing.decimal
            ),
            (b'-', b':', b'.')
        );
        assert_eq!(two_digits(7), *b"07");
        assert_eq!(two_digits(1987), *b"87");
    }
}
