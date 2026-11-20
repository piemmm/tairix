//! The system statements: errors and their handling, `TRACE`, `RUN`,
//! `CHAIN`, `QUIT`, and the machine interface — `CALL`, `USR`, `SYS`,
//! `OSCLI` and `*` lines — which leave for User mode and are resumed when
//! the guest is done.

use tairix_riscos_kernel::os::Regs;
use tairix_riscos_kernel::swi::{os as swi, X};

use super::super::functions::{string_of, Guest};
use super::super::lvalue::{Lvalue, Place};
use super::super::next::{Delivered, Next, Payload};
use super::super::{flag, len32, Leave, Machine, Mode, Step, Trap};
use super::cont;
use crate::messages::Error;
use crate::stack::Kind;
use crate::token;
use crate::value::{Text, Value};
use crate::workspace::{self as ws, reg};

/// The most registers `SYS` passes and takes back: R0 to R9, as recorded.
const SYS_REGISTERS: usize = 10;
/// The flags `SYS`'s SWI is entered with, Z and C, as recorded: what `SYS`'s
/// `;` variable gives where the SWI changes none.
const SYS_FLAGS: u32 = tairix_riscos_arm::psr::Z | tairix_riscos_arm::psr::C;
/// `OS_SWINumberFromString`.
const SWI_NUMBER_FROM_STRING: u32 = 0x39;

/// What follows a command BASIC runs once it returns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Then {
    /// Nothing: the statement goes on.
    Done,
    /// `EDIT`'s editor started, its module ensured.
    StartEditor,
}

impl Then {
    /// The `Then` its record's word is.
    const fn of(word: u32) -> Option<Self> {
        match word {
            0 => Some(Self::Done),
            1 => Some(Self::StartEditor),
            _ => None,
        }
    }
}

impl Machine<'_, '_> {
    // ON ERROR, LOCAL ERROR, RESTORE ERROR.

    /// `ON ERROR`'s forms: `OFF`, `LOCAL` and statements to run on an error.
    pub(crate) fn on_error(&mut self) -> Step<Next> {
        if self.take(token::OFF)? {
            self.os.write_word(reg::ON_ERROR.at(), 0)?;
            return Ok(Next::Done);
        }
        let local = self.take(token::LOCAL)?;
        let sp = if local { self.sp } else { self.himem };
        self.skip_spaces()?;
        self.os.write_word(reg::ON_ERROR.at(), self.ptr)?;
        self.os.write_word(reg::ON_ERROR_SP.at(), sp)?;
        let line = if self.line == 0 { 0 } else { self.line };
        self.os.write_word(reg::ON_ERROR_LINE.at(), line)?;
        self.skip_line()?;
        Ok(Next::Done)
    }

    /// `LOCAL ERROR`: the error handling in force saved, to come back at the
    /// routine's end or `RESTORE ERROR`.
    pub(crate) fn local_error(&mut self) -> Step {
        let handler = self.os.read_word(reg::ON_ERROR.at())?;
        let sp = self.os.read_word(reg::ON_ERROR_SP.at())?;
        let line = self.os.read_word(reg::ON_ERROR_LINE.at())?;
        self.push(Kind::LocalError, 0, &[handler, sp, line])
    }

    /// `RESTORE ERROR`: the error handling `LOCAL ERROR` saved put back.
    pub(crate) fn restore_error(&mut self) -> Step {
        let header = self.top_header()?.ok_or(Error::NoErrorStatus)?;
        if header.kind != Kind::LocalError {
            return Err(Error::NoErrorStatus.into());
        }
        self.restore_local_error()
    }

    /// The saved error handling on top put back.
    pub(crate) fn restore_local_error(&mut self) -> Step {
        let mut words = [0; 3];
        self.pop_record(Kind::LocalError, &mut words)?;
        self.os.write_word(reg::ON_ERROR.at(), words[0])?;
        self.os.write_word(reg::ON_ERROR_SP.at(), words[1])?;
        Ok(self.os.write_word(reg::ON_ERROR_LINE.at(), words[2])?)
    }

    // ERROR and REPORT.

    /// `ERROR [EXT] number, message`.
    pub(crate) fn error_statement(&mut self) -> Step<Next> {
        let ext = self.skip_spaces()? == b'E' && self.byte_at(self.ptr + 1)? == b'X';
        if ext {
            if self.byte_at(self.ptr + 2)? != b'T' {
                return Err(Error::Syntax.into());
            }
            self.ptr += 3;
        }
        self.evaluate(cont::ERROR, Payload::of(&[u32::from(ext), 0, 0]))
    }

