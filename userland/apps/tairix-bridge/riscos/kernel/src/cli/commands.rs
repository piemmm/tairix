//! The kernel's own `*` commands but those for files, modules, help and
//! configuration: output, variables, `*IF`, the numbers `OS_Byte` takes, the
//! application, the caches and the memory areas. Each reaches the kernel
//! through its SWIs.
//!
//! `*TV` alone gives `OS_Byte` 144 noughts, and an `ELSE` ending a line is
//! one.

use alloc::vec::Vec;

use crate::convert::Decimal;
use tairix_obey::expr::{evaluate, Answer};
use tairix_obey::gstrans::Flags;
use tairix_obey::Error as Obey;

use super::LONGEST;
use crate::error::{obey_error, Error, BUFFER_OVERFLOW, SYNTAX};
use crate::os::{len32, Fail, Os, Regs};
use crate::swi::os::fs_control::OPT;
use crate::swi::os::read_unsigned::{BOUNDED, BYTE_SIZED, DECIMAL, TERMINATED};
use crate::swi::{os, X};
use crate::vars::{EXPANDED, MACRO, NUMBER, STRING};

/// Where `*GO` starts the application it is given no address for.
const APPLICATION: u32 = crate::memory::layout::APPLICATION;
/// The command `*GO` starts an application as.
const GO: &[u8] = b"GO \0";
/// `*Time`'s format.
const TIME_FORMAT: &[u8] = b"%w3,%dy %m3 %ce%yr.%24:%mi:%se\0";
/// `*Show`'s pattern given none: every variable.
const EVERY: &[u8] = b"*\0";
/// What `*Show` reads a value into before it claims room for a longer one.
const SHOW_ROOM: u32 = 256;
/// `*ChangeDynamicArea`'s template and the room it reads into.
const AREA_KEYS: &[u8] = b"FontSize/K,SpriteSize/K,RamFSSize/K,RMASize/K,ScreenSize/K\0";
const AREA_ROOM: u32 = 256;
/// Each key's dynamic area, in the order they change: the RAM disc last, so
/// that the others change though it cannot.
const AREA_ORDER: [(u32, u32); 5] = [(3, 1), (4, 2), (0, 4), (1, 3), (2, 5)];

/// The control register's instruction cache, data cache and write buffer.
const CACHES: u32 = (1 << 12) | (1 << 2) | (1 << 3);
const DATA_CACHE: u32 = 1 << 2;

/// `OS_Byte`'s reasons the commands use.
const PRINTER_IGNORE: u32 = 6;
const SHADOW: u32 = 114;
const TV: u32 = 144;
const NO_IGNORE: u32 = crate::byte::var::NO_IGNORE as u32;
/// `OS_Word` 14 reason 3: the time as five bytes.
const READ_CLOCK: u32 = 14;
const UTC: u8 = 3;

/// "Too many parameters".
pub(crate) const TOO_MANY: Error = Error {
    number: SYNTAX,
    token: "TooManyParameters",
};
/// "*If needs a THEN".
const NO_THEN: Error = Error {
    number: SYNTAX,
    token: "IfWithoutThen",
};
/// "The condition is a string, not a number": `*If`'s condition.
const IS_STRING: Error = Error {
    number: SYNTAX,
    token: "ConditionIsString",
};
/// "Key number out of range".
const BAD_KEY: Error = Error {
    number: 0xFB,
    token: "FunctionKeyOutOfRange",
};

