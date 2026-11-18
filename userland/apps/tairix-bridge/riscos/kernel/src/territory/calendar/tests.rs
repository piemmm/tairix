//! The calendar's tests: ordinals, ISO weeks, formats' pieces and the
//! reading of dates and times.

use super::*;
use alloc::vec::Vec;

/// The five-byte time of a UTC date and time.
fn at(year: u32, month: u32, day: u32, hour: u32, minute: u32) -> u64 {
    time_of([0, 0, minute, hour, day, month, year]).expect("a time")
}

#[test]
fn ordinals_count_from_monday_1_january_1900() {
    let first = Ordinals::of(0);
    assert_eq!((first.day, first.month, first.year), (1, 1, 1900));
    assert_eq!(
        (first.weekday, first.yearday),
        (2, 1),
        "a Monday, the first day"
    );
    let millennium = Ordinals::of(at(2000, 1, 1, 0, 0));
    assert_eq!(millennium.weekday, 7, "a Saturday");
    let leap = Ordinals::of(at(2024, 2, 29, 13, 45) + 4_250);
    assert_eq!(
        leap.words(),
        [50, 42, 45, 13, 29, 2, 2024, 5, 60],
        "centiseconds to day of year"
    );
}

#[test]
fn ordinals_name_a_time_that_reads_back_as_them() {
    let time = at(2026, 11, 16, 9, 30);
    let words = Ordinals::of(time).words();
    assert_eq!(time_of(words[..7].try_into().expect("seven")), Some(time));
    assert_eq!(
        time_of([0, 0, 0, 0, 0, 3, 2024]),
        Some(at(2024, 2, 29, 0, 0)),
        "day nought the day before"
    );
    assert_eq!(time_of([0, 0, 0, 0, 1, 0, 2024]), None, "month nought");
    assert_eq!(time_of([0, 0, 0, 0, 1, 13, 2024]), None);
    assert_eq!(
        time_of([0, 0, 0, 0, 32, 1, 2024]),
        Some(at(2024, 2, 1, 0, 0)),
        "a day past the month carries"
    );
    assert_eq!(
        time_of([0, 0, 0, 0, u32::MAX, 3, 2024]),
        Some(at(2024, 2, 28, 0, 0)),
        "and one before it"
    );
    assert_eq!(
        time_of([0, u32::MAX, 0, 0, 1, 1, 2024]),
        Some(at(2023, 12, 31, 23, 59) + 5900),
        "a second before the day"
    );
    assert_eq!(
        time_of([0, 0, 0, 25, 1, 1, 2024]),
        Some(at(2024, 1, 2, 1, 0)),
        "an hour past the day carries"
    );
    assert_eq!(time_of([0, 0, 0, 0, 1, 1, 1899]), None);
    assert_eq!(time_of([0, 0, 0, 0, 1, 1, 2300]), None, "past five bytes");
    assert_eq!(days_in_month(2024, 2), 29);
    assert_eq!(days_in_month(2100, 2), 28);
    assert_eq!(days_in_month(2026, 12), 31);
}

#[test]
fn iso_weeks_start_on_monday_with_the_year_s_first_thursday() {
    let week = |y, m, d| Ordinals::of(at(y, m, d, 12, 0)).iso_week();
    assert_eq!(week(2021, 1, 1), 53, "the last week of 2020");
    assert_eq!(week(2021, 1, 4), 1);
    assert_eq!(week(2024, 12, 30), 1, "the first week of 2025");
    assert_eq!(week(2026, 11, 16), 47);
    assert_eq!(week(2015, 12, 31), 53);
}

#[test]
fn a_format_reads_as_its_codes_and_bytes() {
    let pieces: Vec<_> = Pieces::of(b"%DY.%zmn%%%0x").collect();
    assert_eq!(
        pieces,
        [
            Ok(Piece::Field(Field::Day, false)),
            Ok(Piece::Byte(b'.')),
            Ok(Piece::Field(Field::MonthNumber, true)),
            Ok(Piece::Byte(b'%')),
            Ok(Piece::Byte(0)),
            Ok(Piece::Byte(b'x')),
            Ok(Piece::End),
        ]
    );
    assert_eq!(Pieces::of(b"%QQ").next(), Some(Err(BadField)));
    assert_eq!(Pieces::of(b"%Z").next(), Some(Err(BadField)));
    assert_eq!(
        Pieces::of(b"%D").next(),
        Some(Err(BadField)),
        "the format ends in a code"
    );
}

