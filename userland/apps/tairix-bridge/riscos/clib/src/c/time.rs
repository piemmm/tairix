//! `time.h` as RISC OS 5.30's library has it: `time_t` an unsigned count of
//! seconds since 1970 began, UTC; every fourth year a leap year, so its dates
//! from March 2100 on are a day behind the Gregorian calendar's; local time
//! the configured zone, an hour on wherever daylight saving is configured,
//! whatever the date; and `strftime` English in the C locale, the territory's
//! words and forms in another.

use tairix_riscos_kernel::cmos;
use tairix_riscos_kernel::os::{Fail, Os, Regs, UNIX_EPOCH};
use tairix_riscos_kernel::swi::{os as kos, territory as territory_swi, X};
use tairix_riscos_kernel::word;

use super::statics::{at, ORDINAL_WORDS, TERRITORY_TEXT_BYTES, TIME_TEXT_BYTES, TM_WORDS};
use super::stdio::format::{decimal, Sink};
use super::stdio::stream;
use super::{fpa, locale};
use crate::kernel::context;
use crate::kernel::statics::Statics;

const DAY: u32 = 86_400;
/// `(time_t)-1`: no time.
const NO_TIME: u32 = u32::MAX;
/// `tm_isdst` wherever daylight saving is configured, as genuine sets it.
const DST_CONFIGURED: i32 = 0x8000;
/// A quarter hour, the unit the configured zone is kept in, and an hour.
const QUARTER_HOUR: i32 = 900;
const HOUR: i32 = 3600;

/// A `struct tm`'s fields, in its order.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Tm {
    sec: i32,
    min: i32,
    hour: i32,
    mday: i32,
    mon: i32,
    year: i32,
    wday: i32,
    yday: i32,
    isdst: i32,
}

/// What `gmtime` and `localtime` answer for `(time_t)-1`: genuine's mark,
/// the first moment of 1900.
const NO_TIME_TM: Tm = Tm {
    sec: 0,
    min: 0,
    hour: 0,
    mday: 1,
    mon: 0,
    year: 0,
    wday: 0,
    yday: 0,
    isdst: 0,
};

impl Tm {
    fn read(os: &mut Os<'_>, address: u32) -> Result<Self, Fail> {
        let mut words = [0; TM_WORDS as usize];
        os.as_caller().read_words(address, &mut words)?;
        let [sec, min, hour, mday, mon, year, wday, yday, isdst] = words.map(u32::cast_signed);
        Ok(Self {
            sec,
            min,
            hour,
            mday,
            mon,
            year,
            wday,
            yday,
            isdst,
        })
    }

    fn write(self, os: &mut Os<'_>, address: u32) -> Result<(), Fail> {
        let Self {
            sec,
            min,
            hour,
            mday,
            mon,
            year,
            wday,
            yday,
            isdst,
        } = self;
        os.as_caller().write_words(
            address,
            &[sec, min, hour, mday, mon, year, wday, yday, isdst].map(i32::cast_unsigned),
        )
    }
}

/// Days from 1968, the leap year starting the four-year cycle 1970 is in,
/// to 1970; a cycle's days; and the days before each month of a common year.
const DAYS_1968_TO_1970: i64 = 731;
const CYCLE_DAYS: i64 = 1461;
const DAYS_BEFORE: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

fn days_before(month: usize, leap: bool) -> i64 {
    DAYS_BEFORE[month] + i64::from(leap && month >= 2)
}

/// Days from 1970 to the first of `month`, from nought, of `year`, counted
/// from 1900.
fn days_to(year: i64, month: usize) -> i64 {
    let since_1968 = year - 68;
    let into = since_1968.rem_euclid(4);
    let year_start =
        since_1968.div_euclid(4) * CYCLE_DAYS + if into == 0 { 0 } else { 366 + (into - 1) * 365 };
    year_start + days_before(month, into == 0) - DAYS_1968_TO_1970
}

