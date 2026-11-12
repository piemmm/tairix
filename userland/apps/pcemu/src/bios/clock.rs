//! INT 1Ah's time and date functions, answered from the clock card the ROM
//! was built for, and the time of day the POST starts the tick count at.
//!
//! The card's counter is read and written held, as its manual asks: HOLD
//! set, BUSY checked clear, HOLD released after. Setting the time stops the
//! card, clears its divider and puts it in 24-hour time, so the new second
//! begins as it is set. Years run 1980 to 2079, the card's two digits taken
//! as this century's below 80.

use super::guest::Guest;
use super::layout::{table, SEGMENT};
use super::system::{set_ticks, FLAGS, TICKS_PER_DAY};
use crate::asm::{Reg16, Reg8};
use crate::cpu::flag;
use crate::ports::clock::{cd, cf, digit, reg, PM, YEARS};
use crate::time::Nanos;
use core::ops::RangeInclusive;
use tairix_abi::time::{days_from_civil, days_in_month, weekday_from_days, SECS_PER_DAY};

/// Tries at holding the counter, and the wait between them, which the
/// manual gives: together well past a carry's 190 µs.
const HOLD_TRIES: u8 = 8;
const HOLD_RETRY: Nanos = 61_000;

/// INT 1Ah's functions 02h to 07h: the time and the date read and set;
/// the card has no alarm. The carry flag reports failure.
pub fn service(g: &mut Guest<'_>, function: u8) {
    let done = card(g).and_then(|base| match function {
        0x02 => read_time(g, base).map(|[hours, minutes, seconds]| {
            g.set_r16(Reg16::Cx, u16::from_be_bytes([hours, minutes]));
            g.set_r16(Reg16::Dx, u16::from_be_bytes([seconds, 0]));
        }),
        0x03 => set_time(g, base),
        0x04 => read_date(g, base).map(|[century, year, month, day]| {
            g.set_r16(Reg16::Cx, u16::from_be_bytes([century, year]));
            g.set_r16(Reg16::Dx, u16::from_be_bytes([month, day]));
        }),
        0x05 => set_date(g, base),
        _ => None,
    });
    g.set_returned_flag(FLAGS, flag::CF, done.is_none());
}

/// The POST's start for the tick count: the card's time of day, if it has
/// one to give.
pub fn time_of_day(g: &mut Guest<'_>) {
    let Some([hours, minutes, seconds]) = card(g).and_then(|base| read_time(g, base)) else {
        return;
    };
    let value = |bcd: u8| u64::from(bcd >> 4) * 10 + u64::from(bcd & 0x0F);
    let elapsed = value(hours) * 3_600 + value(minutes) * 60 + value(seconds);
    let ticks = elapsed * u64::from(TICKS_PER_DAY) / SECS_PER_DAY.unsigned_abs();
    set_ticks(g, u32::try_from(ticks).unwrap_or(0));
}

/// The card's base, which the ROM records, if one is fitted.
fn card(g: &mut Guest<'_>) -> Option<u16> {
    let base = g.read16(SEGMENT, table::CLOCK);
    (base != 0).then_some(base)
}

/// Holds the card's counter still, `false` if a carry never lets it go.
fn hold(g: &mut Guest<'_>, base: u16) -> bool {
    for _ in 0..HOLD_TRIES {
        g.output(base + reg::CD, cd::IRQ | cd::HOLD);
        if g.input(base + reg::CD) & cd::BUSY == 0 {
            return true;
        }
        g.output(base + reg::CD, cd::IRQ);
        g.spend(HOLD_RETRY);
    }
    false
}

fn release(g: &mut Guest<'_>, base: u16) {
    g.output(base + reg::CD, cd::IRQ);
}

/// The counters' digits, read held, if the card is running.
fn digits(g: &mut Guest<'_>, base: u16) -> Option<[u8; 13]> {
    if g.input(base + reg::CF) & (cf::STOP | cf::RESET) != 0 || !hold(g, base) {
        return None;
    }
    let mut digits = [0; 13];
    for (offset, slot) in (0..).zip(&mut digits) {
        *slot = g.input(base + offset) & 0x0F;
    }
    release(g, base);
    Some(digits)
}

