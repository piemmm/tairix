//! The interpreter: a machine whose registers, stack, program and variables
//! all live in guest memory, run a slice at a time by BASIC's run routine.
//!
//! A slice loads the registers from the workspace, steps until it has worked
//! a host slice's worth or must leave for User mode — to perform a SWI, to
//! enter code `CALL` names, to read the keyboard — then writes them back and
//! leaves. Nothing it is part way through is held on the host between
//! slices, so a program's depth ends in BASIC's own "No room" and a context
//! can be stopped and resumed at any statement.

mod arrays;
mod assembler;
mod editor;
mod eval;
pub(crate) mod functions;
mod guest;
mod help;
mod lvalue;
pub(crate) mod next;
mod print;
mod program;
mod routines;
mod session;
mod statements;
mod vars;

use alloc::vec::Vec;

use tairix_riscos_kernel::os::{Fail, Os, Unwind};

use crate::messages::Error;
use crate::stack::{Header, Kind, MOST_WORDS};
use crate::value::{Arith, Dialect, Number, Real, Text, Value, STRING_MOST};
use crate::workspace::{self as ws, reg};
use crate::Fault;

pub(crate) use guest::RoutineCall;
pub(crate) use lvalue::{Lvalue, Place};
pub(crate) use tairix_riscos_kernel::os::len32;
pub(crate) use vars::{in_name, starts_name, Name};

/// What the machine does next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    /// Gives the prompt and reads a line.
    Prompt = 0,
    /// Interprets the statement at the program pointer.
    Statement = 1,
    /// Reads an operand of the expression being evaluated.
    Operand = 2,
    /// Has an operand of it: reads an operator, or the expression's end.
    Operator = 3,
    /// Has left for User mode: the record on top says what for.
    Away = 4,
    /// Quits BASIC.
    Quit = 5,
    /// Assembles the statement at the program pointer.
    Assemble = 6,
}

impl Mode {
    const fn of(word: u32) -> Option<Self> {
        Some(match word {
            0 => Self::Prompt,
            1 => Self::Statement,
            2 => Self::Operand,
            3 => Self::Operator,
            4 => Self::Away,
            5 => Self::Quit,
            6 => Self::Assemble,
            _ => return None,
        })
    }
}

/// The interpreter's switches.
pub(crate) mod flag {
    /// BASIC VI: eight-byte reals.
    pub(crate) const BASIC64: u32 = 1;
    /// `-quit`: a program's end ends BASIC.
    pub(crate) const QUIT_AT_END: u32 = 1 << 1;
    /// A program is running, rather than an immediate line.
    pub(crate) const RUNNING: u32 = 1 << 2;
    /// `TRACE` is on.
    pub(crate) const TRACE: u32 = 1 << 3;
    /// `TRACE PROC` is on.
    pub(crate) const TRACE_PROC: u32 = 1 << 4;
    /// The last error came from a SWI rather than BASIC: `REPORT` shows its
    /// text whole.
    pub(crate) const OS_ERROR: u32 = 1 << 5;
    /// Between `[` and `]`: an unknown variable is `P%` where `OPT` hides
    /// errors.
    pub(crate) const ASSEMBLING: u32 = 1 << 6;
    /// The next assembler statement began directly after a colon or `[`.
    pub(crate) const LABEL_FIELD: u32 = 1 << 7;
}

/// Why the machine stopped a statement.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Trap {
    /// One of BASIC's errors.
    Basic(Error),
    /// An error whose block is in guest memory: a SWI's, a command's.
    Os(u32),
    /// A floating-point exception BASIC VI's arithmetic trapped on.
    Floating(crate::Exception),
    /// Control leaving for good: the kernel's unwind, carried out.
    Unwind(Unwind),
}

impl From<Fail> for Trap {
    fn from(fail: Fail) -> Self {
        match fail {
            Fail::Error(block) => Self::Os(block),
            Fail::Unwind(unwind) => Self::Unwind(unwind),
        }
    }
}

impl From<Unwind> for Trap {
    fn from(unwind: Unwind) -> Self {
        Self::Unwind(unwind)
    }
}

impl From<Error> for Trap {
    fn from(error: Error) -> Self {
        Self::Basic(error)
    }
}

