//! The program in guest memory, from `PAGE` to `TOP`: lines typed in,
//! replaced and deleted; loaded and saved, tokenised or as text; listed
//! under `LISTO`; and the commands that work on it.

use alloc::vec::Vec;

use tairix_openbasic::list::{number_field, pieces, unpadded, Lister, Options, Piece};
use tairix_openbasic::program::{self as image, HEADER};
use tairix_openbasic::tokenise::{tokenise, Start};
use tairix_openbasic::{lineref, Dialect, Error as BasicError};
use tairix_riscos_kernel::filing::filetype;
use tairix_riscos_kernel::filing::fs::kind;
use tairix_riscos_kernel::os::Regs;
use tairix_riscos_kernel::swi::{os as swi, X};

use super::next::{Next, Payload};
use super::statements::{cont, setting};
use super::{len32, Machine, Mode, Step, MARGIN};
use crate::messages::Error;
use crate::token::{self, command};
use crate::value::{Text, Value};
use crate::workspace::{self as ws, reg};

/// A program's end.
const END: u8 = image::END;

impl Machine<'_, '_> {
    /// The line the typed `tokens` hold: replaced, put in its place, or
    /// deleted where it has no text; the variables are forgotten.
    pub(crate) fn enter_line(&mut self, tokens: &[u8]) -> Step {
        let [marker, a, b, c, text @ ..] = tokens else {
            return Err(Error::Mistake.into());
        };
        if *marker != lineref::MARKER {
            return Err(Error::Mistake.into());
        }
        self.enter_text(lineref::decode([*a, *b, *c]), text)
    }

    /// The `LISTO` options in force: their word's low byte.
    fn listo(&mut self) -> Step<u8> {
        self.byte_at(reg::LISTO.at())
    }

    /// Line `number` given the tokenised `text` typed for it, or deleted for
    /// none.
    pub(crate) fn enter_text(&mut self, number: u16, text: &[u8]) -> Step {
        let listo = Options::new(self.listo()?);
        let text = if text.is_empty() {
            None
        } else {
            Some(image::stored(Dialect::BasicV, text, listo).map_err(|_| Error::LineTooLong)?)
        };
        self.store_line(number, text.as_deref())?;
        self.clear()
    }

    /// Line `number` given `text`, or deleted for none.
    pub(crate) fn store_line(&mut self, number: u16, text: Option<&[u8]>) -> Step {
        let (at, existing) = self.line_place(number)?;
        let old = if existing {
            u32::from(self.byte_at(at + 3)?)
        } else {
            0
        };
        let length = text
            .map_or(Ok(0), |t| u8::try_from(HEADER + t.len()))
            .map_err(|_| Error::LineTooLong)?;
        let new = u32::from(length);
        let top = self.top;
        let tail = top.checked_sub(at + old).ok_or(Error::BadProgram)?;
        let new_top = top
            .checked_add(new)
            .and_then(|grown| grown.checked_sub(old))
            .ok_or(Error::BadProgram)?;
        if new_top.saturating_add(MARGIN) > self.himem {
            return Err(Error::NoRoom.into());
        }
        self.os.copy_bytes(at + old, at + new, tail)?;
        if let Some(text) = text {
            let [high, low] = number.to_be_bytes();
            self.os.write_bytes(at, &[b'\r', high, low, length])?;
            self.os.write_bytes(at + len32(HEADER), text)?;
        }
        self.set_top(new_top)
    }

    /// Where line `number` is or would go, and whether it is there.
    fn line_place(&mut self, number: u16) -> Step<(u32, bool)> {
        let mut line = self.page;
        loop {
            if self.byte_at(line)? != b'\r' {
                return Err(Error::BadProgram.into());
            }
            let high = self.byte_at(line + 1)?;
            if high == END {
                return Ok((line, false));
            }
            let here = u16::from_be_bytes([high, self.byte_at(line + 2)?]);
            if here >= number {
                return Ok((line, here == number));
            }
            let length = u32::from(self.byte_at(line + 3)?);
            if length < len32(HEADER) {
                return Err(Error::BadProgram.into());
            }
            line += length;
        }
    }

