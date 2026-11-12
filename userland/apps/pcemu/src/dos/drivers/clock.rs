//! `CLOCK$`: the time from the BIOS tick count, the date kept as days since
//! 1980 and moved on by each midnight the tick count reports, as DOS keeps
//! it. Its `INIT` takes the date from the clock card, where there is one; a
//! time set is written to the card as well as to the tick count.

use super::{unknown, Driver};
use crate::asm::{Reg16, Reg8};
use crate::cpu::flag;
use crate::dos::char::{civil_of, days_of};
use crate::dos::data::{driver, SEGMENT};
use crate::dos::device::{command, field};

const TICKS_PER_DAY: u64 = crate::bios::TICKS_PER_DAY as u64;
/// Hundredths of a second in a day.
const HUNDREDTHS_PER_DAY: u64 = 8_640_000;

const READ_TICKS: u8 = 1;
const SET_TICKS: u8 = 2;
const SET_CARD_TIME: u8 = 3;
const SET_CARD_DATE: u8 = 4;

/// A request to `CLOCK$`.
pub fn serve(d: &mut Driver<'_, '_>) {
    match d.get8(field::COMMAND) {
        command::INIT => init(d),
        command::INPUT => read(d),
        command::OUTPUT | command::OUTPUT_VERIFY => write(d),
        command::OPEN | command::CLOSE | command::INPUT_FLUSH | command::OUTPUT_FLUSH => {
            d.finish(None);
        }
        _ => unknown(d),
    }
}

fn days(d: &mut Driver<'_, '_>) -> u16 {
    d.g.read16(SEGMENT, driver::DAYS)
}

fn set_days(d: &mut Driver<'_, '_>, days: u16) {
    d.g.write16(SEGMENT, driver::DAYS, days);
}

const fn from_bcd(byte: u8) -> u8 {
    (byte >> 4) * 10 + (byte & 0x0F)
}

const fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

/// `INIT`: the date from the clock card, or none kept.
fn init(d: &mut Driver<'_, '_>) {
    if d.phase() == READ_TICKS {
        if !d.g.regs().flag(flag::CF) {
            let century = from_bcd(d.g.r8(Reg8::Ch));
            let year = u16::from(century) * 100 + u16::from(from_bcd(d.g.r8(Reg8::Cl)));
            let month = from_bcd(d.g.r8(Reg8::Dh));
            let day = from_bcd(d.g.r8(Reg8::Dl));
            if let Some(days) = days_of(year, month, day) {
                set_days(d, days);
            }
        }
        d.finish(None);
        return;
    }
    set_days(d, 0);
    d.g.set_r8(Reg8::Ah, 0x04);
    d.g.regs_mut().set_flag(flag::CF, true);
    d.set_phase(READ_TICKS);
    d.bios(0x1A);
}

/// The time of day `ticks` stand for: hours, minutes, seconds, hundredths.
#[must_use]
pub fn time_of_ticks(ticks: u32) -> (u8, u8, u8, u8) {
    let hundredths =
        (u64::from(ticks) * HUNDREDTHS_PER_DAY / TICKS_PER_DAY).min(HUNDREDTHS_PER_DAY - 1);
    let part = |n: u64| u8::try_from(n).unwrap_or(0);
    (
        part(hundredths / 360_000),
        part(hundredths / 6_000 % 60),
        part(hundredths / 100 % 60),
        part(hundredths % 100),
    )
}

fn read(d: &mut Driver<'_, '_>) {
    if d.phase() != READ_TICKS {
        d.g.set_r8(Reg8::Ah, 0x00);
        d.set_phase(READ_TICKS);
        d.bios(0x1A);
        return;
    }
    let ticks = u32::from(d.g.r16(Reg16::Cx)) << 16 | u32::from(d.g.r16(Reg16::Dx));
    let midnights = d.g.r8(Reg8::Al);
    if midnights != 0 {
        let next = days(d).saturating_add(u16::from(midnights));
        set_days(d, next);
    }
    let (hours, minutes, seconds, hundredths) = time_of_ticks(ticks);
    let [low, high] = days(d).to_le_bytes();
    let buffer = d.get_far(field::ADDRESS);
    let record = [low, high, minutes, hours, hundredths, seconds];
    d.g.write_bytes(buffer.segment, buffer.offset, &record);
    d.set16(field::COUNT, 6);
    d.finish(None);
}

fn write(d: &mut Driver<'_, '_>) {
    let buffer = d.get_far(field::ADDRESS);
    let mut record = [0u8; 6];
    d.g.read_bytes(buffer.segment, buffer.offset, &mut record);
    let [low, high, minutes, hours, hundredths, seconds] = record;
    match d.phase() {
        SET_TICKS => {
            let (year, month, day) = civil_of(u16::from_le_bytes([low, high]));
            let century = u8::try_from(year / 100).unwrap_or(19);
            let year = u8::try_from(year % 100).unwrap_or(0);
            d.g.set_r8(Reg8::Ah, 0x05);
            d.g.set_r16(
                Reg16::Cx,
                u16::from_le_bytes([to_bcd(year), to_bcd(century)]),
            );
            d.g.set_r16(Reg16::Dx, u16::from_le_bytes([to_bcd(day), to_bcd(month)]));
            d.set_phase(SET_CARD_DATE);
            d.bios(0x1A);
        }
        SET_CARD_DATE => {
            d.g.set_r8(Reg8::Ah, 0x03);
            d.g.set_r16(
                Reg16::Cx,
                u16::from_le_bytes([to_bcd(minutes), to_bcd(hours)]),
            );
            d.g.set_r16(Reg16::Dx, u16::from_le_bytes([0, to_bcd(seconds)]));
            d.set_phase(SET_CARD_TIME);
            d.bios(0x1A);
        }
        SET_CARD_TIME => d.finish(None),
        _ => {
            set_days(d, u16::from_le_bytes([low, high]));
            let hundredths = u64::from(hours) * 360_000
                + u64::from(minutes) * 6_000
                + u64::from(seconds) * 100
                + u64::from(hundredths);
            let ticks = u32::try_from(
                hundredths.min(HUNDREDTHS_PER_DAY - 1) * TICKS_PER_DAY / HUNDREDTHS_PER_DAY,
            )
            .unwrap_or(0);
            let (low, high) = crate::dos::engine::words(ticks);
            d.g.set_r8(Reg8::Ah, 0x01);
            d.g.set_r16(Reg16::Cx, high);
            d.g.set_r16(Reg16::Dx, low);
            d.set_phase(SET_TICKS);
            d.bios(0x1A);
        }
    }
}