    /// `ERROR`'s number, then its message: the error raised.
    pub(crate) fn error_argument(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        if payload.word(1) == 0 {
            let number = self.int_value(value)?.cast_unsigned();
            self.expect(b',', Error::MissingComma)?;
            return self.evaluate(cont::ERROR, payload.with(1, 1).with(2, number));
        }
        let text = string_of(value)?;
        let block = ws::STRACC.at();
        self.os.write_word(block, payload.word(2))?;
        self.os.write_bytes(block + 4, text.as_bytes())?;
        self.os.write_byte(block + 4 + u32::from(text.len()), 0)?;
        if payload.word(0) != 0 {
            return self.leave_with_error(block);
        }
        Err(super::super::Trap::Os(block))
    }

    /// `REPORT`: the last error's message written.
    pub(crate) fn report_statement(&mut self) -> Step<Next> {
        let text = self.report_text()?;
        self.print_text(&text)?;
        Ok(Next::Done)
    }

    // TRACE.

    /// `TRACE ON`, `OFF`, `PROC`, `ENDPROC`, `TO` a file, or a line limit.
    pub(crate) fn trace_statement(&mut self) -> Step<Next> {
        let b = self.skip_spaces()?;
        match b {
            token::ON => {
                self.ptr += 1;
                self.os.write_word(reg::TRACE_LIMIT.at(), u32::MAX)?;
                self.trace_flags(flag::TRACE, 0)?;
            }
            token::OFF => {
                self.ptr += 1;
                self.trace_flags(0, flag::TRACE | flag::TRACE_PROC)?;
                self.close_trace_file()?;
            }
            token::PROC => {
                self.ptr += 1;
                self.trace_flags(flag::TRACE_PROC, 0)?;
            }
            token::ENDPROC => {
                self.ptr += 1;
                self.trace_flags(0, flag::TRACE_PROC)?;
            }
            token::TO => {
                self.ptr += 1;
                return self.evaluate(cont::TRACE, Payload::of(&[trace::TO]));
            }
            token::STEP => {
                self.ptr += 1;
                return self.evaluate(cont::TRACE, Payload::of(&[trace::STEP]));
            }
            _ => {
                // A limit typed as a number is tokenised as a line reference.
                if let Some(limit) = self.line_reference()? {
                    return self.trace_argument(
                        Payload::of(&[trace::LIMIT]),
                        &Value::Int(limit.cast_signed()),
                    );
                }
                return self.evaluate(cont::TRACE, Payload::of(&[trace::LIMIT]));
            }
        }
        Ok(Next::Done)
    }

    /// `TRACE`'s argument: a line limit, `TO`'s file or `STEP`'s limit.
    pub(crate) fn trace_argument(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        if payload.word(0) == trace::TO {
            let name = string_of(value)?;
            let at = ws::STRACC.at();
            self.os.write_bytes(at, name.as_bytes())?;
            self.os.write_byte(at + u32::from(name.len()), b'\r')?;
            let mut regs = Regs::default();
            regs.r[..2].copy_from_slice(&[0x83, at]);
            self.os.perform(X | swi::FIND, &mut regs)?;
            self.close_trace_file()?;
            self.os.write_word(ws::TRACEF.at(), regs.r[0])?;
        } else {
            let limit = self.int_value(value)?.cast_unsigned();
            self.os.write_word(reg::TRACE_LIMIT.at(), limit)?;
            self.trace_flags(flag::TRACE, 0)?;
        }
        Ok(Next::Done)
    }

    /// The trace flags `set` and `cleared`, and with them the trace flag in
    /// `ESCWORD`, which called code reads.
    fn trace_flags(&mut self, set: u32, cleared: u32) -> Step {
        self.flags = self.flags & !cleared | set;
        let word = self.os.read_word(ws::ESCWORD.at())?;
        let word = if self.flags & (flag::TRACE | flag::TRACE_PROC) == 0 {
            word & !ws::TRACING
        } else {
            word | ws::TRACING
        };
        Ok(self.os.write_word(ws::ESCWORD.at(), word)?)
    }

    /// The trace file closed, if one is open.
    fn close_trace_file(&mut self) -> Step {
        let handle = self.os.read_word(ws::TRACEF.at())?;
        if handle != 0 {
            let mut regs = Regs::default();
            regs.r[..2].copy_from_slice(&[0, handle]);
            self.os.write_word(ws::TRACEF.at(), 0)?;
            self.os.perform(X | swi::FIND, &mut regs)?;
        }
        Ok(())
    }

