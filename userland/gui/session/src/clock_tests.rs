//! Host tests for the icon bar clock's reading, spelling and tick: what an
//! unset clock shows, how a reading is written in a zone and a region's
//! conventions, when the lines next go stale, and that an idle desktop arms
//! no deadline it does not owe.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;

use tairix_abi::time::{Time64, WallClockReading, WallTimeState};
use tairix_taskbar::clock::ClockLines;
use tairix_tz::{CivilClock, Decided, ZoneInfo};
use tairix_wallpaper::{DesktopSettings, RegionCode, ZoneName};

use super::{moves_machine_zone, read_machine_zone, SessionClock, UNSET_LABEL};
use tairix_abi::WAITSET_TIMEOUT_NONE;
use tairix_wallpaper::civil_clock;

/// Nanoseconds in one second, for readable deadlines.
const SEC: u64 = 1_000_000_000;

/// A trusted reading at `secs` past the epoch, `nanos` into that second.
fn at(secs: i64, nanos: u32) -> WallClockReading {
    WallClockReading::new(
        Time64::new(secs, nanos).expect("a canonical instant"),
        WallTimeState::Trusted,
    )
}

/// A wall clock that answered once and then stops answering leaves the
/// label's deadline in the past; without a retry of its own the park it is
/// folded into would be nothing, and the loop would spin on it.
#[test]
fn a_failed_read_waits_a_minute_rather_than_spinning() {
    let mut clock = SessionClock::new(CivilClock::UTC);
    assert!(clock.adopt(at(1_709_214_367, 0), 0));
    let stale = 53 * SEC;
    assert!(clock.is_due(stale));
    assert_eq!(clock.park_deadline_ns(stale, WAITSET_TIMEOUT_NONE), 0);
    clock.missed(stale);
    assert!(!clock.is_due(stale), "not asked again at once");
    assert_eq!(
        clock.park_deadline_ns(stale, WAITSET_TIMEOUT_NONE),
        60 * SEC
    );
    assert!(clock.is_due(stale + 60 * SEC));
}

/// What a UTC clock spells `reading` as: the time, the date, the heading.
fn spelled(reading: WallClockReading) -> (String, String, String) {
    let mut clock = SessionClock::new(CivilClock::UTC);
    clock.adopt(reading, 0);
    (
        clock.lines().time.clone(),
        clock.lines().date.clone(),
        String::from(clock.heading()),
    )
}

#[test]
fn a_reading_with_no_zone_spells_utc_the_iso_way() {
    // 2024-02-29 13:46:07 UTC, spelled to the minute the bar shows.
    let at_leap = spelled(at(1_709_214_367, 0));
    assert_eq!(
        (at_leap.0.as_str(), at_leap.1.as_str(), at_leap.2.as_str()),
        ("13:46", "2024-02-29", "2024-02-29 13:46 UTC")
    );
    // Midnight pads both fields rather than collapsing to "0:0".
    assert_eq!(spelled(at(0, 0)).0, "00:00");
    assert_eq!(spelled(at(86_399, 0)).0, "23:59");
    // One second before the epoch is 23:59 of the day before.
    let before = spelled(at(-1, 0));
    assert_eq!(
        (before.0.as_str(), before.1.as_str()),
        ("23:59", "1969-12-31")
    );
}

/// A saver is told the bar's own spelling and the date written out, and when
/// the minute turns; an unset clock tells it nothing to show.
#[test]
fn a_saver_is_told_the_bars_reading_and_when_it_next_changes() {
    let mut clock = SessionClock::new(CivilClock::UTC);
    assert_eq!(
        clock.saver_reading().next_change_ns,
        0,
        "due before any reading"
    );
    clock.adopt(at(1_709_214_367, 0), 5 * SEC);
    let told = clock.saver_reading();
    assert_eq!(
        (
            told.time.as_str(),
            told.numeric_date.as_str(),
            told.long_date.as_str()
        ),
        ("13:46", "2024-02-29", "Thu 29 Feb 2024")
    );
    assert_eq!(told.next_change_ns, 5 * SEC + 53 * SEC);
    clock.missed(60 * SEC);
    assert_eq!(clock.saver_reading().next_change_ns, 120 * SEC, "the retry");

    clock.adopt(WallClockReading::UNSET, 0);
    let unset = clock.saver_reading();
    assert!(unset.time.is_empty() && unset.numeric_date.is_empty() && unset.long_date.is_empty());
}

