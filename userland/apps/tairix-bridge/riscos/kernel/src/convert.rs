//! Conversions and readings of text (`plans/RISCOS-EMULATOR.md` §4.4,
//! §4.5): the `OS_Convert…` family, `OS_BinaryToDecimal`,
//! `OS_ReadUnsigned`, and the `lib/obey` readings — GSTrans,
//! `OS_EvaluateExpression`, `OS_SubstituteArgs` and `OS_ReadArgs` — run
//! against the guest's variables.

use alloc::vec::Vec;

use tairix_obey::args::substitute;
use tairix_obey::expr::{evaluate, Answer};
use tairix_obey::gstrans::{gstrans, Flags, Read, Reader};
use tairix_obey::number::{read_unsigned, Reading};
use tairix_obey::readargs::read_args;
use tairix_riscos_arm::psr;

use crate::error::obey_error;
use crate::error::Error;
use crate::os::{len32, Fail, Os, Regs};
use crate::swi::names::CONVERT_HEX1;
use crate::swi::os::read_unsigned::{BOUNDED, BYTE_SIZED, TERMINATED};

/// An integer in signed decimal, as RISC OS writes one: `-` before a
/// negative one, no leading zeros.
pub(crate) struct Decimal {
    digits: [u8; 12],
    len: usize,
}

impl Decimal {
    /// `n` written out.
    pub(crate) fn of(n: i32) -> Self {
        let mut digits = [0; 12];
        let len = tairix_util::fmt::format_i32(n, &mut digits).len();
        Self { digits, len }
    }

    /// The text.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.digits[..self.len]
    }
}

/// The longest text a reading takes from the guest.
const LONGEST_TEXT: usize = 1 << 16;
/// The most of a number's text `OS_ReadUnsigned` reads: its spaces, base
/// and 64 binary digits, with room to spare.
const NUMBER_TEXT: usize = 256;
/// The `WIDE` word that asks `OS_ReadUnsigned` for its 64-bit form.
const WIDE: u32 = u32::from_le_bytes(*b"WIDE");

/// "That conversion is not supported".
const UNSUPPORTED: Error = Error {
    number: 0x1F1,
    token: "UnsupportedConversion",
};
/// "Station number out of range".
const BAD_STATION: Error = Error {
    number: 0x306,
    token: "BadStationNumber",
};
/// "Network number out of range".
const BAD_NETWORK: Error = Error {
    number: 0x307,
    token: "BadNetworkNumber",
};

/// `OS_ConvertFixedNetStation`.
const CONVERT_FIXED_NET_STATION: u32 = CONVERT_HEX1 + 25;
/// `OS_ConvertNetStation`.
const CONVERT_NET_STATION: u32 = CONVERT_HEX1 + 26;
/// `OS_ConvertFixedFileSize`.
const CONVERT_FIXED_FILE_SIZE: u32 = CONVERT_HEX1 + 27;
/// `OS_ConvertFileSize`.
const CONVERT_FILE_SIZE: u32 = CONVERT_HEX1 + 28;
/// `OS_ConvertVariform`, the last of the number conversions.
pub(crate) const CONVERT_VARIFORM: u32 = CONVERT_HEX1 + 29;

/// What a fixed conversion writes, by SWI from `OS_ConvertHex1`.
#[derive(Clone, Copy)]
enum Fixed {
    Hex(u32),
    Cardinal(u32),
    Integer(u32),
    Binary(u32),
    SpacedCardinal(u32),
    SpacedInteger(u32),
}

/// The fixed conversion of SWI `offset` past `OS_ConvertHex1`.
const fn fixed(offset: u32) -> Option<Fixed> {
    Some(match offset {
        0 => Fixed::Hex(1),
        1 => Fixed::Hex(2),
        2 => Fixed::Hex(4),
        3 => Fixed::Hex(6),
        4 => Fixed::Hex(8),
        5..=8 => Fixed::Cardinal(offset - 4),
        9..=12 => Fixed::Integer(offset - 8),
        13..=16 => Fixed::Binary(offset - 12),
        17..=20 => Fixed::SpacedCardinal(offset - 16),
        21..=24 => Fixed::SpacedInteger(offset - 20),
        _ => return None,
    })
}

