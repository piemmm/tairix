//! The Territory Manager's tests: time zones from the TAIRiX locale, the
//! commands and keywords, the set-up of the current territory, and a
//! territory of the guest's own.

extern crate std;

use alloc::vec::Vec;
use std::sync::OnceLock;

use tairix_riscos_arm::psr;
use tairix_tz::{Zone, ZoneInfo};

use super::formats::{ADJUSTMENT, AS_SUMMER, BY_THE_RULES, STANDARD_UNRULED};
use super::{calendar, CHUNK, CONVERT_TIME_FORMATS, DAYLIGHT_RULES, DAYLIGHT_SAVING, HOUR, SELECT};
use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::msgtrans::tests::swi;
use crate::os::MILLENNIUM;
use crate::service::{POST_INIT, TERRITORY_STARTED, TIME_ZONE_CHANGED};
use crate::swi::os::{BYTE, SERVICE_CALL};
use crate::swi::X;
use crate::testkit::{asm, boot_in, Session, CODE, DATA};

const BUFFER: u32 = DATA + 0x100;
const TIME: u32 = DATA + 0x80;

/// The Territory SWIs, the time formats' flags and the hour the manager
/// names are RISC OS's.
#[test]
fn the_numbers_the_manager_names_are_risc_os_s() {
    assert_eq!(
        [
            DAYLIGHT_RULES,
            SELECT,
            DAYLIGHT_SAVING,
            CONVERT_TIME_FORMATS
        ],
        [0x23, 0x36, 0x37, 0x38]
    );
    assert_eq!(
        [AS_SUMMER, STANDARD_UNRULED, BY_THE_RULES, ADJUSTMENT],
        [1 << 16, 1 << 17, 1 << 18, 1 << 19]
    );
    assert_eq!(HOUR, 360_000, "an hour in centiseconds");
}

/// `name` from the shipped zone store.
fn zone(name: &str) -> Zone<'static> {
    static STORE: OnceLock<Vec<u8>> = OnceLock::new();
    static INFO: OnceLock<ZoneInfo<'static>> = OnceLock::new();
    let bytes = STORE.get_or_init(|| tairix_tzcompile::compile().expect("the store compiles"));
    let info = INFO.get_or_init(|| ZoneInfo::parse(bytes).expect("the store"));
    info.zone(name).expect("in the store")
}

/// A session in `name` at a UTC date and time.
fn session_in(name: &str, (year, month, day, hour, minute): (u32, u32, u32, u32, u32)) -> Session {
    let time = calendar::time_of([0, 0, minute, hour, day, month, year]).expect("a time");
    let mut s = boot_in(&RISC_OS_5, zone(name), (time - MILLENNIUM) / 100);
    s.application(Bits::Bit32, 0x1_0000);
    s.capture_output();
    s
}

fn territory(
    s: &mut Session,
    offset: u32,
    registers: [u32; 13],
) -> Result<crate::os::Regs, (u32, Vec<u8>)> {
    swi(s, CHUNK + offset, registers)
}

/// The time zone in force: its name and offset.
fn current_zone(s: &mut Session) -> (Vec<u8>, i32) {
    let regs = territory(s, 8, [0; 13]).expect("read");
    let name = s.kernel.os().read_zstring(regs.r[0], 16).expect("a name");
    (name, regs.r[1].cast_signed())
}

/// Whether CMOS has daylight saving in force.
fn saving(s: &mut Session) -> bool {
    s.kernel.os().world.cmos.read(crate::cmos::ALARM_AND_TIME) & crate::cmos::DST != 0
}

fn time_zone_cmos(s: &mut Session) -> i8 {
    s.kernel
        .os()
        .world
        .cmos
        .read(crate::cmos::TIME_ZONE)
        .cast_signed()
}

/// The five-byte time of a UTC date and time.
fn utc(year: u32, month: u32, day: u32, hour: u32, minute: u32) -> u64 {
    calendar::time_of([0, 0, minute, hour, day, month, year]).expect("a time")
}

