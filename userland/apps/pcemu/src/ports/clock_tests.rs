use super::*;
use crate::time::NANOS_PER_MILLISECOND as MS;
use tairix_abi::time::SECS_PER_DAY;

const US: Nanos = 1_000;
/// Control register D as software writes it: the fixed-period flag kept.
const KEEP_FLAG: u8 = cd::IRQ;

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

fn card_at(now: &CivilTime) -> ClockCard {
    let mut card = ClockCard::new(0);
    assert!(card.set(now, 0));
    card
}

/// A counter's two digits, read through the ports.
fn read(card: &mut ClockCard, units: u16, t: Nanos) -> u8 {
    let tens = card.input(units + 1, t) & 0x0F;
    tens * 10 + (card.input(units, t) & 0x0F)
}

fn seconds(card: &mut ClockCard, t: Nanos) -> u8 {
    read(card, 0, t)
}

#[test]
fn a_new_card_starts_at_1980_on_its_tuesday() {
    let mut card = ClockCard::new(0);
    assert_eq!(card.time(0), Some(civil(1980, 1, 1, 0, 0, 0)));
    assert_eq!(card.input(0x0C, 0), 0xF2);
}

#[test]
fn it_counts_from_the_last_second_of_a_century_into_the_next() {
    let mut card = card_at(&civil(1999, 12, 31, 23, 59, 58));
    assert_eq!(
        card.time(2 * SECOND - 1),
        Some(civil(1999, 12, 31, 23, 59, 59))
    );
    assert_eq!(card.time(2 * SECOND), Some(civil(2000, 1, 1, 0, 0, 0)));
    assert_eq!(card.input(0x0C, 2 * SECOND) & 0x0F, 6, "a Saturday");
}

#[test]
fn february_has_a_29th_in_years_divisible_by_four() {
    let mut leap = card_at(&civil(2024, 2, 28, 23, 59, 59));
    assert_eq!(leap.time(SECOND), Some(civil(2024, 2, 29, 0, 0, 0)));
    let mut common = card_at(&civil(2023, 2, 28, 23, 59, 59));
    assert_eq!(common.time(SECOND), Some(civil(2023, 3, 1, 0, 0, 0)));
}

#[test]
fn catching_up_decades_at_once_keeps_to_the_calendar() {
    let start = civil(1980, 1, 1, 0, 0, 0);
    let mut card = card_at(&start);
    let epoch = start.to_unix_secs().unwrap();
    for elapsed in [59, 3_661, 86_399, 86_400, 40 * 366 * SECS_PER_DAY + 12_345] {
        let t = u64::try_from(elapsed).unwrap() * SECOND + SECOND / 2;
        let expected = CivilTime::from_unix_secs(epoch + elapsed);
        assert_eq!(card.time(t), Some(expected), "after {elapsed} s");
        let days = (epoch + elapsed).div_euclid(SECS_PER_DAY);
        let weekday = weekday_from_days(days) % 7;
        assert_eq!(u32::from(card.input(0x0C, t) & 0x0F), weekday);
    }
}

#[test]
fn hold_holds_one_carry_and_latches_whether_one_just_came() {
    let mut card = card_at(&civil(2001, 5, 6, 7, 8, 0));
    let t = SECOND + 100 * US;
    card.output(reg::CD, KEEP_FLAG | cd::HOLD, t);
    assert_eq!(
        card.input(reg::CD, t + 500 * US),
        0xF0 | cd::HOLD | cd::BUSY,
        "a carry 100 µs ago"
    );
    card.output(reg::CD, KEEP_FLAG, SECOND + 200 * MS);
    assert_eq!(
        card.input(reg::CD, SECOND + 200 * MS),
        0xF0 | cd::BUSY,
        "unreadable without HOLD"
    );
    card.output(reg::CD, KEEP_FLAG | cd::HOLD, SECOND + 500 * MS);
    assert_eq!(card.input(reg::CD, SECOND + 500 * MS), 0xF0 | cd::HOLD);
    assert_eq!(
        seconds(&mut card, 3 * SECOND + 500 * MS),
        1,
        "held through two carries"
    );
    let released = 3 * SECOND + 600 * MS;
    card.output(reg::CD, KEEP_FLAG, released);
    assert_eq!(seconds(&mut card, released), 2, "one carry kept, one lost");
    assert_eq!(seconds(&mut card, 4 * SECOND), 3);
}

