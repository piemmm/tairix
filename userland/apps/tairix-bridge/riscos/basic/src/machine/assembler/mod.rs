//! BASIC's ARM assembler: `[` to `]`, the machine's assembly mode.
//!
//! A statement is parsed from its start each time a value it waits on
//! arrives, so an operand may call a function that runs BASIC, even one that
//! assembles, and nothing of a parse is held on the host. The values read so
//! far wait on the stack under the statement's continuation, each with the
//! address its expression ended at.

mod core;
mod data;
mod fpa;
mod listing;
mod memory;
mod mnemonic;
mod parse;
mod simd;
mod statement;

use alloc::vec::Vec;

use super::next::{Next, Payload};
use super::statements::cont;
use super::{flag, len32, Leave, Lvalue, Machine, Mode, Step};
use crate::messages::Error;
use crate::stack::Kind;
use crate::value::Value;
use crate::workspace::{self as ws, reg};

use parse::{Context, Evaluated, Halt, Item, Parsed, Parser, Want};

/// `OPT`'s bits.
mod opt {
    /// List each statement.
    pub(super) const LIST: u32 = 1;
    /// Report the errors a first pass meets.
    pub(super) const ERRORS: u32 = 1 << 1;
    /// Lay code down at `O%`, assembled as at `P%`.
    pub(super) const OFFSET: u32 = 1 << 2;
    /// Refuse to lay code down past `L%`.
    pub(super) const LIMIT: u32 = 1 << 3;
}

/// The statement a continuation waits for a value for.
#[derive(Clone, Copy, Debug)]
struct Statement {
    /// Where the statement's text begins, which its listing shows from.
    start: u32,
    /// Where its parse begins: past any label.
    from: u32,
    /// The values it has read, on the stack.
    count: u32,
    /// It began directly after a colon or `[`, so its listing gives a label
    /// a field of its own.
    field: bool,
    /// `P%` has been aligned for its instruction.
    aligned: bool,
    /// How the value it waits on is read.
    want: Want,
}

impl Statement {
    /// The statement beginning at `start`, parsed from `from`.
    const fn new(start: u32, from: u32, field: bool) -> Self {
        Self {
            start,
            from,
            count: 0,
            field,
            aligned: false,
            want: Want::Expression,
        }
    }

    fn payload(self) -> Payload {
        let flags = u32::from(self.field) | u32::from(self.aligned) << 1 | self.want.word() << 2;
        Payload::of(&[self.start, self.from, self.count, flags])
    }

    fn of(payload: Payload) -> Option<Self> {
        let flags = payload.word(3);
        Some(Self {
            start: payload.word(0),
            from: payload.word(1),
            count: payload.word(2),
            field: flags & 1 != 0,
            aligned: flags & 2 != 0,
            want: Want::of(flags >> 2)?,
        })
    }
}

impl Machine<'_, '_> {
    /// `[`: assembly begun under `OPT` 3.
    pub(crate) fn assemble(&mut self) -> Step<Next> {
        self.os.write_word(reg::OPT.at(), 3)?;
        self.flags |= flag::ASSEMBLING | flag::LABEL_FIELD;
        self.mode = Mode::Assemble;
        Ok(Next::Moved)
    }

    /// The assembly step: the statement at the pointer assembled, or `]`
    /// ending assembly.
    pub(crate) fn assembler_step(&mut self) -> Step<Option<Leave>> {
        let field = self.flags & flag::LABEL_FIELD != 0;
        self.flags &= !flag::LABEL_FIELD;
        let start = self.ptr;
        let next = if field && self.peek()? == b'.' {
            self.ptr += 1;
            self.lvalue(cont::ASSEMBLY_LABEL, Payload::of(&[start, 1]))?
        } else {
            match self.skip_spaces()? {
                b']' => {
                    self.ptr += 1;
                    self.flags &= !flag::ASSEMBLING;
                    self.mode = Mode::Statement;
                    return Ok(None);
                }
                b'.' => {
                    self.ptr += 1;
                    self.lvalue(cont::ASSEMBLY_LABEL, Payload::of(&[start, 0]))?
                }
                _ => {
                    let from = self.ptr;
                    self.assembly(Statement::new(start, from, field), None)?
                }
            }
        };
        self.follow(next)
    }

    /// A label's l-value read: `P%` given to it, then the rest of its
    /// statement after spaces.
    pub(crate) fn assembly_label(&mut self, payload: Payload, lvalue: Lvalue) -> Step<Next> {
        let p = self.resident_integer(b'P')?;
        self.write(lvalue, &Value::Int(p.cast_signed()))?;
        self.skip_spaces()?;
        let from = self.ptr;
        self.assembly(
            Statement::new(payload.word(0), from, payload.word(1) != 0),
            None,
        )
    }