/// The date `days` after 1970 began, its time of day nought; none where its
/// year is past `int`'s range.
fn date_of(days: i64) -> Option<Tm> {
    let since_1968 = days + DAYS_1968_TO_1970;
    let mut day = since_1968.rem_euclid(CYCLE_DAYS);
    let into = if day < 366 {
        0
    } else {
        day -= 366;
        let into = 1 + day / 365;
        day %= 365;
        into
    };
    let leap = into == 0;
    let month = (0..12)
        .rev()
        .find(|&m| day >= days_before(m, leap))
        .unwrap_or(0);
    Some(Tm {
        mday: i32::try_from(day - days_before(month, leap) + 1).ok()?,
        mon: i32::try_from(month).ok()?,
        year: i32::try_from(68 + 4 * since_1968.div_euclid(CYCLE_DAYS) + into).ok()?,
        wday: i32::try_from((days + 4).rem_euclid(7)).ok()?,
        yday: i32::try_from(day).ok()?,
        ..Tm::default()
    })
}

/// The `struct tm` of `time` seconds after 1970 began.
fn broken_down(time: u32) -> Tm {
    let rest = (time % DAY).cast_signed();
    let date = date_of(i64::from(time / DAY)).unwrap_or(NO_TIME_TM);
    Tm {
        sec: rest % 60,
        min: rest / 60 % 60,
        hour: rest / 3600,
        ..date
    }
}

/// The configured zone's offset from UTC in seconds, an hour more where
/// daylight saving is configured, and whether it is.
fn configured_zone(os: &mut Os<'_>) -> Result<(i32, bool), Fail> {
    let quarters = i8::from_le_bytes([read_cmos(os, cmos::TIME_ZONE)?]);
    let dst = read_cmos(os, cmos::ALARM_AND_TIME)? & cmos::DST != 0;
    Ok((
        i32::from(quarters) * QUARTER_HOUR + if dst { HOUR } else { 0 },
        dst,
    ))
}

fn read_cmos(os: &mut Os<'_>, location: u8) -> Result<u8, Fail> {
    let mut regs = stream::registers(&[cmos::READ_CMOS, u32::from(location)]);
    os.perform(X | kos::BYTE, &mut regs)?;
    Ok(regs.byte(2))
}

/// The local `struct tm` of `time`: its configured zone's, whatever the date.
fn local(os: &mut Os<'_>, time: u32) -> Result<Tm, Fail> {
    if time == NO_TIME {
        return Ok(NO_TIME_TM);
    }
    let (offset, dst) = configured_zone(os)?;
    Ok(Tm {
        isdst: if dst { DST_CONFIGURED } else { 0 },
        ..broken_down(time.wrapping_add(offset.cast_unsigned()))
    })
}

/// `OS_ReadMonotonicTime`: centiseconds since the machine started.
pub(super) fn monotonic_time(os: &mut Os<'_>) -> Result<u32, Fail> {
    let mut regs = Regs::default();
    os.perform(X | kos::READ_MONOTONIC_TIME, &mut regs)?;
    Ok(regs.r[0])
}

/// `clock()`: centiseconds since the program started.
pub(super) fn clock(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let started = statics.read(os, at::CLOCK_START)?;
    regs.r[0] = monotonic_time(os)?.wrapping_sub(started);
    Ok(())
}

/// `difftime(time1, time0)`: their difference in seconds, a double.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(super) fn difftime(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    fpa::answer(os, f64::from(regs.r[0]) - f64::from(regs.r[1]));
    Ok(())
}

/// The clock's five-byte time now: centiseconds from 1900, UTC.
fn centiseconds(os: &mut Os<'_>, statics: Statics) -> Result<u64, Fail> {
    let block = statics.field(at::FIVE_BYTE_TIME);
    os.as_caller().write_byte(block, word::read::UTC)?;
    let mut read = stream::registers(&[word::READ_CLOCK, block]);
    os.perform(X | kos::WORD, &mut read)?;
    let mut five = [0; 8];
    os.as_caller().read_bytes(block, &mut five[..5])?;
    Ok(u64::from_le_bytes(five))
}

/// The time now as `time` answers it.
pub(crate) fn now(os: &mut Os<'_>, statics: Statics) -> Result<u32, Fail> {
    Ok(time_t(centiseconds(os, statics)?))
}

/// The five-byte time `centiseconds` as a `time_t`: `(time_t)-1` before 1970
/// began and past `time_t`'s range.
fn time_t(centiseconds: u64) -> u32 {
    centiseconds
        .checked_sub(UNIX_EPOCH)
        .and_then(|since| u32::try_from(since / 100).ok())
        .unwrap_or(NO_TIME)
}

/// The only base `timespec_get` answers for.
const TIME_UTC: u32 = 1;
/// Nanoseconds in a centisecond, the clock's step.
const NANOSECONDS_PER_CENTISECOND: u64 = 10_000_000;

