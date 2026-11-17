//! The calendar a territory converts five-byte times by: a time's ordinals,
//! the ISO 8601 week, the fields its date and time formats name, and the
//! reading of a date or time typed as text.
//!
//! A five-byte time counts centiseconds from 00:00:00 on 1 January 1900, so
//! its calendar is the proleptic Gregorian one `lib/abi` reckons; nothing here
//! reckons a date itself.

use tairix_abi::time::{civil_from_days, days_from_civil};

use crate::clock::{CENTISECONDS_PER_DAY, FIVE_BYTES, UNIX_EPOCH};

/// Centiseconds in a second, a minute and an hour.
pub(crate) const PER_SECOND: u64 = 100;
pub(crate) const PER_MINUTE: u64 = 60 * PER_SECOND;
pub(crate) const PER_HOUR: u64 = 60 * PER_MINUTE;
/// Days from 1 January 1900 to the Unix epoch.
const EPOCH_DAYS: i64 = (UNIX_EPOCH / CENTISECONDS_PER_DAY).cast_signed();

/// The ordinals a time reads as.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Ordinals {
    pub(crate) centiseconds: u32,
    pub(crate) second: u32,
    pub(crate) minute: u32,
    pub(crate) hour: u32,
    pub(crate) day: u32,
    pub(crate) month: u32,
    pub(crate) year: u32,
    /// From one, Sunday.
    pub(crate) weekday: u32,
    /// From one, the first of January.
    pub(crate) yearday: u32,
}

/// The days from 1 January 1900 to the first of `month` in `year`.
fn days_to(year: i64, month: u32, day: u32) -> Option<i64> {
    Some(days_from_civil(year, month, day)? + EPOCH_DAYS)
}

impl Ordinals {
    /// The ordinals of `time`, a five-byte time.
    pub(crate) fn of(time: u64) -> Self {
        let time = time % FIVE_BYTES;
        let days = time / CENTISECONDS_PER_DAY;
        let within = time % CENTISECONDS_PER_DAY;
        let days = i64::try_from(days).unwrap_or(0);
        let (year, month, day) = civil_from_days(days - EPOCH_DAYS);
        let first = days_to(year, 1, 1).unwrap_or(days);
        let narrow = |n: u64| u32::try_from(n).unwrap_or(0);
        Self {
            centiseconds: narrow(within % PER_SECOND),
            second: narrow(within / PER_SECOND % 60),
            minute: narrow(within / PER_MINUTE % 60),
            hour: narrow(within / PER_HOUR),
            day,
            month,
            year: u32::try_from(year).unwrap_or(0),
            weekday: u32::try_from((days + 1).rem_euclid(7) + 1).unwrap_or(1),
            yearday: u32::try_from(days - first + 1).unwrap_or(1),
        }
    }

    /// Its nine words, as an ordinals block holds them.
    pub(crate) const fn words(&self) -> [u32; 9] {
        [
            self.centiseconds,
            self.second,
            self.minute,
            self.hour,
            self.day,
            self.month,
            self.year,
            self.weekday,
            self.yearday,
        ]
    }

    /// Its ISO 8601 week: weeks from Monday, the first holding the year's
    /// first Thursday.
    pub(crate) fn iso_week(&self) -> u32 {
        let iso_weekday = i64::from((self.weekday + 5) % 7 + 1);
        let year = i64::from(self.year);
        let week = (i64::from(self.yearday) - iso_weekday + 10) / 7;
        if week < 1 {
            weeks_in(year - 1)
        } else if week > i64::from(weeks_in(year)) {
            1
        } else {
            u32::try_from(week).unwrap_or(1)
        }
    }
}

/// The ISO 8601 weeks in `year`: 53 where it begins on a Thursday, or on a
/// Wednesday in a leap year.
fn weeks_in(year: i64) -> u32 {
    let starts =
        |y: i64| (y + y.div_euclid(4) - y.div_euclid(100) + y.div_euclid(400)).rem_euclid(7);
    if starts(year) == 4 || starts(year - 1) == 3 {
        53
    } else {
        52
    }
}