/// The fixture store of real zones and regions, kept for the test.
fn zones() -> &'static ZoneInfo<'static> {
    let bytes: &'static [u8] = Box::leak(tairix_tz::testkit::store().into_boxed_slice());
    Box::leak(Box::new(
        ZoneInfo::parse(bytes).expect("the fixture parses"),
    ))
}

fn settings(zone: Option<&str>, region: Option<&str>) -> DesktopSettings {
    DesktopSettings {
        time_zone: zone.and_then(ZoneName::new),
        region: region.and_then(RegionCode::new),
        ..DesktopSettings::default()
    }
}

#[test]
fn a_reading_is_written_in_the_users_zone_and_region() {
    // 2026-10-18 13:05 UTC.
    let instant = at(1_792_328_700, 0);
    let (london, decided) = civil_clock(Some(zones()), &settings(None, None), None);
    assert_eq!(decided, (Decided::StoreDefault, Decided::StoreDefault));
    let mut clock = SessionClock::new(london);
    clock.adopt(instant, 0);
    assert_eq!(
        (
            clock.lines().time.as_str(),
            clock.lines().date.as_str(),
            clock.heading()
        ),
        ("14:05", "18/10/2026", "18/10/2026 14:05 BST"),
        "British summer time, the British way"
    );
    let (new_york, decided) = civil_clock(
        Some(zones()),
        &settings(Some("America/New_York"), Some("US")),
        None,
    );
    assert_eq!(decided, (Decided::Preference(0), Decided::Preference(0)));
    let mut clock = SessionClock::new(new_york);
    clock.adopt(instant, 0);
    assert_eq!(
        (
            clock.lines().time.as_str(),
            clock.lines().date.as_str(),
            clock.heading()
        ),
        ("9:05 AM", "10/18/2026", "10/18/2026 9:05 AM EDT")
    );
}

#[test]
fn the_next_tick_is_the_local_clocks_minute() {
    // Local mean time in 1800 is 75 seconds behind UT: 00:00:30 UT is 23:59:15
    // on the clock, 45 seconds short of its minute.
    let (london, _) = civil_clock(Some(zones()), &settings(None, None), None);
    let mut clock = SessionClock::new(london);
    clock.adopt(at(-5_364_662_370, 0), 0);
    assert_eq!(clock.park_deadline_ns(0, WAITSET_TIMEOUT_NONE), 45 * SEC);
}

#[test]
fn a_change_of_zone_is_due_at_once() {
    let mut clock = SessionClock::new(CivilClock::UTC);
    clock.adopt(at(1_709_214_367, 0), 0);
    assert!(!clock.is_due(SEC));
    let (london, _) = civil_clock(Some(zones()), &settings(None, None), None);
    clock.set_civil(london);
    assert!(clock.is_due(SEC), "not left until the minute turns");
    assert_eq!(clock.park_deadline_ns(SEC, WAITSET_TIMEOUT_NONE), 0);
}

#[test]
fn the_widest_lines_hold_every_field_at_its_most_digits_either_side_of_noon() {
    let utc = SessionClock::new(CivilClock::UTC);
    let lines = |time: &str, date: &str| ClockLines {
        time: String::from(time),
        date: String::from(date),
    };
    assert_eq!(
        utc.widest(),
        vec![lines("10:58", "2088-12-28"), lines("22:58", "2088-12-28")]
    );
    let (us, _) = civil_clock(
        Some(zones()),
        &settings(Some("America/New_York"), Some("US")),
        None,
    );
    assert_eq!(
        SessionClock::new(us).widest(),
        vec![
            lines("10:58 AM", "12/28/2088"),
            lines("10:58 PM", "12/28/2088")
        ]
    );
}

