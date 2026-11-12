//! Countries: the conventions functions `38h` and `65h` report and
//! `COUNTRY=` chooses among, all on code page 437, and the tables in the
//! system ROM their pointers name — upper case, file-name upper case, the
//! characters that end a file name, the collating sequence and the empty
//! double-byte table.

use super::data::driver;
use super::engine::{Far, Kernel};
use super::fs::error;
use super::name::{upper, upper_table};
use super::rom::entry;

/// The only code page the kernel has tables for.
pub const CODE_PAGE: u16 = 437;

/// What function `6523h` answers of a character, in AX.
pub mod answer {
    pub const NO: u16 = 0;
    pub const YES: u16 = 1;
    pub const NEITHER: u16 = 2;
}

/// A country's conventions, as its information record holds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Country {
    /// Its code: the international dialling prefix.
    pub code: u16,
    /// `0` month-day-year, `1` day-month-year, `2` year-month-day.
    pub date: u16,
    /// The currency symbol, ASCIZ.
    pub currency: [u8; 5],
    /// The thousands separator.
    pub thousands: u8,
    /// The decimal separator.
    pub decimal: u8,
    /// The date separator.
    pub date_separator: u8,
    /// The time separator.
    pub time_separator: u8,
    /// Bit 0 the symbol follows the value, bit 1 a space between them.
    pub currency_format: u8,
    /// Digits after the decimal separator in a sum of money.
    pub digits: u8,
    /// `0` a 12-hour clock, `1` a 24-hour one.
    pub clock: u8,
    /// The data-list separator.
    pub list: u8,
    /// The letter for yes, upper case.
    pub yes: u8,
    /// The letter for no.
    pub no: u8,
}

/// A country: its code, date order, currency symbol, the thousands,
/// decimal, date and time separators, the currency's format and digits, its
/// clock, and the list separator with the letters for yes and no.
const fn country(
    code: u16,
    date: u16,
    currency: &[u8],
    separators: [u8; 4],
    money: [u8; 2],
    clock: u8,
    words: [u8; 3],
) -> Country {
    let mut symbol = [0u8; 5];
    let mut i = 0;
    while i < currency.len() && i < 4 {
        symbol[i] = currency[i];
        i += 1;
    }
    let [thousands, decimal, date_separator, time_separator] = separators;
    let [currency_format, digits] = money;
    let [list, yes, no] = words;
    Country {
        code,
        date,
        currency: symbol,
        thousands,
        decimal,
        date_separator,
        time_separator,
        currency_format,
        digits,
        clock,
        list,
        yes,
        no,
    }
}

/// The countries `COUNTRY=` may choose, the first the default.
pub const COUNTRIES: [Country; 13] = [
    country(1, 0, b"$", *b",.-:", [0, 2], 0, *b",YN"),
    country(44, 1, &[0x9C], *b",./:", [0, 2], 1, *b",YN"),
    country(61, 1, b"$", *b",.-:", [0, 2], 1, *b",YN"),
    country(33, 1, b"F", *b" ,/:", [3, 2], 1, *b";ON"),
    country(49, 1, b"DM", *b".,.:", [3, 2], 1, *b";JN"),
    country(31, 1, &[0x9F], *b".,-:", [2, 2], 1, *b";JN"),
    country(39, 1, b"L.", *b".,/.", [2, 0], 1, *b";SN"),
    country(34, 1, &[0x9E], *b".,/:", [3, 0], 1, *b";SN"),
    country(41, 1, b"Fr.", *b"'..,", [2, 2], 1, *b";JN"),
    country(45, 1, b"kr", *b".,-.", [2, 2], 1, *b";JN"),
    country(46, 2, b"Kr", *b" ,-.", [3, 2], 1, *b";JN"),
    country(47, 1, b"Kr", *b".,..", [2, 2], 1, *b";JN"),
    country(358, 1, b"mk", *b" ,..", [3, 2], 1, *b";KE"),
];

/// The country `code` names, if the kernel has it.
#[must_use]
pub fn find(code: u16) -> Option<&'static Country> {
    COUNTRIES.iter().find(|c| c.code == code)
}

/// Bytes of a country information record.
pub const INFO: usize = 34;