    /// A line's number traced, in square brackets, where `TRACE` asks.
    pub(crate) fn trace_line(&mut self) -> Step {
        if self.flags & flag::TRACE == 0 {
            return Ok(());
        }
        let number = self.line_number()?;
        let limit = self.os.read_word(reg::TRACE_LIMIT.at())?;
        if number > limit {
            return Ok(());
        }
        let mut text = Text::EMPTY;
        text.push_bytes(b"[").ok_or(Error::StringTooLong)?;
        let digits = super::super::functions::decimal(number.cast_signed());
        text.push_bytes(digits.as_bytes())
            .ok_or(Error::StringTooLong)?;
        text.push_bytes(b"] ").ok_or(Error::StringTooLong)?;
        let handle = self.os.read_word(ws::TRACEF.at())?;
        if handle == 0 {
            return self.output(text.as_bytes());
        }
        for &b in text.as_bytes() {
            let mut regs = Regs::default();
            regs.r[..2].copy_from_slice(&[u32::from(b), handle]);
            self.os.perform(X | swi::BPUT, &mut regs)?;
        }
        Ok(())
    }

    // RUN, CHAIN, QUIT.

    /// `RUN`: the variables cleared, the program run from its first line.
    pub(crate) fn run_statement(&mut self) -> Step<Next> {
        self.run_program()?;
        Ok(Next::Moved)
    }

    /// The program run from its first line, every variable but the
    /// resident integers forgotten.
    pub(crate) fn run_program(&mut self) -> Step {
        self.clear()?;
        self.flags |= flag::RUNNING;
        self.os.write_word(reg::ERL.at(), 0)?;
        self.line = 0;
        let first = self.page;
        if self.byte_at(first)? != b'\r' {
            return Err(Error::BadProgram.into());
        }
        if self.byte_at(first + 1)? == tairix_openbasic::program::END {
            self.ptr = first;
            self.line = first;
            self.mode = Mode::Statement;
            return Ok(());
        }
        self.line = first;
        self.ptr = first + 4;
        self.mode = Mode::Statement;
        self.trace_line()
    }

    /// `CHAIN`'s file: loaded and run.
    pub(crate) fn chain(&mut self, value: &Value) -> Step<Next> {
        self.load_program(string_of(value)?)?;
        self.run_program()?;
        Ok(Next::Moved)
    }

    /// `QUIT`, with an exit code or without.
    pub(crate) fn quit_statement(&mut self) -> Step<Next> {
        if self.at_statement_end()? {
            self.os.write_word(reg::EXIT_CODE.at(), 0)?;
            self.mode = Mode::Quit;
            return Ok(Next::Leave(Leave::Quit));
        }
        self.evaluate(cont::QUIT, Payload::default())
    }

    /// `QUIT`'s code.
    pub(crate) fn quit_code(&mut self, value: &Value) -> Step<Next> {
        let code = self.int_value(value)?.cast_unsigned();
        self.os.write_word(reg::EXIT_CODE.at(), code)?;
        self.mode = Mode::Quit;
        Ok(Next::Leave(Leave::Quit))
    }

    // OSCLI and * lines.

    /// A `*` line: the rest of it a command.
    pub(crate) fn star_line(&mut self) -> Step<Next> {
        let mut text = Text::EMPTY;
        loop {
            let b = self.next()?;
            if b == b'\r' {
                self.ptr -= 1;
                break;
            }
            text.push_bytes(&[b]).ok_or(Error::LineTooLong)?;
        }
        self.command_text(&text)
    }

    /// `OSCLI`'s command.
    pub(crate) fn oscli(&mut self, value: &Value) -> Step<Next> {
        self.command_text(string_of(value)?)
    }

    /// `text` run as a command from User mode, its CR after it on the stack.
    pub(crate) fn command_text(&mut self, text: &Text) -> Step<Next> {
        self.command_then(text, Then::Done)
    }