/// `value`'s low `bytes` bytes, sign-extended when `signed`.
const fn sized(value: u64, bytes: u32, signed: bool) -> i128 {
    let bits = bytes * 8;
    let mask = if bits >= 64 {
        u64::MAX
    } else {
        (1 << bits) - 1
    };
    let unsigned = value & mask;
    if signed && bits < 128 && unsigned >> (bits - 1) & 1 != 0 {
        unsigned as i128 - (1i128 << bits)
    } else {
        unsigned as i128
    }
}

/// `n` in decimal, a space every three digits from the right when
/// `separator` is one.
fn decimal(n: i128, separator: Option<&[u8]>) -> Vec<u8> {
    let mut digits = Vec::new();
    let mut magnitude = n.unsigned_abs();
    loop {
        digits.push(b'0' + (magnitude % 10) as u8);
        magnitude /= 10;
        if magnitude == 0 {
            break;
        }
    }
    let mut out = Vec::new();
    if n < 0 {
        out.push(b'-');
    }
    for (i, &digit) in digits.iter().enumerate().rev() {
        out.push(digit);
        if let Some(separator) = separator {
            if i % 3 == 0 && i != 0 {
                out.extend_from_slice(separator);
            }
        }
    }
    out
}

/// A file size as RISC OS writes one: divided by 1024, rounding halves up,
/// until below 4096, with its unit; `fixed` pads it to eleven characters.
fn file_size(bytes: u64, fixed: bool) -> Vec<u8> {
    let mut value = bytes;
    let mut unit = 0;
    while value >= 4096 && unit < 6 {
        value = (value + 512) / 1024;
        unit += 1;
    }
    let prefix = b" kMGTPE"[unit];
    let mut out = Vec::new();
    let number = decimal(i128::from(value), None);
    if fixed {
        out.resize(4usize.saturating_sub(number.len()), b' ');
        out.extend_from_slice(&number);
        out.push(b' ');
        out.push(prefix);
        out.extend_from_slice(b"byte");
        out.push(if value == 1 { b' ' } else { b's' });
    } else {
        out.extend_from_slice(&number);
        out.push(b' ');
        if unit > 0 {
            out.push(prefix);
        }
        out.extend_from_slice(b"byte");
        if value != 1 {
            out.push(b's');
        }
    }
    out
}