/// Where an information record holds each of a country's conventions.
pub mod field {
    pub const DATE: usize = 0x00;
    pub const CURRENCY: usize = 0x02;
    pub const THOUSANDS: usize = 0x07;
    pub const DECIMAL: usize = 0x09;
    pub const DATE_SEPARATOR: usize = 0x0B;
    pub const TIME_SEPARATOR: usize = 0x0D;
    pub const CURRENCY_FORMAT: usize = 0x0F;
    pub const DIGITS: usize = 0x10;
    pub const CLOCK: usize = 0x11;
    /// The far address of the case-mapping routine.
    pub const CASE_MAP: usize = 0x12;
    pub const LIST: usize = 0x16;
}

impl Country {
    /// Its information record, as function `38h` copies it out.
    #[must_use]
    pub fn info(&self) -> [u8; INFO] {
        let mut out = [0u8; INFO];
        out[field::DATE..field::DATE + 2].copy_from_slice(&self.date.to_le_bytes());
        out[field::CURRENCY..field::CURRENCY + 5].copy_from_slice(&self.currency);
        out[field::THOUSANDS] = self.thousands;
        out[field::DECIMAL] = self.decimal;
        out[field::DATE_SEPARATOR] = self.date_separator;
        out[field::TIME_SEPARATOR] = self.time_separator;
        out[field::CURRENCY_FORMAT] = self.currency_format;
        out[field::DIGITS] = self.digits;
        out[field::CLOCK] = self.clock;
        out[field::CASE_MAP..field::CASE_MAP + 2].copy_from_slice(&entry::CASE_MAP.to_le_bytes());
        out[field::CASE_MAP + 2..field::CASE_MAP + 4]
            .copy_from_slice(&crate::bios::layout::SEGMENT.to_le_bytes());
        out[field::LIST] = self.list;
        out
    }

    /// What function `6523h` makes of `byte`.
    #[must_use]
    pub fn answer(&self, byte: u8) -> u16 {
        match upper(byte) {
            b if b == self.yes => answer::YES,
            b if b == self.no => answer::NO,
            _ => answer::NEITHER,
        }
    }
}

impl Kernel<'_, '_> {
    /// The country chosen.
    pub fn country(&mut self) -> &'static Country {
        let code = self.get16(driver::COUNTRY);
        find(code).unwrap_or(&COUNTRIES[0])
    }

    /// Chooses the country `code`; `false` for one the kernel lacks.
    pub fn set_country(&mut self, code: u16) -> bool {
        if find(code).is_none() {
            return false;
        }
        self.set16(driver::COUNTRY, code);
        true
    }
}

/// The byte a country's collating sequence sorts `byte` as: its upper case,
/// an accented capital as its plain letter.
#[must_use]
pub const fn collate(byte: u8) -> u8 {
    match upper(byte) {
        0x80 => b'C',
        0x8E | 0x8F | 0x92 => b'A',
        0x90 => b'E',
        0x99 => b'O',
        0x9A => b'U',
        0xA5 => b'N',
        other => other,
    }
}

/// The upper-case table: its length, then bytes `80h`–`FFh` in upper case.
#[must_use]
pub fn upper_record() -> [u8; 130] {
    let mut out = [0u8; 130];
    out[..2].copy_from_slice(&128u16.to_le_bytes());
    out[2..].copy_from_slice(upper_table());
    out
}

/// The collating sequence: its length, then every byte's weight.
#[must_use]
pub fn collate_record() -> [u8; 258] {
    let mut out = [0u8; 258];
    out[..2].copy_from_slice(&256u16.to_le_bytes());
    for (byte, slot) in (0..=255u8).zip(&mut out[2..]) {
        *slot = collate(byte);
    }
    out
}

/// The characters that end a file name, as the terminator table lists them.
pub const TERMINATORS: &[u8; 14] = b".\"/\\[]:|<>+=;,";

/// The file-name terminator table: its length, then the legal range, the
/// excluded range, and the terminators.
#[must_use]
pub fn terminator_record() -> [u8; 24] {
    let mut out = [0u8; 24];
    out[..2].copy_from_slice(&22u16.to_le_bytes());
    out[2..10].copy_from_slice(&[0x01, 0x00, 0xFF, 0x00, 0x00, 0x20, 0x02, 14]);
    out[10..].copy_from_slice(TERMINATORS);
    out
}

/// The double-byte lead table: none, then its end.
pub const DBCS_RECORD: [u8; 4] = [0; 4];

