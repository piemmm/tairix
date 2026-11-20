//! The control statements: lines and jumps, `IF` on a line and as a block,
//! the loops, `CASE`, `ON` and the program's end.

use tairix_openbasic::lineref;

use super::super::lvalue::{Lvalue, Place};
use super::super::next::{Next, Payload};
use super::super::routines::Routine;
use super::super::{flag, Leave, Machine, Mode, Step};
use super::cont;
use crate::messages::Error;
use crate::stack::Kind;
use crate::token;
use crate::value::{Number, Value};

/// A program's end in a line number's high byte.
const END: u8 = tairix_openbasic::program::END;

/// What a forward search along line starts for the end of a structure looks
/// for: these must be the first thing on their line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Seek {
    /// A block `IF`'s `ELSE` or `ENDIF`.
    ElseOrEndif,
    /// A block `IF`'s `ENDIF`.
    Endif,
    /// A `CASE`'s next `WHEN`, `OTHERWISE` or `ENDCASE`.
    When,
    /// A `CASE`'s `ENDCASE`.
    Endcase,
}

impl Machine<'_, '_> {
    /// The line's end met: the next line begun, or the program's or the
    /// immediate line's end reached.
    pub(crate) fn line_end(&mut self) -> Step<Option<Leave>> {
        if self.line == 0 {
            self.mode = Mode::Prompt;
            return Ok(None);
        }
        let next = self.ptr;
        if self.byte_at(next)? != b'\r' {
            return Err(Error::BadProgram.into());
        }
        if self.byte_at(next + 1)? == END {
            return Ok(self.program_end());
        }
        let length = self.byte_at(next + 3)?;
        if usize::from(length) < tairix_openbasic::program::HEADER {
            return Err(Error::BadProgram.into());
        }
        self.line = next;
        self.ptr = next + 4;
        self.check_escape()?;
        self.trace_line()?;
        Ok(None)
    }

    /// The program's end reached, by its last line or `END`.
    pub(crate) fn program_end(&mut self) -> Option<Leave> {
        self.flags &= !flag::RUNNING;
        if self.flags & flag::QUIT_AT_END != 0 {
            self.mode = Mode::Quit;
            return Some(Leave::Quit);
        }
        self.mode = Mode::Prompt;
        None
    }

    /// `END`.
    pub(crate) fn end_statement(&mut self) -> Step<Next> {
        if self.skip_spaces()? == b'=' {
            self.ptr += 1;
            return self.evaluate(cont::PSEUDO, Payload::of(&[u32::from(token::END)]));
        }
        Ok(self.program_end().map_or(Next::Moved, Next::Leave))
    }

    /// The line numbered `number`'s address, none where there is none.
    pub(crate) fn find_line(&mut self, number: u32) -> Step<Option<u32>> {
        let mut line = self.page;
        loop {
            if self.byte_at(line)? != b'\r' {
                return Err(Error::BadProgram.into());
            }
            let high = self.byte_at(line + 1)?;
            if high == END {
                return Ok(None);
            }
            let low = self.byte_at(line + 2)?;
            let here = u32::from(u16::from_be_bytes([high, low]));
            if here == number {
                return Ok(Some(line));
            }
            if here > number {
                return Ok(None);
            }
            let length = u32::from(self.byte_at(line + 3)?);
            if length < 4 {
                return Err(Error::BadProgram.into());
            }
            line += length;
            self.os.yield_point()?;
        }
    }

    /// Execution moved to the start of the line at `line`.
    fn go_to_line(&mut self, line: u32) {
        self.line = line;
        self.ptr = line + 4;
        self.flags |= flag::RUNNING;
        self.mode = Mode::Statement;
    }

    /// A line number at the pointer as a program holds one, taken; none
    /// where there is none.
    pub(crate) fn line_reference(&mut self) -> Step<Option<u32>> {
        if self.skip_spaces()? != token::LINE_NUMBER {
            return Ok(None);
        }
        let bytes = [
            self.byte_at(self.ptr + 1)?,
            self.byte_at(self.ptr + 2)?,
            self.byte_at(self.ptr + 3)?,
        ];
        self.ptr += 1 + super::super::len32(lineref::ENCODED);
        Ok(Some(u32::from(lineref::decode(bytes))))
    }

    // GOTO, GOSUB and RETURN.