    /// `TOP` made `top`: `LOMEM` and the variables' end after it.
    pub(crate) fn set_top(&mut self, top: u32) -> Step {
        self.top = top;
        self.os.write_word(ws::TOP.at(), top)?;
        let lomem = top.next_multiple_of(4);
        self.lomem = lomem;
        self.os.write_word(ws::LOMEM.at(), lomem)?;
        self.fsa = lomem;
        self.os.write_word(ws::FSA.at(), lomem)?;
        Ok(())
    }

    /// The program emptied, as `NEW` leaves it: what `OLD` needs kept.
    pub(crate) fn new_program(&mut self) -> Step {
        let first = self.byte_at(self.page.wrapping_add(1))?;
        self.os.write_word(reg::OLD.at(), u32::from(first))?;
        self.os.write_bytes(self.page, &[b'\r', END])?;
        self.set_top(self.page + 2)?;
        self.clear()
    }

    /// `OLD`: the program `NEW` emptied back, where its lines still chain.
    pub(crate) fn old_program(&mut self) -> Step {
        let kept = u8::try_from(self.os.read_word(reg::OLD.at())?)
            .ok()
            .filter(|&kept| kept != END)
            .ok_or(Error::BadProgram)?;
        self.os.write_byte(self.page.wrapping_add(1), kept)?;
        let top = self
            .program_extent(self.page)
            .map_err(|_| Error::BadProgram)?;
        self.set_top(top)?;
        self.clear()
    }

    /// The address past the end of the program from `start`: its lines must
    /// chain to a CR and `&FF` below `HIMEM`.
    pub(crate) fn program_extent(&mut self, start: u32) -> Step<u32> {
        let mut line = start;
        loop {
            if line.saturating_add(2) > self.himem || self.byte_at(line)? != b'\r' {
                return Err(Error::BadProgram.into());
            }
            if self.byte_at(line + 1)? == END {
                return Ok(line + 2);
            }
            let length = u32::from(self.byte_at(line + 3)?);
            if length < len32(HEADER) {
                return Err(Error::BadProgram.into());
            }
            line += length;
        }
    }

    // Loading and saving.

    /// The file `name` loaded at `PAGE` as the program: a tokenised program
    /// whole, a text one tokenised a line at a time.
    pub(crate) fn load_program(&mut self, name: &Text) -> Step {
        let (length, file_type) = self.file_length(name)?;
        let staging = self.staging(length)?;
        self.load_file_at(name, staging)?;
        if file_type == filetype::TEXT {
            self.tokenise_text(staging, length)?;
            return self.clear();
        }
        self.load_staged(staging, length)
    }

    /// Where a program of `length` bytes is staged before it moves to
    /// `PAGE`: as high as memory goes, so a program tokenised from text
    /// grows from `PAGE` toward it.
    fn staging(&mut self, length: u32) -> Step<u32> {
        let room = self.himem.saturating_sub(self.page).saturating_sub(MARGIN);
        if length > room {
            return Err(Error::NoRoom.into());
        }
        Ok(self.himem - MARGIN / 2 - length.next_multiple_of(4))
    }

    /// The `length` bytes staged at `staging` made the program: a tokenised
    /// program moved to `PAGE` whole, anything else tokenised as text.
    pub(crate) fn load_staged(&mut self, staging: u32, length: u32) -> Step {
        if self.is_program(staging, length)? {
            self.os.copy_bytes(staging, self.page, length)?;
            let top = self.program_extent(self.page)?;
            self.set_top(top)?;
        } else {
            self.tokenise_text(staging, length)?;
        }
        self.clear()
    }

    /// Whether the `length` bytes at `at` are a tokenised program.
    fn is_program(&mut self, at: u32, length: u32) -> Step<bool> {
        let mut line = at;
        let end = at + length;
        loop {
            if line + 2 > end || self.byte_at(line)? != b'\r' {
                return Ok(false);
            }
            if self.byte_at(line + 1)? == END {
                return Ok(true);
            }
            let size = u32::from(self.byte_at(line + 3)?);
            if size < len32(HEADER) {
                return Ok(false);
            }
            line += size;
        }
    }

