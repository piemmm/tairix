//! ISO 9660 dates: a directory record's seven bytes (ECMA-119 9.1.5), High
//! Sierra's six, and the seventeen-character form descriptors and Rock
//! Ridge's long `TF` records use (ECMA-119 8.4.26.1).

use tairix_abi::time::{days_from_civil, Time64, SECS_PER_DAY, SECS_PER_HOUR, SECS_PER_MINUTE};

/// Seconds in one step of the offset from UTC a date records.
const OFFSET_STEP_SECS: i64 = 15 * SECS_PER_MINUTE;

/// The instant a civil local date and time recorded `offset` 15-minute steps
/// east of UTC names, `None` for fields outside their ranges.
fn instant(
    year: i64,
    month: u32,
    day: u32,
    (hour, minute, second): (u32, u32, u32),
    nanos: u32,
    offset: i8,
) -> Option<Time64> {
    let valid = (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && hour < 24
        && minute < 60
        && second < 60
        && (-48..=52).contains(&offset);
    if !valid {
        return None;
    }
    let local = days_from_civil(year, month, day)?
        .checked_mul(SECS_PER_DAY)?
        .checked_add(i64::from(hour) * SECS_PER_HOUR + i64::from(minute) * SECS_PER_MINUTE)?
        .checked_add(i64::from(second))?;
    let utc = local.checked_sub(i64::from(offset) * OFFSET_STEP_SECS)?;
    Time64::new(utc, nanos).ok()
}

/// A directory record's recording date: years since 1900, month, day,
/// hour, minute, second, and the offset from UTC in 15-minute steps. All
/// zero records no date.
pub(crate) fn record_date(raw: [u8; 7]) -> Option<Time64> {
    if raw == [0; 7] {
        return None;
    }
    let [years, month, day, hour, minute, second, offset] = raw;
    instant(
        1900 + i64::from(years),
        u32::from(month),
        u32::from(day),
        (u32::from(hour), u32::from(minute), u32::from(second)),
        0,
        i8::from_le_bytes([offset]),
    )
}

/// High Sierra's six-byte date, which records no offset from UTC.
pub(crate) fn high_sierra_date(raw: [u8; 6]) -> Option<Time64> {
    let mut iso = [0u8; 7];
    iso[..6].copy_from_slice(&raw);
    record_date(iso)
}

/// A seventeen-byte date: sixteen ASCII digits `YYYYMMDDHHMMSScc` and the
/// offset from UTC. All digits zero records no date.
pub(crate) fn long_date(raw: &[u8; 17]) -> Option<Time64> {
    let digits = &raw[..16];
    if digits.iter().all(|&b| b == b'0' || b == 0) {
        return None;
    }
    let number = |range: core::ops::Range<usize>| -> Option<u32> {
        digits[range].iter().try_fold(0u32, |value, &b| {
            b.is_ascii_digit().then(|| value * 10 + u32::from(b - b'0'))
        })
    };
    let hundredths = number(14..16)?;
    instant(
        i64::from(number(0..4)?),
        number(4..6)?,
        number(6..8)?,
        (number(8..10)?, number(10..12)?, number(12..14)?),
        hundredths * 10_000_000,
        i8::from_le_bytes([raw[16]]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_date_is_shifted_to_utc_by_its_offset() {
        // 2021-03-04 05:06:07, one hour east of UTC.
        let east = record_date([121, 3, 4, 5, 6, 7, 4]).expect("valid");
        let utc = record_date([121, 3, 4, 4, 6, 7, 0]).expect("valid");
        assert_eq!(east, utc);
        assert_eq!(utc.secs(), 1_614_830_767);
    }

    #[test]
    fn dates_before_1970_and_after_2038_are_kept() {
        assert_eq!(
            record_date([0, 1, 1, 0, 0, 0, 0]).map(|t| t.secs()),
            Some(-2_208_988_800)
        );
        let late = record_date([255, 12, 31, 23, 59, 59, 0]).expect("valid");
        assert!(late.secs() > i64::from(i32::MAX), "2155 is past 2038");
        let long = long_date(b"2100010100000000\0").expect("valid");
        assert_eq!(long.secs(), 4_102_444_800);
    }

    #[test]
    fn a_long_date_keeps_its_hundredths() {
        let t = long_date(b"1999123123595999\x00").expect("valid");
        assert_eq!(t.subsec_nanos(), 990_000_000);
    }

    #[test]
    fn no_date_and_impossible_dates_are_none() {
        assert_eq!(record_date([0; 7]), None);
        assert_eq!(long_date(b"0000000000000000\0"), None);
        assert_eq!(record_date([121, 13, 1, 0, 0, 0, 0]), None);
        assert_eq!(record_date([121, 1, 1, 24, 0, 0, 0]), None);
        assert_eq!(
            record_date([121, 1, 1, 0, 0, 0, 60]),
            None,
            "offset east of +13h"
        );
        assert_eq!(long_date(b"2021AB0100000000\0"), None);
    }

    #[test]
    fn high_sierra_dates_have_no_offset() {
        assert_eq!(
            high_sierra_date([90, 6, 1, 12, 0, 0]),
            record_date([90, 6, 1, 12, 0, 0, 0])
        );
    }
}
