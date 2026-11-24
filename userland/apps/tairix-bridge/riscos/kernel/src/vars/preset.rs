//! The variables the kernel makes at reset: `Sys$Time`, `Sys$Year`,
//! `Sys$Date`, `Sys$ReturnCode` and `Sys$RCLimit`, code variables whose
//! entries are the kernel's routines, and the strings `Sys$DateFormat` and
//! `Alias$.`.
//!
//! Reading a clock's variable has the territory format the time; setting one
//! sets the clock through `OS_Word` 15 from the text given, the rest of the
//! date it needs formatted from the clock as it stands.

use alloc::vec::Vec;

use super::CODE;
use crate::convert::Decimal;
use crate::env::RC_EXCEEDED;
use crate::host::Routine;
use crate::os::{len32, Fail, Os, Regs, Unwind};
use crate::swi::{os, X};
use crate::word::{read, set, READ_CLOCK, SET_CLOCK};

/// The code variables the kernel makes, in the order it makes them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Preset {
    Time,
    Year,
    Date,
    ReturnCode,
    RcLimit,
}

const PRESETS: [Preset; 5] = [
    Preset::Time,
    Preset::Year,
    Preset::Date,
    Preset::ReturnCode,
    Preset::RcLimit,
];

/// The routines their entries are: a write and a read each.
pub(crate) const ENTRIES: u32 = 2 * crate::os::len32(PRESETS.len());

/// The strings the kernel sets at reset.
const STRINGS: [(&[u8], &[u8]); 2] = [
    (b"Sys$DateFormat", b"%24:%mi:%se %dy-%m3-%ce%yr"),
    (b"Alias$.", b"Cat "),
];

/// The buffer a value is made in: `OS_Word` 15's reason, then up to 254
/// characters and a terminator; then a format, then a time.
const VALUE_ROOM: u32 = 256;
const FORMAT_ROOM: u32 = 32;
pub(crate) const BUFFER: u32 = VALUE_ROOM + FORMAT_ROOM + 8;

/// What a clock's variable may be set to: the time, the date without its
/// year, and the year.
const TIME_MOST: u32 = 0xFE;
const DATE_MOST: u32 = 0xF8;
const YEAR_MOST: u32 = 4;
/// What setting the date takes of the clock as it stands, and setting the
/// year.
const YEAR_OF_DATE: &[u8] = b" %ce%yr\0";
const DATE_OF_YEAR: &[u8] = b"%w3,%dy %m3 \0";

impl Preset {
    const fn name(self) -> &'static [u8] {
        match self {
            Self::Time => b"Sys$Time",
            Self::Year => b"Sys$Year",
            Self::Date => b"Sys$Date",
            Self::ReturnCode => b"Sys$ReturnCode",
            Self::RcLimit => b"Sys$RCLimit",
        }
    }
}

