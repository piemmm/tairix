//! Statements: the dispatch at a statement's start, the continuation each
//! statement waiting on a value resumes at, l-values, and assignment.

mod control;
mod data;
mod io;
mod system;

pub(crate) use system::Then;

use super::eval::FACTOR_ONLY;
use super::functions::Bracketed;
use super::lvalue::{Lvalue, Place};
use super::next::{Cont, Delivered, Next, Payload};
use super::routines::Routine;
use super::{Leave, Machine, Mode, Step};
use crate::messages::Error;
use crate::stack::Kind;
use crate::token;
use crate::value::Value;

/// The continuations: each statement waiting on a value, by its number.
pub(crate) mod cont {
    /// An assignment's l-value read: its operator and value next.
    pub(crate) const ASSIGNED: u8 = 1;
    /// An assignment's value.
    pub(crate) const ASSIGN: u8 = 2;
    /// A `PRINT` item's value.
    pub(crate) const PRINT: u8 = 3;
    /// `IF`'s condition.
    pub(crate) const IF: u8 = 4;
    /// `FOR`'s control variable.
    pub(crate) const FOR_VARIABLE: u8 = 5;
    /// `FOR`'s start.
    pub(crate) const FOR_START: u8 = 6;
    /// `FOR`'s limit.
    pub(crate) const FOR_LIMIT: u8 = 7;
    /// `FOR`'s step.
    pub(crate) const FOR_STEP: u8 = 8;
    /// `UNTIL`'s condition.
    pub(crate) const UNTIL: u8 = 9;
    /// `WHILE`'s condition.
    pub(crate) const WHILE: u8 = 10;
    /// A routine's argument.
    pub(crate) const ARGUMENT: u8 = 11;
    /// `RND(n)`'s argument.
    pub(crate) const RND: u8 = 12;
    /// A function's result after `=`.
    pub(crate) const FN_RESULT: u8 = 13;
    /// `GOTO`'s or `GOSUB`'s line.
    pub(crate) const JUMP: u8 = 14;
    /// `ON`'s choice.
    pub(crate) const ON: u8 = 15;
    /// An indirection l-value's address.
    pub(crate) const INDIRECT: u8 = 16;
    /// A binary indirection l-value's offset.
    pub(crate) const BASE: u8 = 17;
    /// `EVAL`'s expression.
    pub(crate) const EVAL: u8 = 18;
    /// A statement's arguments, gathered on the stack.
    pub(crate) const GATHER: u8 = 19;
    /// `NEXT`'s control variable.
    pub(crate) const NEXT: u8 = 20;
    /// `READ`'s l-value.
    pub(crate) const READ: u8 = 21;
    /// `CASE`'s value.
    pub(crate) const CASE: u8 = 22;
    /// A `WHEN`'s value.
    pub(crate) const WHEN: u8 = 23;
    /// A `LOCAL`'s l-value.
    pub(crate) const LOCAL: u8 = 24;
    /// A `DIM`'s size.
    pub(crate) const DIM: u8 = 25;
    /// `RESTORE`'s line.
    pub(crate) const RESTORE: u8 = 26;
    /// `ERROR`'s number, then its message.
    pub(crate) const ERROR: u8 = 27;
    /// `INPUT`'s l-value.
    pub(crate) const INPUT: u8 = 28;
    /// `SYS`'s value or l-value.
    pub(crate) const SYS: u8 = 29;
    /// `CALL`'s address or parameter.
    pub(crate) const CALL: u8 = 30;
    /// `TRACE`'s argument.
    pub(crate) const TRACE: u8 = 31;
    /// `SWAP`'s l-values.
    pub(crate) const SWAP: u8 = 32;
    /// `QUIT`'s code.
    pub(crate) const QUIT: u8 = 33;
    /// A pseudo-variable's new value.
    pub(crate) const PSEUDO: u8 = 34;
    /// A channel's number, after `#`.
    pub(crate) const CHANNEL: u8 = 35;
    /// `PRINT#`'s or `BPUT#`'s value.
    pub(crate) const TO_FILE: u8 = 36;
    /// `INPUT#`'s or `READ#`'s l-value.
    pub(crate) const FROM_FILE: u8 = 37;
    /// A string slice's l-value, then its arguments.
    pub(crate) const SLICE: u8 = 38;
    /// `LIBRARY`'s or `INSTALL`'s file.
    pub(crate) const LIBRARY: u8 = 39;
    /// `CHAIN`'s file.
    pub(crate) const CHAIN: u8 = 40;
    /// `OSCLI`'s command.
    pub(crate) const OSCLI: u8 = 41;
    /// `MOUSE`'s l-values.
    pub(crate) const MOUSE: u8 = 42;
    /// A whole-array operation's operand.
    pub(crate) const ARRAY: u8 = 43;
    /// A statement's or command's one number, which [`super::setting`]
    /// names the use of.
    pub(crate) const SETTING: u8 = 44;
    /// A routine of the table's result, for the guest code that called it.
    pub(crate) const ROUTINE: u8 = 45;
    /// A text's expression evaluated whole.
    pub(crate) const TEXT_END: u8 = 46;
    /// Where the pointer goes back to after a text's expression: never
    /// resumed, popped by [`TEXT_END`]'s step.
    pub(crate) const TEXT_RESTORE: u8 = 47;
    /// A numeric `READ`'s item evaluated.
    pub(crate) const READ_VALUE: u8 = 48;
    /// `INPUT`'s `TAB` or `SPC` argument.
    pub(crate) const INPUT_PLACE: u8 = 49;
    /// An assembler statement's operand.
    pub(crate) const ASSEMBLY: u8 = 50;
    /// An assembler label's l-value.
    pub(crate) const ASSEMBLY_LABEL: u8 = 51;
}