/// The time seven words of ordinals name — centiseconds to year: a field
/// past its range, either way, carries into the next, as genuine RISC OS 5's
/// do; only the month must be one of the year's. None for another month, a
/// year before 1900, or a time past five bytes.
pub(crate) fn time_of(words: [u32; 7]) -> Option<u64> {
    let [cs, second, minute, hour, day, month, year] = words.map(|w| i64::from(w.cast_signed()));
    if year < 1900 || !(1..=12).contains(&month) {
        return None;
    }
    let month = u32::try_from(month).ok()?;
    let days = days_to(year, month, 1)?.checked_add(day - 1)?;
    let total = days
        .checked_mul(CENTISECONDS_PER_DAY.cast_signed())?
        .checked_add(hour * PER_HOUR.cast_signed())?
        .checked_add(minute * PER_MINUTE.cast_signed())?
        .checked_add(second * PER_SECOND.cast_signed())?
        .checked_add(cs)?;
    u64::try_from(total).ok().filter(|&t| t < FIVE_BYTES)
}

/// The five-byte time `year` begins at by a standard time `standard`
/// centiseconds east of UTC; none outside five-byte times.
pub(crate) fn year_start(year: i64, standard: i32) -> Option<u64> {
    time_of([0, 0, 0, 0, 1, 1, u32::try_from(year).ok()?])?
        .checked_add_signed(-i64::from(standard))
        .filter(|&t| t < FIVE_BYTES)
}

/// The year the five-byte time `time` falls in by a standard time
/// `standard` centiseconds east of UTC; none outside five-byte times.
pub(crate) fn standard_year(time: u64, standard: i32) -> Option<i64> {
    let local = time
        .checked_add_signed(i64::from(standard))
        .filter(|&t| t < FIVE_BYTES)?;
    Some(i64::from(Ordinals::of(local).year))
}

/// The days in `month` of `year`, by the rule the territory's date reading
/// keeps: February has 28 in 1900, 2100 and 2200, and in years four does not
/// divide.
fn days_in_read_month(year: u32, month: u32) -> u32 {
    match month {
        2 if !year.is_multiple_of(4) || matches!(year, 1900 | 2100 | 2200) => 28,
        2 => 29,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The days in `month` of `year`.
pub(crate) fn days_in_month(year: u32, month: u32) -> u32 {
    let (next_year, next_month) = if month >= 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let days = days_to(i64::from(next_year), next_month, 1)
        .zip(days_to(i64::from(year), month.max(1), 1))
        .map_or(0, |(next, this)| next - this);
    u32::try_from(days).unwrap_or(0)
}

/// What a format's field shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Field {
    Day,
    Suffix,
    MonthNumber,
    MonthName,
    MonthShort,
    Week,
    YearDay,
    WeekdayNumber,
    WeekdayShort,
    WeekdayName,
    Century,
    Year,
    Hour24,
    Hour12,
    AmPm,
    Minute,
    Centiseconds,
    Second,
    Zone,
}

/// A format's piece.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Piece {
    /// The format's terminator.
    End,
    /// A byte as it is.
    Byte(u8),
    /// A field, its leading zeros left out if asked.
    Field(Field, bool),
}

/// A format names a field it has no such code for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct BadField;

/// Each field's code.
const CODES: [(&[u8; 2], Field); 20] = [
    (b"DY", Field::Day),
    (b"ST", Field::Suffix),
    (b"MN", Field::MonthNumber),
    (b"MO", Field::MonthName),
    (b"M3", Field::MonthShort),
    (b"WK", Field::Week),
    (b"DN", Field::YearDay),
    (b"WN", Field::WeekdayNumber),
    (b"W3", Field::WeekdayShort),
    (b"WE", Field::WeekdayName),
    (b"CE", Field::Century),
    (b"YR", Field::Year),
    (b"24", Field::Hour24),
    (b"12", Field::Hour12),
    (b"AM", Field::AmPm),
    (b"PM", Field::AmPm),
    (b"MI", Field::Minute),
    (b"CS", Field::Centiseconds),
    (b"SE", Field::Second),
    (b"TZ", Field::Zone),
];

/// The format at `format` read a piece at a time: each `%` code its field,
/// `%0` a zero byte and `%%` a percent sign, to its end or its first zero.
pub(crate) struct Pieces<'a> {
    format: &'a [u8],
    at: usize,
    done: bool,
}

impl<'a> Pieces<'a> {
    pub(crate) const fn of(format: &'a [u8]) -> Self {
        Self {
            format,
            at: 0,
            done: false,
        }
    }

    fn next_byte(&mut self) -> u8 {
        let byte = self.format.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        byte
    }
}