    /// `text` run as [`Self::command_text`] runs it, `then` done once it
    /// returns.
    pub(crate) fn command_then(&mut self, text: &Text, then: Then) -> Step<Next> {
        let words = (u32::from(text.len()) + 1).div_ceil(4) as usize;
        let at = self.stack_block(words)?;
        self.os.write_bytes(at, text.as_bytes())?;
        self.os.write_byte(at + u32::from(text.len()), b'\r')?;
        self.push(Kind::Guest, Guest::Command as u8, &[then as u32])?;
        self.mode = Mode::Away;
        let mut regs = [0; 10];
        regs[0] = at;
        Ok(Next::Leave(Leave::Swi {
            number: swi::CLI,
            regs,
            flags: 0,
        }))
    }

    /// A block of `words` words on the stack for the machine's own use, in a
    /// record of its own: its address.
    fn stack_block(&mut self, words: usize) -> Step<u32> {
        self.local_block(words, 1, Error::NoRoom)
    }

    // CALL and USR.

    /// `CALL`: its address, then its parameters' l-values.
    pub(crate) fn call_statement(&mut self) -> Step<Next> {
        self.evaluate(cont::CALL, Payload::of(&[0, 0]))
    }

    /// `CALL`'s address or a parameter.
    pub(crate) fn call_argument(&mut self, payload: Payload, delivered: &Delivered) -> Step<Next> {
        let count = payload.word(1);
        match delivered {
            Delivered::Value(value) => {
                let address = self.int_value(value)?.cast_unsigned();
                let payload = Payload::of(&[address, 0]);
                if self.take(b',')? {
                    return self.lvalue(cont::CALL, payload);
                }
                self.enter_code(address, 0, Guest::Call)
            }
            Delivered::Lvalue(lvalue) => {
                let place = lvalue.place.call_type(self.dialect());
                self.push(Kind::Local, 2, &[lvalue.address, place])?;
                let count = count.checked_add(1).ok_or(Error::StackCorrupt)?;
                let payload = payload.with(1, count);
                if self.take(b',')? {
                    return self.lvalue(cont::CALL, payload);
                }
                self.enter_code(payload.word(0), count, Guest::Call)
            }
        }
    }

    /// `USR`'s code entered, its R0 the value.
    pub(crate) fn usr(&mut self, address: u32) -> Step<Next> {
        self.enter_code(address, 0, Guest::Usr)
    }

    /// The code at `address` entered as `CALL` documents: A% to H% in R0 to
    /// R7, the workspace, the list of the `count` parameters on top, the
    /// string accumulator, the line and the stack in R8 to R13. The list is
    /// each parameter's l-value and type in two words, the last parameter
    /// first, as recorded.
    fn enter_code(&mut self, address: u32, count: u32, kind: Guest) -> Step<Next> {
        let mut base = self.sp;
        let list = self.stack_block(2 * count as usize)?;
        for i in 0..count {
            let header = self.header_at(base)?.ok_or(Error::StackCorrupt)?;
            if (header.kind, header.aux) != (Kind::Local, 2) {
                return Err(Error::StackCorrupt.into());
            }
            let (lvalue, kind) = (self.os.read_word(base + 4)?, self.os.read_word(base + 8)?);
            self.os.write_word(list + 8 * i, lvalue)?;
            self.os.write_word(list + 8 * i + 4, kind)?;
            base += header.length;
        }
        let mut regs = [0; 14];
        for (i, slot) in (0..).zip(regs[..8].iter_mut()) {
            *slot = self.os.read_word(ws::RESIDENT.at() + 4 * i)?;
        }
        regs[8] = ws::ARGP;
        regs[9] = list;
        regs[10] = count;
        regs[11] = ws::STRACC.at();
        regs[12] = self.ptr;
        self.push(Kind::Guest, kind as u8, &[base])?;
        regs[13] = self.sp;
        self.mode = Mode::Away;
        Ok(Next::Leave(Leave::Call { address, regs }))
    }

    // SYS.

    /// `SYS`: the SWI, its inputs, then its outputs after `TO`.
    pub(crate) fn sys_statement(&mut self) -> Step<Next> {
        self.evaluate(cont::SYS, Payload::of(&[SYS_SWI, 0]))
    }

