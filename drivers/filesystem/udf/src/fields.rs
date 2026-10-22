//! Little-endian fields of an on-disc descriptor, read by offset: a field
//! past the descriptor's end is damage, never a panic.

use tairix_abi::time::{days_from_civil, Time64, SECS_PER_DAY, SECS_PER_HOUR, SECS_PER_MINUTE};
use tairix_abi::DriverError;
use tairix_fsprobe::udf::LongAd;

/// A descriptor's bytes.
#[derive(Copy, Clone)]
pub(crate) struct Fields<'a>(pub &'a [u8]);

impl<'a> Fields<'a> {
    pub(crate) fn bytes(self, at: usize, len: usize) -> Result<&'a [u8], DriverError> {
        self.0
            .get(at..at.checked_add(len).ok_or(DriverError::DeviceFault)?)
            .ok_or(DriverError::DeviceFault)
    }

    fn array<const N: usize>(self, at: usize) -> Result<[u8; N], DriverError> {
        self.bytes(at, N)?
            .try_into()
            .map_err(|_| DriverError::DeviceFault)
    }

    pub(crate) fn u8(self, at: usize) -> Result<u8, DriverError> {
        self.0.get(at).copied().ok_or(DriverError::DeviceFault)
    }

    pub(crate) fn u16(self, at: usize) -> Result<u16, DriverError> {
        self.array(at).map(u16::from_le_bytes)
    }

    pub(crate) fn u32(self, at: usize) -> Result<u32, DriverError> {
        self.array(at).map(u32::from_le_bytes)
    }

    pub(crate) fn u64(self, at: usize) -> Result<u64, DriverError> {
        self.array(at).map(u64::from_le_bytes)
    }

    pub(crate) fn long_ad(self, at: usize) -> Result<LongAd, DriverError> {
        LongAd::at(self.0, at).ok_or(DriverError::DeviceFault)
    }

    /// A timestamp (ECMA-167 1/7.3), `None` for one recording no instant.
    pub(crate) fn timestamp(self, at: usize) -> Result<Option<Time64>, DriverError> {
        self.array(at).map(timestamp)
    }
}

/// Bytes of a timestamp.
pub(crate) const TIMESTAMP_LEN: usize = 12;

/// The timezone field's "no offset recorded".
const NO_TIMEZONE: i16 = -2047;

/// The instant a timestamp records: a type and signed offset from UTC in
/// minutes, year, month, day, hour, minute, second, then centiseconds,
/// hundreds of microseconds and microseconds. A local time without an offset
/// is read as UTC; an all-zero stamp, or a field out of its range, records
/// none.
pub(crate) fn timestamp(raw: [u8; TIMESTAMP_LEN]) -> Option<Time64> {
    if raw == [0; TIMESTAMP_LEN] {
        return None;
    }
    let type_and_zone = u16::from_le_bytes([raw[0], raw[1]]);
    // Twelve-bit two's complement minutes.
    let zone = i16::from_le_bytes((type_and_zone << 4).to_le_bytes()) >> 4;
    let offset = if zone == NO_TIMEZONE || !(-1440..=1440).contains(&zone) {
        0
    } else {
        i64::from(zone)
    };
    let year = i64::from(i16::from_le_bytes([raw[2], raw[3]]));
    let [month, day, hour, minute, second, centi, hundred_micros, micros] = [
        raw[4], raw[5], raw[6], raw[7], raw[8], raw[9], raw[10], raw[11],
    ];
    let valid = (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && hour < 24
        && minute < 60
        && second < 60
        && centi < 100
        && hundred_micros < 100
        && micros < 100;
    if !valid {
        return None;
    }
    let local = days_from_civil(year, u32::from(month), u32::from(day))?
        .checked_mul(SECS_PER_DAY)?
        .checked_add(i64::from(hour) * SECS_PER_HOUR + i64::from(minute) * SECS_PER_MINUTE)?
        .checked_add(i64::from(second))?;
    let utc = local.checked_sub(offset * SECS_PER_MINUTE)?;
    let nanos = u32::from(centi) * 10_000_000
        + u32::from(hundred_micros) * 100_000
        + u32::from(micros) * 1_000;
    Time64::new(utc, nanos).ok()
}

/// `time` as a timestamp: local time at UTC, to the microsecond. `None` for
/// an instant outside the years a timestamp can hold.
pub(crate) fn encode_timestamp(time: Time64) -> Option<[u8; TIMESTAMP_LEN]> {
    const LOCAL_TIME: u16 = 1 << 12;
    let days = time.secs().div_euclid(SECS_PER_DAY);
    let within = time.secs().rem_euclid(SECS_PER_DAY);
    let (year, month, day) = tairix_abi::time::civil_from_days(days);
    let year = i16::try_from(year)
        .ok()
        .filter(|year| (1..=9999).contains(year))?;
    let nanos = time.subsec_nanos();
    let small = |value: i64| u8::try_from(value).unwrap_or(0);
    let mut raw = [0u8; TIMESTAMP_LEN];
    raw[..2].copy_from_slice(&LOCAL_TIME.to_le_bytes());
    raw[2..4].copy_from_slice(&year.to_le_bytes());
    raw[4] = small(i64::from(month));
    raw[5] = small(i64::from(day));
    raw[6] = small(within / SECS_PER_HOUR);
    raw[7] = small(within % SECS_PER_HOUR / SECS_PER_MINUTE);
    raw[8] = small(within % SECS_PER_MINUTE);
    raw[9] = small(i64::from(nanos / 10_000_000));
    raw[10] = small(i64::from(nanos / 100_000 % 100));
    raw[11] = small(i64::from(nanos / 1_000 % 100));
    Some(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timestamp_round_trips_to_the_microsecond() {
        for (secs, nanos) in [
            (0, 0),
            (1_623_760_200, 123_456_000),
            (-2_208_988_800, 990_000_000),
            (4_102_444_800, 1_000),
        ] {
            let time = Time64::new(secs, nanos).expect("valid");
            let raw = encode_timestamp(time).expect("in range");
            assert_eq!(timestamp(raw), Some(time));
        }
    }

    #[test]
    fn a_timestamp_is_shifted_by_its_offset_and_none_without_a_date() {
        let mut east = encode_timestamp(Time64::from_secs(1_000_000_000)).expect("range");
        // One hour east of UTC: the same clock reading is an hour earlier.
        let zone: u16 = (1 << 12) | 0x3C;
        east[..2].copy_from_slice(&zone.to_le_bytes());
        assert_eq!(
            timestamp(east).map(|t| t.secs()),
            Some(1_000_000_000 - 3600)
        );
        let mut west = east;
        let zone: u16 = (1 << 12) | (0x1000 - 90);
        west[..2].copy_from_slice(&zone.to_le_bytes());
        assert_eq!(
            timestamp(west).map(|t| t.secs()),
            Some(1_000_000_000 + 5400)
        );
        assert_eq!(timestamp([0; TIMESTAMP_LEN]), None);
        let mut bad = east;
        bad[4] = 13;
        assert_eq!(timestamp(bad), None);
    }
}