impl From<Fault> for Trap {
    fn from(fault: Fault) -> Self {
        match (Error::of_fault(fault), fault) {
            (Some(error), _) => Self::Basic(error),
            (None, Fault::Floating(exception)) => Self::Floating(exception),
            (None, _) => Self::Basic(Error::TooBig),
        }
    }
}

/// A step's result.
pub(crate) type Step<T = ()> = Result<T, Trap>;

/// Where the machine goes once a slice ends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Leave {
    /// On with the next slice.
    Continue,
    /// To perform SWI `number` with R0 to R9 `regs` from User mode, BASIC's
    /// SWI-done routine resuming the machine after it.
    Swi {
        /// The SWI, its X bit deciding how an error is met.
        number: u32,
        /// R0 to R9.
        regs: [u32; 10],
        /// The flags it is entered with, which it hands back but for those
        /// it sets.
        flags: u32,
    },
    /// To enter code at `address` with R0 to R13 `regs` and R14 the routine
    /// table, whose first word returns to BASIC.
    Call {
        /// The code.
        address: u32,
        /// R0 to R13.
        regs: [u32; 14],
    },
    /// Back to guest code that called a routine of the table, at `pc` with
    /// R0 to R14 `regs` and the condition flags `flags`.
    Return {
        /// Where.
        pc: u32,
        /// R0 to R14.
        regs: [u32; 15],
        /// N, Z, C and V.
        flags: u32,
    },
    /// Out of BASIC, its handlers given back and the program's return code
    /// given to `OS_Exit`.
    Quit,
}

/// The least room left between the variables' end and the stack: what an
/// allocation of either side may not take.
pub(crate) const MARGIN: u32 = 512;

/// Work a statement weighs toward the host's slice: the kernel's own step.
const SLICE_STEPS: u32 = 4096;

/// The machine, its registers loaded from the workspace for one slice.
pub(crate) struct Machine<'o, 'a> {
    pub(crate) os: &'o mut Os<'a>,
    pub(crate) arith: Arith,
    pub(crate) mode: Mode,
    /// The next byte to interpret.
    pub(crate) ptr: u32,
    /// The current line's CR, zero in an immediate line.
    pub(crate) line: u32,
    /// The BASIC stack's pointer.
    pub(crate) sp: u32,
    pub(crate) flags: u32,
    /// The workspace's words the machine checks most, kept as written.
    pub(crate) himem: u32,
    pub(crate) fsa: u32,
    pub(crate) lomem: u32,
    pub(crate) page: u32,
    pub(crate) top: u32,
}

impl<'o, 'a> Machine<'o, 'a> {
    /// The machine as the workspace leaves it.
    pub(crate) fn load(os: &'o mut Os<'a>) -> Step<Self> {
        let flags = os.read_word(reg::FLAGS.at())?;
        let dialect = if flags & flag::BASIC64 == 0 {
            Dialect::V
        } else {
            Dialect::VI
        };
        let fpsr = os.fpsr();
        let mode = Mode::of(os.read_word(reg::MODE.at())?).ok_or(Error::StackCorrupt)?;
        let machine = Self {
            arith: Arith::new(dialect, fpsr),
            mode,
            ptr: os.read_word(reg::PTR.at())?,
            line: os.read_word(reg::LINE.at())?,
            sp: os.read_word(reg::SP.at())?,
            flags,
            himem: os.read_word(ws::HIMEM.at())?,
            fsa: os.read_word(ws::FSA.at())?,
            lomem: os.read_word(ws::LOMEM.at())?,
            page: os.read_word(ws::PAGE.at())?,
            top: os.read_word(ws::TOP.at())?,
            os,
        };
        Ok(machine)
    }

    /// The registers written back, as the next slice loads them.
    pub(crate) fn save(&mut self) -> Step {
        self.os.write_word(reg::MODE.at(), self.mode as u32)?;
        self.os.write_word(reg::PTR.at(), self.ptr)?;
        self.os.write_word(reg::LINE.at(), self.line)?;
        self.os.write_word(reg::SP.at(), self.sp)?;
        self.os.write_word(reg::FLAGS.at(), self.flags)?;
        let fpsr = self.arith.fpsr();
        self.os.set_fpsr(fpsr);
        Ok(())
    }

