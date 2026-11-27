//! Data and memory: `DATA`, `READ` and `RESTORE`; `DIM`; `LOCAL`; `SWAP`;
//! `CLEAR`; and `PAGE`, `LOMEM`, `HIMEM` and `END` set.

use super::super::functions::{SUBSCRIPTS_MOST, UNDIMENSIONED_BELOW};
use super::super::lvalue::{Element, Lvalue, Place};
use super::super::next::{Next, Payload};
use super::super::{len32, Machine, Step, MARGIN};
use super::cont;
use crate::messages::Error;
use crate::stack::Kind;
use crate::token;
use crate::value::{Text, Value};
use crate::workspace::{self as ws, reg};

impl Machine<'_, '_> {
    // DATA, READ and RESTORE.

    /// `READ`: each variable given the next `DATA` item.
    pub(crate) fn read_statement(&mut self) -> Step<Next> {
        if self.at_statement_end()? {
            return Ok(Next::Done);
        }
        self.lvalue(cont::READ, Payload::default())
    }

    /// A `READ` variable given its item: a string as it is, a number by
    /// evaluating the item as an expression, as BASIC V reads one.
    pub(crate) fn read_into(&mut self, _payload: Payload, lvalue: Lvalue) -> Step<Next> {
        let (item, quoted) = self.next_data()?;
        if lvalue.place.is_string() {
            self.write(lvalue, &Value::Str(item))?;
            return self.read_more();
        }
        if quoted {
            return Err(Error::NeedNumber.into());
        }
        let place = lvalue.place.call_type(self.dialect());
        self.evaluate_text(
            &item,
            cont::READ_VALUE,
            Payload::of(&[lvalue.address, place]),
        )
    }

    /// A numeric `READ`'s item evaluated: kept, then the next variable.
    pub(crate) fn read_value(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let place =
            Place::of_call_type(payload.word(1), self.dialect()).ok_or(Error::StackCorrupt)?;
        let lvalue = Lvalue {
            address: payload.word(0),
            place,
        };
        self.write(lvalue, value)?;
        self.read_more()
    }

    /// `READ`'s next variable after a comma, or its end.
    fn read_more(&mut self) -> Step<Next> {
        if self.take(b',')? {
            return self.lvalue(cont::READ, Payload::default());
        }
        Ok(Next::Done)
    }

    /// The next `DATA` item, the pointer moved past it, and whether it was
    /// quoted: unquoted, its leading spaces dropped; quoted, `""` a quote.
    fn next_data(&mut self) -> Step<(Text, bool)> {
        let mut at = self.os.read_word(reg::DATA.at())?;
        if at == 0 {
            at = self.data_from(self.page)?.ok_or(Error::OutOfData)?;
        }
        let mut b = self.byte_at(at)?;
        if b == b'\r' {
            at = self.data_from(at)?.ok_or(Error::OutOfData)?;
            b = self.byte_at(at)?;
        }
        if b == b',' {
            at += 1;
        }
        while self.byte_at(at)? == b' ' {
            at += 1;
        }
        let mut text = Text::EMPTY;
        let quoted = self.byte_at(at)? == b'"';
        if quoted {
            at += 1;
            loop {
                let c = self.byte_at(at)?;
                at += 1;
                match c {
                    b'"' if self.byte_at(at)? == b'"' => at += 1,
                    b'"' | b'\r' => break,
                    _ => {}
                }
                text.push_bytes(&[c]).ok_or(Error::StringTooLong)?;
            }
            while !matches!(self.byte_at(at)?, b',' | b'\r') {
                at += 1;
            }
        } else {
            loop {
                let c = self.byte_at(at)?;
                if matches!(c, b',' | b'\r') {
                    break;
                }
                text.push_bytes(&[c]).ok_or(Error::StringTooLong)?;
                at += 1;
            }
        }
        self.os.write_word(reg::DATA.at(), at)?;
        Ok((text, quoted))
    }

    /// The first `DATA` item's address on the line at `line` or after it.
    fn data_from(&mut self, line: u32) -> Step<Option<u32>> {
        let mut line = line;
        loop {
            if self.byte_at(line)? != b'\r'
                || self.byte_at(line + 1)? == tairix_openbasic::program::END
            {
                return Ok(None);
            }
            let length = u32::from(self.byte_at(line + 3)?);
            if length < 4 {
                return Err(Error::BadProgram.into());
            }
            let mut at = line + 4;
            while self.byte_at(at)? == b' ' {
                at += 1;
            }
            if self.byte_at(at)? == token::DATA {
                return Ok(Some(at + 1));
            }
            line += length;
            self.os.yield_point()?;
        }
    }