    /// `SYS`'s SWI, an input or an output l-value, by its stage.
    pub(crate) fn sys_argument(&mut self, payload: Payload, delivered: &Delivered) -> Step<Next> {
        match (payload.word(0), delivered) {
            (SYS_SWI, Delivered::Value(value)) => {
                let number = match value {
                    Value::Str(name) => match self.swi_named(name)? {
                        Ok(number) => number,
                        Err(leave) => return Ok(Next::Leave(leave)),
                    },
                    number => self.int_value(number)?.cast_unsigned(),
                };
                self.push(Kind::Int, 0, &[number])?;
                self.sys_inputs(0)
            }
            (SYS_INPUT, Delivered::Value(value)) => {
                let index = payload.word(1);
                if index as usize >= SYS_REGISTERS {
                    return Err(Error::StackCorrupt.into());
                }
                match value {
                    Value::Str(text) => {
                        let words = (u32::from(text.len()) + 1).div_ceil(4) as usize;
                        let at = self.stack_block(words)?;
                        self.os.write_bytes(at, text.as_bytes())?;
                        self.os.write_byte(at + u32::from(text.len()), 0)?;
                        self.push(Kind::Int, 1, &[at])?;
                    }
                    number => {
                        let n = self.int_value(number)?;
                        self.push(Kind::Int, 1, &[n.cast_unsigned()])?;
                    }
                }
                self.sys_inputs(index + 1)
            }
            (SYS_OUTPUT, Delivered::Lvalue(lvalue)) => self.sys_output(payload, *lvalue),
            _ => Err(Error::StackCorrupt.into()),
        }
    }

    /// `SYS`'s input `index` on: another after a comma, an empty one between
    /// commas, or the SWI left for.
    fn sys_inputs(&mut self, index: u32) -> Step<Next> {
        let mut index = index;
        loop {
            if !self.take(b',')? {
                return self.sys_leave(index);
            }
            if index as usize >= SYS_REGISTERS {
                return Err(Error::TooManyInputs.into());
            }
            let b = self.skip_spaces()?;
            if !(b == b',' || b == token::TO || self.at_statement_end()?) {
                return self.evaluate(cont::SYS, Payload::of(&[SYS_INPUT, index]));
            }
            self.push(Kind::Int, 1, &[0])?;
            index += 1;
        }
    }

    /// The SWI with the `count` inputs on top left for. The inputs are read
    /// where they lie, among the blocks their strings were copied to, which
    /// stay on the stack until the SWI is back.
    fn sys_leave(&mut self, count: u32) -> Step<Next> {
        let mut regs = [0; SYS_REGISTERS];
        let mut at = self.sp;
        let mut left = count as usize;
        if left > regs.len() {
            return Err(Error::StackCorrupt.into());
        }
        while left > 0 {
            let header = self.header_at(at)?.ok_or(Error::StackCorrupt)?;
            match (header.kind, header.aux) {
                (Kind::Int, 1) => {
                    left -= 1;
                    regs[left] = self.os.read_word(at + 4)?;
                }
                (Kind::LocalBlock, 1) => {}
                _ => return Err(Error::StackCorrupt.into()),
            }
            at += header.length;
        }
        let header = self.header_at(at)?.ok_or(Error::StackCorrupt)?;
        if (header.kind, header.aux) != (Kind::Int, 0) {
            return Err(Error::StackCorrupt.into());
        }
        let number = [self.os.read_word(at + 4)?];
        let base = at + header.length;
        let outputs = self.take(token::TO)?;
        self.push(
            Kind::Guest,
            Guest::Sys as u8,
            &[self.ptr, u32::from(outputs), base],
        )?;
        self.mode = Mode::Away;
        Ok(Next::Leave(Leave::Swi {
            number: number[0],
            regs,
            flags: SYS_FLAGS,
        }))
    }

    /// A SWI's number from its name; a name `OS_SWINumberFromString` does
    /// not know raised through the OS, as BASIC calls it in its non-X form,
    /// which ends any redirection before the error handler runs: the SWI
    /// left for to raise it.
    pub(crate) fn swi_named(&mut self, name: &Text) -> Step<Result<u32, Leave>> {
        match self.swi_number(name) {
            Ok(number) => Ok(Ok(number)),
            Err(Trap::Os(block)) => Ok(Err(self.raise_block_through_os(block)?)),
            Err(trap) => Err(trap),
        }
    }

    /// A SWI's number from its name, as `OS_SWINumberFromString` gives it.
    fn swi_number(&mut self, name: &Text) -> Step<u32> {
        let at = ws::STRACC.at();
        self.os.write_bytes(at, name.as_bytes())?;
        self.os.write_byte(at + u32::from(name.len()), 0)?;
        let mut regs = Regs::default();
        regs.r[1] = at;
        self.os.perform(X | SWI_NUMBER_FROM_STRING, &mut regs)?;
        Ok(regs.r[0])
    }