/// A counter's two digits, if they make a decimal number within `range`.
fn value(digits: &[u8; 13], units: usize, range: RangeInclusive<u8>) -> Option<u8> {
    let (ones, tens) = (digits[units], digits[units + 1]);
    Some(tens * 10 + ones).filter(|n| ones <= 9 && range.contains(n))
}

/// A BCD byte's value, if it is one and within `range`.
fn from_bcd(byte: u8, range: RangeInclusive<u8>) -> Option<u8> {
    let (tens, ones) = (byte >> 4, byte & 0x0F);
    Some(tens * 10 + ones).filter(|n| tens <= 9 && ones <= 9 && range.contains(n))
}

const fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

/// The hours, minutes and seconds in BCD, the hours in 24-hour time.
fn read_time(g: &mut Guest<'_>, base: u16) -> Option<[u8; 3]> {
    let digits = digits(g, base)?;
    let twelve = g.input(base + reg::CF) & cf::HOURS_24 == 0;
    let mut clock = digits;
    clock[digit::HOURS + 1] &= !PM;
    let hours = if twelve {
        let pm = if digits[digit::HOURS + 1] & PM != 0 {
            12
        } else {
            0
        };
        value(&clock, digit::HOURS, 1..=12)? % 12 + pm
    } else {
        value(&clock, digit::HOURS, 0..=23)?
    };
    Some([
        to_bcd(hours),
        to_bcd(value(&digits, digit::MINUTES, 0..=59)?),
        to_bcd(value(&digits, digit::SECONDS, 0..=59)?),
    ])
}

/// The century, year, month and day in BCD.
fn read_date(g: &mut Guest<'_>, base: u16) -> Option<[u8; 4]> {
    let digits = digits(g, base)?;
    let year = value(&digits, digit::YEAR, 0..=99)?;
    Some([
        if year < 80 { 0x20 } else { 0x19 },
        to_bcd(year),
        to_bcd(value(&digits, digit::MONTH, 1..=12)?),
        to_bcd(value(&digits, digit::DAY, 1..=31)?),
    ])
}

/// Writes `value` into the two digits from `units`.
fn write(g: &mut Guest<'_>, base: u16, units: usize, value: u8) {
    let Ok(units) = u16::try_from(units) else {
        return;
    };
    g.output(base + units, value % 10);
    g.output(base + units + 1, value / 10);
}

/// The time from CH, CL and DH; DL's daylight-saving flag has nothing to
/// set on this card.
fn set_time(g: &mut Guest<'_>, base: u16) -> Option<()> {
    let [hours, minutes] = g.r16(Reg16::Cx).to_be_bytes();
    let hours = from_bcd(hours, 0..=23)?;
    let minutes = from_bcd(minutes, 0..=59)?;
    let seconds = from_bcd(g.r8(Reg8::Dh), 0..=59)?;
    g.output(base + reg::CF, cf::HOURS_24 | cf::STOP | cf::RESET);
    g.output(base + reg::CF, cf::HOURS_24 | cf::STOP);
    write(g, base, digit::SECONDS, seconds);
    write(g, base, digit::MINUTES, minutes);
    write(g, base, digit::HOURS, hours);
    g.output(base + reg::CF, cf::HOURS_24);
    Some(())
}

/// The date from CH, CL, DH and DL, and its day of the week, Sunday 0.
fn set_date(g: &mut Guest<'_>, base: u16) -> Option<()> {
    let [century, year] = g.r16(Reg16::Cx).to_be_bytes();
    let [month, day] = g.r16(Reg16::Dx).to_be_bytes();
    let century = from_bcd(century, 0..=99)?;
    let year = from_bcd(year, 0..=99)?;
    let month = from_bcd(month, 1..=12)?;
    let day = from_bcd(day, 1..=31)?;
    let full = i64::from(century) * 100 + i64::from(year);
    if !YEARS.contains(&full) || u32::from(day) > days_in_month(full, u32::from(month)) {
        return None;
    }
    let days = days_from_civil(full, u32::from(month), u32::from(day))?;
    let weekday = u8::try_from(weekday_from_days(days) % 7).ok()?;
    if !hold(g, base) {
        return None;
    }
    write(g, base, digit::DAY, day);
    write(g, base, digit::MONTH, month);
    write(g, base, digit::YEAR, year);
    let weekday_port = u16::try_from(digit::WEEKDAY).ok()?;
    g.output(base + weekday_port, weekday);
    release(g, base);
    Some(())
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