/// Function `65h`, the extended country information, for the country
/// `country` (`FFFFh` the chosen one) and code page `page` (`FFFFh` the
/// active one): subfunctions `01h`–`07h` copy a record or a pointer to
/// `buffer`, of `size` bytes, and return what they wrote; `20h`–`22h` and
/// `A0h`–`A2h` put characters, a counted string or an ASCIZ one in upper
/// case; `23h` answers whether a character means yes.
///
/// # Errors
///
/// The extended error DOS answers with.
pub fn extended(k: &mut Kernel<'_, '_>, al: u8, request: Extended) -> Result<u16, u16> {
    let chosen = k.country();
    match al {
        0x20 | 0xA0 => return Ok(u16::from(upper(request.character))),
        0x21 | 0xA1 => {
            upper_in_place(k, request.buffer, request.size);
            return Ok(0);
        }
        0x22 | 0xA2 => {
            upper_in_place(k, request.buffer, 0xFFFF);
            return Ok(0);
        }
        0x23 => return Ok(chosen.answer(request.character)),
        _ => {}
    }
    let country = match request.country {
        0xFFFF => chosen,
        code => find(code).ok_or(error::FILE_NOT_FOUND)?,
    };
    if !matches!(request.page, 0xFFFF | CODE_PAGE) {
        return Err(error::FILE_NOT_FOUND);
    }
    let pointer = |offset: u16| Far::new(crate::bios::layout::SEGMENT, offset);
    let table = match al {
        0x01 => {
            if request.size < 5 {
                return Err(error::INVALID_FUNCTION);
            }
            let mut record = [0u8; 7 + INFO];
            record[0] = 0x01;
            record[1..3].copy_from_slice(&u16::try_from(4 + INFO).unwrap_or(0).to_le_bytes());
            record[3..5].copy_from_slice(&country.code.to_le_bytes());
            record[5..7].copy_from_slice(&CODE_PAGE.to_le_bytes());
            record[7..].copy_from_slice(&country.info());
            let len = usize::from(request.size).min(record.len());
            k.g.write_bytes(
                request.buffer.segment,
                request.buffer.offset,
                &record[..len],
            );
            return Ok(u16::try_from(len).unwrap_or(0));
        }
        0x02 | 0x04 => pointer(entry::UPPER),
        0x05 => pointer(entry::TERMINATORS),
        0x06 => pointer(entry::COLLATE),
        0x07 => pointer(entry::DBCS),
        _ => return Err(error::INVALID_FUNCTION),
    };
    if request.size < 5 {
        return Err(error::INVALID_FUNCTION);
    }
    let mut record = [al, 0, 0, 0, 0];
    record[1..3].copy_from_slice(&table.offset.to_le_bytes());
    record[3..5].copy_from_slice(&table.segment.to_le_bytes());
    k.g.write_bytes(request.buffer.segment, request.buffer.offset, &record);
    Ok(5)
}

/// What a function `65h` call names.
#[derive(Clone, Copy, Debug)]
pub struct Extended {
    /// The country, `FFFFh` the chosen one.
    pub country: u16,
    /// The code page, `FFFFh` the active one.
    pub page: u16,
    /// The buffer, or the string.
    pub buffer: Far,
    /// The buffer's size, or the string's length.
    pub size: u16,
    /// The character a single-character subfunction takes.
    pub character: u8,
}

/// Puts the string at `at` in upper case: `len` bytes, or up to its
/// terminator for `FFFFh`, within its segment.
fn upper_in_place(k: &mut Kernel<'_, '_>, at: Far, len: u16) {
    let asciz = len == 0xFFFF;
    let limit = if asciz { 0xFFFF } else { len };
    for n in 0..limit {
        let place = at.add(n);
        let byte = k.g.read8(place.segment, place.offset);
        if asciz && byte == 0 {
            return;
        }
        k.g.write8(place.segment, place.offset, upper(byte));
    }
}

/// Function `38h`: with `set`, chooses the country `code`; else copies the
/// country `code`'s record — `0` the chosen one — to `buffer`. Returns the
/// country's code.
///
/// # Errors
///
/// [`error::FILE_NOT_FOUND`], as DOS answers for a country it has no
/// information for.
pub fn country_call(k: &mut Kernel<'_, '_>, code: u16, buffer: Far, set: bool) -> Result<u16, u16> {
    if set {
        return if k.set_country(code) {
            Ok(code)
        } else {
            Err(error::FILE_NOT_FOUND)
        };
    }
    let country = if code == 0 {
        k.country()
    } else {
        find(code).ok_or(error::FILE_NOT_FOUND)?
    };
    k.g.write_bytes(buffer.segment, buffer.offset, &country.info());
    Ok(country.code)
}