#[test]
fn stop_freezes_the_divider_and_reset_restarts_its_second() {
    let mut card = card_at(&civil(2001, 5, 6, 7, 8, 0));
    card.output(reg::CF, cf::HOURS_24 | cf::STOP, SECOND / 4);
    card.output(reg::CF, cf::HOURS_24, SECOND + SECOND / 4);
    assert_eq!(seconds(&mut card, 2 * SECOND - 1), 0);
    assert_eq!(
        seconds(&mut card, 2 * SECOND),
        1,
        "three quarters of a second after it went on"
    );
    card.output(reg::CF, cf::HOURS_24 | cf::RESET, 2 * SECOND + SECOND / 2);
    card.output(reg::CF, cf::HOURS_24, 3 * SECOND);
    assert_eq!(seconds(&mut card, 4 * SECOND - 1), 1);
    assert_eq!(
        seconds(&mut card, 4 * SECOND),
        2,
        "a whole second after the release"
    );
}

#[test]
fn the_24_12_bit_takes_effect_when_reset_is_released() {
    let mut card = card_at(&civil(2001, 5, 6, 13, 0, 0));
    card.output(reg::CF, 0, MS);
    assert_eq!(card.time(MS).map(|t| t.hour), Some(13), "not yet");
    card.output(reg::CF, cf::RESET, 2 * MS);
    card.output(reg::CF, 0, 3 * MS);
    assert_eq!(card.time(3 * MS), None, "13 is no 12-hour time");
    card.output(5, 0x04, 4 * MS);
    card.output(4, 0x01, 4 * MS);
    assert_eq!(card.time(4 * MS).map(|t| t.hour), Some(13), "1 PM");
}

#[test]
fn twelve_hour_time_turns_from_11_59_pm_to_12_am_on_the_next_day() {
    let mut card = ClockCard::new(0);
    card.output(reg::CF, cf::RESET, 0);
    card.output(reg::CF, 0, 0);
    assert!(card.set(&civil(2001, 3, 31, 23, 59, 59), 0));
    assert_eq!((card.input(5, 0), card.input(4, 0)), (0xF5, 0xF1), "11 PM");
    assert_eq!(
        (card.input(5, SECOND), card.input(4, SECOND)),
        (0xF1, 0xF2),
        "12 AM"
    );
    assert_eq!(card.time(SECOND), Some(civil(2001, 4, 1, 0, 0, 0)));
}

#[test]
fn thirty_second_adjust_rounds_to_the_nearest_minute() {
    let mut down = card_at(&civil(2001, 5, 6, 10, 15, 29));
    let t = SECOND / 2;
    down.output(reg::CD, KEEP_FLAG | cd::ADJUST, t);
    assert_eq!(down.input(reg::CD, t) & cd::ADJUST, cd::ADJUST);
    assert_eq!(down.input(reg::CD, t + 125 * US) & cd::ADJUST, 0);
    assert_eq!(down.time(t), Some(civil(2001, 5, 6, 10, 15, 0)));
    assert_eq!(seconds(&mut down, t + SECOND - 1), 0, "the divider cleared");
    assert_eq!(seconds(&mut down, t + SECOND), 1);
    let mut up = card_at(&civil(2001, 5, 6, 10, 59, 30));
    up.output(reg::CD, KEEP_FLAG | cd::ADJUST, t);
    assert_eq!(up.time(t), Some(civil(2001, 5, 6, 11, 0, 0)));
}