impl Os<'_> {
    /// `*FX`'s and `*TV`'s numbers: up to `most`, each after spaces and at
    /// most one comma.
    fn byte_numbers(&mut self, at: u32, most: usize) -> Result<Vec<u32>, Fail> {
        let mut numbers = Vec::new();
        let (mut byte, mut at) = self.skip_spaces(at)?;
        while !super::is_end(byte) {
            if numbers.len() == most {
                return Err(self.error(TOO_MANY));
            }
            if !numbers.is_empty() && byte == b',' {
                at = at.wrapping_add(1);
            }
            let (value, end) = self.number_at(at, 10, 0)?;
            if numbers.try_reserve(1).is_err() {
                return Err(self.no_memory());
            }
            numbers.push(value);
            (byte, at) = self.skip_spaces(end)?;
        }
        Ok(numbers)
    }

    /// `*Show`'s lines for the variables `pattern` matches, read into
    /// `buffer`.
    fn show_variables(&mut self, pattern: u32, buffer: u32) -> Result<(), Fail> {
        let mut context = 0;
        loop {
            let mut read = Regs::default();
            read.r[..5].copy_from_slice(&[pattern, buffer, SHOW_ROOM, context, 0]);
            match self.perform(X | os::READ_VAR_VAL, &mut read) {
                Ok(()) => self.show_line(read.r[3], buffer, read.r[2], read.r[4])?,
                Err(Fail::Error(block)) => match self.read_word(block)? {
                    n if n == crate::vars::NOT_FOUND.number => return Ok(()),
                    n if n == BUFFER_OVERFLOW.number => self.show_long(read.r[3])?,
                    _ => return Err(Fail::Error(block)),
                },
                Err(unwind) => return Err(unwind),
            }
            context = read.r[3];
            self.yield_point()?;
        }
    }

    /// `*Show`'s line for the variable named at `name`, too long for its
    /// buffer: read again into one claimed to fit it.
    fn show_long(&mut self, name: u32) -> Result<(), Fail> {
        let mut probe = Regs::default();
        probe.r[..5].copy_from_slice(&[name, 0, u32::MAX, 0, 0]);
        match self.perform(X | os::READ_VAR_VAL, &mut probe) {
            Ok(()) | Err(Fail::Error(_)) => {}
            Err(unwind) => return Err(unwind),
        }
        let room = (!probe.r[2]).wrapping_add(0xFF) & !0xFF;
        let block = self.rma_claim(room)?;
        let mut read = Regs::default();
        read.r[..5].copy_from_slice(&[name, block, room, 0, 0]);
        let result = self
            .perform(X | os::READ_VAR_VAL, &mut read)
            .and_then(|()| self.show_line(read.r[3], block, read.r[2], read.r[4]));
        self.free_rma_quietly(block);
        result
    }

    /// The line for the variable named at `name`, its value the `length`
    /// bytes at `value` and of type `kind`.
    fn show_line(&mut self, name: u32, value: u32, length: u32, kind: u32) -> Result<(), Fail> {
        let (title, _) = self.read_until(name, crate::vars::LONGEST_NAME, |b| b == 0)?;
        self.write_text(&title)?;
        let tag: &[u8] = match u8::try_from(kind) {
            Ok(NUMBER) => b"ShowNumberTag",
            Ok(MACRO) => b"ShowMacroTag",
            _ => b"",
        };
        if !tag.is_empty() {
            let tag = self.message_with(tag, &[])?;
            self.write_text(b" ")?;
            self.write_text(&tag)?;
        }
        self.write_text(b" : ")?;
        let text = if kind == u32::from(NUMBER) {
            let number = Decimal::of(self.read_word(value)?.cast_signed());
            number.as_bytes().to_vec()
        } else {
            self.read_until(value, length as usize, |_| false)?.0
        };
        let mut out = Vec::new();
        for &c in &text {
            self.escape_point()?;
            out.clear();
            shown(c, kind == u32::from(MACRO), &mut out);
            self.write_text(&out)?;
        }
        self.write_text(b"\n\r")
    }

    /// The `OS_ReadUnsigned` byte `*Ignore` and `*Shadow` read when they
    /// are given one, which must end at a space or the line's end.
    fn optional_byte(&mut self, regs: &Regs) -> Result<Option<u32>, Fail> {
        let (value, end) = if regs.r[1] == 0 {
            (None, regs.r[0])
        } else {
            let (value, end) = self.number_at(regs.r[0], BYTE_SIZED | DECIMAL, 0)?;
            (Some(value), end)
        };
        if !matches!(self.read_byte(end)?, b' ' | b'\r' | b'\n' | 0) {
            return Err(self.error(obey_error(Obey::BadNumber)));
        }
        Ok(value)
    }

    /// The size at `at`: a number, then K, M or G for kibibytes, mebibytes
    /// or gibibytes, ending at a space or a control character.
    fn size_given(&mut self, at: u32) -> Result<u32, Fail> {
        let (value, end) = self.number_at(at, 10, 0)?;
        let (shift, end) = match self.read_byte(end)?.to_ascii_uppercase() {
            b'K' => (10, end.wrapping_add(1)),
            b'M' => (20, end.wrapping_add(1)),
            b'G' => (30, end.wrapping_add(1)),
            _ => (0, end),
        };
        if self.read_byte(end)? > b' ' {
            return Err(self.error(obey_error(Obey::BadNumber)));
        }
        Ok(value.wrapping_shl(shift))
    }
}