    /// `SYS`'s outputs, the SWI's registers kept on the stack: each given to
    /// its variable, the flags to the one after `;`.
    fn sys_output(&mut self, payload: Payload, lvalue: Lvalue) -> Step<Next> {
        let index = payload.word(1);
        let regs_at = payload.word(2);
        let flags = payload.word(3) != 0;
        // The flags come after every register's output, so past the last.
        let past = if flags {
            SYS_REGISTERS + 1
        } else {
            SYS_REGISTERS
        };
        if index as usize >= past {
            return Err(Error::StackCorrupt.into());
        }
        let slot = if flags { len32(SYS_REGISTERS) } else { index };
        let value = self.os.read_word(regs_at.wrapping_add(4 * slot))?;
        let value = if lvalue.place.is_string() {
            Value::Str(self.string_at(value)?)
        } else {
            Value::Int(value.cast_signed())
        };
        self.write(lvalue, &value)?;
        self.sys_next_output(regs_at, index + 1, flags)
    }

    /// The next `SYS` output after the one at `index`.
    fn sys_next_output(&mut self, regs_at: u32, index: u32, flags_done: bool) -> Step<Next> {
        if flags_done {
            return self.sys_done();
        }
        if self.take(b';')? {
            return self.lvalue(cont::SYS, Payload::of(&[SYS_OUTPUT, index, regs_at, 1]));
        }
        if !self.take(b',')? {
            return self.sys_done();
        }
        if index as usize >= SYS_REGISTERS {
            return Err(Error::TooManyOutputs.into());
        }
        let b = self.skip_spaces()?;
        if b == b',' || b == b';' {
            return self.sys_next_output(regs_at, index + 1, false);
        }
        self.lvalue(cont::SYS, Payload::of(&[SYS_OUTPUT, index, regs_at, 0]))
    }

    /// `SYS` done: its registers' record and its inputs dropped.
    fn sys_done(&mut self) -> Step<Next> {
        let mut kept = [0_u32; SYS_REGISTERS + 2];
        let header = self.pop_record(Kind::LocalBlock, &mut kept)?;
        let base = kept[SYS_REGISTERS + 1];
        if header.aux != 2 || base < self.sp || base > self.himem {
            return Err(Error::StackCorrupt.into());
        }
        self.sp = base;
        Ok(Next::Done)
    }

    /// The string at `at`, to a zero, LF or CR.
    fn string_at(&mut self, at: u32) -> Step<Text> {
        let mut text = Text::EMPTY;
        for i in 0..crate::value::STRING_MOST {
            let b = self.os.read_byte(at.wrapping_add(len32(i)))?;
            if matches!(b, 0 | b'\n' | b'\r') {
                break;
            }
            text.push_bytes(&[b]).ok_or(Error::StringTooLong)?;
        }
        Ok(text)
    }

    /// A SWI performed for the machine come back: what it was for resumed
    /// with its registers and flags.
    pub(crate) fn swi_returned(&mut self, regs: &Regs) -> Step<Next> {
        let mut words = [0; 3];
        let header = self.top_header()?.ok_or(Error::StackCorrupt)?;
        let carried = (header.length / 4 - 1).min(3) as usize;
        let guest = self.pop_record(Kind::Guest, &mut words[..carried])?;
        let kind = Guest::of(guest.aux).ok_or(Error::StackCorrupt)?;
        self.mode = Mode::Statement;
        let escape = regs.flag(tairix_riscos_arm::psr::C);
        match kind {
            Guest::Key => {
                if escape {
                    return Err(Error::Escape.into());
                }
                let code = regs.r[0] & 0xFF;
                let value = if words[0] != 0 {
                    Value::Int(code.cast_signed())
                } else {
                    Value::Str(Text::of(&[code as u8]).ok_or(Error::StringTooLong)?)
                };
                self.mode = Mode::Operator;
                self.operand_is(&value)
            }
            Guest::Inkey => {
                let number = words[0] != 0;
                let n = words[1].cast_signed();
                let value = inkey_value(n, number, regs);
                self.mode = Mode::Operator;
                self.operand_is(&value)
            }
            Guest::Input => self.input_read(words, regs.r[1], escape),
            Guest::Prompt => self.typed(regs.r[1], escape),
            Guest::Command => {
                self.drop_top()?;
                match Then::of(words[0]).ok_or(Error::StackCorrupt)? {
                    Then::Done => Ok(Next::Done),
                    Then::StartEditor => self.start_editor(),
                }
            }
            Guest::Sys => self.sys_returned(words, regs),
            // A claimant of ErrorV swallowed it: met as BASIC meets the
            // error all the same.
            Guest::Raise => Err(super::super::Trap::Os(words[0])),
            Guest::Call | Guest::Usr => Err(Error::StackCorrupt.into()),
        }
    }