#[test]
fn fields_give_their_numbers_and_names() {
    let ordinals = Ordinals::of(at(2026, 3, 2, 0, 5));
    assert_eq!(
        number(Field::Hour12, &ordinals),
        Some((12, 2)),
        "midnight is twelve"
    );
    assert_eq!(number(Field::YearDay, &ordinals), Some((61, 3)));
    assert_eq!(digits(5, 2, false), (*b"05\0", 2));
    assert_eq!(digits(5, 3, true), (*b"5\0\0", 1));
    assert_eq!(digits(0, 2, true), (*b"0\0\0", 1), "the last digit always");
    let token = |field| name_token(field, &ordinals).map(|t| t.as_bytes().to_vec());
    assert_eq!(token(Field::MonthName), Some(b"MonthName03".to_vec()));
    assert_eq!(token(Field::MonthShort), Some(b"ShortMonth03".to_vec()));
    assert_eq!(token(Field::WeekdayShort), Some(b"ShortDay02".to_vec()));
    assert_eq!(token(Field::Suffix), Some(b"DaySuffixSecond".to_vec()));
    assert_eq!(token(Field::AmPm), Some(b"BeforeNoon".to_vec()));
    assert_eq!(token(Field::Hour24), None);
}

const MONTHS: [&[u8]; 12] = [
    b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
];

/// What a string reads as: all seven words, or refused.
fn words(text: &[u8], reason: u32) -> Result<[u32; 7], BadString> {
    let (written, read) = read(text, reason, &MONTHS);
    read.map(|()| written.map(|word| word.expect("every word written")))
}

#[test]
fn a_refused_time_leaves_the_words_written_before_it() {
    let none = Some(u32::MAX);
    assert_eq!(
        read(b"13:60:00", 1, &MONTHS),
        (
            [Some(0), None, None, Some(13), none, none, none],
            Err(BadString)
        ),
        "the date settled and the hour read"
    );
    assert_eq!(
        read(b"x", 1, &MONTHS),
        (
            [Some(0), None, None, None, none, none, none],
            Err(BadString)
        )
    );
    assert_eq!(
        read(b"32-Jan-2024", 2, &MONTHS),
        ([None; 7], Err(BadString)),
        "a date written only once read"
    );
}

#[test]
fn dates_and_times_are_read_as_risc_os_reads_them() {
    assert_eq!(
        words(b"Mon, 16-Nov-2026.09:30:05", 3),
        Ok([0, 5, 30, 9, 16, 11, 2026])
    );
    assert_eq!(
        words(b"  16 november 26", 2),
        Ok([u32::MAX, u32::MAX, u32::MAX, u32::MAX, 16, 11, 2026])
    );
    assert_eq!(
        words(b"12:00:61", 1),
        Ok([0, 61, 0, 12, u32::MAX, u32::MAX, u32::MAX])
    );
    assert_eq!(words(b"1-Jan-67", 2).map(|w| w[6]), Ok(1967));
    assert_eq!(
        words(b"1-Jan-66", 2).map(|w| w[6]),
        Ok(2066),
        "before 26 January"
    );
    assert_eq!(words(b"26-Jan-66", 2).map(|w| w[6]), Ok(1966));
    assert_eq!(words(b"29-Feb-2100", 2), Err(BadString));
    assert_eq!(words(b"29-Feb-2104", 2).map(|w| w[4]), Ok(29));
    assert_eq!(words(b"Ja", 2), Err(BadString));
    assert_eq!(words(b"1-Ju", 2), Err(BadString), "a name cut short");
    assert_eq!(words(b"24:00:00", 1), Err(BadString));
}