    /// `RESTORE`: the data pointer to the program's start, a line, a line
    /// a number of lines on, `RESTORE ERROR` or `RESTORE DATA`.
    pub(crate) fn restore_statement(&mut self) -> Step<Next> {
        if self.take(token::ERROR)? {
            self.restore_error()?;
            return Ok(Next::Done);
        }
        if self.take(token::DATA)? {
            self.restore_local_data()?;
            return Ok(Next::Done);
        }
        if self.at_statement_end()? {
            self.os.write_word(reg::DATA.at(), 0)?;
            return Ok(Next::Done);
        }
        let relative = self.take(b'+')?;
        if !relative {
            if let Some(number) = self.line_reference()? {
                return self.restore_to(Payload::of(&[0]), &Value::Int(number.cast_signed()));
            }
        }
        self.evaluate(cont::RESTORE, Payload::of(&[u32::from(relative)]))
    }

    /// `RESTORE`'s line.
    pub(crate) fn restore_to(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let n = self.int_value(value)?;
        let line = if payload.word(0) != 0 {
            let mut line = self.line;
            for _ in 0..n.max(0) {
                if line == 0
                    || self.byte_at(line.wrapping_add(1))? == tairix_openbasic::program::END
                {
                    return Err(Error::NoSuchLine.into());
                }
                line = line.wrapping_add(u32::from(self.byte_at(line.wrapping_add(3))?));
                self.os.yield_point()?;
            }
            line
        } else {
            let number = u32::try_from(n).map_err(|_| Error::NoSuchLine)?;
            self.find_line(number)?.ok_or(Error::NoSuchLine)?
        };
        let at = self.data_from(line)?.unwrap_or(0);
        self.os.write_word(reg::DATA.at(), at)?;
        Ok(Next::Done)
    }

    /// `LOCAL DATA`'s saved pointer put back, as `RESTORE DATA` does.
    pub(crate) fn restore_local_data(&mut self) -> Step {
        let header = self.top_header()?.ok_or(Error::NoDataPointer)?;
        if header.kind != Kind::LocalData {
            return Err(Error::NoDataPointer.into());
        }
        let mut words = [0; 1];
        self.pop_record(Kind::LocalData, &mut words)?;
        Ok(self.os.write_word(reg::DATA.at(), words[0])?)
    }

    // LOCAL.

    /// `LOCAL`: variables made local, `LOCAL ERROR` or `LOCAL DATA`.
    pub(crate) fn local_statement(&mut self) -> Step<Next> {
        if !self.in_routine()? {
            return Err(Error::LocalOutside.into());
        }
        if self.take(token::ERROR)? {
            self.local_error()?;
            return Ok(Next::Done);
        }
        if self.take(token::DATA)? {
            let data = self.os.read_word(reg::DATA.at())?;
            self.push(Kind::LocalData, 0, &[data])?;
            return Ok(Next::Done);
        }
        self.lvalue(cont::LOCAL, Payload::default())
    }

    /// A `LOCAL` variable's value saved and emptied, then the next.
    pub(crate) fn local_item(&mut self, lvalue: Lvalue) -> Step<Next> {
        self.save_local(lvalue, None)?;
        if self.take(b',')? {
            return self.lvalue(cont::LOCAL, Payload::default());
        }
        Ok(Next::Done)
    }

    /// Whether a procedure's or function's frame is on the stack.
    fn in_routine(&mut self) -> Step<bool> {
        let mut at = self.sp;
        while let Some(header) = self.header_at(at)? {
            if matches!(header.kind, Kind::Proc | Kind::Fn) {
                return Ok(true);
            }
            at += header.length;
        }
        Ok(false)
    }

    // DIM.

    /// `DIM`: arrays dimensioned and blocks of bytes made, comma-separated.
    pub(crate) fn dim_statement(&mut self) -> Step<Next> {
        self.skip_spaces()?;
        let name = self.read_name()?.ok_or(Error::BadDim)?;
        if name.is_array() {
            let lvalue = self.variable(&name, true)?.ok_or(Error::BadDim)?;
            let pointer = self.os.read_word(lvalue.address)?;
            if pointer >= UNDIMENSIONED_BELOW {
                return Err(Error::Redimension.into());
            }
            let Place::Array(element) = lvalue.place else {
                return Err(Error::BadDim.into());
            };
            let local = pointer == super::super::routines::LOCAL_ARRAY;
            return self.evaluate(
                cont::DIM,
                Payload::of(&[lvalue.address, element as u32, 0, u32::from(local)]),
            );
        }
        let lvalue = self.variable(&name, true)?.ok_or(Error::BadDim)?;
        if !matches!(lvalue.place, Place::Int | Place::Real) {
            return Err(Error::BadDim.into());
        }
        let place = lvalue.place.call_type(self.dialect());
        let local = self.take(token::LOCAL)?;
        self.evaluate(
            cont::DIM,
            Payload::of(&[lvalue.address, place, u32::MAX, u32::from(local)]),
        )
    }