    /// The text at `at` of `length` bytes tokenised line by line at `PAGE`:
    /// a line without a number numbered ten on from the last.
    fn tokenise_text(&mut self, at: u32, length: u32) -> Step {
        let mut text = Vec::new();
        text.try_reserve_exact(length as usize)
            .map_err(|_| Error::NoRoom)?;
        text.resize(length as usize, 0);
        self.os.read_bytes(at, &mut text)?;
        self.os.write_bytes(self.page, &[b'\r', END])?;
        self.set_top(self.page + 2)?;
        let mut last: u32 = 0;
        for raw in text.split(|&b| b == b'\n' || b == b'\r') {
            if raw.iter().all(|&b| b == b' ' || b == b'\t') {
                continue;
            }
            let mut tokens = Vec::new();
            tokenise(Dialect::BasicV, raw, Start::Line, &mut tokens)
                .map_err(|_| Error::LineTooLong)?;
            let (number, body) = match tokens.as_slice() {
                [marker, a, b, c, body @ ..] if *marker == lineref::MARKER => {
                    (u32::from(lineref::decode([*a, *b, *c])), body.to_vec())
                }
                _ => {
                    let mut body = Vec::new();
                    tokenise(Dialect::BasicV, raw, Start::Statement, &mut body)
                        .map_err(|_| Error::LineTooLong)?;
                    (last + 10, body)
                }
            };
            let number = u16::try_from(number)
                .ok()
                .filter(|&n| n <= tairix_openbasic::basic5::LAST_LINE)
                .ok_or(Error::RenumberTooHigh)?;
            if body.is_empty() {
                // A number alone deletes its line, as typed.
                self.store_line(number, None)?;
            } else {
                let stored = image::stored(Dialect::BasicV, &body, Options::default())
                    .map_err(|_| Error::LineTooLong)?;
                self.store_line(number, Some(&stored))?;
            }
            last = u32::from(number);
            self.os.yield_point()?;
        }
        Ok(())
    }