    /// The machine's dialect.
    pub(crate) const fn dialect(&self) -> Dialect {
        self.arith.dialect()
    }

    /// Runs a slice: steps until the host's slice is spent or the machine
    /// must leave, an error met on the way handled as BASIC handles one.
    pub(crate) fn slice(&mut self) -> Result<Leave, Unwind> {
        let mut steps = 0;
        loop {
            match self.step() {
                Ok(None) => {}
                Ok(Some(leave)) => return Ok(leave),
                Err(Trap::Unwind(unwind)) => return Err(unwind),
                Err(trap) => {
                    if let Some(leave) = self.error(trap)? {
                        return Ok(leave);
                    }
                }
            }
            self.os.yield_point()?;
            steps += 1;
            if steps >= SLICE_STEPS {
                return Ok(Leave::Continue);
            }
        }
    }

    /// One step of what the mode says.
    fn step(&mut self) -> Step<Option<Leave>> {
        let next = match self.mode {
            Mode::Statement => return self.statement(),
            Mode::Operand => self.operand()?,
            Mode::Operator => self.operator()?,
            Mode::Prompt => return self.prompt().map(Some),
            Mode::Away => return Err(Error::StackCorrupt.into()),
            Mode::Quit => return Ok(Some(Leave::Quit)),
            Mode::Assemble => return self.assembler_step(),
        };
        self.follow(next)
    }

    // The program's text.

    /// The byte at `at`.
    pub(crate) fn byte_at(&mut self, at: u32) -> Step<u8> {
        Ok(self.os.read_byte(at)?)
    }

    /// `length` bytes of memory from `at`, copied for the interpreter to
    /// read: BASIC's own "No room" where the host cannot hold them.
    pub(crate) fn copied(&mut self, at: u32, length: usize) -> Step<Vec<u8>> {
        let mut bytes = tairix_util::fallible::zeroed(length).ok_or(Error::NoRoom)?;
        self.os.read_bytes(at, &mut bytes)?;
        Ok(bytes)
    }

    /// The byte at the program pointer.
    pub(crate) fn peek(&mut self) -> Step<u8> {
        self.byte_at(self.ptr)
    }

    /// The byte at the program pointer, which moves past it.
    pub(crate) fn next(&mut self) -> Step<u8> {
        let b = self.peek()?;
        self.ptr = self.ptr.wrapping_add(1);
        Ok(b)
    }

    /// The first byte at or after the pointer that is not a space, which
    /// the pointer is left at.
    pub(crate) fn skip_spaces(&mut self) -> Step<u8> {
        loop {
            let b = self.peek()?;
            if b != b' ' {
                return Ok(b);
            }
            self.ptr = self.ptr.wrapping_add(1);
        }
    }

    /// Whether the next byte after spaces is `b`, taking it if so.
    pub(crate) fn take(&mut self, b: u8) -> Step<bool> {
        if self.skip_spaces()? == b {
            self.ptr = self.ptr.wrapping_add(1);
            return Ok(true);
        }
        Ok(false)
    }

    /// Whether the next bytes after spaces are the two-byte keyword `prefix`,
    /// `second`, taking both if so.
    pub(crate) fn take_two(&mut self, prefix: u8, second: u8) -> Step<bool> {
        if self.skip_spaces()? == prefix && self.byte_at(self.ptr.wrapping_add(1))? == second {
            self.ptr = self.ptr.wrapping_add(2);
            return Ok(true);
        }
        Ok(false)
    }

    /// `b` next after spaces, or `error`.
    pub(crate) fn expect(&mut self, b: u8, error: Error) -> Step {
        if self.take(b)? {
            Ok(())
        } else {
            Err(error.into())
        }
    }

    /// Whether a statement ends at the pointer, after spaces: a colon, the
    /// line's end or an `ELSE`.
    pub(crate) fn at_statement_end(&mut self) -> Step<bool> {
        Ok(matches!(
            self.skip_spaces()?,
            b':' | b'\r' | crate::token::ELSE
        ))
    }

    // The stack.