/// `c` of a value as `*Eval` and `*Show` show it: a control character as
/// `|` and the letter it is the control of, `DEL` as `|?`, and `|`, `"` and
/// `<` after a `|` unless `raw`.
fn shown(c: u8, raw: bool, out: &mut Vec<u8>) {
    match c {
        0..=31 => out.extend_from_slice(&[b'|', c + b'@']),
        0x7F => out.extend_from_slice(b"|?"),
        b'|' | b'"' | b'<' if !raw => out.extend_from_slice(&[b'|', c]),
        _ => out.push(c),
    }
}

/// `*Echo`: the tail GSTrans'd, quotes kept, then a newline.
pub(crate) fn echo(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.gs_each(regs.r[0], Flags::NO_QUOTES.bits(), Os::write_c)?;
    os.write_text(b"\n\r")
}

/// `*Error`: an error of the number, if one starts the tail, its text the
/// rest GSTrans'd as far as GSTrans gets.
pub(crate) fn error(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (number, text_at) = match os.number_at(regs.r[0], 0, 0) {
        Ok(read) => read,
        Err(Fail::Error(_)) => (0, regs.r[0]),
        Err(unwind) => return Err(unwind),
    };
    let (buffer, held) = os.cli_buffer();
    let mut translate = Regs::default();
    translate.r[..3].copy_from_slice(&[text_at, buffer, len32(crate::error::TEXT) + 1]);
    let translated = match os.perform(X | os::GS_TRANS, &mut translate) {
        Ok(()) | Err(Fail::Error(_)) => {
            let length = (translate.r[2] as usize).min(crate::error::TEXT);
            os.read_until(buffer, length, |_| false)
        }
        Err(unwind) => Err(unwind),
    };
    os.world.cli.release(held);
    Err(os.raw_error(number, &translated?.0))
}

/// `*Eval`: the expression's value, and whether it is an integer or a
/// string.
pub(crate) fn eval(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (buffer, held) = os.cli_buffer();
    let mut evaluate = Regs::default();
    evaluate.r[..3].copy_from_slice(&[regs.r[0], buffer, super::BUFFER]);
    let result = os
        .perform(X | os::EVALUATE_EXPRESSION, &mut evaluate)
        .and_then(|()| {
            let (kind, value) = if evaluate.r[1] == 0 {
                let text = Decimal::of(evaluate.r[2].cast_signed());
                (&b"EvalIntegerKind"[..], text.as_bytes().to_vec())
            } else {
                (
                    &b"EvalStringKind"[..],
                    os.read_until(buffer, evaluate.r[2] as usize, |_| false)?.0,
                )
            };
            let kind = os.message_with(kind, &[])?;
            let mut out = os.message_with(b"EvalResult", &[&kind])?;
            out.push(b' ');
            for &c in &value {
                shown(c, false, &mut out);
            }
            out.extend_from_slice(b"\n\r");
            os.write_text(&out)
        });
    os.world.cli.release(held);
    result
}