    /// The length and file type `OS_File` 5 gives for `name`; for what is
    /// not a file, the error FileSwitch makes for it.
    pub(crate) fn file_length(&mut self, name: &Text) -> Step<(u32, u32)> {
        let at = self.name_block(name)?;
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[swi::file::READ_INFO, at]);
        self.os.perform(X | swi::FILE, &mut regs)?;
        if regs.r[0] & kind::FILE == 0 {
            let mut raise = Regs::default();
            raise.r[..3].copy_from_slice(&[swi::file::MAKE_ERROR, at, regs.r[0]]);
            self.os.perform(X | swi::FILE, &mut raise)?;
            return Err(Error::BadProgram.into());
        }
        let file_type = if regs.r[2] >> 20 == 0xFFF {
            (regs.r[2] >> 8) & 0xFFF
        } else {
            0
        };
        Ok((regs.r[4], file_type))
    }

    /// The file `name` loaded at `at`.
    pub(crate) fn load_file_at(&mut self, name: &Text, at: u32) -> Step {
        let block = self.name_block(name)?;
        let mut regs = Regs::default();
        regs.r[..4].copy_from_slice(&[swi::file::LOAD, block, at, 0]);
        self.os.perform(X | swi::FILE, &mut regs)?;
        Ok(())
    }

    /// `name` with its terminator in the string accumulator: its address.
    fn name_block(&mut self, name: &Text) -> Step<u32> {
        let at = ws::STRACC.at();
        self.os.write_bytes(at, name.as_bytes())?;
        self.os.write_byte(at + u32::from(name.len()), 0)?;
        Ok(at)
    }

    /// `SAVE`: the program to `name`, typed as BASIC.
    pub(crate) fn save_program(&mut self, name: &Text) -> Step {
        let at = self.name_block(name)?;
        let mut regs = Regs::default();
        regs.r[..6].copy_from_slice(&[
            swi::file::SAVE_TYPED,
            at,
            filetype::BASIC,
            0,
            self.page,
            self.top,
        ]);
        self.os.perform(X | swi::FILE, &mut regs)?;
        Ok(())
    }

    /// `TEXTSAVE`: the program listed under `listo` to `name` as text.
    pub(crate) fn text_save(&mut self, name: &Text, listo: u8) -> Step {
        let text = self.listing(0, u16::MAX, listo)?;
        let at = self.name_block(name)?;
        let room = len32(text.len());
        let block = self.top.checked_next_multiple_of(4).ok_or(Error::NoRoom)?;
        if block.saturating_add(room).saturating_add(MARGIN) > self.sp {
            return Err(Error::NoRoom.into());
        }
        self.os.write_bytes(block, &text)?;
        let mut regs = Regs::default();
        regs.r[..6].copy_from_slice(&[
            swi::file::SAVE_TYPED,
            at,
            filetype::TEXT,
            0,
            block,
            block + room,
        ]);
        self.os.perform(X | swi::FILE, &mut regs)?;
        Ok(())
    }

    // Listing.

    /// The lines from `first` to `last` listed under `listo`, each ending
    /// with an LF.
    fn listing(&mut self, first: u16, last: u16, listo: u8) -> Step<Vec<u8>> {
        let mut lister = Lister::new(Dialect::BasicV, Options::new(listo));
        let mut out = Vec::new();
        let mut line = self.page;
        loop {
            if self.byte_at(line)? != b'\r' {
                return Err(Error::BadProgram.into());
            }
            let high = self.byte_at(line + 1)?;
            if high == END {
                return Ok(out);
            }
            let number = u16::from_be_bytes([high, self.byte_at(line + 2)?]);
            let length = u32::from(self.byte_at(line + 3)?);
            if length < len32(HEADER) {
                return Err(Error::BadProgram.into());
            }
            if (first..=last).contains(&number) {
                let mut text = [0; 256];
                let n = (length as usize) - HEADER;
                self.os.read_bytes(line + len32(HEADER), &mut text[..n])?;
                match lister.line(number, &text[..n], &mut out) {
                    Ok(()) => {}
                    Err(BasicError::ReferenceListed) => return Err(Error::ReferenceListed.into()),
                    Err(_) => return Err(Error::NoRoom.into()),
                }
            }
            line += length;
            self.os.yield_point()?;
        }
    }

    /// `LIST`: the lines in its range written, each LF a screen's LF CR.
    fn list(&mut self, first: u16, last: u16) -> Step {
        let listo = self.listo()?;
        let listed = self.listing(first, last, listo)?;
        for chunk in listed.split_inclusive(|&b| b == b'\n') {
            self.check_escape()?;
            match chunk.split_last() {
                Some((b'\n', body)) => {
                    self.output(body)?;
                    self.newline()?;
                }
                _ => self.output(chunk)?,
            }
        }
        Ok(())
    }

    // The commands.

    /// A command behind the commands' prefix: a syntax error while a program
    /// runs, as every one is recorded to be.
    pub(crate) fn command(&mut self, b: u8) -> Step<Next> {
        if self.flags & super::flag::RUNNING != 0 {
            return Err(Error::Syntax.into());
        }
        match b {
            command::NEW => {
                self.new_program()?;
                self.mode = Mode::Prompt;
                Ok(Next::Moved)
            }
            command::OLD => {
                self.old_program()?;
                self.mode = Mode::Prompt;
                Ok(Next::Moved)
            }
            command::LIST if self.peek()? == b'O' => {
                self.ptr += 1;
                self.evaluate(cont::SETTING, Payload::of(&[setting::LISTO]))
            }
            command::LIST => self.list_command(),
            command::LOAD
            | command::TEXTLOAD
            | command::SAVE
            | command::TEXTSAVE
            | command::APPEND
            | command::INSTALL => {
                self.evaluate(cont::LIBRARY, Payload::of(&[u32::from(b) | FILE_COMMAND]))
            }
            command::LVAR => {
                self.lvar()?;
                Ok(Next::Done)
            }
            command::CRUNCH => self.evaluate(cont::SETTING, Payload::of(&[setting::CRUNCH])),
            _ => self.editor_command(b),
        }
    }

    /// `LIST [from][,[to]]`.
    fn list_command(&mut self) -> Step<Next> {
        let mut first = 0_u16;
        let mut last = u16::MAX;
        if let Some(n) = self.list_number()? {
            first = n;
            last = n;
        }
        if self.take(b',')? {
            last = self.list_number()?.unwrap_or(u16::MAX);
            if first == last && first != 0 {
                last = u16::MAX;
            }
        }
        if !self.at_statement_end()? {
            return Err(Error::Syntax.into());
        }
        self.list(first, last)?;
        Ok(Next::Done)
    }

    /// A line number in `LIST`'s range, as typed or encoded.
    fn list_number(&mut self) -> Step<Option<u16>> {
        let b = self.skip_spaces()?;
        if b == token::LINE_NUMBER {
            let bytes = [
                self.byte_at(self.ptr + 1)?,
                self.byte_at(self.ptr + 2)?,
                self.byte_at(self.ptr + 3)?,
            ];
            self.ptr += 4;
            return Ok(Some(lineref::decode(bytes)));
        }
        if !b.is_ascii_digit() {
            return Ok(None);
        }
        let mut n: u16 = 0;
        while self.peek()?.is_ascii_digit() {
            let digit = u16::from(self.next()? - b'0');
            n = n
                .checked_mul(10)
                .and_then(|n| n.checked_add(digit))
                .ok_or(Error::NoSuchLine)?;
        }
        Ok(Some(n))
    }

    /// `LVAR`: `@%` and the resident integers, then the variables with
    /// their values or bounds, the procedures and functions known and each
    /// library's first line, a section where there are any, as genuine
    /// BASIC lays them out.
    fn lvar(&mut self) -> Step {
        let mut row = 0;
        self.lvar_item(&mut row, LVAR_RESIDENT)?;
        let mut at = Text::EMPTY;
        at.push_bytes(b"@% = \"").ok_or(Error::StringTooLong)?;
        crate::text::write_at_percent(
            self.os.read_word(ws::AT_PERCENT.at())?,
            &mut TextOut(&mut at),
        )
        .map_err(|_| Error::StringTooLong)?;
        at.push_bytes(b"\"").ok_or(Error::StringTooLong)?;
        self.lvar_item(&mut row, at.as_bytes())?;
        self.lvar_end(&mut row)?;
        for (i, letter) in (0..).zip(b'A'..=b'Z') {
            let value = Value::Int(self.os.read_word(ws::RESIDENT.at() + 4 * i)?.cast_signed());
            let mut item = Text::EMPTY;
            item.push_bytes(&[letter, b'%'])
                .ok_or(Error::StringTooLong)?;
            self.lvar_value(&mut item, &value)?;
            self.lvar_item(&mut row, item.as_bytes())?;
        }
        self.lvar_end(&mut row)?;
        let variables =
            (ws::PROC_HEAD + 1..ws::FN_HEAD).filter(|&first| super::vars::starts_name(first));
        self.lvar_section(LVAR_VARIABLES, variables, b"")?;
        self.lvar_section(LVAR_PROCEDURES, [ws::PROC_HEAD].into_iter(), b"PROC")?;
        self.lvar_section(LVAR_FUNCTIONS, [ws::FN_HEAD].into_iter(), b"FN")?;
        self.lvar_libraries(LVAR_LIBRARIES, ws::LIBRARYLIST.at())?;
        self.lvar_libraries(LVAR_INSTALLED, ws::INSTALLLIST.at())
    }

    /// An `LVAR` section, its heading where it has any items: each entry of
    /// the chains of `heads`, its name after `prefix`, a variable's with its
    /// value or an array's with its bounds.
    fn lvar_section(
        &mut self,
        heading: &[u8],
        heads: impl Iterator<Item = u8>,
        prefix: &[u8],
    ) -> Step {
        let mut row = 0;
        let mut opened = false;
        for first in heads {
            let mut entry = self.os.read_word(Self::head(first)?)?;
            let most = self.fsa.saturating_sub(self.lomem) / 8 + 1;
            for _ in 0..most {
                if entry == 0 || entry < self.lomem || entry >= self.fsa {
                    break;
                }
                if !opened {
                    self.output(heading)?;
                    self.newline()?;
                    opened = true;
                }
                let rest = self
                    .os
                    .read_zstring(entry.wrapping_add(4), super::NAME_MOST)?;
                let mut item = Text::EMPTY;
                if prefix.is_empty() {
                    item.push_bytes(&[first]).ok_or(Error::StringTooLong)?;
                } else {
                    item.push_bytes(prefix).ok_or(Error::StringTooLong)?;
                }
                item.push_bytes(&rest).ok_or(Error::StringTooLong)?;
                if prefix.is_empty() {
                    self.lvar_variable(&mut item, entry, &rest)?;
                }
                self.lvar_item(&mut row, item.as_bytes())?;
                entry = self.os.read_word(entry)?;
                self.check_escape()?;
            }
        }
        self.lvar_end(&mut row)
    }

    /// The variable whose entry is at `entry`, named `item`, its value or
    /// bounds after its name.
    fn lvar_variable(&mut self, item: &mut Text, entry: u32, rest: &[u8]) -> Step {
        let name = super::vars::Name::of(item.as_bytes()).ok_or(Error::StringTooLong)?;
        if name.is_array() {
            let element = name.kind().element();
            let address = self.value_of(entry, rest, super::lvalue::Place::Array(element));
            if let Ok(shape) = self.shape(address, element) {
                for (i, size) in shape.sizes().iter().enumerate() {
                    if i > 0 {
                        item.push_bytes(b",").ok_or(Error::StringTooLong)?;
                    }
                    let bound = super::functions::decimal(size.wrapping_sub(1).cast_signed());
                    item.push_bytes(bound.as_bytes())
                        .ok_or(Error::StringTooLong)?;
                }
            }
            return item
                .push_bytes(b")")
                .ok_or_else(|| Error::StringTooLong.into());
        }
        let place = match name.kind() {
            super::vars::Type::Int => super::lvalue::Place::Int,
            super::vars::Type::Real => super::lvalue::Place::Real,
            super::vars::Type::Str => super::lvalue::Place::Sib,
        };
        let address = self.value_of(entry, rest, place);
        let value = self.read(super::lvalue::Lvalue { address, place })?;
        self.lvar_value(item, &value)
    }

    /// ` = ` and `value` after an `LVAR` item's name: a number in nine
    /// digits' general form whatever `@%` says, a string in quotes.
    fn lvar_value(&mut self, item: &mut Text, value: &Value) -> Step {
        item.push_bytes(b" = ").ok_or(Error::StringTooLong)?;
        match value {
            Value::Str(text) => {
                item.push_bytes(b"\"").ok_or(Error::StringTooLong)?;
                item.push_bytes(text.as_bytes())
                    .ok_or(Error::StringTooLong)?;
                item.push_bytes(b"\"").ok_or(Error::StringTooLong)?;
            }
            number => {
                let text = self.number_text(number, crate::text::Format::of(LVAR_NUMBERS))?;
                item.push_bytes(text.as_bytes())
                    .ok_or(Error::StringTooLong)?;
            }
        }
        Ok(())
    }

    /// The first line of each library on the chain at `list`, under
    /// `heading` where there are any. The walk stops at a block outside
    /// BASIC's memory, as a chain the program overwrote would lead it, and
    /// at the most blocks that memory holds.
    fn lvar_libraries(&mut self, heading: &[u8], list: u32) -> Step {
        let limit = self.os.read_word(ws::MEMLIMIT.at())?;
        let most = limit.saturating_sub(self.page) / 8;
        let mut block = self.os.read_word(list)?;
        let mut opened = false;
        for _ in 0..most {
            if block == 0 || block < self.page || block >= limit || block % 4 != 0 {
                break;
            }
            if !opened {
                self.output(heading)?;
                self.newline()?;
                opened = true;
            }
            let line = block.wrapping_add(4);
            let length = u32::from(self.byte_at(line + 3)?);
            if self.byte_at(line)? != b'\r' || length < len32(HEADER) {
                return Err(Error::BadProgram.into());
            }
            let mut text = Vec::new();
            text.try_reserve_exact((length - len32(HEADER)) as usize)
                .map_err(|_| Error::NoRoom)?;
            text.resize((length - len32(HEADER)) as usize, 0);
            self.os.read_bytes(line + len32(HEADER), &mut text)?;
            self.output(&spelt(&text)?)?;
            self.newline()?;
            block = self.os.read_word(block)?;
            self.check_escape()?;
        }
        Ok(())
    }

    /// An `LVAR` item at the next column past the last on `row`, a line
    /// started where the row has no column left.
    fn lvar_item(&mut self, row: &mut usize, item: &[u8]) -> Step {
        if *row > 0 {
            let next = (*row / LVAR_COLUMN + 1) * LVAR_COLUMN;
            if next >= LVAR_LINE {
                self.newline()?;
                *row = 0;
            } else {
                self.output_spaces(next - *row)?;
                *row = next;
            }
        }
        self.output(item)?;
        *row += item.len();
        Ok(())
    }

    /// An `LVAR` row ended, if it has begun.
    fn lvar_end(&mut self, row: &mut usize) -> Step {
        if *row > 0 {
            *row = 0;
            self.newline()?;
        }
        Ok(())
    }

    /// The file a file command names: the command done.
    pub(crate) fn file_command(&mut self, which: u8, name: &Text) -> Step<Next> {
        match which {
            command::LOAD | command::TEXTLOAD => {
                self.load_program(name)?;
                self.mode = Mode::Prompt;
                Ok(Next::Moved)
            }
            command::SAVE => {
                self.save_program(name)?;
                Ok(Next::Done)
            }
            command::TEXTSAVE => {
                let listo = self.listo()?;
                self.text_save(name, listo)?;
                Ok(Next::Done)
            }
            _ => self.library_file(which, name),
        }
    }
}