#[test]
fn an_unset_clock_shows_a_placeholder_rather_than_a_fabricated_time() {
    let unset = WallClockReading::new(Time64::UNIX_EPOCH, WallTimeState::Unset);
    let shown = spelled(unset);
    assert_eq!(
        (shown.0.as_str(), shown.1.as_str(), shown.2.as_str()),
        (UNSET_LABEL, "", UNSET_LABEL),
        "00:00 would be a time nobody set"
    );
    let mut clock = SessionClock::new(CivilClock::UTC);
    assert!(
        clock.adopt(unset, 0),
        "the placeholder owes the bar a repaint, or the clock is never drawn"
    );
    assert_eq!(clock.lines().time, UNSET_LABEL);
    assert!(
        !clock.adopt(unset, SEC),
        "still unset a second later is the same label"
    );
    // A firmware-seeded reading *is* set — plausible rather than verified,
    // but a real time — so it draws.
    let seeded = WallClockReading::new(Time64::from_secs(1_709_214_367), WallTimeState::Firmware);
    assert!(clock.adopt(seeded, 0));
    assert_eq!(clock.lines().time, "13:46");
}

#[test]
fn adopting_reports_only_a_label_that_actually_moved() {
    let mut clock = SessionClock::new(CivilClock::UTC);
    assert!(clock.adopt(at(1_709_214_367, 0), 0), "the first reading");
    // Another second in the same minute is the same label: no repaint.
    assert!(!clock.adopt(at(1_709_214_368, 0), SEC));
    // The next minute is a new label.
    assert!(clock.adopt(at(1_709_214_420, 0), 53 * SEC));
    assert_eq!(clock.lines().time, "13:47");
}

#[test]
fn the_tick_is_armed_at_the_next_minute_boundary_and_never_at_zero() {
    let mut clock = SessionClock::new(CivilClock::UTC);
    // 7 seconds into the minute: 53 to go.
    clock.adopt(at(1_709_214_367, 0), 0);
    assert_eq!(clock.park_deadline_ns(0, WAITSET_TIMEOUT_NONE), 53 * SEC);
    // Sub-second precision is spent too, so the wake lands on the boundary
    // rather than a fraction past it.
    clock.adopt(at(1_709_214_367, 250_000_000), 0);
    assert_eq!(
        clock.park_deadline_ns(0, WAITSET_TIMEOUT_NONE),
        53 * SEC - 250_000_000
    );
    // Exactly on the boundary waits a whole minute, never zero — a deadline
    // of zero would spin the park.
    clock.adopt(at(1_709_214_360, 0), 0);
    assert_eq!(clock.park_deadline_ns(0, WAITSET_TIMEOUT_NONE), 60 * SEC);
}

/// The tick is the zone's own: local mean time turns its minutes on its own
/// seconds, and only the end of the range, which has no later minute, waits a
/// whole one rather than arming a deadline of now.
#[test]
fn the_tick_follows_the_zones_own_minute() {
    let info = zones();
    let mut london = SessionClock::new(CivilClock::new(
        info.default_zone(),
        info.default_region().as_ref(),
    ));
    // 00:00:30 UT on 1 January 1800 reads 23:59:15 local mean time.
    let mean = tairix_tz::testkit::utc(1800, 1, 1, 0, 0) + 30;
    london.adopt(at(mean, 0), 0);
    assert_eq!(london.park_deadline_ns(0, WAITSET_TIMEOUT_NONE), 45 * SEC);
    let mut clock = SessionClock::new(CivilClock::UTC);
    clock.adopt(at(i64::MAX, 5), 0);
    assert_eq!(clock.park_deadline_ns(0, WAITSET_TIMEOUT_NONE), 60 * SEC);
}