/// `timespec_get(ts, base)`: for `TIME_UTC`, the time now as `time` gives
/// it and the nanoseconds past its second, to the centisecond, answering the
/// base; nought for any other base, `ts` untouched.
pub(crate) fn timespec_get(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (ts, base) = (regs.r[0], regs.r[1]);
    if base != TIME_UTC {
        regs.r[0] = 0;
        return Ok(());
    }
    let statics = context::statics_at(os, regs.r[10])?;
    let centiseconds = centiseconds(os, statics)?;
    let seconds = time_t(centiseconds);
    let nanoseconds = u32::try_from(centiseconds % 100 * NANOSECONDS_PER_CENTISECOND).unwrap_or(0);
    os.as_caller().write_words(ts, &[seconds, nanoseconds])?;
    regs.r[0] = TIME_UTC;
    Ok(())
}

/// `time(timer)`: the time now, stored at `timer` too where it is not null.
pub(super) fn time(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let now = now(os, statics)?;
    if regs.r[0] != 0 {
        os.as_caller().write_word(regs.r[0], now)?;
    }
    regs.r[0] = now;
    Ok(())
}

/// `tm` normalised, its weekday and day of the year set and `tm_isdst` left
/// as it is, and the time it names: read as UTC whatever the zone, an hour
/// earlier where `tm_isdst` is positive, `(time_t)-1` past `time_t`'s range.
/// None where the normalised year is past `int`'s.
fn normalised(tm: Tm) -> Option<(Tm, u32)> {
    let second = i64::from(tm.sec);
    let minute = i64::from(tm.min) + second.div_euclid(60);
    let hour = i64::from(tm.hour) + minute.div_euclid(60);
    let month = i64::from(tm.mon);
    let year = i64::from(tm.year) + month.div_euclid(12);
    let first_of_month = days_to(year, usize::try_from(month.rem_euclid(12)).ok()?);
    let days = first_of_month + i64::from(tm.mday) - 1 + hour.div_euclid(24);
    let (second, minute, hour) = (
        second.rem_euclid(60),
        minute.rem_euclid(60),
        hour.rem_euclid(24),
    );
    let normal = Tm {
        sec: i32::try_from(second).ok()?,
        min: i32::try_from(minute).ok()?,
        hour: i32::try_from(hour).ok()?,
        isdst: tm.isdst,
        ..date_of(days)?
    };
    let summer = if tm.isdst > 0 { i64::from(HOUR) } else { 0 };
    let seconds = days * i64::from(DAY) + hour * 3600 + minute * 60 + second - summer;
    let time = u32::try_from(seconds).unwrap_or(NO_TIME);
    Some((normal, time))
}

/// `mktime(timeptr)`.
pub(super) fn mktime(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let address = regs.r[0];
    regs.r[0] = match normalised(Tm::read(os, address)?) {
        Some((normal, time)) => {
            normal.write(os, address)?;
            time
        }
        None => NO_TIME,
    };
    Ok(())
}

/// A `struct tm` written to the library's buffer at `field`, and its address
/// answered.
fn answer_tm(
    os: &mut Os<'_>,
    regs: &mut Regs,
    statics: Statics,
    field: u32,
    tm: Tm,
) -> Result<(), Fail> {
    let buffer = statics.field(field);
    tm.write(os, buffer)?;
    regs.r[0] = buffer;
    Ok(())
}

/// `gmtime(timer)`.
pub(super) fn gmtime(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let time = os.as_caller().read_word(regs.r[0])?;
    let tm = if time == NO_TIME {
        NO_TIME_TM
    } else {
        broken_down(time)
    };
    answer_tm(os, regs, statics, at::GMTIME, tm)
}

/// `localtime(timer)`.
pub(super) fn localtime(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let time = os.as_caller().read_word(regs.r[0])?;
    let tm = local(os, time)?;
    answer_tm(os, regs, statics, at::LOCALTIME, tm)
}

const WEEKDAYS: [&[u8]; 7] = [
    b"Sunday",
    b"Monday",
    b"Tuesday",
    b"Wednesday",
    b"Thursday",
    b"Friday",
    b"Saturday",
];
const MONTHS: [&[u8]; 12] = [
    b"January",
    b"February",
    b"March",
    b"April",
    b"May",
    b"June",
    b"July",
    b"August",
    b"September",
    b"October",
    b"November",
    b"December",
];
/// A name for a weekday or month out of range.
const NO_NAME: &[u8] = b"???";