impl Os<'_> {
    /// The kernel's variables, made at reset.
    pub(crate) fn set_presets(&mut self) -> Result<(), Fail> {
        for (index, preset) in (0..).zip(PRESETS) {
            let write = self.world.routine_address(Routine::Variable(2 * index));
            let read = self.world.routine_address(Routine::Variable(2 * index + 1));
            let code = crate::host::variable_code(write, read).map_err(|_| self.no_memory())?;
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(4 * code.len())
                .map_err(|_| self.no_memory())?;
            bytes.extend(code.iter().flat_map(|word| word.to_le_bytes()));
            self.store_variable(preset.name(), None, CODE, &bytes)?;
        }
        for (name, value) in STRINGS {
            self.set_string_variable(name, value)?;
        }
        Ok(())
    }

    /// One of the kernel's code variables' entries, entered as a code
    /// variable's are: `entry` the variable this many in, write then read.
    pub(crate) fn preset_entered(&mut self, entry: u32) -> Result<(), Unwind> {
        let Some(&preset) = PRESETS.get((entry / 2) as usize) else {
            self.undefined_here();
            return Ok(());
        };
        let read = entry % 2 == 1;
        self.as_subroutine(|os, regs| {
            if read {
                os.read_preset(preset, regs)
            } else {
                os.write_preset(preset, regs)
            }
        })
    }

    /// A read entry: R0 the value, R2 its length.
    fn read_preset(&mut self, preset: Preset, regs: &mut Regs) -> Result<(), Fail> {
        let text = self.world.vars.buffer.wrapping_add(1);
        regs.r[0] = text;
        regs.r[2] = match preset {
            Preset::Time => self.clock_text(b"%24:%mi:%se\0", text)?,
            Preset::Year => self.clock_text(b"%ce%yr\0", text)?,
            Preset::Date => self.clock_text(b"%w3,%dy %m3\0", text)?,
            Preset::ReturnCode => self.number_text(self.world.env.return_code, text)?,
            Preset::RcLimit => self.number_text(self.world.env.rc_limit, text)?,
        };
        Ok(())
    }

    /// A write entry: R1 the text, R2 its length.
    fn write_preset(&mut self, preset: Preset, regs: &Regs) -> Result<(), Fail> {
        let (value, length) = (regs.r[1], regs.r[2]);
        let block = self.world.vars.buffer;
        let text = block.wrapping_add(1);
        let length = match preset {
            Preset::Time | Preset::Date | Preset::Year => {
                let most = match preset {
                    Preset::Time => TIME_MOST,
                    Preset::Date => DATE_MOST,
                    _ => YEAR_MOST,
                };
                if length > most {
                    return Err(self.error(super::TOO_LONG));
                }
                length
            }
            // Only the number is read, so no length is too long.
            Preset::ReturnCode | Preset::RcLimit => length.min(VALUE_ROOM - 2),
        };
        let given = self.read_vec(value, length as usize)?;
        match preset {
            Preset::Time => {
                self.write_bytes(text, &given)?;
                self.set_clock(set::TIME, text.wrapping_add(length))
            }
            Preset::Date => {
                self.write_bytes(text, &given)?;
                let end = text.wrapping_add(length);
                let year = self.clock_text(YEAR_OF_DATE, end)?;
                self.set_clock(set::DATE, end.wrapping_add(year))
            }
            Preset::Year => {
                let date = self.clock_text(DATE_OF_YEAR, text)?;
                let at = text.wrapping_add(date);
                self.write_bytes(at, &given)?;
                self.set_clock(set::DATE, at.wrapping_add(length))
            }
            Preset::ReturnCode => {
                let code = self.preset_number(&given, text)?;
                self.world.env.return_code = code;
                let limit = self.world.env.rc_limit;
                if code > limit {
                    return Err(self.error(RC_EXCEEDED));
                }
                Ok(())
            }
            Preset::RcLimit => {
                let limit = self.preset_number(&given, text)?.cast_signed();
                // A negative limit is its size; the most negative, nought.
                self.world.env.rc_limit = limit.checked_abs().map_or(0, i32::cast_unsigned);
                Ok(())
            }
        }
    }

    /// The clock formatted with `format` at `at`, as `Territory_ConvertDateAndTime`
    /// formats it: its length.
    fn clock_text(&mut self, format: &[u8], at: u32) -> Result<u32, Fail> {
        let block = self.world.vars.buffer;
        let format_at = block.wrapping_add(VALUE_ROOM);
        let time = format_at.wrapping_add(FORMAT_ROOM);
        self.write_bytes(format_at, format)?;
        self.write_byte(time, read::UTC)?;
        let mut word = Regs::default();
        word.r[..2].copy_from_slice(&[READ_CLOCK, time]);
        self.perform(X | os::WORD, &mut word)?;
        let room = block.wrapping_add(VALUE_ROOM).wrapping_sub(at);
        let mut convert = Regs::default();
        convert.r[..5].copy_from_slice(&[crate::territory::CURRENT, time, at, room, format_at]);
        self.perform(X | crate::territory::CONVERT_DATE_AND_TIME, &mut convert)?;
        Ok(convert.r[1].wrapping_sub(convert.r[0]))
    }

    /// `value` in signed decimal at `at`: its length.
    fn number_text(&mut self, value: u32, at: u32) -> Result<u32, Fail> {
        let text = Decimal::of(value.cast_signed());
        self.write_bytes(at, text.as_bytes())?;
        Ok(len32(text.as_bytes().len()))
    }

    /// The clock set by `OS_Word` 15 from the text the buffer holds, which
    /// ends at `end`: `reason` its kind.
    fn set_clock(&mut self, reason: u8, end: u32) -> Result<(), Fail> {
        let block = self.world.vars.buffer;
        self.write_byte(end, 0)?;
        self.write_byte(block, reason)?;
        let mut word = Regs::default();
        word.r[..2].copy_from_slice(&[SET_CLOCK, block]);
        self.perform(X | os::WORD, &mut word)
    }

    /// `given`, a number a return code's variable is set to: signed, in
    /// decimal unless it says otherwise, read at `at` where it is copied.
    fn preset_number(&mut self, given: &[u8], at: u32) -> Result<u32, Fail> {
        if given.is_empty() {
            return Ok(0);
        }
        self.write_terminated(at, given, b'\r', VALUE_ROOM as usize - 1)?;
        let (negative, digits) = match given[0] {
            b'-' => (true, at.wrapping_add(1)),
            b'+' => (false, at.wrapping_add(1)),
            _ => (false, at),
        };
        let mut read = Regs::default();
        read.r[1] = digits;
        // A number too big to hold, or none, is nought, as RISC OS 5 reads one.
        match self.perform(X | os::READ_UNSIGNED, &mut read) {
            Ok(()) => {}
            Err(Fail::Error(_)) => return Ok(0),
            Err(unwind @ Fail::Unwind(_)) => return Err(unwind),
        }
        Ok(if negative {
            read.r[2].wrapping_neg()
        } else {
            read.r[2]
        })
    }
}

#[cfg(test)]
mod tests;