    /// A `DIM`'s size: an array's next dimension, or a block's bytes.
    pub(crate) fn dim_size(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let n = self.int_value(value)?;
        if payload.word(2) == u32::MAX {
            let place =
                Place::of_call_type(payload.word(1), self.dialect()).ok_or(Error::StackCorrupt)?;
            self.dim_block(
                Lvalue {
                    address: payload.word(0),
                    place,
                },
                n,
                payload.word(3) != 0,
            )?;
            return self.dim_more();
        }
        if n < 0 {
            return Err(Error::NegativeDim.into());
        }
        let count = payload.word(2) + 1;
        if count as usize > SUBSCRIPTS_MOST {
            return Err(Error::ImpossibleDimension.into());
        }
        self.push(Kind::Int, 0, &[n.cast_unsigned()])?;
        if self.take(b',')? {
            return self.evaluate(cont::DIM, payload.with(2, count));
        }
        self.expect(b')', Error::MissingDimEnd)?;
        let element = match payload.word(1) {
            0 => Element::Int,
            1 => Element::Real,
            _ => Element::Str,
        };
        self.dim_array(payload.word(0), element, count, payload.word(3) != 0)?;
        self.dim_more()
    }

    /// Another of a `DIM`'s items after a comma, or its end.
    fn dim_more(&mut self) -> Step<Next> {
        if self.take(b',')? {
            return self.dim_statement();
        }
        Ok(Next::Done)
    }

    /// A block of `n + 1` bytes, its address kept in `lvalue`: from the
    /// heap, or from the stack for `DIM ... LOCAL`.
    fn dim_block(&mut self, lvalue: Lvalue, n: i32, local: bool) -> Step {
        let bytes = u32::try_from(n.checked_add(1).ok_or(Error::NoDimRoom)?)
            .map_err(|_| Error::NegativeDim)?;
        let size = bytes.checked_add(3).ok_or(Error::NoDimRoom)? & !3;
        let at = if local {
            if !self.in_routine()? {
                return Err(Error::LocalOutside.into());
            }
            self.local_block((size / 4) as usize, 0, Error::NoDimRoom)?
        } else {
            self.allocate(size, Error::NoDimRoom)?
        };
        self.write(lvalue, &Value::Int(at.cast_signed()))
    }

    /// The array whose pointer is at `pointer_at` dimensioned with the
    /// `count` sizes on top: its block of sizes, a zero, the total and its
    /// elements, all zero, after the variables or, for a `LOCAL` array, on
    /// the stack.
    fn dim_array(&mut self, pointer_at: u32, element: Element, count: u32, local: bool) -> Step {
        let mut sizes = [0_u32; SUBSCRIPTS_MOST];
        let n = count as usize;
        for slot in sizes[..n].iter_mut().rev() {
            let top = self.pop_int()?;
            *slot = u32::try_from(top)
                .ok()
                .and_then(|s| s.checked_add(1))
                .ok_or(Error::ImpossibleDimension)?;
        }
        let total = sizes[..n]
            .iter()
            .try_fold(1_u32, |total, &s| total.checked_mul(s))
            .ok_or(Error::NoDimensionRoom)?;
        let element_size = element.place().bytes(self.dialect());
        let data = total
            .checked_mul(element_size)
            .ok_or(Error::NoDimensionRoom)?;
        let header = 4 * (len32(n) + 2);
        let size = data
            .checked_add(header)
            .and_then(|s| s.checked_add(3))
            .ok_or(Error::NoDimensionRoom)?
            & !3;
        let block = if local {
            self.local_block((size / 4) as usize, 0, Error::NoDimensionRoom)?
        } else {
            self.allocate(size, Error::NoDimensionRoom)?
        };
        self.os.fill(block, size, 0)?;
        self.os.write_words(block, &sizes[..n])?;
        self.os.write_word(block + 4 * len32(n) + 4, total)?;
        Ok(self.os.write_word(pointer_at, block)?)
    }

    // SWAP.