    /// `GOTO` and `GOSUB`.
    pub(crate) fn jump_statement(&mut self, gosub: bool) -> Step<Next> {
        if let Some(number) = self.line_reference()? {
            return self.jump(number, gosub);
        }
        self.evaluate(cont::JUMP, Payload::of(&[u32::from(gosub)]))
    }

    /// The jump's line computed.
    pub(crate) fn jump_to(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let number = self.int_value(value)?;
        let number = u32::try_from(number).map_err(|_| Error::NoSuchLine)?;
        self.jump(number, payload.word(0) != 0)
    }

    /// To the line `number`, a `GOSUB` keeping where it returns to.
    fn jump(&mut self, number: u32, gosub: bool) -> Step<Next> {
        let line = self.find_line(number)?.ok_or(Error::NoSuchLine)?;
        if gosub {
            self.skip_to_statement_end()?;
            self.push(
                Kind::Gosub,
                0,
                &[self.ptr, self.line, self.flags & flag::RUNNING],
            )?;
        }
        self.go_to_line(line);
        Ok(Next::Moved)
    }

    /// The pointer moved on to the statement's end, past any string: where a
    /// `GOSUB` returns.
    fn skip_to_statement_end(&mut self) -> Step {
        loop {
            match self.peek()? {
                b':' | b'\r' | token::ELSE => return Ok(()),
                b'"' => {
                    self.ptr += 1;
                    while !matches!(self.next()?, b'"' | b'\r') {}
                }
                _ => self.ptr += 1,
            }
        }
    }

    /// `RETURN`: back to after the innermost `GOSUB`, loops begun since it
    /// left.
    pub(crate) fn return_statement(&mut self) -> Step<Next> {
        loop {
            let header = self.top_header()?.ok_or(Error::NotInSubroutine)?;
            match header.kind {
                Kind::Gosub => break,
                Kind::For | Kind::Repeat | Kind::While => {
                    self.drop_top()?;
                }
                _ => return Err(Error::NotInSubroutine.into()),
            }
        }
        let mut words = [0; 3];
        self.pop_record(Kind::Gosub, &mut words)?;
        self.ptr = words[0];
        self.line = words[1];
        self.flags = (self.flags & !flag::RUNNING) | (words[2] & flag::RUNNING);
        Ok(Next::Done)
    }

    // IF.

    /// `IF`'s condition: the statements after `THEN`, a line jumped to, or
    /// a block begun; when false, what follows `ELSE`.
    pub(crate) fn if_condition(&mut self, value: &Value) -> Step<Next> {
        let truth = self.int_value(value)? != 0;
        let then = self.take(token::THEN)?;
        if then && self.skip_spaces()? == b'\r' {
            if truth {
                return Ok(Next::Done);
            }
            self.seek(Seek::ElseOrEndif)?;
            return Ok(Next::Moved);
        }
        if truth {
            if let Some(number) = self.line_reference()? {
                return self.jump(number, false);
            }
            return Ok(Next::Moved);
        }
        self.skip_to_else()?;
        if self.peek()? == b'\r' {
            return Ok(Next::Moved);
        }
        self.ptr += 1;
        if let Some(number) = self.line_reference()? {
            return self.jump(number, false);
        }
        Ok(Next::Moved)
    }

    /// The pointer moved to the line's first `ELSE` after it, past strings,
    /// or its end.
    fn skip_to_else(&mut self) -> Step {
        loop {
            match self.peek()? {
                b'\r' | token::ELSE => return Ok(()),
                b'"' => {
                    self.ptr += 1;
                    while !matches!(self.next()?, b'"' | b'\r') {}
                }
                _ => self.ptr += 1,
            }
        }
    }

    /// An `ELSE` reached by running: the `THEN` part is over, so the rest of
    /// the line — or of a block, to its `ENDIF` — is passed over.
    pub(crate) fn else_reached(&mut self) -> Step {
        let block = self.byte_at(self.ptr - 1)? == token::ELSE_BLOCK;
        if block {
            return self.seek(Seek::Endif);
        }
        self.skip_line()
    }

