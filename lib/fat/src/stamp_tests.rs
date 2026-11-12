use super::*;
use tairix_abi::{StampChange, TimesKept};

#[test]
fn a_stamp_keeps_the_odd_second_and_hundredths_within_its_two() {
    let time = Time64::new(1_000_000_001, 370_000_000).unwrap();
    let stamp = Stamp::of(time).unwrap();
    assert_eq!(stamp.centis, 137);
    assert_eq!(stamp.instant(), time);
    assert_eq!(
        Stamp::of(Time64::new(315_532_799, 0).unwrap()),
        None,
        "before 1980"
    );
    assert_eq!(Stamp::NONE.instant(), Time64::UNIX_EPOCH);
    assert_eq!(
        Stamp {
            centis: 200,
            ..stamp
        }
        .instant(),
        Time64::UNIX_EPOCH
    );
}

#[test]
fn new_stamps_are_all_now_and_read_back_to_their_precision() {
    let now = Time64::new(1_000_000_001, 370_000_000).unwrap();
    let stamps = Stamps::at(Some(now));
    assert_eq!(stamps.created(), now);
    assert_eq!(
        stamps.written(),
        Time64::new(1_000_000_000, 0).unwrap(),
        "two seconds"
    );
    assert_eq!(stamps.accessed().secs() % 86_400, 0, "the day alone");
    assert_eq!(Stamps::at(None), Stamps::NONE);
    assert_eq!(Stamps::NONE.created(), Time64::UNIX_EPOCH);
}

/// A date word from its fields.
fn date(year: u16, month: u16, day: u16) -> u16 {
    ((year - 1980) << 9) | (month << 5) | day
}

/// A time word from its fields.
fn time(hour: u16, minute: u16, second: u16) -> u16 {
    (hour << 11) | (minute << 5) | (second / 2)
}

fn at(date: u16, time: u16) -> Time64 {
    Stamp {
        date,
        time,
        centis: 0,
    }
    .instant()
}

#[test]
fn the_fields_span_1980_to_2107_beyond_2038_and_name_only_real_days() {
    assert_eq!(
        at(date(1980, 1, 1), time(0, 0, 0)),
        Time64::from_secs(315_532_800)
    );
    assert_eq!(
        at(date(2020, 2, 29), time(0, 0, 0)),
        Time64::from_secs(1_582_934_400)
    );
    assert_eq!(
        at(date(2107, 12, 31), time(23, 59, 58)),
        Time64::from_secs(4_354_819_198)
    );
    let noon = time(12, 0, 0);
    for no_day in [
        (44 << 9) | 15,
        (44 << 9) | (13 << 5) | 15,
        date(2024, 6, 0),
        date(2023, 2, 29),
    ] {
        assert_eq!(at(no_day, noon), Time64::UNIX_EPOCH, "{no_day:#06X}");
    }
    assert_eq!(
        at(date(2024, 6, 15), 24 << 11),
        Time64::UNIX_EPOCH,
        "hour 24"
    );
}

#[test]
fn a_stamp_reaches_the_last_second_of_2107_and_no_further() {
    let civil = |year, month, day, hour, minute, second| {
        tairix_abi::time::CivilTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
        .to_time64()
        .unwrap()
    };
    assert!(Stamp::of(civil(1980, 1, 1, 0, 0, 0)).is_some());
    assert!(Stamp::of(civil(2107, 12, 31, 23, 59, 59)).is_some());
    assert_eq!(Stamp::of(civil(2108, 1, 1, 0, 0, 0)), None);
    assert_eq!(Stamp::of(civil(1979, 12, 31, 23, 59, 59)), None);
}

#[test]
fn hundredths_count_on_through_the_odd_second_and_no_further() {
    let base = at(date(2024, 6, 15), time(12, 0, 0));
    let with = |centis| {
        Stamp {
            date: date(2024, 6, 15),
            time: time(12, 0, 0),
            centis,
        }
        .instant()
    };
    assert_eq!(
        with(199),
        Time64::new(base.secs() + 1, 990_000_000).unwrap()
    );
    assert_eq!(with(200), Time64::UNIX_EPOCH);
}

#[test]
fn a_stamp_drops_what_is_finer_than_a_hundredth() {
    let stamp = Stamp::of(Time64::new(1_792_067_697, 123_456_789).unwrap()).unwrap();
    assert_eq!(
        (stamp.date, stamp.time, stamp.centis),
        (date(2026, 10, 15), time(12, 34, 56), 112)
    );
    assert_eq!(
        stamp.instant(),
        Time64::new(1_792_067_697, 120_000_000).unwrap()
    );
}

/// Every instant the declared fields call exact is one the entry keeps to the
/// nanosecond, the ends of their range included, and a stamp changed in one
/// field leaves the other.
#[test]
fn the_declared_fields_hold_exactly_what_an_entry_keeps() {
    let stamps = Stamps::at(Some(Time64::from_secs(1_000_000_000)));
    for written in [315_532_800, 1_000_000_002, 4_354_819_198] {
        let at = Time64::from_secs(written);
        assert_eq!(WRITTEN.fit(at, false), Ok((at, TimesKept::Exactly)));
        let set = stamps.changed(StampChange::Keep, StampChange::At(at), None);
        assert_eq!(set.written(), at);
    }
    for day in [315_532_800, 1_000_080_000, 4_354_732_800] {
        let at = Time64::from_secs(day);
        assert_eq!(ACCESSED.fit(at, false), Ok((at, TimesKept::Exactly)));
        let set = stamps.changed(StampChange::At(at), StampChange::Keep, None);
        assert_eq!(set.accessed(), at);
    }
    let first = StampChange::At(Time64::from_secs(315_532_800));
    let changed = stamps.changed(first, StampChange::Keep, None);
    assert_eq!(changed.written(), stamps.written());
    assert_eq!(changed.created(), stamps.created());
    let unset = stamps.changed(StampChange::Now, StampChange::Now, None);
    assert_eq!(
        (unset.accessed(), unset.written()),
        (Time64::UNIX_EPOCH, Time64::UNIX_EPOCH),
        "a now with no clock records no stamp"
    );
    assert_eq!(
        WRITTEN.fit(Time64::from_secs(4_354_819_199), false),
        Err(tairix_abi::DriverError::OutOfRange),
        "past the last two seconds"
    );
}