fn byte(s: &mut Session, r0: u32, r1: u32) -> u32 {
    s.user_swi(X | BYTE, [r0, r1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .0
        .r[1]
}

#[test]
fn the_locale_s_summer_time_is_in_force_from_the_start() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    assert_eq!(time_zone_cmos(&mut s), 0, "the locale's standard time");
    assert!(saving(&mut s));
    assert_eq!(current_zone(&mut s), (b"BST".to_vec(), 360_000));
    let zones =
        territory(&mut s, 0x0A, [u32::MAX, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("zones");
    let mut os = s.kernel.os();
    assert_eq!(os.read_zstring(zones.r[0], 16).expect("standard"), b"GMT");
    assert_eq!(os.read_zstring(zones.r[1], 16).expect("summer"), b"BST");
    assert_eq!((zones.r[2], zones.r[3]), (0, 360_000));
    let mut s = session_in("Europe/London", (2026, 1, 15, 12, 0));
    assert!(!saving(&mut s), "winter");
    assert_eq!(current_zone(&mut s), (b"GMT".to_vec(), 0));
}

#[test]
fn new_york_dublin_and_kolkata_keep_their_own_time() {
    let mut s = session_in("America/New_York", (2026, 1, 15, 12, 0));
    assert_eq!(time_zone_cmos(&mut s), -20);
    assert_eq!(current_zone(&mut s), (b"EST".to_vec(), -1_800_000));
    let mut s = session_in("Europe/Dublin", (2026, 7, 1, 12, 0));
    assert_eq!(
        current_zone(&mut s),
        (b"IST".to_vec(), 360_000),
        "the summer reading is the one further east"
    );
    assert_eq!(time_zone_cmos(&mut s), 0);
    let mut s = session_in("Asia/Kolkata", (2026, 7, 1, 12, 0));
    assert_eq!(time_zone_cmos(&mut s), 22, "five and a half hours");
    assert_eq!(current_zone(&mut s), (b"IST".to_vec(), 1_980_000));
    assert!(!saving(&mut s), "no summer time");
}

#[test]
fn local_ordinals_convert_at_the_zone_s_offset() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    s.kernel
        .os()
        .write_five(TIME, utc(2026, 7, 1, 12, 0))
        .expect("written");
    territory(
        &mut s,
        0x0F,
        [u32::MAX, TIME, BUFFER, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("converted");
    assert_eq!(s.peek(BUFFER + 12), 13, "local in summer is an hour on");
    s.kernel.os().write_five(TIME, 0).expect("cleared");
    territory(
        &mut s,
        0x11,
        [u32::MAX, TIME, BUFFER, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("converted back");
    assert_eq!(
        s.kernel.os().read_five(TIME).expect("five"),
        utc(2026, 7, 1, 12, 0)
    );
    s.poke(BUFFER + 24, &[1066]);
    let refused = territory(
        &mut s,
        0x11,
        [u32::MAX, TIME, BUFFER, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(
        refused.expect_err("a year out of range"),
        (0x195, b"Time values out of range".to_vec())
    );
}

#[test]
fn configuring_the_time_zone_and_daylight_saving() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    let run = |s: &mut Session, line: &str| crate::cli::tests::cli(s, line).expect(line);
    run(&mut s, "Configure TimeZone -5:30");
    assert_eq!(time_zone_cmos(&mut s), -22);
    let _ = s.output();
    run(&mut s, "Status TimeZone");
    assert_eq!(s.output(), b"TimeZone   -5:30\n\r");
    assert_eq!(
        current_zone(&mut s),
        (b"Local".to_vec(), -1_620_000),
        "no zone of the territory's, an hour of summer time on"
    );
    run(&mut s, "Configure GMT");
    assert!(!saving(&mut s), "the standard time's name as NoDST");
    assert_eq!(time_zone_cmos(&mut s), -22, "the time zone left");
    run(&mut s, "Configure BST");
    assert!(saving(&mut s), "the summer time's as DST");
    let _ = s.output();
    run(&mut s, "Status DST");
    assert_eq!(s.output(), b"DST\n\r");
    run(&mut s, "Configure NoDST");
    assert!(!saving(&mut s));
    let _ = s.output();
    run(&mut s, "Status DST");
    assert_eq!(s.output(), b"NoDST\n\r");
    run(&mut s, "Configure DST");
    assert!(saving(&mut s));
    run(&mut s, "Configure AutoDST");
    let _ = s.output();
    run(&mut s, "Status DST");
    assert_eq!(s.output(), b"AutoDST\n\r");
    run(&mut s, "Configure DST");
    run(&mut s, "Configure Territory germany");
    assert_eq!(s.kernel.os().world.cmos.read(crate::cmos::TERRITORY), 7 ^ 1);
    let _ = s.output();
    run(&mut s, "Status Territory");
    assert_eq!(s.output(), b"Territory  7\n\r");
}

#[test]
fn the_territories_command_lists_those_loaded() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    let _ = s.output();
    assert_eq!(
        crate::cli::tests::written(&mut s, "Territories"),
        b"1 UK\n\r"
    );
}

#[test]
fn selecting_the_alphabet_defines_its_characters_and_leaves_the_number() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    assert_eq!(
        byte(&mut s, 71, 255),
        1,
        "the UK keyboard set up at the start"
    );
    byte(&mut s, 71, 102);
    let _ = s.output();
    territory(&mut s, 6, [u32::MAX, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("selected");
    let pound = tairix_vdu::font::glyph_of('\u{A3}').expect("in the font");
    assert!(
        crate::international::tests::defined(&s.output()).contains(&(0xA3, pound)),
        "Latin1's characters defined"
    );
    assert_eq!(byte(&mut s, 71, 127), 102, "the alphabet's number left");
}

#[test]
fn the_reserved_swis_answer_as_risc_os_5_s_do() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    let reserved = territory(&mut s, 0x24, [u32::MAX, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        reserved.expect_err("not defined"),
        (0x1E8, b"Unknown Territory SWI".to_vec())
    );
    let last = territory(&mut s, 0x34, [u32::MAX, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(last.expect_err("not defined").0, 0x1E8);
    let registers = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 0, 0, 0];
    let text = territory(&mut s, 0x35, registers).expect("does nothing");
    assert_eq!(&text.r[..10], &registers[..10]);
    let beyond = territory(&mut s, 0x39, [0; 13]);
    assert_eq!(beyond.expect_err("no such SWI").0, 0x1E8);
}

#[test]
fn a_territory_of_the_guest_s_own_answers_through_its_entries() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    // Every entry answers R0 = its index and R9 = R12 plus R8.
    s.poke(
        CODE,
        &[
            asm::mov_reg(0, 11),
            asm::add_reg(9, 12, 8),
            asm::mov_pc_lr(),
        ],
    );
    let table = DATA + 0x800;
    s.poke(table, &[CODE; super::ENTRIES]);
    territory(&mut s, 1, [7, table, 0x1234, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("registered");
    let (exists, _) = s.user_swi(X | (CHUNK + 4), [7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(exists.flag(psr::Z));
    let alphabet =
        territory(&mut s, 0x12, [7, 0, 0, 0, 0, 0, 0, 0, 0x10, 0, 0, 0, 0]).expect("asked");
    assert_eq!(alphabet.r[0], 8, "the entry's index in R11");
    assert_eq!(alphabet.r[9], 0x1244, "R12 as registered, R8 as passed");
    let last = territory(&mut s, 0x34, [7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("asked");
    assert_eq!(last.r[0], 42, "all 43 entries");
    let _ = s.output();
    assert_eq!(
        crate::cli::tests::written(&mut s, "Territories"),
        b"7 Germany\n\r1 UK\n\r"
    );
    territory(&mut s, 2, [7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("deregistered");
    let gone = territory(&mut s, 0x12, [7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        gone.expect_err("not there"),
        (0x192, b"That territory is not loaded".to_vec())
    );
}

#[test]
fn a_registration_stands_when_its_territory_cannot_be_set_up() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    // Every entry fails, the keyboard handler's the set-up calls first.
    let error = DATA + 0x900;
    s.poke(error, &[0x99, u32::from_le_bytes(*b"No\0\0")]);
    s.poke(
        CODE,
        &[
            asm::ldr(0, 15, 4),
            asm::set_flags(psr::V),
            asm::mov_pc_lr(),
            error,
        ],
    );
    let table = DATA + 0x800;
    s.poke(table, &[CODE; super::ENTRIES]);
    territory(&mut s, 2, [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("the UK gone");
    let registered = territory(&mut s, 1, [1, table, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(registered.is_ok(), "{registered:?}");
    let (exists, _) = s.user_swi(X | (CHUNK + 4), [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(exists.flag(psr::Z));
}

/// Where watchers count `Service_TimeZoneChanged` and
/// `Service_TerritoryStarted`.
const ANNOUNCED: u32 = DATA + 0xA00;
const STARTED_SEEN: u32 = DATA + 0xB00;

/// The times `Service_TimeZoneChanged` has come, and the R2 and R3 it last
/// came with.
fn announced(s: &mut Session) -> (u32, u32, u32) {
    (
        s.peek(ANNOUNCED),
        s.peek(ANNOUNCED + 4),
        s.peek(ANNOUNCED + 8),
    )
}

fn daylight(s: &mut Session, low: &[u32]) -> Result<crate::os::Regs, (u32, Vec<u8>)> {
    let mut r = [0; 13];
    r[..low.len()].copy_from_slice(low);
    territory(s, DAYLIGHT_SAVING, r)
}

fn rules(s: &mut Session, low: &[u32]) -> Result<crate::os::Regs, (u32, Vec<u8>)> {
    let mut r = [0; 13];
    r[..low.len()].copy_from_slice(low);
    territory(s, DAYLIGHT_RULES, r)
}

/// The five-byte times at `BUFFER` and eight bytes on.
fn buffers(s: &mut Session) -> (u64, u64) {
    let mut os = s.kernel.os();
    (
        os.read_five(BUFFER).expect("start"),
        os.read_five(BUFFER + 8).expect("end"),
    )
}

/// `Territory_DaylightSaving` 5's bounds for `year`.
fn bounds(s: &mut Session, year: u32) -> Result<(u64, u64), u32> {
    daylight(s, &[u32::MAX, 5, 0, year, BUFFER, BUFFER + 8]).map_err(|(number, _)| number)?;
    Ok(buffers(s))
}

/// Whether `Territory_DaylightSaving` 4 has daylight saving in force at
/// `time`.
fn in_force_at(s: &mut Session, time: u64) -> u32 {
    s.kernel.os().write_five(TIME, time).expect("written");
    daylight(s, &[u32::MAX, 4, 0, TIME]).expect("time").r[0]
}

/// Whether `Territory_DaylightSaving` 3 has daylight saving in force at the
/// ordinals `words`, centiseconds to year.
fn in_force_on(s: &mut Session, words: [u32; 7]) -> u32 {
    s.poke(TIME, &words);
    daylight(s, &[u32::MAX, 3, 0, TIME]).expect("ordinals").r[0]
}

fn auto_cmos(s: &mut Session) -> bool {
    s.kernel.os().world.cmos.read(crate::cmos::DBTB) & 1 != 0
}

#[test]
fn the_uk_s_bounds_are_its_zone_s_in_the_years_genuine_risc_os_5_keeps_rules_for() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    assert_eq!(
        bounds(&mut s, 2024),
        Ok((utc(2024, 3, 31, 1, 0), utc(2024, 10, 27, 1, 0)))
    );
    assert_eq!(
        bounds(&mut s, 1980),
        Ok((utc(1980, 3, 16, 2, 0), utc(1980, 10, 26, 2, 0)))
    );
    assert_eq!(
        bounds(&mut s, 1972),
        Ok((utc(1972, 3, 19, 2, 0), utc(1972, 10, 29, 2, 0)))
    );
    assert_eq!(
        bounds(&mut s, 1969),
        Ok((utc(1969, 1, 1, 0, 0), utc(1970, 1, 1, 0, 0))),
        "British Standard Time all year"
    );
    assert_eq!(
        bounds(&mut s, 1968),
        Ok((utc(1968, 2, 18, 2, 0), utc(1969, 1, 1, 0, 0)))
    );
    assert_eq!(
        bounds(&mut s, 1971),
        Ok((utc(1971, 1, 1, 0, 0), utc(1971, 10, 31, 2, 0)))
    );
    assert_eq!(
        bounds(&mut s, 2099),
        Ok((utc(2099, 3, 29, 1, 0), utc(2099, 10, 25, 1, 0)))
    );
    for year in [1967, 1947, 1941, 1916, 1900, 2100, 0, 65535] {
        assert_eq!(bounds(&mut s, year), Err(0x197), "{year} has no rule");
    }
}

#[test]
fn daylight_saving_s_bounds_keep_the_caller_s_registers_and_fail_as_one_error() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    let this_year = daylight(&mut s, &[u32::MAX, 5, 0, u32::MAX, BUFFER, BUFFER + 8]);
    assert_eq!(
        &this_year.expect("the current year").r[..4],
        &[u32::MAX, 5, 0, u32::MAX]
    );
    assert_eq!(
        buffers(&mut s),
        (utc(2026, 3, 29, 1, 0), utc(2026, 10, 25, 1, 0))
    );
    let named = daylight(&mut s, &[1, 5, 0, 2024, BUFFER, BUFFER + 8]).expect("the UK");
    assert_eq!(named.r[0], 1);
    s.kernel.os().write_five(BUFFER, 0).expect("cleared");
    for asked in [[99, 5, 0, 2024], [u32::MAX, 5, 1, 2024]] {
        let refused = daylight(
            &mut s,
            &[asked[0], asked[1], asked[2], asked[3], BUFFER, BUFFER + 8],
        );
        assert_eq!(
            refused.expect_err("no rule"),
            (
                0x197,
                b"No daylight saving rule for that time zone and year".to_vec()
            )
        );
    }
    assert_eq!(buffers(&mut s).0, 0, "nothing written");
}

#[test]
fn a_southern_zone_s_year_ends_daylight_saving_before_it_begins_it_again() {
    let mut s = session_in("Australia/Sydney", (2026, 7, 1, 12, 0));
    assert_eq!(
        bounds(&mut s, 2024),
        Ok((utc(2024, 10, 5, 16, 0), utc(2024, 4, 6, 16, 0)))
    );
    assert_eq!(
        in_force_at(&mut s, utc(2023, 12, 31, 15, 0)),
        1,
        "2024 by standard time"
    );
    assert_eq!(in_force_at(&mut s, utc(2024, 7, 1, 12, 0)), 0);
    assert_eq!(in_force_at(&mut s, utc(2024, 12, 1, 12, 0)), 1);
}

#[test]
fn the_uk_territory_s_rules_answer_as_genuine_risc_os_5_s_do() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    let has = rules(&mut s, &[u32::MAX, 0, 7, 7, 7, 7]).expect("rules");
    assert_eq!(&has.r[..6], &[1, 0, 7, 7, 7, 7]);
    let year = rules(&mut s, &[u32::MAX, 1, 0, 2024, BUFFER, BUFFER + 8]).expect("2024");
    assert_eq!(
        &year.r[..4],
        &[0xB625_E240, 1, 0, 2024],
        "R0 the end's low word"
    );
    assert_eq!(
        buffers(&mut s),
        (utc(2024, 3, 31, 1, 0), utc(2024, 10, 27, 1, 0))
    );
    assert!(
        rules(&mut s, &[1, 1, 0, 1968, BUFFER, BUFFER + 8]).is_ok(),
        "the UK by number"
    );
    for (zone, year) in [(0, 1967), (0, 2100), (0, u32::MAX), (1, 2024)] {
        let refused = rules(&mut s, &[u32::MAX, 1, zone, year, BUFFER, BUFFER + 8]);
        assert_eq!(
            refused.map_err(|(number, _)| number).err(),
            Some(0xAC2),
            "{zone} {year}"
        );
    }
    for reason in [2, 3, 6, u32::MAX] {
        let refused = rules(&mut s, &[u32::MAX, reason, 0, 2024]);
        assert_eq!(
            refused.map_err(|(number, _)| number).err(),
            Some(0x1E8),
            "{reason}"
        );
    }
    for unloaded in [0, 99] {
        let refused = rules(&mut s, &[unloaded, 0]);
        assert_eq!(
            refused.map_err(|(number, _)| number).err(),
            Some(0x192),
            "{unloaded}"
        );
    }
    let mut s = session_in("Asia/Kolkata", (2026, 7, 1, 12, 0));
    assert_eq!(rules(&mut s, &[u32::MAX, 0]).expect("the call").r[0], 1);
    assert_eq!(daylight(&mut s, &[u32::MAX, 2]).expect("supported").r[0], 1);
    assert_eq!(bounds(&mut s, 2024), Err(0x197), "but no year has a rule");
}

#[test]
fn daylight_saving_is_in_force_by_the_year_s_bounds_and_a_question_never_fails() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    assert_eq!(in_force_at(&mut s, utc(1968, 2, 18, 1, 0)), 0);
    assert_eq!(in_force_at(&mut s, utc(1968, 2, 18, 3, 0)), 1);
    assert_eq!(in_force_at(&mut s, utc(2099, 10, 25, 0, 0)), 1);
    assert_eq!(in_force_at(&mut s, utc(2099, 10, 25, 2, 0)), 0);
    for (year, month, day) in [(1967, 7, 1), (1941, 6, 15), (2100, 7, 1)] {
        assert_eq!(
            in_force_at(&mut s, utc(year, month, day, 12, 0)),
            0,
            "{year}: no rule"
        );
    }
    assert_eq!(in_force_on(&mut s, [0, 0, 30, 0, 27, 10, 2024]), 1);
    assert_eq!(
        in_force_on(&mut s, [0, 0, 30, 1, 27, 10, 2024]),
        0,
        "UTC, not local time"
    );
    assert_eq!(
        in_force_on(&mut s, [0, 0, 0, 12, 31, 6, 2024]),
        1,
        "31 June is 1 July"
    );
    assert_eq!(
        in_force_on(&mut s, [0, 0, 0, 12, 1, 19, 2024]),
        0,
        "no 19th month"
    );
    assert_eq!(
        in_force_on(&mut s, [0, 0, 0, 12, 1, 7, 1899]),
        0,
        "before five-byte times"
    );
    s.poke(TIME, &[0, 0, 0, 12, 1, 7, 2024]);
    let kept = daylight(&mut s, &[u32::MAX, 3, 0, TIME, 9, 9]).expect("ordinals");
    assert_eq!(
        (kept.r[0], kept.r[1], kept.r[2], kept.r[4], kept.r[5]),
        (1, 3, 0, 9, 9)
    );
    for (territory, zone) in [(u32::MAX, 1), (99, 0)] {
        let asked = daylight(&mut s, &[territory, 3, zone, TIME]).expect("an answer");
        assert_eq!(asked.r[0], 0, "{territory} {zone}");
        s.kernel
            .os()
            .write_five(BUFFER, utc(2024, 7, 1, 12, 0))
            .expect("written");
        let asked = daylight(&mut s, &[territory, 4, zone, BUFFER]).expect("an answer");
        assert_eq!(asked.r[0], 0, "{territory} {zone}");
    }
}

#[test]
fn daylight_saving_is_set_by_hand_or_kept_by_the_rules_in_cmos() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    let state = |s: &mut Session| daylight(s, &[u32::MAX, 0]).expect("read").r[0];
    daylight(&mut s, &[u32::MAX, 1, 0]).expect("off");
    assert_eq!(
        (state(&mut s), saving(&mut s), auto_cmos(&mut s)),
        (0, false, false)
    );
    daylight(&mut s, &[u32::MAX, 1, 1]).expect("on");
    assert_eq!((state(&mut s), saving(&mut s)), (1, true));
    daylight(&mut s, &[u32::MAX, 1, 2]).expect("automatic");
    assert_eq!(
        (state(&mut s), auto_cmos(&mut s)),
        (3, true),
        "summer: in force by the rules"
    );
    assert!(
        s.kernel.os().world.clock.zone_due.is_some(),
        "awaiting the next change"
    );
    daylight(&mut s, &[u32::MAX, 1, 4]).expect("reserved bits ignored");
    assert_eq!(state(&mut s), 0);
    assert!(s.kernel.os().world.clock.zone_due.is_none());
    let kept = daylight(&mut s, &[u32::MAX, 2, 9, 9]).expect("supported");
    assert_eq!(&kept.r[..4], &[1, 2, 9, 9]);
    assert_eq!(daylight(&mut s, &[99, 2]).expect("no territory").r[0], 0);
    assert_eq!(
        daylight(&mut s, &[1, 0])
            .expect("the current one by number")
            .r[0],
        0
    );
    assert_eq!(
        daylight(&mut s, &[2, 0]).map_err(|(n, _)| n).err(),
        Some(0x190)
    );
    assert_eq!(
        daylight(&mut s, &[99, 1, 0]).map_err(|(n, _)| n).err(),
        Some(0x190)
    );
    assert_eq!(
        daylight(&mut s, &[u32::MAX, 6]).map_err(|(n, _)| n).err(),
        Some(0x1E8)
    );
    let mut s = session_in("Asia/Kolkata", (2026, 7, 1, 12, 0));
    daylight(&mut s, &[u32::MAX, 1, 2]).expect("automatic");
    assert_eq!(state(&mut s), 2, "never in force");
    let due = s
        .kernel
        .os()
        .world
        .clock
        .zone_due
        .expect("the next year's rules");
    let new_year = utc(2026, 12, 31, 18, 30);
    assert_eq!(
        due - s.clock.get(),
        new_year - utc(2026, 7, 1, 12, 0),
        "midnight in India"
    );
}

#[test]
fn every_change_of_daylight_saving_is_announced_with_the_state_and_offset() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    crate::module::tests::watch_service(&mut s, TIME_ZONE_CHANGED, ANNOUNCED);
    daylight(&mut s, &[u32::MAX, 1, 0]).expect("off");
    assert_eq!(announced(&mut s), (1, 0, 0), "Greenwich Mean Time");
    daylight(&mut s, &[u32::MAX, 1, 0]).expect("off again");
    assert_eq!(announced(&mut s).0, 1, "nothing changed");
    daylight(&mut s, &[u32::MAX, 1, 2]).expect("automatic");
    assert_eq!(
        announced(&mut s),
        (2, 3, 360_000),
        "summer time, kept by the rules"
    );
}

#[test]
fn selecting_a_territory_records_it_and_says_so_and_does_nothing_more() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    crate::module::tests::watch_service(&mut s, TIME_ZONE_CHANGED, ANNOUNCED);
    crate::module::tests::watch_service(&mut s, TERRITORY_STARTED, STARTED_SEEN);
    for unloaded in [99, 0, u32::MAX] {
        let refused = territory(
            &mut s,
            SELECT,
            [unloaded, 5, 6, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        );
        assert_eq!(
            refused.expect_err("not loaded"),
            (0x192, b"That territory is not loaded".to_vec())
        );
    }
    let _ = s.output();
    let kept = territory(&mut s, SELECT, [1, 5, 6, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("the UK");
    assert_eq!(&kept.r[..4], &[1, 5, 6, 7]);
    assert_eq!(s.peek(STARTED_SEEN), 1, "started again");
    // Every entry of the guest's territory answers Latin1, and no offsets.
    s.poke(
        CODE,
        &[
            asm::mov(0, 101),
            asm::mov(2, 0),
            asm::mov(3, 0),
            asm::mov_pc_lr(),
        ],
    );
    let table = DATA + 0x800;
    s.poke(table, &[CODE; super::ENTRIES]);
    territory(&mut s, 1, [7, table, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("registered");
    let configured = s.kernel.os().world.cmos.read(crate::cmos::TERRITORY);
    territory(&mut s, SELECT, [7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("selected");
    assert_eq!(territory(&mut s, 0, [0; 13]).expect("current").r[0], 7);
    assert_eq!(s.peek(STARTED_SEEN), 2);
    assert!(
        crate::international::tests::defined(&s.output()).is_empty(),
        "no alphabet defined"
    );
    assert_eq!(announced(&mut s).0, 0, "nothing announced of the zone");
    assert_eq!(
        s.kernel.os().world.cmos.read(crate::cmos::TERRITORY),
        configured
    );
}

#[test]
fn the_territory_command_names_or_selects_the_current_territory() {
    use crate::cli::tests::{cli, written};
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    assert_eq!(written(&mut s, "Territory"), b"UK\n\r");
    crate::module::tests::watch_service(&mut s, TERRITORY_STARTED, STARTED_SEEN);
    cli(&mut s, "Territory UK").expect("by name");
    cli(&mut s, "Territory 1").expect("by number");
    assert_eq!(s.peek(STARTED_SEEN), 2, "selected twice");
    assert_eq!(cli(&mut s, "Territory 99").map_err(|(n, _)| n), Err(0x192));
    assert_eq!(
        cli(&mut s, "Territory Nowhere").map_err(|(n, _)| n),
        Err(0x16B)
    );
    assert_eq!(
        cli(&mut s, "Territory UK extra").map_err(|(n, _)| n),
        Err(0xDC)
    );
}

#[test]
fn the_rules_change_the_clock_over_when_the_zone_does() {
    let mut s = session_in("Europe/London", (2026, 3, 29, 0, 59));
    daylight(&mut s, &[u32::MAX, 1, 2]).expect("automatic");
    assert!(!saving(&mut s), "a minute before the clocks go forward");
    crate::module::tests::watch_service(&mut s, TIME_ZONE_CHANGED, ANNOUNCED);
    let due = s.kernel.os().world.clock.zone_due.expect("due");
    assert_eq!(due - s.clock.get(), 60 * 100, "a minute from now");
    s.clock.set(due);
    let spin = asm::b(CODE, CODE);
    s.poke(CODE, &[spin]);
    s.enter(Bits::Bit32, CODE, [0; 13]);
    s.run(10_000);
    assert!(saving(&mut s), "in force from the changeover");
    assert_eq!(announced(&mut s), (1, 3, 360_000), "announced");
    let next = s.kernel.os().world.clock.zone_due.expect("the autumn's");
    assert_eq!(next - due, utc(2026, 10, 25, 1, 0) - utc(2026, 3, 29, 1, 0));
}

#[test]
fn past_a_year_s_bounds_the_rules_await_the_next_year_s() {
    let mut s = session_in("Europe/London", (2026, 11, 1, 12, 0));
    daylight(&mut s, &[u32::MAX, 1, 2]).expect("automatic");
    let due = s.kernel.os().world.clock.zone_due.expect("due");
    assert_eq!(
        due - s.clock.get(),
        utc(2027, 1, 1, 0, 0) - utc(2026, 11, 1, 12, 0)
    );
    s.clock.set(due);
    let spin = asm::b(CODE, CODE);
    s.poke(CODE, &[spin]);
    s.enter(Bits::Bit32, CODE, [0; 13]);
    s.run(10_000);
    assert!(!saving(&mut s), "winter still");
    let next = s.kernel.os().world.clock.zone_due.expect("the spring's");
    assert_eq!(next - due, utc(2027, 3, 28, 1, 0) - utc(2027, 1, 1, 0, 0));
}

#[test]
fn starting_up_brings_daylight_saving_up_to_date_where_the_rules_keep_it() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    {
        let os = s.kernel.os();
        let auto = os.world.cmos.read(crate::cmos::DBTB);
        os.world.cmos.write(crate::cmos::DBTB, auto | 1);
        let alarm = os.world.cmos.read(crate::cmos::ALARM_AND_TIME);
        os.world
            .cmos
            .write(crate::cmos::ALARM_AND_TIME, alarm & !crate::cmos::DST);
    }
    s.user_swi(
        X | SERVICE_CALL,
        [0, POST_INIT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert!(saving(&mut s), "summer by the rules");
    assert!(
        s.kernel.os().world.clock.zone_due.is_some(),
        "the autumn's awaited"
    );
}

/// `Territory_ConvertTimeFormats` of `TIME` into `BUFFER`.
fn convert(s: &mut Session, number: u32, flags: u32, zone: u32) -> Result<crate::os::Regs, u32> {
    let registers = [number, TIME, BUFFER, flags, zone, 0, 0, 0, 0, 0, 0, 0, 0];
    territory(s, CONVERT_TIME_FORMATS, registers).map_err(|(error, _)| error)
}

/// What `Territory_ConvertTimeFormats` makes of the UTC ordinals of a time,
/// as a five-byte time.
fn local_five(
    s: &mut Session,
    (year, month, day, hour, minute): (u32, u32, u32, u32, u32),
    flags: u32,
    zone: u32,
) -> u64 {
    s.poke(TIME, &[0, 0, minute, hour, day, month, year]);
    convert(s, u32::MAX, 0x102 | flags, zone).expect("converted");
    s.kernel.os().read_five(BUFFER).expect("five bytes")
}

/// What it makes of a local time's ordinals, as a UTC five-byte time.
fn utc_five(
    s: &mut Session,
    (year, month, day, hour, minute): (u32, u32, u32, u32, u32),
    flags: u32,
    zone: u32,
) -> u64 {
    s.poke(TIME, &[0, 0, minute, hour, day, month, year]);
    convert(s, u32::MAX, 0x300 | flags, zone).expect("converted");
    s.kernel.os().read_five(BUFFER).expect("five bytes")
}

const HOUR_CS: u64 = HOUR.unsigned_abs();

#[test]
fn time_formats_convert_local_time_and_utc_as_risc_os_5_s_do() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    let noon = utc(2024, 7, 1, 12, 0);
    let july = (2024, 7, 1, 12, 0);
    assert_eq!(
        local_five(&mut s, july, 0, 0),
        noon,
        "standard time unless asked"
    );
    assert_eq!(local_five(&mut s, july, AS_SUMMER, 0), noon + HOUR_CS);
    assert_eq!(local_five(&mut s, july, BY_THE_RULES, 0), noon + HOUR_CS);
    let january = (2024, 1, 15, 12, 0);
    let winter = utc(2024, 1, 15, 12, 0);
    assert_eq!(
        local_five(&mut s, january, BY_THE_RULES | AS_SUMMER, 0),
        winter,
        "the rules win"
    );
    let unruled = (1960, 7, 1, 12, 0);
    assert_eq!(
        local_five(&mut s, unruled, BY_THE_RULES, 0),
        utc(1960, 7, 1, 12, 0)
    );
    assert_eq!(
        local_five(&mut s, unruled, BY_THE_RULES | STANDARD_UNRULED, 0),
        utc(1960, 7, 1, 12, 0)
    );
    assert_eq!(
        local_five(&mut s, july, ADJUSTMENT, 360_000),
        noon + HOUR_CS
    );
    assert_eq!(
        local_five(&mut s, july, ADJUSTMENT, (-360_000_i32).cast_unsigned()),
        noon - HOUR_CS
    );
    assert_eq!(
        local_five(&mut s, july, ADJUSTMENT, u32::MAX),
        noon - 1,
        "centiseconds"
    );
    assert_eq!(
        local_five(&mut s, july, ADJUSTMENT | AS_SUMMER, 360_000),
        noon + 2 * HOUR_CS
    );
    assert_eq!(utc_five(&mut s, (2024, 7, 1, 13, 0), BY_THE_RULES, 0), noon);
    assert_eq!(utc_five(&mut s, july, ADJUSTMENT, 360_000), noon - HOUR_CS);
    assert_eq!(
        utc_five(&mut s, (2024, 3, 31, 1, 30), BY_THE_RULES, 0),
        utc(2024, 3, 31, 0, 30),
        "the hour the spring skips read as summer time"
    );
    assert_eq!(
        utc_five(&mut s, (2024, 10, 27, 1, 30), BY_THE_RULES, 0),
        utc(2024, 10, 27, 1, 30),
        "the hour the autumn repeats read as standard time"
    );
}

#[test]
fn time_formats_write_ordinals_and_keep_the_caller_s_registers() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    s.kernel
        .os()
        .write_five(TIME, utc(2024, 7, 1, 12, 0))
        .expect("written");
    let kept = convert(&mut s, u32::MAX, 0x003 | BY_THE_RULES, 0).expect("to local ordinals");
    assert_eq!(
        &kept.r[..5],
        &[u32::MAX, TIME, BUFFER, 0x003 | BY_THE_RULES, 0]
    );
    let words: Vec<u32> = (0..9).map(|i| s.peek(BUFFER + 4 * i)).collect();
    assert_eq!(
        words,
        [0, 0, 0, 13, 1, 7, 2024, 2, 183],
        "Monday, the 183rd day"
    );
    s.poke(TIME, &[0, 0, 0, 12, 1, 7, 2024]);
    convert(&mut s, u32::MAX, 0x202, 0).expect("UTC to UTC");
    let words: Vec<u32> = (0..9).map(|i| s.peek(BUFFER + 4 * i)).collect();
    assert_eq!(words, [0, 0, 0, 12, 1, 7, 2024, 2, 183]);
    s.kernel
        .os()
        .write_five(TIME, utc(2024, 7, 1, 12, 0))
        .expect("written");
    convert(&mut s, u32::MAX, 0x201 | AS_SUMMER, 0).expect("local to UTC");
    assert_eq!(s.peek(BUFFER + 12), 11, "an hour earlier");
}

#[test]
fn time_formats_refuse_what_genuine_risc_os_5_refuses() {
    let mut s = session_in("Europe/London", (2026, 7, 1, 12, 0));
    s.poke(TIME, &[0, 0, 0, 12, 1, 7, 2024]);
    assert_eq!(
        convert(&mut s, u32::MAX, 0x402, 0).err(),
        Some(0x1E8),
        "no fifth form"
    );
    assert_eq!(
        convert(&mut s, u32::MAX, 0x302 | 1 << 20, 0).err(),
        Some(0x1E8),
        "reserved"
    );
    let both = 0x102 | ADJUSTMENT | BY_THE_RULES;
    assert_eq!(
        convert(&mut s, u32::MAX, both, 0).err(),
        Some(0x1E8),
        "an adjustment or the rules"
    );
    assert_eq!(
        convert(&mut s, 99, 0x102 | BY_THE_RULES, 0).err(),
        Some(0x192)
    );
    assert_eq!(
        convert(&mut s, 99, 0x302, 0)
            .expect("no territory needed")
            .r[0],
        99
    );
    assert_eq!(
        convert(&mut s, 99, 0x102 | ADJUSTMENT, 0)
            .expect("nor here")
            .r[0],
        99
    );
    assert_eq!(
        convert(&mut s, u32::MAX, 0x102, 1).err(),
        Some(0x196),
        "one time zone"
    );
    assert_eq!(
        convert(&mut s, u32::MAX, 0x302, 7)
            .expect("no zone needed")
            .r[4],
        7
    );
    s.poke(TIME, &[0, 0, 0, 12, 1, 13, 2024]);
    assert_eq!(
        convert(&mut s, u32::MAX, 0x302, 0).err(),
        Some(0x195),
        "no thirteenth month"
    );
}