    /// Execution moved past the end of the structure `seek` names, along the
    /// lines after the current one, nested structures passed whole.
    fn seek(&mut self, seek: Seek) -> Step {
        let mut depth = 0_u32;
        self.skip_line()?;
        loop {
            let line = self.ptr;
            if self.byte_at(line)? != b'\r' {
                return Err(Error::BadProgram.into());
            }
            if self.line == 0 || self.byte_at(line + 1)? == END {
                return Err(match seek {
                    Seek::ElseOrEndif | Seek::Endif => Error::MissingEndif,
                    Seek::When | Seek::Endcase => Error::MissingEndcase,
                }
                .into());
            }
            self.line = line;
            self.ptr = line + 4;
            let first = self.skip_spaces()?;
            let opens = self.line_opens(seek)?;
            match (seek, first) {
                (Seek::ElseOrEndif, token::ELSE_BLOCK) if depth == 0 => {
                    self.ptr += 1;
                    return Ok(());
                }
                (Seek::ElseOrEndif | Seek::Endif, token::ENDIF)
                | (Seek::When | Seek::Endcase, token::ENDCASE) => {
                    if depth == 0 {
                        self.ptr += 1;
                        return Ok(());
                    }
                    depth -= 1;
                }
                (Seek::When, token::WHEN | token::OTHERWISE) if depth == 0 => {
                    return Ok(());
                }
                _ => {}
            }
            if opens {
                depth += 1;
            }
            self.skip_line()?;
            self.os.yield_point()?;
        }
    }

    /// Whether the line at the pointer opens a structure of `seek`'s kind
    /// that its own end closes: a block `IF`, a `WHILE`, a `CASE`.
    fn line_opens(&mut self, seek: Seek) -> Step<bool> {
        let start = self.ptr;
        let mut opens = false;
        loop {
            let b = self.peek()?;
            match b {
                b'\r' | token::REM | token::DATA => break,
                b'"' => {
                    self.ptr += 1;
                    while !matches!(self.next()?, b'"' | b'\r') {}
                    continue;
                }
                token::THEN if matches!(seek, Seek::ElseOrEndif | Seek::Endif) => {
                    self.ptr += 1;
                    if self.skip_spaces()? == b'\r' {
                        opens = true;
                    }
                    continue;
                }
                token::STATEMENT | token::COMMAND | token::FUNCTION => {
                    let second = self.byte_at(self.ptr + 1)?;
                    if b == token::STATEMENT
                        && matches!(seek, Seek::When | Seek::Endcase)
                        && second == token::statement::CASE
                    {
                        opens = true;
                    }
                    self.ptr += 1;
                }
                _ => {}
            }
            self.ptr += 1;
        }
        self.ptr = start;
        Ok(opens)
    }

    // FOR and NEXT.

    /// `FOR`'s control variable: a numeric variable, `=` and its start next.
    pub(crate) fn for_variable(&mut self, lvalue: Lvalue) -> Step<Next> {
        if !matches!(lvalue.place, Place::Int | Place::Real) {
            return Err(Error::BadForVariable.into());
        }
        self.expect(b'=', Error::MissingEqualsInFor)?;
        let place = lvalue.place.call_type(self.dialect());
        self.evaluate(cont::FOR_START, Payload::of(&[lvalue.address, place]))
    }

    /// `FOR`'s start: kept in the variable, then `TO` and the limit.
    pub(crate) fn for_start(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let lvalue = self.for_lvalue(payload)?;
        self.write(lvalue, value)?;
        self.expect(token::TO, Error::MissingTo)?;
        self.evaluate(cont::FOR_LIMIT, payload)
    }

    /// `FOR`'s limit: then its step, one where none is given.
    pub(crate) fn for_limit(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let (low, high) = self.loop_words(payload, value)?;
        let payload = payload.with(2, low).with(3, high);
        if self.take(token::STEP)? {
            return self.evaluate(cont::FOR_STEP, payload);
        }
        self.for_step(payload, &Value::Int(1))
    }

    /// `FOR`'s step: the loop's frame pushed, the body run once at least.
    pub(crate) fn for_step(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let (step_low, step_high) = self.loop_words(payload, value)?;
        let lvalue = self.for_lvalue(payload)?;
        if self.is_zero_step(lvalue.place, step_low, step_high) {
            return Err(Error::ZeroStep.into());
        }
        self.push(
            Kind::For,
            0,
            &[
                payload.word(0),
                payload.word(1),
                payload.word(2),
                payload.word(3),
                step_low,
                step_high,
                self.ptr,
                self.line,
            ],
        )?;
        Ok(Next::Done)
    }

    /// The control variable a `FOR`'s payload names.
    fn for_lvalue(&self, payload: Payload) -> Step<Lvalue> {
        let place =
            Place::of_call_type(payload.word(1), self.dialect()).ok_or(Error::StackCorrupt)?;
        Ok(Lvalue {
            address: payload.word(0),
            place,
        })
    }