    /// The value a statement waited on delivered, a SWI's name looked up.
    pub(crate) fn assembly_value(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let statement = Statement::of(payload).ok_or(Error::StackCorrupt)?;
        let end = self.ptr;
        let value = match (statement.want, value) {
            (Want::Swi, Value::Str(name)) => match self.swi_named(name)? {
                Ok(number) => Value::Int(number.cast_signed()),
                Err(leave) => return Ok(Next::Leave(leave)),
            },
            (_, value) => *value,
        };
        self.assembly(statement, Some(&Evaluated { value, end }))
    }

    /// `statement` parsed with what it has read and `latest`: the next value
    /// it waits on evaluated, or what it lays down laid down and listed.
    fn assembly(&mut self, mut statement: Statement, latest: Option<&Evaluated>) -> Step<Next> {
        let tape = self.tape(statement.count, latest)?;
        let text = self.statement_text(statement.from)?;
        let opt = self.opt()?;
        let context = Context {
            address: self.resident_integer(b'P')?.wrapping_add(3) & !3,
            suppress: opt & opt::ERRORS == 0,
        };
        let outcome = {
            let mut parser = Parser::new(&text, statement.from, &tape, &mut self.arith, context);
            statement::statement(&mut parser)
        };
        if outcome.instruction && !statement.aligned {
            self.align()?;
            statement.aligned = true;
        }
        match outcome.parsed {
            Err(Halt::Fail(trap)) => {
                self.drop_tape(statement.count)?;
                Err(trap)
            }
            Err(Halt::Need { at, want }) => {
                if let Some(latest) = latest {
                    self.store_value(latest)?;
                    statement.count += 1;
                }
                statement.want = want;
                self.ptr = at;
                let payload = statement.payload();
                match want {
                    Want::Factor => self.evaluate_factor(cont::ASSEMBLY, payload),
                    Want::Expression | Want::Swi => self.evaluate(cont::ASSEMBLY, payload),
                }
            }
            Ok(parsed) => {
                self.drop_tape(statement.count)?;
                self.lay_down(statement, &parsed)
            }
        }
    }

    /// What `parsed` lays down laid down at `P%`, or `O%`, and listed; then
    /// the next statement.
    fn lay_down(&mut self, statement: Statement, parsed: &Parsed) -> Step<Next> {
        let mut opt = self.opt()?;
        match parsed.item {
            Item::Opt(value) => {
                opt = value.cast_unsigned();
                self.os.write_word(reg::OPT.at(), opt)?;
            }
            Item::Align => self.align()?,
            _ => {}
        }
        let address = self.resident_integer(b'P')?;
        let word;
        let bytes = match &parsed.item {
            Item::Instruction(instruction) => {
                word = instruction.to_le_bytes();
                &word[..]
            }
            item => item.bytes(),
        };
        if !bytes.is_empty() {
            self.store(bytes, opt)?;
        }
        if opt & opt::LIST != 0 {
            self.listing_line(statement, address, bytes, parsed.end)?;
        }
        self.statement_end(parsed.end)
    }

    /// `bytes` laid down at `P%`, or under offset assembly `O%`, and the
    /// pointers moved past them: "Assembler limit reached" where they would
    /// pass `L%` under `OPT`'s limit.
    fn store(&mut self, bytes: &[u8], opt: u32) -> Step {
        let offset = opt & opt::OFFSET != 0;
        let at = self.resident_integer(if offset { b'O' } else { b'P' })?;
        let size = len32(bytes.len());
        if opt & opt::LIMIT != 0 {
            let limit = self.resident_integer(b'L')?;
            if at.checked_add(size).is_none_or(|end| end > limit) {
                return Err(Error::AssemblerLimit.into());
            }
        }
        self.os.write_bytes(at, bytes)?;
        let p = self.resident_integer(b'P')?;
        self.set_resident_integer(b'P', p.wrapping_add(size))?;
        if offset {
            let o = self.resident_integer(b'O')?;
            self.set_resident_integer(b'O', o.wrapping_add(size))?;
        }
        Ok(())
    }