    /// Room for `bytes` more on the stack, or "No room".
    fn room(&mut self, bytes: u32) -> Step<u32> {
        let below = self
            .sp
            .checked_sub(bytes)
            .filter(|&below| below >= self.fsa.saturating_add(MARGIN))
            .ok_or(Error::NoRoom)?;
        Ok(below)
    }

    /// A block of `words` words on the stack, in a long `LocalBlock` record
    /// with `aux`: its first word's address, or `full` where it has no room.
    pub(crate) fn local_block(&mut self, words: usize, aux: u8, full: Error) -> Step<u32> {
        let length = u32::try_from(words)
            .ok()
            .and_then(|words| words.checked_mul(4))
            .and_then(|bytes| bytes.checked_add(crate::stack::LONG_HEADER))
            .ok_or(full)?;
        let at = self.room(length).map_err(|_| full)?;
        self.os
            .write_word(at, Header::long(Kind::LocalBlock, aux))?;
        self.os.write_word(at + 4, length)?;
        self.sp = at;
        Ok(at + crate::stack::LONG_HEADER)
    }

    /// Pushes a record of `kind` with `aux` and `words` after its header.
    pub(crate) fn push(&mut self, kind: Kind, aux: u8, words: &[u32]) -> Step {
        let length = u32::try_from(words.len())
            .ok()
            .and_then(|count| count.checked_mul(4))
            .and_then(|bytes| bytes.checked_add(4))
            .filter(|&length| length <= crate::stack::SHORT_MOST)
            .ok_or(Error::StackCorrupt)?;
        let at = self.room(length)?;
        self.os.write_word(at, Header::word(kind, aux, length))?;
        self.os.write_words(at + 4, words)?;
        self.sp = at;
        Ok(())
    }

    /// The header of the record on top, none on an empty stack.
    pub(crate) fn top_header(&mut self) -> Step<Option<Header>> {
        self.header_at(self.sp)
    }

    /// The header of the record at `at`, none at the stack's bottom.
    pub(crate) fn header_at(&mut self, at: u32) -> Step<Option<Header>> {
        if at >= self.himem {
            return Ok(None);
        }
        let word = self.os.read_word(at)?;
        let header = Header::of(word, || -> Step<u32> {
            let next = at.checked_add(4).ok_or(Error::StackCorrupt)?;
            Ok(self.os.read_word(next)?)
        })?
        .ok_or(Error::StackCorrupt)?;
        if at
            .checked_add(header.length)
            .is_none_or(|end| end > self.himem)
        {
            return Err(Error::StackCorrupt.into());
        }
        Ok(Some(header))
    }

    /// The record on top, which must be of `kind`: its header, its words
    /// read into `words`, and it popped.
    pub(crate) fn pop_record(&mut self, kind: Kind, words: &mut [u32]) -> Step<Header> {
        let header = self
            .top_header()?
            .filter(|h| h.kind == kind)
            .ok_or(Error::StackCorrupt)?;
        let carried = (header.length / 4 - 1) as usize;
        if carried < words.len() {
            return Err(Error::StackCorrupt.into());
        }
        self.os.read_words(self.sp + 4, words)?;
        self.sp += header.length;
        Ok(header)
    }

    /// The top record dropped, whatever it is.
    pub(crate) fn drop_top(&mut self) -> Step<Header> {
        let header = self.top_header()?.ok_or(Error::StackCorrupt)?;
        self.sp += header.length;
        Ok(header)
    }

    /// The `n`th word after the header of the record at `at`.
    pub(crate) fn record_word(&mut self, at: u32, n: u32) -> Step<u32> {
        Ok(self.os.read_word(at + 4 + 4 * n)?)
    }

    /// Sets the `n`th word after the header of the record at `at`.
    pub(crate) fn set_record_word(&mut self, at: u32, n: u32, value: u32) -> Step {
        Ok(self.os.write_word(at + 4 + 4 * n, value)?)
    }

    // Values on the stack.

    /// Pushes `value`.
    pub(crate) fn push_value(&mut self, value: &Value) -> Step {
        match *value {
            Value::Int(n) => self.push(Kind::Int, 0, &[n.cast_unsigned()]),
            Value::Real(r) => {
                let words = self.arith.encode_words(r);
                self.push(Kind::Real, 0, &words)
            }
            Value::Str(ref text) => self.push_string(text),
        }
    }