    /// `value` as the loop's own kind of number, in two words.
    fn loop_words(&mut self, payload: Payload, value: &Value) -> Step<(u32, u32)> {
        let lvalue = self.for_lvalue(payload)?;
        if lvalue.place == Place::Int {
            return Ok((self.int_value(value)?.cast_unsigned(), 0));
        }
        let real = self.real_value(value)?;
        let [low, high] = self.arith.encode_words(real);
        Ok((low, high))
    }

    /// The number two loop words hold.
    fn loop_number(&self, place: Place, low: u32, high: u32) -> Number {
        if place == Place::Int {
            return Number::Int(low.cast_signed());
        }
        Number::Real(self.arith.decode_words([low, high]))
    }

    fn is_zero_step(&self, place: Place, low: u32, high: u32) -> bool {
        match self.loop_number(place, low, high) {
            Number::Int(n) => n == 0,
            Number::Real(r) => self.arith.is_zero(r),
        }
    }

    /// `NEXT`, with or without its variables.
    pub(crate) fn next_statement(&mut self) -> Step<Next> {
        if self.at_statement_end()? || self.skip_spaces()? == b',' {
            return self.next_loop(None);
        }
        self.lvalue(cont::NEXT, Payload::default())
    }

    /// `NEXT`'s variable read.
    pub(crate) fn next_variable(&mut self, lvalue: Lvalue) -> Step<Next> {
        self.next_loop(Some(lvalue))
    }

    /// The innermost loop, or the innermost of `lvalue`'s, stepped: round
    /// again, or ended and the next variable after a comma stepped.
    fn next_loop(&mut self, lvalue: Option<Lvalue>) -> Step<Next> {
        loop {
            let header = self.top_header()?.ok_or(Error::NotInFor)?;
            if header.kind != Kind::For {
                return Err(match header.kind {
                    Kind::Repeat | Kind::While | Kind::Gosub => Error::CannotMatchFor,
                    _ => Error::NotInFor,
                }
                .into());
            }
            let variable = self.record_word(self.sp, 0)?;
            match lvalue {
                Some(lvalue) if lvalue.address != variable => {
                    self.drop_top()?;
                }
                _ => break,
            }
        }
        let mut words = [0; 8];
        let frame = self.sp;
        for (i, slot) in (0..).zip(words.iter_mut()) {
            *slot = self.record_word(frame, i)?;
        }
        let place = Place::of_call_type(words[1], self.dialect()).ok_or(Error::StackCorrupt)?;
        let variable = Lvalue {
            address: words[0],
            place,
        };
        let limit = self.loop_number(place, words[2], words[3]);
        let step = self.loop_number(place, words[4], words[5]);
        let current = self.read(variable)?.number().ok_or(Error::StackCorrupt)?;
        // An integer loop stepping past the integers' range ends there.
        let (stepped, overflowed) = match (current, step) {
            (Number::Int(a), Number::Int(b)) => {
                let (sum, overflowed) = a.overflowing_add(b);
                (Number::Int(sum), overflowed)
            }
            _ => (self.arith.add(current, step)?, false),
        };
        self.write(variable, &stepped.into())?;
        let rising = match step {
            Number::Int(n) => n > 0,
            Number::Real(r) => !self.arith.is_negative(r),
        };
        let order = self.arith.compare(stepped, limit)?;
        let past = overflowed
            || match order {
                Some(core::cmp::Ordering::Greater) => rising,
                Some(core::cmp::Ordering::Less) => !rising,
                Some(core::cmp::Ordering::Equal) => false,
                None => true,
            };
        if !past {
            self.ptr = words[6];
            self.line = words[7];
            return Ok(Next::Done);
        }
        self.drop_top()?;
        if self.take(b',')? {
            return self.lvalue(cont::NEXT, Payload::default());
        }
        Ok(Next::Done)
    }

    // REPEAT and UNTIL.

    /// `REPEAT`: its frame, the loop's start after it.
    pub(crate) fn repeat_statement(&mut self) -> Step<Next> {
        self.push(Kind::Repeat, 0, &[self.ptr, self.line])?;
        Ok(Next::Moved)
    }