    /// `SWAP`'s two l-values, then their values exchanged.
    pub(crate) fn swap_item(&mut self, payload: Payload, lvalue: Lvalue) -> Step<Next> {
        if payload.words().is_empty() {
            self.expect(b',', Error::MissingComma)?;
            let place = lvalue.place.call_type(self.dialect());
            return self.lvalue(cont::SWAP, Payload::of(&[lvalue.address, place]));
        }
        let place =
            Place::of_call_type(payload.word(1), self.dialect()).ok_or(Error::StackCorrupt)?;
        let first = Lvalue {
            address: payload.word(0),
            place,
        };
        match (first.place, lvalue.place) {
            (Place::Array(a), Place::Array(b)) => {
                if a != b {
                    return Err(Error::SwapArrays.into());
                }
                let x = self.os.read_word(first.address)?;
                let y = self.os.read_word(lvalue.address)?;
                self.os.write_word(first.address, y)?;
                self.os.write_word(lvalue.address, x)?;
            }
            (Place::Sib, Place::Sib) => {
                let mut x = [0; 5];
                let mut y = [0; 5];
                self.os.read_bytes(first.address, &mut x)?;
                self.os.read_bytes(lvalue.address, &mut y)?;
                self.os.write_bytes(first.address, &y)?;
                self.os.write_bytes(lvalue.address, &x)?;
            }
            (a, b) if a.is_string() != b.is_string() => {
                return Err(Error::NeedString.into());
            }
            _ => {
                let x = self.read(first)?;
                let y = self.read(lvalue)?;
                self.write(first, &y)?;
                self.write(lvalue, &x)?;
            }
        }
        Ok(Next::Done)
    }

    // CLEAR and the memory pointers.

    /// `CLEAR`: every variable but the resident integers forgotten, the
    /// heap emptied and the stack with it.
    pub(crate) fn clear(&mut self) -> Step {
        self.fsa = self.lomem;
        self.os.write_word(ws::FSA.at(), self.lomem)?;
        self.os
            .write_words(ws::FREE_LISTS.at(), &[0; ws::LAST_HEAD as usize + 1])?;
        for slot in [ws::LOCALARLIST, ws::LIBRARYLIST, ws::OVERPTR] {
            self.os.write_word(slot.at(), 0)?;
        }
        self.os.write_word(reg::DATA.at(), 0)?;
        self.os.write_word(reg::ON_ERROR.at(), 0)?;
        self.sp = self.himem;
        Ok(())
    }

    /// `PAGE` set: where the program starts, word-aligned.
    pub(crate) fn set_page(&mut self, page: u32) -> Step {
        if !page.is_multiple_of(4) || page < ws::PAGE_DEFAULT.min(ws::END) || page >= self.himem {
            return Err(Error::InsufficientMemory.into());
        }
        self.page = page;
        self.os.write_word(ws::PAGE.at(), page)?;
        Ok(())
    }

    /// `LOMEM` set: where variables start, which forgets them.
    pub(crate) fn set_lomem(&mut self, lomem: u32) -> Step {
        if !lomem.is_multiple_of(4) || lomem < self.top || lomem >= self.himem {
            return Err(Error::InsufficientMemory.into());
        }
        self.lomem = lomem;
        self.os.write_word(ws::LOMEM.at(), lomem)?;
        self.clear()
    }

    /// `HIMEM` set: where the stack starts, only while the stack holds
    /// nothing but the statement's own work.
    pub(crate) fn set_himem(&mut self, himem: u32) -> Step {
        let limit = self.os.read_word(ws::MEMLIMIT.at())?;
        if !himem.is_multiple_of(4) || himem > limit || himem < self.fsa.saturating_add(MARGIN) {
            return Err(Error::InsufficientMemory.into());
        }
        let in_use = self.himem - self.sp;
        let sp = himem.checked_sub(in_use).ok_or(Error::InsufficientMemory)?;
        if sp < self.fsa.saturating_add(MARGIN) {
            return Err(Error::InsufficientMemory.into());
        }
        self.move_stack(sp, in_use)?;
        self.himem = himem;
        self.os.write_word(ws::HIMEM.at(), himem)?;
        Ok(())
    }

    /// The stack's `bytes` moved to start at `sp`.
    fn move_stack(&mut self, sp: u32, bytes: u32) -> Step {
        if bytes > 0 {
            self.os.copy_bytes(self.sp, sp, bytes)?;
        }
        self.sp = sp;
        Ok(())
    }

    /// `END=`: the memory BASIC runs in changed through the Wimp's slot or
    /// the environment, `HIMEM` then set to its top.
    pub(crate) fn set_end(&mut self, end: u32) -> Step {
        if !end.is_multiple_of(4) || end < self.fsa.saturating_add(MARGIN) {
            return Err(Error::InsufficientMemory.into());
        }
        let size = end.checked_sub(ws::BASE).ok_or(Error::InsufficientMemory)?;
        let mut regs = tairix_riscos_kernel::os::Regs::default();
        regs.r[..2].copy_from_slice(&[size, u32::MAX]);
        self.os
            .perform(tairix_riscos_kernel::swi::X | WIMP_SLOT_SIZE, &mut regs)
            .map_err(|_| Error::InsufficientMemory)?;
        let limit = ws::BASE.saturating_add(regs.r[0]);
        if limit < end {
            return Err(Error::InsufficientMemory.into());
        }
        self.os.write_word(ws::MEMLIMIT.at(), end)?;
        self.set_himem(end)
    }
}

/// `Wimp_SlotSize`.
const WIMP_SLOT_SIZE: u32 = 0x4_00EC;