/// What a [`cont::SETTING`]'s number is for, its payload's first word.
pub(crate) mod setting {
    /// `VOICE`'s channel.
    pub(crate) const VOICE: u32 = 1;
    /// `VOICE`'s name, after its channel.
    pub(crate) const VOICE_NAME: u32 = 2;
    /// `CRUNCH`'s bits.
    pub(crate) const CRUNCH: u32 = 3;
    /// `LISTO`'s options.
    pub(crate) const LISTO: u32 = 4;
}

/// The assignment operators.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub(crate) enum Assign {
    Set = 0,
    Add = 1,
    Sub = 2,
}

impl Machine<'_, '_> {
    /// The statement step: what begins at the pointer interpreted.
    pub(crate) fn statement(&mut self) -> Step<Option<Leave>> {
        let b = self.skip_spaces()?;
        let next = match b {
            b':' => {
                self.ptr += 1;
                return Ok(None);
            }
            b'\r' => return self.line_end(),
            token::ELSE | token::ELSE_BLOCK => {
                self.ptr += 1;
                return self.else_reached().map(|()| None);
            }
            b'*' => {
                self.ptr += 1;
                self.star_line()?
            }
            b'=' => {
                self.ptr += 1;
                self.evaluate(cont::FN_RESULT, Payload::default())?
            }
            b'[' => {
                self.ptr += 1;
                self.assemble()?
            }
            b']' => return Err(Error::Syntax.into()),
            b'?' | b'!' | b'$' | b'|' => self.lvalue(cont::ASSIGNED, Payload::default())?,
            b if super::vars::starts_name(b) || b == b'@' => {
                self.lvalue(cont::ASSIGNED, Payload::default())?
            }
            _ if b >= 0x7F => {
                self.ptr += 1;
                self.keyword_statement(b)?
            }
            _ => return Err(Error::Mistake.into()),
        };
        self.follow(next)
    }

    /// `next` followed: results delivered to waiting statements in turn, the
    /// statement's end met, until the machine is elsewhere.
    pub(crate) fn follow(&mut self, mut next: Next) -> Step<Option<Leave>> {
        loop {
            next = match next {
                Next::Done => {
                    self.statement_done()?;
                    return Ok(None);
                }
                Next::Moved => return Ok(None),
                Next::Leave(leave) => return Ok(Some(leave)),
                Next::Resume(cont, payload, delivered) => {
                    self.mode = Mode::Statement;
                    self.resume(cont, payload, &delivered)?
                }
            };
        }
    }