    /// `UNTIL`'s condition: round again while it is false.
    pub(crate) fn until_condition(&mut self, value: &Value) -> Step<Next> {
        let header = self.top_header()?.ok_or(Error::NotInRepeat)?;
        if header.kind != Kind::Repeat {
            return Err(Error::NotInRepeat.into());
        }
        if self.int_value(value)? == 0 {
            self.ptr = self.record_word(self.sp, 0)?;
            self.line = self.record_word(self.sp, 1)?;
            return Ok(Next::Moved);
        }
        self.drop_top()?;
        Ok(Next::Done)
    }

    // WHILE and ENDWHILE.

    /// `WHILE`: its condition, the statement's own start kept to come back
    /// to.
    pub(crate) fn while_statement(&mut self) -> Step<Next> {
        let start = self.ptr - 2;
        self.evaluate(cont::WHILE, Payload::of(&[start, self.line]))
    }

    /// `WHILE`'s condition: the body run with its frame, or passed to its
    /// `ENDWHILE`.
    pub(crate) fn while_condition(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        if self.int_value(value)? != 0 {
            self.push(Kind::While, 0, &[payload.word(0), payload.word(1)])?;
            return Ok(Next::Done);
        }
        self.seek_endwhile()?;
        Ok(Next::Moved)
    }

    /// Execution moved past the `ENDWHILE` matching the `WHILE` just
    /// tested, wherever on a line it lies: each `WHILE` met nests one more,
    /// strings and the text after `REM` and `DATA` passed whole.
    fn seek_endwhile(&mut self) -> Step {
        let mut depth = 0_u32;
        loop {
            let b = self.next()?;
            match b {
                b'\r' => {
                    let line = self.ptr - 1;
                    if self.line == 0 || self.byte_at(line + 1)? == END {
                        return Err(Error::NotInWhile.into());
                    }
                    self.line = line;
                    self.ptr = line + 4;
                    self.os.yield_point()?;
                }
                b'"' => while !matches!(self.next()?, b'"' | b'\r') {},
                token::REM | token::DATA => self.skip_line()?,
                token::ENDWHILE if depth == 0 => return Ok(()),
                token::ENDWHILE => depth -= 1,
                token::STATEMENT | token::COMMAND | token::FUNCTION => {
                    if b == token::STATEMENT && self.peek()? == token::statement::WHILE {
                        depth += 1;
                    }
                    self.ptr += 1;
                }
                _ => {}
            }
        }
    }

    /// `ENDWHILE`: back to the `WHILE` to test again.
    pub(crate) fn endwhile_statement(&mut self) -> Step<Next> {
        let header = self.top_header()?.ok_or(Error::NotInWhile)?;
        if header.kind != Kind::While {
            return Err(Error::NotInWhile.into());
        }
        let mut words = [0; 2];
        self.pop_record(Kind::While, &mut words)?;
        self.ptr = words[0];
        self.line = words[1];
        Ok(Next::Moved)
    }

    // CASE.

    /// `CASE`'s value: `OF` closes the line, then the first `WHEN` whose
    /// value matches is run, else `OTHERWISE`'s statements.
    pub(crate) fn case_value(&mut self, value: &Value) -> Step<Next> {
        self.expect(token::OF, Error::MissingOf)?;
        if self.skip_spaces()? != b'\r' {
            return Err(Error::CaseNotLast.into());
        }
        if self.line == 0 {
            return Err(Error::MissingEndcase.into());
        }
        self.push_value(value)?;
        self.next_when()
    }

    /// The next `WHEN`, `OTHERWISE` or `ENDCASE` sought.
    fn next_when(&mut self) -> Step<Next> {
        self.seek(Seek::When)?;
        let b = self.skip_spaces()?;
        if b == token::ENDCASE || self.byte_at(self.ptr - 1)? == token::ENDCASE {
            self.pop_value()?;
            return Ok(Next::Done);
        }
        self.ptr += 1;
        if b == token::OTHERWISE {
            self.pop_value()?;
            return Ok(Next::Moved);
        }
        self.evaluate(cont::WHEN, Payload::default())
    }

    /// A `WHEN`'s value against `CASE`'s: its statements run if they match,
    /// else its next value or the next `WHEN`.
    pub(crate) fn when_value(&mut self, _payload: Payload, value: &Value) -> Step<Next> {
        let case = self.pop_value()?;
        let matched = self.binary(super::super::eval::Op::Eq, &case, value)?;
        if self.int_value(&matched)? != 0 {
            while self.peek()? != b':' && self.peek()? != b'\r' {
                self.ptr += 1;
                if self.byte_at(self.ptr - 1)? == b'"' {
                    while !matches!(self.next()?, b'"' | b'\r') {}
                }
            }
            return Ok(Next::Moved);
        }
        self.push_value(&case)?;
        if self.take(b',')? {
            return self.evaluate(cont::WHEN, Payload::default());
        }
        self.next_when()
    }