    /// Pushes a string.
    pub(crate) fn push_string(&mut self, text: &Text) -> Step {
        let length = u32::from(text.len());
        let record = crate::stack::string_record(length);
        let at = self.room(record)?;
        self.os
            .write_word(at, Header::word(Kind::Str, text.len(), record))?;
        self.os.write_bytes(at + 4, text.as_bytes())?;
        self.sp = at;
        Ok(())
    }

    /// The value on top, popped.
    pub(crate) fn pop_value(&mut self) -> Step<Value> {
        let value = self.value_at(self.sp)?;
        self.drop_top()?;
        Ok(value)
    }

    /// The value the record at `at` holds.
    pub(crate) fn value_at(&mut self, at: u32) -> Step<Value> {
        let header = self.header_at(at)?.ok_or(Error::StackCorrupt)?;
        match header.kind {
            Kind::Int => Ok(Value::Int(self.record_word(at, 0)?.cast_signed())),
            Kind::Real => {
                let words = [self.record_word(at, 0)?, self.record_word(at, 1)?];
                Ok(Value::Real(self.arith.decode_words(words)))
            }
            Kind::Str => {
                let mut text = Text::EMPTY;
                let length = usize::from(header.aux);
                let os = &mut *self.os;
                let mut read = Ok(());
                text.extend(length, |room| read = os.read_bytes(at + 4, room))
                    .ok_or(Error::StackCorrupt)?;
                read?;
                Ok(Value::Str(text))
            }
            _ => Err(Error::StackCorrupt.into()),
        }
    }

    /// The number on top, popped; "Type mismatch: number needed" for a
    /// string.
    pub(crate) fn pop_number(&mut self) -> Step<Number> {
        self.pop_value()?
            .number()
            .ok_or_else(|| Error::NeedNumber.into())
    }

    /// The integer on top, popped, a real truncated.
    pub(crate) fn pop_int(&mut self) -> Step<i32> {
        let number = self.pop_number()?;
        Ok(self.arith.int(number)?)
    }

    // Numbers.

    /// `number` as an integer, a real truncated.
    pub(crate) fn int_of(&mut self, number: Number) -> Step<i32> {
        Ok(self.arith.int(number)?)
    }

    /// `value` as an integer: "Type mismatch: number needed" for a string.
    pub(crate) fn int_value(&mut self, value: &Value) -> Step<i32> {
        let number = value.number().ok_or(Error::NeedNumber)?;
        self.int_of(number)
    }

    /// `value` as a real.
    pub(crate) fn real_value(&mut self, value: &Value) -> Step<Real> {
        let number = value.number().ok_or(Error::NeedNumber)?;
        Ok(self.arith.real_of(number)?)
    }

    /// The current line's number, zero in an immediate line.
    pub(crate) fn line_number(&mut self) -> Step<u32> {
        if self.line == 0 {
            return Ok(0);
        }
        let high = self.byte_at(self.line.wrapping_add(1))?;
        let low = self.byte_at(self.line.wrapping_add(2))?;
        Ok(u32::from(u16::from_be_bytes([high, low])))
    }

    /// The escape flag checked: "Escape" if it is set, the condition
    /// acknowledged.
    pub(crate) fn check_escape(&mut self) -> Step {
        let word = self.os.read_word(ws::ESCWORD.at())?;
        if word & ws::ESCAPE == 0 {
            return Ok(());
        }
        self.os.write_word(ws::ESCWORD.at(), word & !ws::ESCAPE)?;
        let mut regs = tairix_riscos_kernel::os::Regs::default();
        regs.r[0] = tairix_riscos_kernel::input::ACKNOWLEDGE_ESCAPE;
        self.os.perform(
            tairix_riscos_kernel::swi::X | tairix_riscos_kernel::swi::os::BYTE,
            &mut regs,
        )?;
        Err(Error::Escape.into())
    }
}

/// The longest name a program's text holds: a line's.
pub(crate) const NAME_MOST: usize = STRING_MOST;

/// The most words of a continuation's payload.
pub(crate) const PAYLOAD_MOST: usize = MOST_WORDS;