impl Iterator for Pieces<'_> {
    type Item = Result<Piece, BadField>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let byte = self.next_byte();
        if byte == 0 {
            self.done = true;
            return Some(Ok(Piece::End));
        }
        if byte != b'%' {
            return Some(Ok(Piece::Byte(byte)));
        }
        let mut first = self.next_byte();
        match first {
            b'0' => return Some(Ok(Piece::Byte(0))),
            b'%' => return Some(Ok(Piece::Byte(b'%'))),
            _ => {}
        }
        let suppress = first.eq_ignore_ascii_case(&b'Z');
        if suppress {
            first = self.next_byte();
        }
        if first == 0 {
            self.done = true;
            return Some(Err(BadField));
        }
        let code = [
            first.to_ascii_uppercase(),
            self.next_byte().to_ascii_uppercase(),
        ];
        let field = CODES
            .iter()
            .find(|(c, _)| **c == code)
            .map(|&(_, field)| field);
        if field.is_none() {
            self.done = true;
        }
        Some(
            field
                .map(|field| Piece::Field(field, suppress))
                .ok_or(BadField),
        )
    }
}

/// A numeric field's value and digits.
pub(crate) const fn number(field: Field, ordinals: &Ordinals) -> Option<(u32, u32)> {
    Some(match field {
        Field::Day => (ordinals.day, 2),
        Field::MonthNumber => (ordinals.month, 2),
        Field::YearDay => (ordinals.yearday, 3),
        Field::WeekdayNumber => (ordinals.weekday, 1),
        Field::Century => (ordinals.year / 100, 2),
        Field::Year => (ordinals.year % 100, 2),
        Field::Hour24 => (ordinals.hour, 2),
        Field::Hour12 => {
            let hour = ordinals.hour % 12;
            (if hour == 0 { 12 } else { hour }, 2)
        }
        Field::Minute => (ordinals.minute, 2),
        Field::Centiseconds => (ordinals.centiseconds, 2),
        Field::Second => (ordinals.second, 2),
        _ => return None,
    })
}

/// `value` in `digits` decimal digits, the leading zeros left out when
/// `suppress` but for the last.
pub(crate) fn digits(value: u32, digits: u32, suppress: bool) -> ([u8; 3], usize) {
    let mut out = [0; 3];
    let mut length = 0;
    let mut shown = !suppress;
    for place in (0..digits).rev() {
        let digit = (value / 10u32.pow(place) % 10) as u8;
        shown |= digit != 0 || place == 0;
        if shown {
            out[length] = b'0' + digit;
            length += 1;
        }
    }
    (out, length)
}

/// The stem of the territory's tokens for the short months, each followed
/// by its month's two digits.
pub(crate) const SHORT_MONTH: &[u8] = b"ShortMonth";

/// A message token naming a field in the territory's file: a stem and, for
/// a month or a day, its number in two digits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Token {
    bytes: [u8; Token::MOST],
    length: usize,
}

impl Token {
    /// The longest a token is.
    const MOST: usize = 16;