    /// A statement's end: a colon, the line's end or `ELSE`, else a syntax
    /// error.
    fn statement_done(&mut self) -> Step {
        self.mode = Mode::Statement;
        if self.at_statement_end()? {
            return Ok(());
        }
        Err(Error::Syntax.into())
    }

    /// The statement a keyword `b` begins.
    fn keyword_statement(&mut self, b: u8) -> Step<Next> {
        match b {
            token::LET => self.lvalue(cont::ASSIGNED, Payload::of(&[ASSIGNED_BY_LET])),
            token::REM | token::DATA | token::DEF => {
                self.skip_line()?;
                Ok(Next::Done)
            }
            token::PRINT => self.print_statement(),
            token::IF => self.evaluate(cont::IF, Payload::default()),
            token::FOR => self.lvalue(cont::FOR_VARIABLE, Payload::default()),
            token::NEXT => self.next_statement(),
            token::REPEAT => self.repeat_statement(),
            token::UNTIL => self.evaluate(cont::UNTIL, Payload::default()),
            token::GOTO | token::GOSUB => self.jump_statement(b == token::GOSUB),
            token::RETURN => self.return_statement(),
            token::END => self.end_statement(),
            token::STOP => Err(Error::Stopped.into()),
            token::PROC => self.proc_statement(),
            token::ENDPROC => {
                self.return_from(Routine::Proc)?;
                Ok(Next::Done)
            }
            token::LOCAL => self.local_statement(),
            token::ON => self.on_statement(),
            token::DIM => self.dim_statement(),
            token::READ => self.read_statement(),
            token::RESTORE => self.restore_statement(),
            token::CLEAR => {
                self.clear()?;
                Ok(Next::Done)
            }
            token::RUN => self.run_statement(),
            token::ERROR => self.error_statement(),
            token::REPORT => self.report_statement(),
            token::INPUT => self.input_statement(),
            token::OSCLI => self.evaluate(cont::OSCLI, Payload::default()),
            token::CALL => self.call_statement(),
            token::TRACE => self.trace_statement(),
            token::VDU => self.vdu_statement(),
            token::CLS => self.clear_screen(12),
            token::CLG => self.clear_screen(16),
            token::MODE
            | token::COLOUR
            | token::GCOL
            | token::MOVE
            | token::DRAW
            | token::PLOT
            | token::SOUND
            | token::ENVELOPE
            | token::WIDTH
            | token::LINE => self.gathered_statement(b, 0),
            token::OFF => self
                .vdu_codes(&[23, 1, 0, 0, 0, 0, 0, 0, 0, 0])
                .map(|()| Next::Done),
            token::BPUT => self.bput_statement(),
            token::CLOSE => self.close_statement(),
            token::CHAIN => self.evaluate(cont::CHAIN, Payload::default()),
            token::PTR_LHS
            | token::PAGE_LHS
            | token::TIME_LHS
            | token::LOMEM_LHS
            | token::HIMEM_LHS => self.pseudo_statement(b),
            token::ENDWHILE => self.endwhile_statement(),
            token::ENDIF | token::ENDCASE => Ok(Next::Done),
            token::WHEN | token::OTHERWISE => self.when_reached(),
            token::MID | token::LEFT | token::RIGHT => self.slice_statement(b),
            token::STATEMENT => {
                let second = self.next()?;
                self.prefixed_statement(second)
            }
            token::COMMAND => {
                let second = self.next()?;
                self.command(second)
            }
            _ => Err(Error::Mistake.into()),
        }
    }