/// A file command's flag in `LIBRARY`'s continuation payload.
pub(crate) const FILE_COMMAND: u32 = 1 << 8;

/// `LVAR`'s columns, and the line they fill.
const LVAR_COLUMN: usize = 20;
const LVAR_LINE: usize = 80;
/// The layout `LVAR` writes numbers in: nine digits' general form.
const LVAR_NUMBERS: u32 = 0x900;
/// `LVAR`'s headings, the first in TAIRiX Bridge's own words.
const LVAR_RESIDENT: &[u8] = b"Resident integers and their format:";
const LVAR_VARIABLES: &[u8] = b"Dynamic variables:";
const LVAR_PROCEDURES: &[u8] = b"Procedures:";
const LVAR_FUNCTIONS: &[u8] = b"Functions:";
const LVAR_LIBRARIES: &[u8] = b"Libraries:";
const LVAR_INSTALLED: &[u8] = b"Installed libraries:";

/// A text written into a [`Text`], too long a one refused.
struct TextOut<'t>(&'t mut Text);

impl core::fmt::Write for TextOut<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.push_bytes(s.as_bytes()).ok_or(core::fmt::Error)
    }
}

/// Tokenised `text` spelt as `LIST` spells it, a line reference as its
/// number.
pub(crate) fn spelt(text: &[u8]) -> Step<Vec<u8>> {
    let mut out = Vec::new();
    for (_, piece) in pieces(text) {
        let field;
        let bytes: &[u8] = match piece {
            Piece::Byte { ref byte, .. } => core::slice::from_ref(byte),
            Piece::Word(name) => name.as_bytes(),
            Piece::Line(number) => {
                field = number_field(number);
                &field[unpadded(&field)..]
            }
        };
        out.try_reserve(bytes.len()).map_err(|_| Error::NoRoom)?;
        out.extend_from_slice(bytes);
    }
    Ok(out)
}