/// `bytes` as hex digits, the highest first, upper or lower case.
fn hex(bytes: &[u8], digits: usize, upper: bool) -> Vec<u8> {
    let table: &[u8; 16] = if upper {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    (0..digits)
        .rev()
        .map(|nibble| {
            let byte = bytes.get(nibble / 2).copied().unwrap_or(0);
            table[usize::from((byte >> (4 * (nibble % 2))) & 0xF)]
        })
        .collect()
}

impl Os<'_> {
    /// Writes a conversion's `text` and a zero into R1's buffer of R2 bytes:
    /// R0 the buffer, R1 its zero, R2 the room left; or, where the zero does
    /// not fit, as much of the text as does, R2 zero and "Buffer overflow".
    fn converted(&mut self, regs: &mut Regs, text: &[u8]) -> Result<(), Fail> {
        let (buffer, room) = (regs.r[1], regs.r[2] as usize);
        if text.len() >= room {
            self.write_bytes(buffer, &text[..room])?;
            regs.r[2] = 0;
            return Err(self.buffer_overflow());
        }
        self.write_bytes(buffer, text)?;
        let end = buffer.wrapping_add(len32(text.len()));
        self.write_byte(end, 0)?;
        regs.r[0] = buffer;
        regs.r[1] = end;
        regs.r[2] = len32(room - text.len());
        Ok(())
    }

    /// The conversions from `OS_ConvertHex1` to `OS_ConvertVariform`.
    pub(crate) fn convert(&mut self, number: u32, regs: &mut Regs) -> Result<(), Fail> {
        let offset = number - CONVERT_HEX1;
        let value = u64::from(regs.r[0]);
        let text = match fixed(offset) {
            Some(Fixed::Hex(digits)) => hex(&regs.r[0].to_le_bytes(), digits as usize, true),
            Some(Fixed::Cardinal(bytes)) => decimal(sized(value, bytes, false), None),
            Some(Fixed::Integer(bytes)) => decimal(sized(value, bytes, true), None),
            Some(Fixed::Binary(bytes)) => (0..bytes * 8)
                .rev()
                .map(|bit| if value >> bit & 1 != 0 { b'1' } else { b'0' })
                .collect(),
            Some(Fixed::SpacedCardinal(bytes)) => decimal(sized(value, bytes, false), Some(b" ")),
            Some(Fixed::SpacedInteger(bytes)) => decimal(sized(value, bytes, true), Some(b" ")),
            None => match number {
                CONVERT_FIXED_NET_STATION | CONVERT_NET_STATION => {
                    self.net_station(regs.r[0], number == CONVERT_FIXED_NET_STATION)?
                }
                CONVERT_FIXED_FILE_SIZE => file_size(value, true),
                CONVERT_FILE_SIZE => file_size(value, false),
                _ => return self.variform(regs),
            },
        };
        self.converted(regs, &text)
    }

    /// `OS_ConvertFixedNetStation` and `OS_ConvertNetStation`.
    fn net_station(&mut self, block: u32, fixed: bool) -> Result<Vec<u8>, Fail> {
        let station = self.read_word(block)?;
        let net = self.read_word(block.wrapping_add(4))?;
        if net >= 256 {
            return Err(self.error(BAD_NETWORK));
        }
        if station == 0 || station >= 256 {
            return Err(self.error(BAD_STATION));
        }
        let mut out = Vec::new();
        let pad = |text: Vec<u8>, width: usize, fill: u8| {
            let mut padded = Vec::new();
            padded.resize(width.saturating_sub(text.len()), fill);
            padded.extend_from_slice(&text);
            padded
        };
        let station_text = decimal(i128::from(station), None);
        if fixed {
            if net == 0 {
                out.extend_from_slice(b"    ");
                out.extend_from_slice(&pad(station_text, 3, b' '));
            } else {
                out.extend_from_slice(&pad(decimal(i128::from(net), None), 3, b' '));
                out.push(b'.');
                out.extend_from_slice(&pad(station_text, 3, b'0'));
            }
        } else {
            if net != 0 {
                out.extend_from_slice(&decimal(i128::from(net), None));
                out.push(b'.');
            }
            out.extend_from_slice(&station_text);
        }
        Ok(out)
    }

    /// `OS_ConvertVariform`: R3 bytes (nibbles for hex) at R0 converted as
    /// R4 says; with R2 negative, only measured, which genuine RISC OS
    /// always answers with "Buffer overflow": R2 the inverse of the length,
    /// hex measured at two digits for each of R3's nibbles, and nothing
    /// reported for a conversion it could not make.
    fn variform(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let (size, kind) = (regs.r[3], regs.r[4]);
        let text = self.variform_text(regs.r[0], size, kind)?;
        if regs.r[2].cast_signed() < 0 {
            if let Ok(text) = &text {
                let length = if matches!(kind, 0 | 14) {
                    2 * size as usize
                } else {
                    text.len()
                };
                regs.r[2] = !len32(length);
            }
            return Err(self.buffer_overflow());
        }
        match text {
            Ok(text) => self.converted(regs, &text),
            Err(error) => Err(self.error(error)),
        }
    }

    /// What `OS_ConvertVariform` makes of `size` bytes (nibbles for hex) at
    /// `at` converted as `kind` says, or the error refusing it.
    fn variform_text(
        &mut self,
        at: u32,
        size: u32,
        kind: u32,
    ) -> Result<Result<Vec<u8>, Error>, Fail> {
        let bad_number = crate::error::BAD_NUMBER;
        let bytes_needed = match kind {
            0 | 14 => size.div_ceil(2),
            _ => size,
        } as usize;
        if bytes_needed > 64 {
            return Ok(Err(bad_number));
        }
        let mut bytes = [0u8; 64];
        self.read_bytes(at, &mut bytes[..bytes_needed])?;
        let bytes = &bytes[..bytes_needed];
        let value = {
            let mut word = [0u8; 8];
            let n = bytes.len().min(8);
            word[..n].copy_from_slice(&bytes[..n]);
            u64::from_le_bytes(word)
        };
        let decimal_size = size <= 8;
        Ok(Ok(match kind {
            0 => hex(bytes, size as usize, true),
            14 => hex(bytes, size as usize, false),
            1 | 2 | 4 | 5 | 6 | 7 | 8 | 9 if !decimal_size => return Ok(Err(bad_number)),
            1 => decimal(sized(value, size, false), None),
            2 => decimal(sized(value, size, true), None),
            3 => (0..size * 8)
                .rev()
                .map(|bit| {
                    let byte = bytes.get((bit / 8) as usize).copied().unwrap_or(0);
                    if byte >> (bit % 8) & 1 != 0 {
                        b'1'
                    } else {
                        b'0'
                    }
                })
                .collect(),
            4 => decimal(sized(value, size, false), Some(b" ")),
            5 => decimal(sized(value, size, true), Some(b" ")),
            6 | 7 => {
                let separator = self.thousands_separator()?;
                decimal(sized(value, size, kind == 7), Some(&separator))
            }
            8 => file_size(value, true),
            9 => file_size(value, false),
            10 => {
                let mut out = Vec::new();
                for (i, &byte) in bytes.iter().rev().enumerate() {
                    if i > 0 {
                        out.push(b'.');
                    }
                    out.extend_from_slice(&decimal(i128::from(byte), None));
                }
                out
            }
            11 => {
                let mut out = Vec::new();
                for (i, &byte) in bytes.iter().rev().enumerate() {
                    if i > 0 {
                        out.push(b':');
                    }
                    out.extend_from_slice(&hex(&[byte], 2, true));
                }
                out
            }
            12 | 13 if !size.is_multiple_of(2) => return Ok(Err(bad_number)),
            12 | 13 => ipv6(bytes, kind == 13),
            15 if size != 16 => return Ok(Err(bad_number)),
            15 => {
                let mut out = Vec::new();
                for (i, &byte) in bytes.iter().enumerate() {
                    if matches!(i, 4 | 6 | 8 | 10) {
                        out.push(b'-');
                    }
                    out.extend_from_slice(&hex(&[byte], 2, false));
                }
                out
            }
            _ => return Ok(Err(UNSUPPORTED)),
        }))
    }

    /// The current territory's thousands separator.
    fn thousands_separator(&mut self) -> Result<Vec<u8>, Fail> {
        let mut symbols = Regs::new([u32::MAX, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.perform(crate::swi::X | crate::territory::READ_SYMBOLS, &mut symbols)?;
        self.read_zstring(symbols.r[0], 16)
    }

    /// `OS_BinaryToDecimal`: R0 in signed decimal into R1's buffer of R2
    /// bytes, unterminated; R2 the characters written.
    pub(crate) fn binary_to_decimal(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let text = Decimal::of(regs.r[0].cast_signed());
        let text = text.as_bytes();
        let room = regs.r[2] as usize;
        let written = text.len().min(room);
        self.write_bytes(regs.r[1], &text[..written])?;
        regs.r[2] = len32(written);
        if written < text.len() {
            return Err(self.buffer_overflow());
        }
        Ok(())
    }

    /// The number at `at`, read by `OS_ReadUnsigned` with `flags` beside its
    /// base, at most `limit` where they ask for it; and where it ends.
    pub fn number_at(&mut self, at: u32, flags: u32, limit: u32) -> Result<(u32, u32), Fail> {
        let mut read = Regs::default();
        read.r[..3].copy_from_slice(&[flags, at, limit]);
        self.perform(crate::swi::X | crate::swi::os::READ_UNSIGNED, &mut read)?;
        Ok((read.r[2], read.r[1]))
    }

    /// `OS_ReadUnsigned`: a number from R1's text, in R0's base, limited as
    /// R0's flags say; in the `WIDE` form, 64 bits in R2 and R3.
    pub(crate) fn read_unsigned_swi(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let flags = regs.r[0];
        let wide = regs.r[4] == WIDE;
        let base = if wide {
            flags & 0xFF
        } else {
            flags & 0x0FFF_FFFF
        };
        let base = if (2..=36).contains(&base) { base } else { 10 };
        let wide_result = wide && flags & (1 << 28) != 0;
        let limit = if flags & BYTE_SIZED != 0 {
            255
        } else if flags & BOUNDED != 0 {
            if wide_result {
                u64::from(regs.r[2]) | u64::from(regs.r[3]) << 32
            } else {
                u64::from(regs.r[2])
            }
        } else if wide_result {
            u64::MAX
        } else {
            u64::from(u32::MAX)
        };
        // Only what a number can hold is read, so a reading never strays
        // past the number into memory that is not there.
        let (text, _) = self.read_until(regs.r[1], NUMBER_TEXT, |b| {
            !(b == b' ' || b == b'&' || b == b'_' || b.is_ascii_alphanumeric())
        })?;
        let reading = Reading {
            base,
            limit,
            wide: wide_result,
            terminated: flags & TERMINATED != 0,
        };
        if wide {
            regs.r[4] = 0xF000_0000;
        }
        match read_unsigned(&text, reading) {
            Ok((value, used)) => {
                regs.r[1] = regs.r[1].wrapping_add(len32(used));
                regs.r[2] = (value & 0xFFFF_FFFF) as u32;
                if wide_result {
                    regs.r[3] = (value >> 32) as u32;
                }
                Ok(())
            }
            Err(error) => {
                regs.r[2] = 0;
                if wide_result {
                    regs.r[3] = 0;
                }
                Err(self.error(obey_error(error)))
            }
        }
    }

    /// The string at `address` to and including the null, carriage return or
    /// line feed that ends it, as the readings take one.
    fn terminated_text(&mut self, address: u32) -> Result<Vec<u8>, Fail> {
        let (mut text, ended) = self.read_until(address, LONGEST_TEXT, crate::cli::is_end)?;
        if ended {
            let end = self.read_byte(address.wrapping_add(len32(text.len())))?;
            if text.try_reserve(1).is_err() {
                return Err(self.no_memory());
            }
            text.push(end);
        }
        Ok(text)
    }

    /// `OS_GSInit`: R0's string begun, with R2's flags; R0 where it goes on,
    /// R1 its first character, R2 the state `OS_GSRead` continues from.
    pub(crate) fn gs_init(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let base = regs.r[0];
        let flags = Flags::from_bits(regs.r[2]);
        let text = self.terminated_text(base)?;
        let reader = Reader::new(&text, flags);
        let first = reader.first();
        let empty = reader.is_empty();
        let position = reader.position();
        let progress = reader.into_progress();
        self.world.gs = Some(crate::os::GsState {
            base,
            text,
            flags,
            progress,
            generation: self.world.gs_generation,
        });
        regs.r[0] = base.wrapping_add(len32(position));
        regs.r[1] = u32::from(first);
        regs.r[2] = flags.bits() | (self.world.gs_generation & 0x1FFF_FFFF);
        regs.set_flag(psr::Z, empty);
        Ok(())
    }

    /// `OS_GSRead`: the next character of the string `OS_GSInit` began, R1
    /// it and C clear, or C set at the end.
    pub(crate) fn gs_read(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let flags = Flags::from_bits(regs.r[2]);
        let state = match self.world.gs.take() {
            Some(state) if state.generation & 0x1FFF_FFFF == regs.r[2] & 0x1FFF_FFFF => state,
            _ => {
                // A string begun elsewhere, or no longer held: read on from
                // R0 with nothing part way through.
                let base = regs.r[0];
                let text = self.terminated_text(base)?;
                let progress = Reader::new(&text, flags.union(Flags::NO_QUOTES)).into_progress();
                crate::os::GsState {
                    base,
                    text,
                    flags,
                    progress,
                    generation: self.world.gs_generation,
                }
            }
        };
        let crate::os::GsState {
            base,
            text,
            flags,
            progress,
            generation,
        } = state;
        let mut reader = Reader::resume(&text, flags, progress);
        let read = self.with_variables(0, |vars, budget| reader.read(vars, budget));
        let position = reader.position();
        let progress = reader.into_progress();
        match read {
            Ok(Read::Char(c)) => {
                regs.r[1] = c & 0xFF;
                regs.set_flag(psr::C, false);
                regs.r[0] = base.wrapping_add(len32(position));
                regs.r[2] = flags.bits() | (generation & 0x1FFF_FFFF);
                self.world.gs = Some(crate::os::GsState {
                    base,
                    text,
                    flags,
                    progress,
                    generation,
                });
                Ok(())
            }
            Ok(Read::End(end)) => {
                regs.r[1] = u32::from(end);
                regs.set_flag(psr::C, true);
                regs.r[0] = base.wrapping_add(len32(position));
                Ok(())
            }
            Err(fail) => Err(fail),
        }
    }

    /// `OS_GSTrans`: R0's string translated whole into R1's buffer of R2's
    /// low 29 bits' bytes, R2's top bits its flags; R0 past its end, R2 the
    /// bytes stored, C set if the buffer filled. Where the reading stopped
    /// inside a variable's value, R0 is not in the text, as genuine RISC OS
    /// leaves it: here zero.
    pub(crate) fn gs_trans(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let flags = Flags::from_bits(regs.r[2]);
        let room = (regs.r[2] & 0x1FFF_FFFF) as usize;
        let text = self.terminated_text(regs.r[0])?;
        let mut buffer = Vec::new();
        if buffer.try_reserve_exact(room.min(LONGEST_TEXT)).is_err() {
            return Err(self.no_memory());
        }
        buffer.resize(room.min(LONGEST_TEXT), 0);
        let translation = self.with_variables(0, |vars, budget| {
            Ok(gstrans(&text, flags, vars, budget, &mut buffer))
        })?;
        let stored = translation.stored.min(buffer.len());
        self.write_bytes(regs.r[1], &buffer[..stored])?;
        regs.r[0] = translation
            .next
            .map_or(0, |next| regs.r[0].wrapping_add(len32(next)));
        regs.r[2] = len32(stored);
        match translation.ended {
            Ok(Some(end)) => {
                self.write_byte(regs.r[1].wrapping_add(len32(stored)), end)?;
                regs.set_flag(psr::C, false);
                Ok(())
            }
            Ok(None) => {
                regs.set_flag(psr::C, true);
                Ok(())
            }
            Err(error) => Err(self.error(obey_error(error))),
        }
    }

    /// `OS_EvaluateExpression`: R0's expression; an integer in R2 with R1
    /// zero, or a string in R1's buffer of R2 bytes, R2 its length.
    pub(crate) fn evaluate_expression(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let text = self.read_text(regs.r[0], LONGEST_TEXT)?;
        let answer = self.with_variables(0, |vars, budget| evaluate(&text, vars, budget))?;
        match answer {
            Answer::Integer(n) => {
                regs.r[1] = 0;
                regs.r[2] = n.cast_unsigned();
            }
            Answer::String(bytes) => {
                // A negative size is no room, and a buffer too small is filled
                // as far as it goes.
                let room = usize::try_from(regs.r[2].cast_signed()).unwrap_or(0);
                if bytes.len() > room {
                    self.write_bytes(regs.r[1], &bytes[..room])?;
                    return Err(self.buffer_overflow());
                }
                self.write_bytes(regs.r[1], &bytes)?;
                regs.r[2] = len32(bytes.len());
            }
        }
        Ok(())
    }

    /// `OS_SubstituteArgs` and `OS_SubstituteArgs32`: R3's R4 bytes with
    /// R0's arguments put in, into R1's buffer of R2 bytes; R2 its length.
    /// R5's top bit keeps the unused arguments off the end.
    pub(crate) fn substitute_args(&mut self, regs: &mut Regs, flags: u32) -> Result<(), Fail> {
        let args = self.terminated_text(regs.r[0])?;
        let template = self.read_vec(regs.r[3], (regs.r[4] as usize).min(LONGEST_TEXT))?;
        let append = flags >> 31 == 0;
        let out = substitute(&args, &template, append, regs.r[2] as usize)
            .map_err(|error| self.error(obey_error(error)))?;
        self.write_bytes(regs.r[1], &out)?;
        regs.r[2] = len32(out.len());
        Ok(())
    }

    /// `OS_SubstituteArgs32`.
    pub(crate) fn substitute_args32(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let flags = regs.r[5];
        self.substitute_args(regs, flags)
    }

    /// `OS_SubstituteArgs`: the 32-bit form with no flags.
    pub(crate) fn substitute_args_plain(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.substitute_args(regs, 0)
    }

    /// `OS_ReadArgs`: R1's arguments read against R0's template into R2's
    /// buffer of R3 bytes; R3 the bytes left.
    pub(crate) fn read_args_swi(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let template = self.read_text(regs.r[0], LONGEST_TEXT)?;
        let args = self.read_text(regs.r[1], LONGEST_TEXT)?;
        let (base, capacity) = (regs.r[2], regs.r[3] as usize);
        let read = self.with_variables(0, |vars, budget| {
            read_args(
                &template,
                &args,
                vars,
                budget,
                base,
                capacity.min(LONGEST_TEXT),
            )
        })?;
        self.write_bytes(base, &read.buffer)?;
        regs.r[3] = len32(read.left);
        Ok(())
    }

    /// `OS_ConvertStandardDateAndTime`: through the current territory.
    pub(crate) fn convert_standard_date_and_time(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        if regs.r[2].cast_signed() < 0 {
            return Err(self.buffer_overflow());
        }
        let mut call = Regs::new([
            u32::MAX,
            regs.r[0],
            regs.r[1],
            regs.r[2],
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        self.perform(
            crate::swi::X | crate::territory::CONVERT_STANDARD_DATE_AND_TIME,
            &mut call,
        )?;
        regs.r[..3].copy_from_slice(&call.r[..3]);
        Ok(())
    }

    /// `OS_ConvertDateAndTime`: R3's format, through the current territory.
    pub(crate) fn convert_date_and_time(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        if regs.r[2].cast_signed() < 0 {
            return Err(self.buffer_overflow());
        }
        let mut call = Regs::new([
            u32::MAX,
            regs.r[0],
            regs.r[1],
            regs.r[2],
            regs.r[3],
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        self.perform(
            crate::swi::X | crate::territory::CONVERT_DATE_AND_TIME,
            &mut call,
        )?;
        regs.r[..3].copy_from_slice(&call.r[..3]);
        Ok(())
    }
}

/// `bytes` as an IPv6 address, its highest group first, each in the fewest
/// hex digits; `shortest` writes the longest run of two or more zero groups,
/// the first of equals, as `::`.
fn ipv6(bytes: &[u8], shortest: bool) -> Vec<u8> {
    let groups: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .rev()
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let mut run = (0, 0);
    if shortest {
        let mut at = 0;
        while at < groups.len() {
            let length = groups[at..].iter().take_while(|&&g| g == 0).count();
            if length >= 2 && length > run.1 {
                run = (at, length);
            }
            at += length.max(1);
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < groups.len() {
        if run.1 > 0 && i == run.0 {
            out.extend_from_slice(b"::");
            i += run.1;
            continue;
        }
        if i > 0 && !out.ends_with(b"::") {
            out.push(b':');
        }
        let text = hex(&groups[i].to_le_bytes(), 4, false);
        let start = text.iter().position(|&b| b != b'0').unwrap_or(3);
        out.extend_from_slice(&text[start..]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests;