    /// `SYS`'s SWI come back: its registers kept for its outputs, above its
    /// inputs, whose strings an output may name, with where they end.
    fn sys_returned(&mut self, words: [u32; 3], regs: &Regs) -> Step<Next> {
        self.ptr = words[0];
        if words[2] < self.sp || words[2] > self.himem {
            return Err(Error::StackCorrupt.into());
        }
        let mut kept = [0_u32; SYS_REGISTERS + 2];
        kept[..SYS_REGISTERS].copy_from_slice(&regs.r[..SYS_REGISTERS]);
        kept[SYS_REGISTERS] = nzcv(regs.flags);
        kept[SYS_REGISTERS + 1] = words[2];
        self.push(Kind::LocalBlock, 2, &kept)?;
        let regs_at = self.sp + 4;
        if words[1] == 0 {
            return self.sys_done();
        }
        let b = self.skip_spaces()?;
        if b == b';' {
            self.ptr += 1;
            return self.lvalue(cont::SYS, Payload::of(&[SYS_OUTPUT, 0, regs_at, 1]));
        }
        if b == b',' {
            // R0 left out: the comma takes the outputs on to R1.
            return self.sys_next_output(regs_at, 1, false);
        }
        self.lvalue(cont::SYS, Payload::of(&[SYS_OUTPUT, 0, regs_at, 0]))
    }

    /// Code `CALL` or `USR` entered come back: the parameter list dropped,
    /// `USR`'s value its R0.
    pub(crate) fn call_returned(&mut self, regs: &Regs) -> Step<Next> {
        let mut words = [0; 1];
        let guest = self.pop_record(Kind::Guest, &mut words)?;
        let kind = Guest::of(guest.aux).ok_or(Error::StackCorrupt)?;
        let base = words[0];
        if base < self.sp || base > self.himem {
            return Err(Error::StackCorrupt.into());
        }
        self.sp = base;
        if regs.flag(tairix_riscos_arm::psr::V) {
            return Err(super::super::Trap::Os(regs.r[0]));
        }
        match kind {
            Guest::Call => {
                self.mode = Mode::Statement;
                Ok(Next::Done)
            }
            Guest::Usr => {
                self.mode = Mode::Operator;
                self.operand_is(&Value::Int(regs.r[0].cast_signed()))
            }
            _ => Err(Error::StackCorrupt.into()),
        }
    }

    // EVAL and RND.

    /// `EVAL`'s string evaluated, the value then an operand.
    pub(crate) fn eval_string(&mut self, text: &Text) -> Step<Next> {
        self.evaluate_text(text, cont::EVAL, Payload::default())
    }

    /// `text` tokenised on the stack and evaluated there as an expression,
    /// then `cont` resumed with its value, the pointer put back first.
    pub(crate) fn evaluate_text(&mut self, text: &Text, cont: u8, payload: Payload) -> Step<Next> {
        let mut tokens = alloc::vec::Vec::new();
        tairix_openbasic::tokenise::tokenise(
            tairix_openbasic::Dialect::BasicV,
            text.as_bytes(),
            tairix_openbasic::tokenise::Start::Expression,
            &mut tokens,
        )
        .map_err(|_| Error::NoRoom)?;
        self.push(Kind::Continuation, cont, payload.words())?;
        self.push(
            Kind::Continuation,
            cont::TEXT_RESTORE,
            &[self.ptr, self.line],
        )?;
        let at = self.stack_block((tokens.len() + 1).div_ceil(4))?;
        self.os.write_bytes(at, &tokens)?;
        self.os.write_byte(at + len32(tokens.len()), b'\r')?;
        self.push(Kind::Continuation, cont::TEXT_END, &[])?;
        self.push(Kind::Expression, 0, &[])?;
        self.ptr = at;
        self.mode = Mode::Operand;
        Ok(Next::Moved)
    }

    /// A text's expression evaluated whole: the text dropped, the pointer
    /// put back, the value delivered.
    pub(crate) fn text_done(&mut self, value: &Value) -> Step<Next> {
        if self.skip_spaces()? != b'\r' {
            return Err(Error::Mistake.into());
        }
        self.drop_top()?;
        let mut words = [0; 2];
        self.pop_record(Kind::Continuation, &mut words)?;
        self.ptr = words[0];
        self.line = words[1];
        self.deliver(Delivered::Value(*value))
    }