    /// A statement behind the statements' prefix.
    fn prefixed_statement(&mut self, b: u8) -> Step<Next> {
        use token::statement as s;
        match b {
            s::CASE => self.evaluate(cont::CASE, Payload::default()),
            s::WHILE => self.while_statement(),
            s::SWAP => self.lvalue(cont::SWAP, Payload::default()),
            s::QUIT => self.quit_statement(),
            s::SYS => self.sys_statement(),
            s::LIBRARY | s::INSTALL => {
                self.evaluate(cont::LIBRARY, Payload::of(&[u32::from(b == s::INSTALL)]))
            }
            s::MOUSE => self.mouse_statement(),
            s::CIRCLE
            | s::ELLIPSE
            | s::RECTANGLE
            | s::FILL
            | s::ORIGIN
            | s::POINT
            | s::TINT
            | s::WAIT
            | s::BEATS
            | s::TEMPO
            | s::VOICES
            | s::VOICE
            | s::STEREO => self.gathered_statement(token::STATEMENT, b),
            s::OVERLAY => self.overlay_statement(),
            _ => Err(Error::Mistake.into()),
        }
    }

    /// `cont` resumed with what it waited on and what it carries.
    pub(crate) fn resume(
        &mut self,
        cont: Cont,
        payload: Payload,
        delivered: &Delivered,
    ) -> Step<Next> {
        match cont {
            cont::ASSIGNED => self.assigned(payload, delivered.lvalue()?),
            cont::ASSIGN => self.assign(payload, delivered.value()?),
            cont::PRINT => self.print_item(payload, delivered.value()?),
            cont::IF => self.if_condition(delivered.value()?),
            cont::FOR_VARIABLE => self.for_variable(delivered.lvalue()?),
            cont::FOR_START => self.for_start(payload, delivered.value()?),
            cont::FOR_LIMIT => self.for_limit(payload, delivered.value()?),
            cont::FOR_STEP => self.for_step(payload, delivered.value()?),
            cont::UNTIL => self.until_condition(delivered.value()?),
            cont::WHILE => self.while_condition(payload, delivered.value()?),
            cont::ARGUMENT => self.argument(payload, delivered),
            cont::RND => self.rnd_argument(delivered.value()?),
            cont::FN_RESULT => self.fn_result(delivered.value()?),
            cont::JUMP => self.jump_to(payload, delivered.value()?),
            cont::ON => self.on_choice(payload, delivered.value()?),
            cont::INDIRECT => self.indirect_lvalue(payload, delivered.value()?),
            cont::BASE => self.base_lvalue(payload, delivered.value()?),
            cont::EVAL => self.eval_done(delivered.value()?),
            cont::TEXT_END => self.text_done(delivered.value()?),
            cont::READ_VALUE => self.read_value(payload, delivered.value()?),
            cont::GATHER => self.gathered(payload, delivered.value()?),
            cont::NEXT => self.next_variable(delivered.lvalue()?),
            cont::READ => self.read_into(payload, delivered.lvalue()?),
            cont::CASE => self.case_value(delivered.value()?),
            cont::WHEN => self.when_value(payload, delivered.value()?),
            cont::LOCAL => self.local_item(delivered.lvalue()?),
            cont::DIM => self.dim_size(payload, delivered.value()?),
            cont::RESTORE => self.restore_to(payload, delivered.value()?),
            cont::ERROR => self.error_argument(payload, delivered.value()?),
            cont::INPUT => self.input_into(payload, delivered.lvalue()?),
            cont::INPUT_PLACE => self.input_place(payload, delivered.value()?),
            cont::SYS => self.sys_argument(payload, delivered),
            cont::CALL => self.call_argument(payload, delivered),
            cont::TRACE => self.trace_argument(payload, delivered.value()?),
            cont::SWAP => self.swap_item(payload, delivered.lvalue()?),
            cont::QUIT => self.quit_code(delivered.value()?),
            cont::PSEUDO => self.pseudo_value(payload, delivered.value()?),
            cont::CHANNEL => self.channel(payload, delivered.value()?),
            cont::TO_FILE => self.file_value(payload, delivered.value()?),
            cont::FROM_FILE => self.file_variable(payload, delivered.lvalue()?),
            cont::SLICE => self.slice_argument(payload, delivered),
            cont::LIBRARY => self.library(payload, delivered.value()?),
            cont::CHAIN => self.chain(delivered.value()?),
            cont::OSCLI => self.oscli(delivered.value()?),
            cont::MOUSE => self.mouse_item(payload, delivered.lvalue()?),
            cont::ARRAY => self.array_operand(payload, delivered),
            cont::SETTING => self.setting(payload, delivered.value()?),
            cont::ROUTINE => self.routine_done(delivered),
            cont::ASSEMBLY => self.assembly_value(payload, delivered.value()?),
            cont::ASSEMBLY_LABEL => self.assembly_label(payload, delivered.lvalue()?),
            _ => Err(Error::StackCorrupt.into()),
        }
    }