/// `*Show`: each variable the tail matches, every one with none, paged.
pub(crate) fn show(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (buffer, held) = os.cli_buffer();
    let result = (|| {
        let pattern = if regs.r[1] == 0 {
            let at = buffer.wrapping_add(super::BUFFER - len32(EVERY.len()));
            os.write_bytes(at, EVERY)?;
            at
        } else {
            regs.r[0]
        };
        let status = os.paged()?;
        let shown = os.show_variables(pattern, buffer);
        os.unpaged(status)?;
        shown
    })();
    os.world.cli.release(held);
    result
}

/// `*Time`: the date and time, as the territory writes them.
pub(crate) fn time(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let (buffer, held) = os.cli_buffer();
    let result = (|| {
        let format = buffer.wrapping_add(super::BUFFER - len32(TIME_FORMAT.len()));
        os.write_bytes(format, TIME_FORMAT)?;
        let block = format.wrapping_sub(8);
        os.write_byte(block, UTC)?;
        let mut word = Regs::default();
        word.r[..2].copy_from_slice(&[READ_CLOCK, block]);
        os.perform(X | os::WORD, &mut word)?;
        let mut convert = Regs::default();
        convert.r[..5].copy_from_slice(&[u32::MAX, block, buffer, 256, format]);
        os.perform(X | crate::territory::CONVERT_DATE_AND_TIME, &mut convert)?;
        let (text, _) = os.read_until(convert.r[0], 256, |b| b == 0)?;
        os.write_line(&text)
    })();
    os.world.cli.release(held);
    result
}

/// `*Set`, `*SetMacro` and `*SetEval`: the variable named first set to the
/// rest, as `OS_SetVarVal` sets a value of `kind`.
fn set_as(os: &mut Os<'_>, regs: &Regs, kind: u8) -> Result<(), Fail> {
    let tail = regs.r[0];
    let mut value = tail;
    while os.read_byte(value)? > b' ' {
        value = value.wrapping_add(1);
    }
    let (_, value) = os.skip_spaces(value)?;
    let mut set = Regs::default();
    set.r[..5].copy_from_slice(&[tail, value, 1, 0, u32::from(kind)]);
    os.perform(X | os::SET_VAR_VAL, &mut set)
}

/// `*Set`.
pub(crate) fn set(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    set_as(os, regs, STRING)
}

/// `*SetMacro`.
pub(crate) fn set_macro(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    set_as(os, regs, MACRO)
}

/// `*SetEval`.
pub(crate) fn set_eval(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    set_as(os, regs, EXPANDED)
}

/// `*Unset`: every variable the tail matches deleted but code variables,
/// which RISC OS's `*Unset` leaves; it never fails.
pub(crate) fn unset(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut context = 0;
    loop {
        // R4 is not a code variable's type, so those are passed over.
        let mut delete = Regs::default();
        delete.r[..5].copy_from_slice(&[regs.r[0], 0, u32::MAX, context, 1]);
        match os.perform(X | os::SET_VAR_VAL, &mut delete) {
            Ok(()) => context = delete.r[3],
            Err(Fail::Error(_)) => return Ok(()),
            Err(unwind) => return Err(unwind),
        }
        os.yield_point()?;
    }
}

/// `*Key`: function key `n`'s string, `Key$n`, set from the rest of the
/// tail, GSTrans'd now.
pub(crate) fn key(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (number, value) = match os.number_at(regs.r[0], BOUNDED | DECIMAL, 15) {
        Ok(read) => read,
        Err(Fail::Error(_)) => return Err(os.error(BAD_KEY)),
        Err(unwind) => return Err(unwind),
    };
    let mut name = b"Key$".to_vec();
    name.extend_from_slice(Decimal::of(number.cast_signed()).as_bytes());
    name.push(0);
    let (buffer, held) = os.cli_buffer();
    let result = os.write_bytes(buffer, &name).and_then(|()| {
        let mut set = Regs::default();
        set.r[..5].copy_from_slice(&[buffer, value, 0, 0, u32::from(STRING)]);
        os.perform(X | os::SET_VAR_VAL, &mut set)
    });
    os.world.cli.release(held);
    result
}

