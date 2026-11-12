use super::super::run_until;
use super::super::tests::{booted, call, carry, machine, stop, stopped_at};
use crate::cpu::Bus;
use crate::machine::{Machine, Template};
use crate::time::{Nanos, NANOS_PER_SECOND};
use tairix_abi::time::CivilTime;

const SECOND: Nanos = NANOS_PER_SECOND;

fn civil(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> CivilTime {
    CivilTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
    }
}

fn clocked(now: &CivilTime) -> Machine {
    let mut m = booted(Template::IbmXt, None);
    assert!(m.board_mut().set_clock(now));
    m
}

#[test]
fn int_1ah_reads_the_cards_time_and_date() {
    let mut m = clocked(&civil(2026, 11, 10, 9, 30, 15));
    let time = call(&mut m, 0x1A, |r| r.set16(0, 0x0200));
    assert!(!carry(&time));
    assert_eq!((time.get16(1), time.get16(2)), (0x0930, 0x1500));
    let date = call(&mut m, 0x1A, |r| r.set16(0, 0x0400));
    assert!(!carry(&date));
    assert_eq!((date.get16(1), date.get16(2)), (0x2026, 0x1110));
}

#[test]
fn int_1ah_sets_the_time_from_its_second_and_the_date_with_its_weekday() {
    let mut m = clocked(&civil(2026, 11, 10, 9, 30, 15));
    let set = call(&mut m, 0x1A, |r| {
        r.set16(0, 0x0300);
        r.set16(1, 0x2359);
        r.set16(2, 0x5800);
    });
    assert!(!carry(&set));
    let dated = call(&mut m, 0x1A, |r| {
        r.set16(0, 0x0500);
        r.set16(1, 0x1999);
        r.set16(2, 0x1231);
    });
    assert!(!carry(&dated));
    let now = m.now();
    run_until(&mut m, now + 3 * SECOND);
    let held = m.board_mut().clock().unwrap();
    assert_eq!(
        (held.year, held.month, held.day, held.hour),
        (2000, 1, 1, 0)
    );
    assert!(held.second <= 2, "{held:?}");
}

#[test]
fn int_1ah_refuses_what_the_card_cannot_hold() {
    let mut m = clocked(&civil(2026, 11, 10, 9, 30, 15));
    for (function, cx, dx) in [
        (0x0300, 0x2400, 0x0000),
        (0x0300, 0x1260, 0x0000),
        (0x0300, 0x1A00, 0x0000),
        (0x0500, 0x2080, 0x0101),
        (0x0500, 0x1979, 0x1231),
        (0x0500, 0x2001, 0x0229),
        (0x0500, 0x2001, 0x1301),
        (0x0600, 0x0000, 0x0000),
    ] {
        let refused = call(&mut m, 0x1A, |r| {
            r.set16(0, function);
            r.set16(1, cx);
            r.set16(2, dx);
        });
        assert!(carry(&refused), "{function:04X} {cx:04X} {dx:04X}");
    }
    let date = call(&mut m, 0x1A, |r| r.set16(0, 0x0400));
    assert_eq!(
        (date.get16(1), date.get16(2)),
        (0x2026, 0x1110),
        "unchanged"
    );
}

#[test]
fn the_post_starts_the_tick_count_at_the_cards_time_of_day() {
    let mut m = machine(Template::IbmXt);
    assert!(m.board_mut().set_clock(&civil(2026, 11, 10, 12, 0, 0)));
    m.board_mut()
        .insert_floppy(0, super::super::tests::boot_disk(stop))
        .unwrap();
    run_until(&mut m, 3 * SECOND);
    assert!(stopped_at(&m, 0, 0x7C00..0x7E00));
    let peek = |at| u32::from(m.board().peek(at));
    let ticks = peek(0x46C) | peek(0x46D) << 8 | peek(0x46E) << 16 | peek(0x46F) << 24;
    assert!((786_520..786_520 + 60).contains(&ticks), "{ticks}");
}

#[test]
fn without_a_clock_card_its_functions_fail() {
    let mut config = Template::IbmXt.config();
    config.ports.clock = false;
    let mut m = crate::bios::machine(&config, [None, None]).unwrap();
    m.board_mut()
        .insert_floppy(0, super::super::tests::boot_disk(stop))
        .unwrap();
    run_until(&mut m, 3 * SECOND);
    let time = call(&mut m, 0x1A, |r| r.set16(0, 0x0200));
    assert!(carry(&time));
}