    /// The rest of the line skipped: the pointer at its CR.
    pub(crate) fn skip_line(&mut self) -> Step {
        for _ in 0..super::NAME_MOST * 2 {
            if self.peek()? == b'\r' {
                return Ok(());
            }
            self.ptr += 1;
        }
        Err(Error::LineTooLong.into())
    }

    // L-values.

    /// The l-value at the pointer read, then `cont` resumed with it: at once
    /// for a variable, after their evaluation for a subscript or an
    /// indirection's address.
    pub(crate) fn lvalue(&mut self, cont: Cont, payload: Payload) -> Step<Next> {
        let b = self.skip_spaces()?;
        if let Some(op) = indirection(b) {
            self.ptr += 1;
            self.push(Kind::Continuation, cont, payload.words())?;
            self.push(Kind::Continuation, cont::INDIRECT, &[u32::from(op)])?;
            self.push(Kind::Expression, FACTOR_ONLY, &[])?;
            self.mode = Mode::Operand;
            return Ok(Next::Moved);
        }
        let name = self.read_name()?.ok_or(Error::Mistake)?;
        let lvalue = self.variable(&name, true)?.ok_or(Error::NoSuchVariable)?;
        if name.is_array() {
            if self.skip_spaces()? == b')' {
                self.ptr += 1;
                return Ok(Next::Resume(cont, payload, Delivered::Lvalue(lvalue)));
            }
            let pointer = self.os.read_word(lvalue.address)?;
            let Place::Array(element) = lvalue.place else {
                return Err(Error::ArrayHere.into());
            };
            self.push(Kind::Continuation, cont, payload.words())?;
            self.push(
                Kind::Arguments,
                Bracketed::Subscript as u8,
                &[0, pointer, element as u32, 1],
            )?;
            self.mode = Mode::Operand;
            return Ok(Next::Moved);
        }
        self.after_lvalue(cont, payload, lvalue)
    }

    /// An l-value read: a binary `?` or `!` after it makes its value an
    /// indirection's base.
    pub(crate) fn after_lvalue(
        &mut self,
        cont: Cont,
        payload: Payload,
        lvalue: Lvalue,
    ) -> Step<Next> {
        if !matches!(self.peek()?, b'?' | b'!') {
            return Ok(Next::Resume(cont, payload, Delivered::Lvalue(lvalue)));
        }
        self.push(Kind::Continuation, cont, payload.words())?;
        self.deliver_lvalue(lvalue)
    }

    /// An l-value read for the continuation waiting on top: a binary `?` or
    /// `!` after it makes its value an indirection's base first.
    pub(crate) fn deliver_lvalue(&mut self, lvalue: Lvalue) -> Step<Next> {
        let op @ (b'?' | b'!') = self.peek()? else {
            return self.deliver(Delivered::Lvalue(lvalue));
        };
        self.ptr += 1;
        let value = self.read(lvalue)?;
        let base = self.int_value(&value)?;
        self.push(
            Kind::Continuation,
            cont::BASE,
            &[base.cast_unsigned(), u32::from(op)],
        )?;
        self.push(Kind::Expression, FACTOR_ONLY, &[])?;
        self.mode = Mode::Operand;
        Ok(Next::Moved)
    }