#[test]
fn a_clock_that_has_read_nothing_arms_no_deadline() {
    // An idle desktop must not wake a core for a clock that has never been
    // read: the park is left exactly as it was.
    let clock = SessionClock::new(CivilClock::UTC);
    assert_eq!(
        clock.park_deadline_ns(0, WAITSET_TIMEOUT_NONE),
        WAITSET_TIMEOUT_NONE
    );
    assert_eq!(clock.park_deadline_ns(12_345, 7 * SEC), 7 * SEC);
}

#[test]
fn a_stale_label_shortens_the_park_to_nothing_because_the_tick_is_owed() {
    let mut clock = SessionClock::new(CivilClock::UTC);
    clock.adopt(at(1_709_214_367, 0), 0);
    // The deadline has passed while the loop was busy elsewhere: the tick is
    // still owed, so the next park does not wait for it a second time.
    assert_eq!(clock.park_deadline_ns(53 * SEC, WAITSET_TIMEOUT_NONE), 0);
    assert_eq!(clock.park_deadline_ns(600 * SEC, WAITSET_TIMEOUT_NONE), 0);
}

#[test]
fn the_clock_never_lengthens_a_park_another_surface_already_shortened() {
    // The fold only ever tightens: an animation due sooner keeps its own
    // deadline, exactly as `park_within` promises.
    let mut clock = SessionClock::new(CivilClock::UTC);
    clock.adopt(at(1_709_214_367, 0), 0);
    assert_eq!(clock.park_deadline_ns(0, SEC / 2), SEC / 2);
}

#[test]
fn a_clock_that_has_read_nothing_is_due() {
    // The first reading is owed unconditionally: there is no label yet, and
    // no deadline that could say when one falls due.
    assert!(SessionClock::new(CivilClock::UTC).is_due(0));
    assert!(SessionClock::new(CivilClock::UTC).is_due(u64::MAX));
}

/// The gate the run loop reads the wall clock behind, pinned against the
/// deadline it folds into the park.
///
/// Something wakes the desktop every couple of seconds whatever the clock is
/// doing, so `is_due` is what keeps a minute-granular label from costing a
/// syscall on each of those wakes. The two views must agree exactly: a wake
/// the park was *not* shortened for has nothing to ask the clock about, and
/// one it *was* shortened for is owed a reading.
#[test]
fn the_read_is_due_exactly_when_the_park_it_armed_has_elapsed() {
    let mut clock = SessionClock::new(CivilClock::UTC);
    // 53 seconds short of the next minute boundary.
    clock.adopt(at(1_709_214_367, 0), 0);

    for now in [0, SEC, 52 * SEC, 53 * SEC - 1] {
        assert!(!clock.is_due(now), "read early at {now}");
        assert!(
            clock.park_deadline_ns(now, WAITSET_TIMEOUT_NONE) > 0,
            "parked for nothing at {now}"
        );
    }
    for now in [53 * SEC, 53 * SEC + 1, 600 * SEC] {
        assert!(clock.is_due(now), "read owed but not due at {now}");
        assert_eq!(
            clock.park_deadline_ns(now, WAITSET_TIMEOUT_NONE),
            0,
            "owed a tick but still parking at {now}"
        );
    }
}

#[test]
fn adopting_a_reading_clears_the_debt_until_the_next_minute() {
    let mut clock = SessionClock::new(CivilClock::UTC);
    clock.adopt(at(1_709_214_367, 0), 0);
    assert!(clock.is_due(53 * SEC));
    // The tick is paid at the boundary, which arms the following whole minute.
    clock.adopt(at(1_709_214_420, 0), 53 * SEC);
    assert!(!clock.is_due(53 * SEC));
    assert!(!clock.is_due(112 * SEC));
    assert!(clock.is_due(113 * SEC));
}