/// `*IF`: the command after `THEN` if the expression before it is not
/// zero, otherwise the one after `ELSE`, if there is one.
pub(crate) fn if_command(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (line, _) = os.read_until(regs.r[0], LONGEST, super::is_end)?;
    let Some(then) = keyword(&line, b"THEN", 0) else {
        return Err(os.error(NO_THEN));
    };
    let answer = os.with_variables(0, |vars, budget| evaluate(&line[..then], vars, budget))?;
    let Answer::Integer(value) = answer else {
        return Err(os.error(IS_STRING));
    };
    let after_then = then + 5;
    let otherwise = keyword(&line, b"ELSE", after_then);
    let command = match (value, otherwise) {
        (0, Some(at)) => &line[at + 5..],
        (0, None) => return Ok(()),
        (_, Some(at)) => &line[after_then..at],
        (_, None) => &line[after_then..],
    };
    let (buffer, held) = os.cli_buffer();
    let result = os
        .write_terminated(buffer, command, b'\r', super::BUFFER as usize)
        .and_then(|()| os.command_line(buffer));
    os.cli_buffer_done(held, result)
}

/// Where a space and `word` in any case, then a space or the line's end,
/// are in `line` from `from`: the space before the word.
fn keyword(line: &[u8], word: &[u8], from: usize) -> Option<usize> {
    (from..line.len()).find(|&at| {
        line[at] == b' '
            && line
                .get(at + 1..at + 1 + word.len())
                .is_some_and(|w| w.eq_ignore_ascii_case(word))
            && line.get(at + 1 + word.len()).is_none_or(|&b| b == b' ')
    })
}

/// `*FX`: `OS_Byte` with the numbers given, R1 and R2 zero if left out.
pub(crate) fn fx(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let numbers = os.byte_numbers(regs.r[0], 3)?;
    let mut byte = Regs::default();
    byte.r[..numbers.len()].copy_from_slice(&numbers);
    os.perform(X | os::BYTE, &mut byte)
}

/// `*TV`: `OS_Byte` 144 with the position and interlace given, zero if left
/// out.
pub(crate) fn tv(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let numbers = os.byte_numbers(regs.r[0], 2)?;
    let mut byte = Regs::default();
    byte.r[0] = TV;
    byte.r[1..=numbers.len()].copy_from_slice(&numbers);
    os.perform(X | os::BYTE, &mut byte)
}

/// `*Opt`: the filing system's options, zero if left out; a comma before
/// the second is a separator, never its end.
pub(crate) fn opt(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut values = [0, 0];
    if regs.r[1] != 0 {
        let (first, end) = os.number_at(regs.r[0], 10, 0)?;
        values[0] = first;
        let (byte, at) = os.skip_spaces(end)?;
        if byte >= b' ' {
            let at = if byte == b',' {
                os.skip_spaces(at.wrapping_add(1))?.1
            } else {
                at
            };
            values[1] = os.number_at(at, TERMINATED | DECIMAL, 0)?.0;
        }
    }
    let mut options = Regs::default();
    options.r[..3].copy_from_slice(&[OPT, values[0], values[1]]);
    os.perform(X | os::FS_CONTROL, &mut options)
}