    /// `EVAL`'s value, an operand of the expression that asked.
    pub(crate) fn eval_done(&mut self, value: &Value) -> Step<Next> {
        self.mode = Mode::Operator;
        self.operand_is(value)
    }

    /// `RND(n)`'s argument.
    pub(crate) fn rnd_argument(&mut self, value: &Value) -> Step<Next> {
        self.expect(b')', Error::MissingBracket)?;
        let n = self.int_value(value)?;
        let result = self.rnd(n)?;
        self.mode = Mode::Operator;
        self.operand_is(&result)
    }

    /// The machine left BASIC with the error at `block`, as `ERROR EXT`
    /// does: its handlers given back and the error raised to its parent.
    fn leave_with_error(&mut self, block: u32) -> Step<Next> {
        self.os.write_word(reg::EXIT_CODE.at(), block)?;
        self.flags |= flag::OS_ERROR;
        self.mode = Mode::Quit;
        Ok(Next::Leave(Leave::Quit))
    }

    // Whole arrays and the rest.

    /// `MOUSE`'s forms.
    pub(crate) fn mouse_statement(&mut self) -> Step<Next> {
        self.lvalue(cont::MOUSE, Payload::of(&[0]))
    }

    /// `MOUSE`'s l-values: x, y and the buttons, then the time.
    pub(crate) fn mouse_item(&mut self, payload: Payload, lvalue: Lvalue) -> Step<Next> {
        let index = payload.word(0);
        if index >= 4 {
            return Err(Error::StackCorrupt.into());
        }
        let place = lvalue.place.call_type(self.dialect());
        self.push(Kind::Local, 3, &[lvalue.address, place])?;
        let count = index + 1;
        if count < 4 && self.take(b',')? {
            return self.lvalue(cont::MOUSE, Payload::of(&[count]));
        }
        if count < 3 {
            return Err(Error::BadMouseVariable.into());
        }
        let mut regs = Regs::default();
        self.os.perform(X | swi::MOUSE, &mut regs)?;
        let values = [regs.r[0], regs.r[1], regs.r[2], regs.r[3]];
        for i in (0..count as usize).rev() {
            let mut words = [0; 2];
            self.pop_record(Kind::Local, &mut words)?;
            let place = Place::of_call_type(words[1], self.dialect()).ok_or(Error::StackCorrupt)?;
            let target = Lvalue {
                address: words[0],
                place,
            };
            self.write(target, &Value::Int(values[i].cast_signed()))?;
        }
        Ok(Next::Done)
    }
}

/// `SYS`'s stages.
const SYS_SWI: u32 = 0;
const SYS_INPUT: u32 = 1;
const SYS_OUTPUT: u32 = 2;

/// What `TRACE`'s argument is: a line limit, `TO`'s file or `STEP`'s limit.
mod trace {
    pub(super) const LIMIT: u32 = 0;
    pub(super) const TO: u32 = 1;
    pub(super) const STEP: u32 = 2;
}

/// The flags as `SYS` gives them: `%NZCV`.
const fn nzcv(flags: u32) -> u32 {
    flags >> 28 & 0xF
}

/// `INKEY`'s value from `OS_Byte` 129's answer.
fn inkey_value(n: i32, number: bool, regs: &Regs) -> Value {
    let (x, y) = (regs.r[1], regs.r[2]);
    if n < 0 {
        if n == -256 {
            return Value::Int(x.cast_signed());
        }
        let pressed = x == 0xFF;
        return super::super::eval::truth_value(pressed);
    }
    let timed_out = y == 0xFF;
    match (number, timed_out) {
        (true, true) => Value::Int(-1),
        (true, false) => Value::Int((x & 0xFF).cast_signed()),
        (false, true) => Value::Str(Text::EMPTY),
        (false, false) => Value::Str(Text::of(&x.to_le_bytes()[..1]).unwrap_or(Text::EMPTY)),
    }
}

#[cfg(test)]
mod tests {
    use super::nzcv;

    #[test]
    fn sys_gives_the_flags_as_nzcv() {
        assert_eq!(nzcv(0xF000_0000), 0xF);
        assert_eq!(nzcv(1 << 28), 1, "V");
        assert_eq!(nzcv(1 << 31), 8, "N");
    }
}