#[test]
fn the_fixed_period_flag_pulses_after_each_second_or_latches_until_cleared() {
    let mut card = card_at(&civil(2001, 5, 6, 10, 0, 0));
    card.output(reg::CE, 0x04, 0);
    let flag = |card: &mut ClockCard, t| card.input(reg::CD, t) & cd::IRQ != 0;
    assert!(!flag(&mut card, SECOND - 1));
    assert!(flag(&mut card, SECOND));
    assert!(flag(&mut card, SECOND + PULSE - 1));
    assert!(!flag(&mut card, SECOND + PULSE), "7.8125 ms");
    card.output(reg::CE, 0x04 | ce::INTERRUPT, SECOND + SECOND / 2);
    assert!(
        !flag(&mut card, 2 * SECOND - 1),
        "the earlier second forgotten"
    );
    assert!(flag(&mut card, 2 * SECOND + SECOND / 2), "latched");
    card.output(reg::CD, 0, 2 * SECOND + 600 * MS);
    assert!(!flag(&mut card, 2 * SECOND + 600 * MS));
    assert!(flag(&mut card, 3 * SECOND));
    card.output(reg::CE, 0x04 | ce::INTERRUPT | ce::MASK, 3 * SECOND);
    assert!(!flag(&mut card, 4 * SECOND), "masked");
}

#[test]
fn the_fastest_period_is_the_dividers_64_hz_square_wave() {
    let mut card = card_at(&civil(2001, 5, 6, 10, 0, 0));
    card.output(reg::CE, 0, 0);
    let flag = |card: &mut ClockCard, t| card.input(reg::CD, t) & cd::IRQ != 0;
    assert!(!flag(&mut card, TICK - 1));
    assert!(flag(&mut card, TICK));
    assert!(!flag(&mut card, TICK + PULSE));
    assert!(flag(&mut card, 5 * TICK + 1));
}

#[test]
fn a_counter_written_out_of_range_counts_on_from_its_last_value() {
    let mut card = card_at(&civil(2001, 5, 6, 10, 0, 0));
    card.output(1, 7, SECOND / 2);
    card.output(0, 5, SECOND / 2);
    assert_eq!(seconds(&mut card, SECOND / 2), 75);
    assert_eq!(card.time(SECOND), Some(civil(2001, 5, 6, 10, 1, 0)));
}

#[test]
fn the_high_lines_float_and_unbuilt_bits_read_zero() {
    let mut card = ClockCard::new(0);
    card.output(1, 0xFF, 0);
    assert_eq!(card.input(1, 0), 0xF7);
    card.output(9, 0xFF, 0);
    assert_eq!(card.input(9, 0), 0xF1);
    card.output(5, 0xFF, 0);
    assert_eq!(card.input(5, 0), 0xF3, "no PM bit in 24-hour time");
}

#[test]
fn set_refuses_what_the_card_cannot_hold() {
    let mut card = ClockCard::new(0);
    for refused in [
        civil(2080, 1, 1, 0, 0, 0),
        civil(1979, 12, 31, 23, 59, 59),
        civil(2001, 2, 29, 0, 0, 0),
        civil(2001, 1, 1, 24, 0, 0),
    ] {
        assert!(!card.set(&refused, 0), "{refused:?}");
    }
    assert_eq!(card.time(0), Some(civil(1980, 1, 1, 0, 0, 0)));
}

#[test]
fn a_guest_setting_is_taken_once_the_card_runs_with_it() {
    let start = civil(1991, 6, 11, 5, 0, 0);
    let mut card = card_at(&start);
    assert_eq!(
        card.take_guest_setting(0),
        None,
        "the host's own setting is not the guest's"
    );
    card.output(reg::CD, KEEP_FLAG | cd::HOLD, MS);
    card.output(u16::try_from(digit::MINUTES).unwrap(), 7, MS);
    assert_eq!(
        card.take_guest_setting(MS),
        None,
        "held while it is written"
    );
    card.output(reg::CD, KEEP_FLAG, 2 * MS);
    let set = card
        .take_guest_setting(2 * MS)
        .expect("let go, the setting is taken");
    assert_eq!((set.hour, set.minute), (5, 7));
    assert_eq!(card.take_guest_setting(3 * MS), None, "once");
    card.output(reg::CD, KEEP_FLAG | cd::ADJUST, 4 * MS);
    assert!(
        card.take_guest_setting(4 * MS).is_some(),
        "the thirty-second adjust moves it too"
    );
}