    /// The continuation on top resumed with `delivered`.
    #[expect(
        clippy::large_types_passed_by_value,
        reason = "it is moved into the step's result"
    )]
    pub(crate) fn deliver(&mut self, delivered: Delivered) -> Step<Next> {
        let header = self.top_header()?.ok_or(Error::StackCorrupt)?;
        let carried = ((header.length / 4) as usize - 1).min(super::PAYLOAD_MOST);
        let mut words = [0; super::PAYLOAD_MOST];
        let cont = self.pop_record(Kind::Continuation, &mut words[..carried])?;
        Ok(Next::Resume(
            cont.aux,
            Payload::of(&words[..carried]),
            delivered,
        ))
    }

    /// An indirection's address read: its l-value delivered.
    fn indirect_lvalue(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let address = self.int_value(value)?.cast_unsigned();
        let place = match payload.byte(0) {
            b'?' => Place::Byte,
            b'!' => Place::Int,
            b'$' => Place::Dollar,
            _ => Place::Real,
        };
        if place == Place::Dollar && address < super::functions::DOLLAR_LEAST {
            return Err(Error::LowDollar.into());
        }
        self.deliver(Delivered::Lvalue(Lvalue { address, place }))
    }

    /// A binary indirection's offset read: its l-value delivered.
    fn base_lvalue(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let offset = self.int_value(value)?;
        let address = payload.word(0).wrapping_add_signed(offset);
        let place = if payload.word(1) == u32::from(b'?') {
            Place::Byte
        } else {
            Place::Int
        };
        self.deliver(Delivered::Lvalue(Lvalue { address, place }))
    }

    // Assignment.

    /// An assignment's l-value read: `=`, `+=` or `-=`, then its value. As
    /// recorded, an l-value with none of them is a mistake unless `LET`
    /// began the statement, which misses its `=`.
    fn assigned(&mut self, payload: Payload, lvalue: Lvalue) -> Step<Next> {
        let op = match self.skip_spaces()? {
            b'=' => {
                self.ptr += 1;
                Assign::Set
            }
            b'+' if self.byte_at(self.ptr + 1)? == b'=' => {
                self.ptr += 2;
                Assign::Add
            }
            b'-' if self.byte_at(self.ptr + 1)? == b'=' => {
                self.ptr += 2;
                Assign::Sub
            }
            _ if payload.word(0) == ASSIGNED_BY_LET => return Err(Error::MissingEquals.into()),
            _ => return Err(Error::Mistake.into()),
        };
        if let Place::Array(_) = lvalue.place {
            return self.array_assignment(lvalue, op);
        }
        let place = lvalue.place.call_type(self.dialect());
        self.evaluate(
            cont::ASSIGN,
            Payload::of(&[lvalue.address, place, op as u32]),
        )
    }

    /// An assignment's value kept.
    fn assign(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let place =
            Place::of_call_type(payload.word(1), self.dialect()).ok_or(Error::StackCorrupt)?;
        let lvalue = Lvalue {
            address: payload.word(0),
            place,
        };
        let value = match (payload.word(2), value) {
            (1 | 2, _) => {
                let current = self.read(lvalue)?;
                let op = if payload.word(2) == 1 {
                    super::eval::Op::Add
                } else {
                    super::eval::Op::Sub
                };
                self.binary(op, &current, value)?
            }
            (_, Value::Str(text)) if lvalue.address == crate::workspace::AT_PERCENT.at() => {
                let at = self.os.read_word(lvalue.address)?;
                Value::Int(crate::text::at_percent_of(at, text.as_bytes()).cast_signed())
            }
            _ => *value,
        };
        self.write(lvalue, &value)?;
        Ok(Next::Done)
    }
}

/// `ASSIGNED`'s payload word for an assignment `LET` began.
const ASSIGNED_BY_LET: u32 = 1;

/// The l-value an indirection operator begins.
const fn indirection(b: u8) -> Option<u8> {
    match b {
        b'?' | b'!' | b'$' | b'|' => Some(b),
        _ => None,
    }
}