    fn of(stem: &[u8], number: Option<u32>) -> Self {
        let mut bytes = [0; Self::MOST];
        bytes[..stem.len()].copy_from_slice(stem);
        let mut length = stem.len();
        if let Some(n) = number {
            bytes[length] = b'0' + u8::try_from(n / 10 % 10).unwrap_or(0);
            bytes[length + 1] = b'0' + u8::try_from(n % 10).unwrap_or(0);
            length += 2;
        }
        Self { bytes, length }
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}

/// The message token a name field shows: its name in the territory's file.
pub(crate) fn name_token(field: Field, ordinals: &Ordinals) -> Option<Token> {
    Some(match field {
        Field::MonthName => Token::of(b"MonthName", Some(ordinals.month)),
        Field::MonthShort => Token::of(SHORT_MONTH, Some(ordinals.month)),
        Field::WeekdayName => Token::of(b"DayName", Some(ordinals.weekday)),
        Field::WeekdayShort => Token::of(b"ShortDay", Some(ordinals.weekday)),
        Field::AmPm if ordinals.hour < 12 => Token::of(b"BeforeNoon", None),
        Field::AmPm => Token::of(b"AfterNoon", None),
        Field::Suffix => Token::of(
            match ordinals.day {
                1 | 21 | 31 => b"DaySuffixFirst",
                2 | 22 => b"DaySuffixSecond",
                3 | 23 => b"DaySuffixThird",
                _ => b"DaySuffixOther",
            },
            None,
        ),
        _ => return None,
    })
}

/// What `MessageTrans` lookups a reading needs: the territory's short month
/// names, in its file's order.
pub(crate) type Months<'a> = &'a [&'a [u8]];

/// A date or time typed as text cannot be read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct BadString;

/// Reads text as a day of the month, a month, a year and a time.
struct Reader<'a> {
    text: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn peek(&self) -> u8 {
        self.text.get(self.at).copied().unwrap_or(0)
    }

    fn spaces(&mut self) {
        while self.peek() == b' ' {
            self.at += 1;
        }
    }

    /// Spaces, and at most one `separator` among them.
    fn separator(&mut self, separator: u8) {
        self.spaces();
        if self.peek() == separator {
            self.at += 1;
            self.spaces();
        }
    }

    /// A decimal number and how many digits it had.
    fn number(&mut self) -> Result<(u32, usize), BadString> {
        let start = self.at;
        let mut value: u32 = 0;
        while self.peek().is_ascii_digit() {
            value = value
                .checked_mul(10)
                .and_then(|v| v.checked_add(u32::from(self.peek() - b'0')))
                .ok_or(BadString)?;
            self.at += 1;
        }
        let length = self.at - start;
        if length == 0 {
            return Err(BadString);
        }
        Ok((value, length))
    }

    fn within(&mut self, range: core::ops::RangeInclusive<u32>) -> Result<u32, BadString> {
        let (value, _) = self.number()?;
        range.contains(&value).then_some(value).ok_or(BadString)
    }

    /// A month by its short name: the whole of a name in the file's order,
    /// either case, then any more letters to a space or a dash.
    fn month(&mut self, months: Months<'_>) -> Result<u32, BadString> {
        let rest = &self.text[self.at.min(self.text.len())..];
        for (number, name) in (1..).zip(months) {
            let mut matched = true;
            for (i, &want) in name.iter().enumerate() {
                let have = rest.get(i).copied().unwrap_or(0);
                if have < b' ' {
                    return Err(BadString);
                }
                if have | 0x20 != want | 0x20 {
                    matched = false;
                    break;
                }
            }
            if matched {
                self.at += name.len();
                while !matches!(self.peek(), b' ' | b'-') && self.peek() >= b' ' {
                    self.at += 1;
                }
                return Ok(number);
            }
        }
        Err(BadString)
    }

    /// A date: a weekday skipped to its comma, then the day, the month and
    /// the year.
    fn date(&mut self, months: Months<'_>) -> Result<(u32, u32, u32), BadString> {
        if !self.peek().is_ascii_digit() {
            loop {
                let byte = self.peek();
                if byte < b' ' {
                    return Err(BadString);
                }
                self.at += 1;
                if byte == b',' {
                    break;
                }
            }
            self.spaces();
        }
        let day = self.within(1..=31)?;
        self.separator(b'-');
        let month = self.month(months)?;
        self.separator(b'-');
        let (number, length) = self.number()?;
        let year = if length > 2 {
            number
        } else {
            match number {
                0..=65 => 2000 + number,
                66 if (month, day) >= (1, 26) => 1966,
                66 => 2066,
                _ => 1900 + number,
            }
        };
        if day > days_in_read_month(year, month) {
            return Err(BadString);
        }
        Ok((day, month, year))
    }
}

/// The seven words of ordinals `Territory_ConvertTimeStringToOrdinals`
/// writes, by place; none where it writes nothing.
pub(crate) type Written = [Option<u32>; 7];

/// What `Territory_ConvertTimeStringToOrdinals` reads: bit 1 of `reason` a
/// date, bit 0 a time, both a date then a time, the fields not read all
/// ones. The date is settled first and the time's fields written as each is
/// read, as RISC OS writes them, so a refused string leaves what came before.
pub(crate) fn read(
    text: &[u8],
    reason: u32,
    months: Months<'_>,
) -> (Written, Result<(), BadString>) {
    let mut written = [None; 7];
    let result = read_into(text, reason, months, &mut written);
    (written, result)
}

fn read_into(
    text: &[u8],
    reason: u32,
    months: Months<'_>,
    written: &mut Written,
) -> Result<(), BadString> {
    let mut reader = Reader { text, at: 0 };
    reader.spaces();
    let date = if reason & 2 != 0 {
        let (day, month, year) = reader.date(months)?;
        reader.separator(b'.');
        [day, month, year]
    } else {
        [u32::MAX; 3]
    };
    for (slot, word) in written[4..].iter_mut().zip(date) {
        *slot = Some(word);
    }
    if reason & 1 == 0 {
        written[..4].fill(Some(u32::MAX));
        return Ok(());
    }
    written[0] = Some(0);
    written[3] = Some(reader.within(0..=23)?);
    reader.separator(b':');
    written[2] = Some(reader.within(0..=59)?);
    reader.separator(b':');
    written[1] = Some(reader.within(0..=61)?);
    Ok(())
}

#[cfg(test)]
mod tests;