/// The machine's zone is the machine configuration's `time.zone` as it
/// stands: the administrator's layer, else the image's, else the default; a
/// configuration that cannot be read or understood is stated, never guessed at.
#[test]
fn the_machine_zone_is_the_configuration_s_and_a_refusal_is_said_in_words() {
    use crate::tests::MemoryAssets;
    use tairix_abi::driver_store::SystemConfigFile;

    let [administrator, image] = SystemConfigFile::System.layers();
    let mut named = MemoryAssets::default().with(administrator, b"time.zone Europe/Paris\n");
    assert_eq!(
        read_machine_zone(&mut named),
        Ok(ZoneName::new("Europe/Paris"))
    );
    let mut shipped = MemoryAssets::default().with(image, b"time.zone Asia/Tokyo\n");
    assert_eq!(
        read_machine_zone(&mut shipped),
        Ok(ZoneName::new("Asia/Tokyo"))
    );
    let mut replaced = MemoryAssets::default()
        .with(image, b"time.zone Asia/Tokyo\n")
        .with(administrator, b"time.zone default\n");
    assert_eq!(read_machine_zone(&mut replaced), Ok(None));
    assert_eq!(read_machine_zone(&mut MemoryAssets::default()), Ok(None));
    let mut malformed = MemoryAssets::default().with(administrator, b"time.zone Paris France\n");
    assert!(read_machine_zone(&mut malformed).is_err_and(|said| said.contains("not understood")));
}

/// A drained batch moves the machine zone when it names the configuration
/// or cannot say what changed, and says when more changes wait.
#[test]
fn a_change_to_the_configuration_directory_moves_the_zone_only_when_it_names_the_file() {
    use tairix_abi::fs::{DirChange, DirChangeBatch, DirEntry, DirWatchStatus, FileKind};

    let batch = |status, more, names: &[&[u8]]| {
        let mut out = vec![0u8; 512];
        let mut at = DirChangeBatch::HEADER_LEN;
        for name in names {
            at += DirChange::Absent(name)
                .encode_into(&mut out[at..])
                .expect("room");
        }
        let count = u32::try_from(names.len()).expect("few");
        DirChangeBatch::encode_header(&mut out, status, more, count).expect("room");
        out.truncate(at);
        out
    };
    let (dir, file) = super::machine_zone_place();
    assert_eq!(dir, "/System/Settings/Configuration");
    let file = file.as_bytes();
    assert_eq!(
        moves_machine_zone(&batch(
            DirWatchStatus::Changes,
            false,
            &[b"system.conf.0123456789abcdef.new", file]
        )),
        (true, false)
    );
    assert_eq!(
        moves_machine_zone(&batch(
            DirWatchStatus::Changes,
            true,
            &[b"system.conf.0123456789abcdef.new"]
        )),
        (false, true)
    );
    assert_eq!(
        moves_machine_zone(&batch(DirWatchStatus::Rescan, false, &[])),
        (true, false)
    );
    assert_eq!(
        moves_machine_zone(b"\xff"),
        (true, false),
        "an unreadable batch"
    );
    let present = {
        let mut out = vec![0u8; 512];
        let entry = DirEntry {
            kind: FileKind::Regular,
            size: 20,
            allocated: 4096,
            modified: Time64::UNIX_EPOCH,
            id: tairix_abi::fs::FileId::NONE,
            nlink: 1,
            content_gen: 0,
            name: file,
        };
        let at = DirChangeBatch::HEADER_LEN
            + DirChange::Present(entry)
                .encode_into(&mut out[DirChangeBatch::HEADER_LEN..])
                .expect("room");
        DirChangeBatch::encode_header(&mut out, DirWatchStatus::Changes, false, 1).expect("room");
        out.truncate(at);
        out
    };
    assert_eq!(moves_machine_zone(&present), (true, false));
}