/// `*GO`: the code at the address given, the application space's start if
/// none or one that does not read, entered in User mode as the application
/// with the rest of the line after an optional `;` its parameters.
pub(crate) fn go(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (byte, at) = os.skip_spaces(regs.r[0])?;
    let (address, after) = if byte <= b'\r' || byte == b';' {
        (APPLICATION, at)
    } else {
        match os.number_at(at, 16, 0) {
            Ok(read) => read,
            Err(Fail::Error(_)) => (APPLICATION, at),
            Err(unwind) => return Err(unwind),
        }
    };
    let (byte, mut tail) = os.skip_spaces(after)?;
    if byte == b';' {
        tail = os.skip_spaces(tail.wrapping_add(1))?.1;
    }
    let (buffer, held) = os.cli_buffer();
    let started = os
        .write_bytes(buffer, GO)
        .and_then(|()| os.start_application(tail, address, buffer));
    os.world.cli.release(held);
    started?;
    let bits = os.caller_bits();
    Err(os.enter_application(bits, address, |_| {}))
}

/// `*Quit`: `OS_Exit` with no return code.
pub(crate) fn quit(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let mut exit = Regs::default();
    os.perform(os::EXIT, &mut exit)
}

/// `*Ignore`: the printer stream's ignored character, or none.
pub(crate) fn ignore(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let character = os.optional_byte(regs)?;
    let mut none = Regs::default();
    none.r[..2].copy_from_slice(&[NO_IGNORE, if character.is_some() { 0 } else { 255 }]);
    let cleared = os.perform(X | os::BYTE, &mut none);
    let Some(character) = character else {
        return cleared;
    };
    if let Err(Fail::Unwind(unwind)) = cleared {
        return Err(Fail::Unwind(unwind));
    }
    let mut set = Regs::default();
    set.r[..2].copy_from_slice(&[PRINTER_IGNORE, character]);
    os.perform(X | os::BYTE, &mut set)
}

/// `*Shadow`: `OS_Byte` 114 with the bank given, zero if none.
pub(crate) fn shadow(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let bank = os.optional_byte(regs)?.unwrap_or(0);
    let mut byte = Regs::default();
    byte.r[..2].copy_from_slice(&[SHADOW, bank]);
    os.perform(X | os::BYTE, &mut byte)
}

/// `*Cache`: the caches turned on or off, or with no parameter, which they
/// are.
pub(crate) fn cache(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut control = Regs::default();
    if regs.r[1] == 0 {
        control.r[..3].copy_from_slice(&[0, 0, u32::MAX]);
        os.perform(X | os::MMU_CONTROL, &mut control)?;
        // English, as the words that set it are.
        let state: &[u8] = if control.r[1] & DATA_CACHE != 0 {
            b"On"
        } else {
            b"Off"
        };
        os.write_text(b"Cache ")?;
        return os.write_line(state);
    }
    let (word, _) = os.read_until(regs.r[0], 4, |b| b <= b' ')?;
    let (after, _) = os.skip_spaces(regs.r[0].wrapping_add(len32(word.len())))?;
    let on = match &*word.to_ascii_lowercase() {
        b"on" if after < b' ' => CACHES,
        b"off" if after < b' ' => 0,
        _ => return Err(os.error(crate::error::BAD_PARAMETERS)),
    };
    control.r[..3].copy_from_slice(&[0, on, !CACHES]);
    os.perform(X | os::MMU_CONTROL, &mut control)
}

/// `*ChangeDynamicArea`: each memory area the tail names set to its size.
pub(crate) fn change_dynamic_area(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (buffer, held) = os.cli_buffer();
    let result = (|| {
        os.read_args_into(AREA_KEYS, regs.r[0], buffer, AREA_ROOM)?;
        for (key, area) in AREA_ORDER {
            let given = os.read_word(buffer.wrapping_add(4 * key))?;
            if given == 0 {
                continue;
            }
            let mut current = Regs::default();
            current.r[0] = area;
            os.perform(X | os::READ_DYNAMIC_AREA, &mut current)?;
            let size = os.size_given(given)?;
            let mut change = Regs::default();
            change.r[..2].copy_from_slice(&[area, size.wrapping_sub(current.r[1])]);
            os.perform(X | os::CHANGE_DYNAMIC_AREA, &mut change)?;
        }
        Ok(())
    })();
    os.world.cli.release(held);
    result
}

#[cfg(test)]
mod tests;