fn name(names: &[&'static [u8]], index: i32) -> &'static [u8] {
    usize::try_from(index)
        .ok()
        .and_then(|n| names.get(n))
        .copied()
        .unwrap_or(NO_NAME)
}

/// A name cut to its first three letters.
fn short(name: &[u8]) -> &[u8] {
    &name[..name.len().min(3)]
}

/// `asctime`'s text, `printf`'s `"%.3s %.3s%3d %.2d:%.2d:%.2d %d\n"` of
/// `tm`, its newline left off where `newline` is false.
fn write_time_text(
    os: &mut Os<'_>,
    output: &mut Output,
    tm: &Tm,
    newline: bool,
) -> Result<(), Fail> {
    output.put(os, short(name(&WEEKDAYS, tm.wday)))?;
    output.put(os, b" ")?;
    output.put(os, short(name(&MONTHS, tm.mon)))?;
    decimal(os, output, tm.mday, 1, 3)?;
    output.put(os, b" ")?;
    two_digits_apart(os, output, b":", [tm.hour, tm.min, tm.sec])?;
    output.put(os, b" ")?;
    decimal(os, output, tm.year.wrapping_add(1900), 1, 0)?;
    if newline {
        output.put(os, b"\n")?;
    }
    Ok(())
}

/// `value` as `%.2d` writes it.
fn two_digits(os: &mut Os<'_>, output: &mut Output, value: i32) -> Result<(), Fail> {
    decimal(os, output, value, 2, 0)
}

/// `values` as `%.2d` writes each, `separator` between them.
fn two_digits_apart(
    os: &mut Os<'_>,
    output: &mut Output,
    separator: &[u8],
    values: [i32; 3],
) -> Result<(), Fail> {
    for (n, value) in values.into_iter().enumerate() {
        if n > 0 {
            output.put(os, separator)?;
        }
        two_digits(os, output, value)?;
    }
    Ok(())
}

/// `tm`'s text written to the library's buffer, which holds the longest any
/// `struct tm` gives, and its address answered.
fn answer_text(os: &mut Os<'_>, regs: &mut Regs, statics: Statics, tm: &Tm) -> Result<(), Fail> {
    let mut output = Output::new(statics.field(at::TIME_TEXT), TIME_TEXT_BYTES);
    write_time_text(os, &mut output, tm, true)?;
    if output.terminated(os)?.is_none() {
        return Err(os.buffer_overflow());
    }
    regs.r[0] = output.at;
    Ok(())
}

/// `asctime(timeptr)`.
pub(super) fn asctime(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let tm = Tm::read(os, regs.r[0])?;
    answer_text(os, regs, statics, &tm)
}

/// `ctime(timer)`: `asctime(localtime(timer))`, `localtime`'s buffer left
/// holding the local time.
pub(super) fn ctime(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let time = os.as_caller().read_word(regs.r[0])?;
    let tm = local(os, time)?;
    tm.write(os, statics.field(at::LOCALTIME))?;
    answer_text(os, regs, statics, &tm)
}

/// `strftime`'s answer as it is written: where it goes, the bytes it may
/// take with its terminator, and the bytes it has come to.
struct Output {
    at: u32,
    room: u32,
    length: u32,
}

impl Output {
    /// An answer to be written at `at`, taking `room` bytes with its
    /// terminator.
    const fn new(at: u32, room: u32) -> Self {
        Self {
            at,
            room,
            length: 0,
        }
    }

    /// Whether the answer, terminated, still fits.
    const fn fits(&self) -> bool {
        self.length < self.room
    }

    /// The answer's terminator written where it fits, and its length; none,
    /// with nothing written, where the answer does not.
    fn terminated(&self, os: &mut Os<'_>) -> Result<Option<u32>, Fail> {
        if !self.fits() {
            return Ok(None);
        }
        os.as_caller()
            .write_byte(self.at.wrapping_add(self.length), 0)?;
        Ok(Some(self.length))
    }

    /// The part of `count` bytes the room left takes, and where it goes.
    fn take(&mut self, count: usize) -> (u32, u32) {
        let count = u32::try_from(count).unwrap_or(u32::MAX);
        let taken = count.min(self.room.saturating_sub(self.length));
        let place = self.at.wrapping_add(self.length);
        self.length = self.length.saturating_add(count);
        (place, taken)
    }

    /// `count` bytes of the guest's at `from`.
    fn copy(&mut self, os: &mut Os<'_>, from: u32, count: u32) -> Result<(), Fail> {
        let (place, taken) = self.take(count as usize);
        os.as_caller().copy_bytes(from, place, taken)
    }
}

impl Sink for Output {
    fn put(&mut self, os: &mut Os<'_>, bytes: &[u8]) -> Result<(), Fail> {
        let (place, taken) = self.take(bytes.len());
        os.as_caller().write_bytes(place, &bytes[..taken as usize])
    }
}

/// What a territory writes a conversion through: one of its formats, or one
/// of its standard forms.
#[derive(Clone, Copy)]
enum Form {
    Format(&'static [u8; 4]),
    Standard(u32),
}

/// The five-byte time a territory's forms are written from.
#[derive(Clone, Copy)]
enum FiveByteTime {
    /// No conversion has asked for it yet.
    Unread,
    /// The territory took the `struct tm` for no time.
    NoTime,
    /// Laid at this address.
    At(u32),
}

/// `LC_TIME`'s territory, and the five-byte time its forms are written from.
struct Territory {
    number: u32,
    time: FiveByteTime,
}

impl Territory {
    /// The five-byte time of `tm` as the territory reads it, in the library's
    /// statics; none where it takes `tm` for no time.
    fn time(&mut self, os: &mut Os<'_>, statics: Statics, tm: &Tm) -> Result<Option<u32>, Fail> {
        match self.time {
            FiveByteTime::At(block) => return Ok(Some(block)),
            FiveByteTime::NoTime => return Ok(None),
            FiveByteTime::Unread => {}
        }
        let ordinals = statics.field(at::ORDINALS);
        let block = statics.field(at::FIVE_BYTE_TIME);
        let fields: [i32; ORDINAL_WORDS as usize] = [
            0,
            tm.sec,
            tm.min,
            tm.hour,
            tm.mday,
            tm.mon.wrapping_add(1),
            tm.year.wrapping_add(1900),
        ];
        os.as_caller()
            .write_words(ordinals, &fields.map(i32::cast_unsigned))?;
        let mut convert = stream::registers(&[self.number, block, ordinals]);
        let time = os
            .perform_quietly(X | territory_swi::ORDINALS_TO_TIME, &mut convert)?
            .then_some(block);
        self.time = time.map_or(FiveByteTime::NoTime, FiveByteTime::At);
        Ok(time)
    }

    /// `form` of `tm` as the territory writes it, put to `output`; nothing
    /// where the territory refuses it.
    fn write(
        &mut self,
        os: &mut Os<'_>,
        statics: Statics,
        tm: &Tm,
        form: Form,
        output: &mut Output,
    ) -> Result<(), Fail> {
        let Some(time) = self.time(os, statics, tm)? else {
            return Ok(());
        };
        let text = statics.field(at::TERRITORY_TEXT);
        let mut convert = stream::registers(&[self.number, time, text, TERRITORY_TEXT_BYTES]);
        let swi = match form {
            Form::Format(code) => {
                let place = statics.field(at::FORMAT_CODE);
                os.as_caller().write_bytes(place, code)?;
                convert.r[4] = place;
                territory_swi::CONVERT_DATE_AND_TIME
            }
            Form::Standard(swi) => swi,
        };
        if os.perform_quietly(X | swi, &mut convert)? {
            output.copy(
                os,
                text,
                convert.r[1].wrapping_sub(text).min(TERRITORY_TEXT_BYTES),
            )?;
        }
        Ok(())
    }
}

/// The week of the year by `%U`'s count, from the first Sunday, and by
/// `%W`'s, from the first Monday, as `int` arithmetic works them.
fn sunday_week(tm: &Tm) -> i32 {
    tm.yday.wrapping_add(7).wrapping_sub(tm.wday) / 7
}

fn monday_week(tm: &Tm) -> i32 {
    tm.yday
        .wrapping_add(7)
        .wrapping_sub(tm.wday.wrapping_add(6) % 7)
        / 7
}

/// The hour on a twelve-hour clock.
fn twelve_hour(hour: i32) -> i32 {
    match hour.rem_euclid(12) {
        0 => 12,
        hour => hour,
    }
}

/// One conversion of `strftime`'s, `c` being its letter.
fn convert(
    os: &mut Os<'_>,
    statics: Statics,
    territory: &mut Option<Territory>,
    tm: &Tm,
    c: u8,
    output: &mut Output,
) -> Result<(), Fail> {
    let by_territory = |form| (territory.is_some()).then_some(form);
    let form = match c {
        b'a' => by_territory(Form::Format(b"%W3\0")),
        b'A' => by_territory(Form::Format(b"%WE\0")),
        b'b' => by_territory(Form::Format(b"%M3\0")),
        b'B' => by_territory(Form::Format(b"%MO\0")),
        b'p' => by_territory(Form::Format(b"%AM\0")),
        b'Z' => by_territory(Form::Format(b"%TZ\0")),
        b'c' => by_territory(Form::Standard(
            territory_swi::CONVERT_STANDARD_DATE_AND_TIME,
        )),
        b'x' => by_territory(Form::Standard(territory_swi::CONVERT_STANDARD_DATE)),
        b'X' => by_territory(Form::Standard(territory_swi::CONVERT_STANDARD_TIME)),
        _ => None,
    };
    if let (Some(form), Some(territory)) = (form, territory.as_mut()) {
        return territory.write(os, statics, tm, form, output);
    }
    match c {
        b'a' => output.put(os, short(name(&WEEKDAYS, tm.wday))),
        b'A' => output.put(os, name(&WEEKDAYS, tm.wday)),
        b'b' => output.put(os, short(name(&MONTHS, tm.mon))),
        b'B' => output.put(os, name(&MONTHS, tm.mon)),
        b'c' => write_time_text(os, output, tm, false),
        b'd' => two_digits(os, output, tm.mday),
        b'H' => two_digits(os, output, tm.hour),
        b'I' => two_digits(os, output, twelve_hour(tm.hour)),
        b'j' => decimal(os, output, tm.yday.wrapping_add(1), 3, 0),
        b'm' => two_digits(os, output, tm.mon.wrapping_add(1)),
        b'M' => two_digits(os, output, tm.min),
        b'p' => output.put(os, if tm.hour < 12 { b"AM" } else { b"PM" }),
        b'S' => two_digits(os, output, tm.sec),
        b'U' => two_digits(os, output, sunday_week(tm)),
        b'w' => decimal(os, output, tm.wday, 1, 0),
        b'W' => two_digits(os, output, monday_week(tm)),
        b'x' => {
            let date = [tm.mon.wrapping_add(1), tm.mday, tm.year % 100];
            two_digits_apart(os, output, b"/", date)
        }
        b'X' => two_digits_apart(os, output, b":", [tm.hour, tm.min, tm.sec]),
        b'y' => two_digits(os, output, tm.year % 100),
        b'Y' => decimal(os, output, tm.year.wrapping_add(1900), 1, 0),
        b'Z' => Ok(()),
        b'%' => output.put(os, b"%"),
        other => output.put(os, &[b'%', other]),
    }
}

/// `strftime(s, maxsize, format, timeptr)`: the bytes written, or nought —
/// the buffer holding as much as fits, unterminated — where the answer and
/// its terminator would not fit. `E` and `O` before a conversion are passed
/// over; a format ending in `%` ends the answer with one.
pub(super) fn strftime(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let mut output = Output::new(regs.r[0], regs.r[1]);
    let tm = Tm::read(os, regs.r[3])?;
    let mut territory = locale::time_territory(os, statics)?.map(|number| Territory {
        number,
        time: FiveByteTime::Unread,
    });
    let mut format = regs.r[2];
    while output.fits() {
        let c = os.as_caller().read_byte(format)?;
        format = format.wrapping_add(1);
        match c {
            0 => break,
            b'%' => {
                let mut letter = os.as_caller().read_byte(format)?;
                while matches!(letter, b'E' | b'O') {
                    format = format.wrapping_add(1);
                    letter = os.as_caller().read_byte(format)?;
                }
                if letter == 0 {
                    output.put(os, b"%")?;
                    break;
                }
                format = format.wrapping_add(1);
                convert(os, statics, &mut territory, &tm, letter, &mut output)?;
            }
            literal => output.put(os, &[literal])?,
        }
    }
    regs.r[0] = output.terminated(os)?.unwrap_or(0);
    Ok(())
}

#[cfg(test)]
mod tests;