    /// `P%`, and under offset assembly `O%`, each aligned to a word, the
    /// bytes skipped where code is laid down zeroed.
    fn align(&mut self) -> Step {
        let opt = self.opt()?;
        let p = self.resident_integer(b'P')?;
        let p_pad = p.wrapping_neg() & 3;
        if opt & opt::OFFSET != 0 {
            let o = self.resident_integer(b'O')?;
            let o_pad = o.wrapping_neg() & 3;
            self.os.write_bytes(o, &[0; 3][..o_pad as usize])?;
            self.set_resident_integer(b'O', o.wrapping_add(o_pad))?;
        } else {
            self.os.write_bytes(p, &[0; 3][..p_pad as usize])?;
        }
        self.set_resident_integer(b'P', p.wrapping_add(p_pad))
    }

    /// The statement's terminator at `end` passed: a colon, the next
    /// statement's label field open; a CR, the next line, or the end of
    /// assembly where the program or the immediate line ends.
    fn statement_end(&mut self, end: u32) -> Step<Next> {
        self.ptr = end;
        self.mode = Mode::Assemble;
        if self.peek()? == b':' {
            self.ptr += 1;
            self.flags |= flag::LABEL_FIELD;
            return Ok(Next::Moved);
        }
        let leave = self.line_end()?;
        if self.mode != Mode::Assemble {
            self.flags &= !(flag::ASSEMBLING | flag::LABEL_FIELD);
        }
        Ok(leave.map_or(Next::Moved, Next::Leave))
    }

    /// Whether an unknown variable in an expression is `P%` instead: while
    /// assembling under `OPT` with bit 1 clear, as BASIC passes over a label
    /// a first pass has not met yet.
    pub(crate) fn unknown_is_p(&mut self) -> Step<bool> {
        if self.flags & flag::ASSEMBLING == 0 {
            return Ok(false);
        }
        Ok(self.opt()? & opt::ERRORS == 0)
    }

    /// `P%`'s value.
    pub(crate) fn p_percent(&mut self) -> Step<u32> {
        self.resident_integer(b'P')
    }

    fn opt(&mut self) -> Step<u32> {
        Ok(self.os.read_word(reg::OPT.at())?)
    }

    /// Resident integer `letter%`.
    fn resident_integer(&mut self, letter: u8) -> Step<u32> {
        Ok(self.os.read_word(resident_at(letter))?)
    }

    fn set_resident_integer(&mut self, letter: u8, value: u32) -> Step {
        Ok(self.os.write_word(resident_at(letter), value)?)
    }

    /// The values a statement has read: `count` on the stack, oldest first,
    /// then `latest`.
    fn tape(&mut self, count: u32, latest: Option<&Evaluated>) -> Step<Vec<Evaluated>> {
        let mut tape = Vec::new();
        tape.try_reserve_exact(count as usize + 1)
            .map_err(|_| Error::NoRoom)?;
        let mut at = self.sp;
        for _ in 0..count {
            let end = match self.value_at(at)? {
                Value::Int(end) => end.cast_unsigned(),
                _ => return Err(Error::StackCorrupt.into()),
            };
            at += self.header_at(at)?.ok_or(Error::StackCorrupt)?.length;
            let value = self.value_at(at)?;
            at += self.header_at(at)?.ok_or(Error::StackCorrupt)?.length;
            tape.push(Evaluated { value, end });
        }
        tape.reverse();
        tape.extend(latest.copied());
        Ok(tape)
    }

    /// `evaluated` kept on the stack: its value, then where it ended.
    fn store_value(&mut self, evaluated: &Evaluated) -> Step {
        self.push_value(&evaluated.value)?;
        self.push(Kind::Int, 0, &[evaluated.end])
    }

    /// A statement's `count` values dropped from the stack.
    fn drop_tape(&mut self, count: u32) -> Step {
        for _ in 0..2 * count {
            self.drop_top()?;
        }
        Ok(())
    }

    /// The text from `from` to its line's CR: the line's own bytes, or the
    /// immediate line's room.
    fn statement_text(&mut self, from: u32) -> Step<Vec<u8>> {
        let end = if self.line == 0 {
            ws::IMMEDIATE.at() + ws::LINE_ROOM
        } else {
            let length = self.byte_at(self.line.wrapping_add(3))?;
            self.line.wrapping_add(u32::from(length))
        };
        let room = end.saturating_sub(from).min(ws::LINE_ROOM);
        let mut text = Vec::new();
        text.try_reserve_exact(room as usize + 1)
            .map_err(|_| Error::NoRoom)?;
        self.os.read_onto(&mut text, from, room as usize)?;
        match text.iter().position(|&b| b == b'\r') {
            Some(cr) => text.truncate(cr + 1),
            None => text.push(b'\r'),
        }
        Ok(text)
    }
}

/// Where resident integer `letter%` lives.
fn resident_at(letter: u8) -> u32 {
    ws::RESIDENT.at() + 4 * u32::from(letter - b'A')
}