    /// A `WHEN` or `OTHERWISE` reached by running: the matched part is over,
    /// so the rest of the `CASE` is passed.
    pub(crate) fn when_reached(&mut self) -> Step<Next> {
        self.seek(Seek::Endcase)?;
        Ok(Next::Moved)
    }

    // ON.

    /// `ON`: `ON ERROR` and its forms, or a choice of lines or procedures.
    pub(crate) fn on_statement(&mut self) -> Step<Next> {
        if self.take(token::ERROR)? {
            return self.on_error();
        }
        if self.at_statement_end()? {
            // `ON` alone: the cursor shown, as `OFF` hides it.
            self.vdu_codes(&[23, 1, 1, 0, 0, 0, 0, 0, 0, 0])?;
            return Ok(Next::Done);
        }
        self.evaluate(cont::ON, Payload::default())
    }

    /// `ON`'s choice: the `n`th of the `GOTO` or `GOSUB` lines, or `PROC`s,
    /// or what follows `ELSE` past the last.
    pub(crate) fn on_choice(&mut self, _payload: Payload, value: &Value) -> Step<Next> {
        let n = self.int_value(value)?;
        let kind = self.skip_spaces()?;
        if !matches!(kind, token::GOTO | token::GOSUB | token::PROC) {
            return Err(Error::OnSyntax.into());
        }
        self.ptr += 1;
        let mut index = 1;
        loop {
            if index == n {
                if kind == token::PROC {
                    let item = self.ptr;
                    self.skip_to_line_or_else()?;
                    let end = self.ptr;
                    self.ptr = item;
                    return self.proc_call(Some(end));
                }
                let number = self.line_reference()?.ok_or(Error::OnSyntax)?;
                self.skip_to_line_or_else()?;
                return self.jump(number, kind == token::GOSUB);
            }
            self.skip_on_item()?;
            if !self.take(b',')? {
                break;
            }
            index += 1;
            if kind == token::PROC {
                self.expect(token::PROC, Error::OnSyntax)?;
            }
        }
        if self.take(token::ELSE)? {
            return Ok(Next::Moved);
        }
        Err(Error::OnRange.into())
    }

    /// The `ON` item at the pointer passed: a line number or a procedure
    /// call with its brackets.
    fn skip_on_item(&mut self) -> Step {
        let mut depth = 0_u32;
        loop {
            match self.peek()? {
                b'(' => depth += 1,
                b')' => depth = depth.saturating_sub(1),
                b',' | b':' | b'\r' | token::ELSE if depth == 0 => return Ok(()),
                b'"' => {
                    self.ptr += 1;
                    while !matches!(self.next()?, b'"' | b'\r') {}
                    continue;
                }
                _ => {}
            }
            self.ptr += 1;
        }
    }

    /// The rest of an `ON`'s list passed, a `GOSUB`'s return at its end.
    fn skip_to_line_or_else(&mut self) -> Step {
        loop {
            self.skip_on_item()?;
            if !self.take(b',')? {
                break;
            }
        }
        if self.peek()? == token::ELSE {
            self.skip_line()?;
        }
        Ok(())
    }

    // PROC.

    /// `PROC`: the procedure called.
    pub(crate) fn proc_statement(&mut self) -> Step<Next> {
        self.proc_call(None)
    }

    /// The procedure named at the pointer called, to return to `return_to`
    /// or where its arguments end.
    fn proc_call(&mut self, return_to: Option<u32>) -> Step<Next> {
        let start = self.ptr;
        while super::super::vars::in_name(self.peek()?) {
            self.ptr += 1;
        }
        let length = self.ptr - start;
        if length == 0 {
            return Err(Error::BadCall.into());
        }
        self.begin_call(Routine::Proc, start, length, return_to)
    }

    /// A function's result after `=`: its routine returned from, the value
    /// handed to the expression that called it.
    pub(crate) fn fn_result(&mut self, value: &Value) -> Step<Next> {
        self.return_from(Routine::Fn)?;
        self.operand_is(value)
    }
}
