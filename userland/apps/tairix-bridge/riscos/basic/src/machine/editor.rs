//! The commands that change the program as a whole — `AUTO`, `RENUMBER`,
//! `DELETE`, `LISTO` and `CRUNCH` — with `EDIT` and `HELP`; and the
//! libraries — `LIBRARY`, `INSTALL`, `APPEND` and `OVERLAY` — with a
//! program loaded from memory.

use alloc::vec::Vec;

use tairix_openbasic::lineref;
use tairix_openbasic::program::HEADER;

use super::next::{Next, Payload};
use super::statements::{setting, Then};
use super::{len32, Machine, Mode, Step, MARGIN};
use crate::messages::Error;
use crate::token::{self, command};
use crate::value::{Text, Value};
use crate::workspace::{self as ws, reg};

/// A program's end.
const END: u8 = tairix_openbasic::program::END;
/// The highest line number.
pub(crate) const LAST_LINE: u32 = tairix_openbasic::basic5::LAST_LINE as u32;

/// The command `EDIT` ensures its editor with, as RISC OS 5's BASIC issues
/// it: the ARM BASIC Editor's module title, then the file the BBC BASIC
/// Reference Manual names, in `!System`'s modules.
const EDITOR_ENSURED: &[u8] = b"RMEnsure ARMBasicEditor 0 RMLoad System:Modules.BasicEdit";
/// The command that starts the editor.
const EDITOR_START: &[u8] = b"ARMBE";

/// A line of the program, as the commands walk it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Line {
    at: u32,
    number: u16,
    length: u32,
}

impl Machine<'_, '_> {
    /// The program's lines, in order.
    fn lines(&mut self) -> Step<Vec<Line>> {
        let mut lines = Vec::new();
        let mut at = self.page;
        loop {
            if self.byte_at(at)? != b'\r' {
                return Err(Error::BadProgram.into());
            }
            let high = self.byte_at(at + 1)?;
            if high == END {
                return Ok(lines);
            }
            let number = u16::from_be_bytes([high, self.byte_at(at + 2)?]);
            let length = u32::from(self.byte_at(at + 3)?);
            if length < len32(HEADER) {
                return Err(Error::BadProgram.into());
            }
            lines.try_reserve(1).map_err(|_| Error::NoRoom)?;
            lines.push(Line { at, number, length });
            at += length;
        }
    }

    /// A command other than the program's own: the editor's and the
    /// libraries'.
    pub(crate) fn editor_command(&mut self, b: u8) -> Step<Next> {
        match b {
            command::AUTO => self.auto_command(),
            command::RENUMBER => {
                let (start, step) = self.two_numbers(10, 10)?;
                self.renumber(start, step)?;
                Ok(Next::Done)
            }
            command::DELETE => {
                let first = self.number_constant()?.ok_or(Error::Syntax)?;
                self.expect(b',', Error::MissingComma)?;
                let last = self.number_constant()?.ok_or(Error::Syntax)?;
                self.delete_lines(first, last)?;
                Ok(Next::Done)
            }
            command::EDIT => self.edit_command(),
            command::HELP => self.help_command(),
            // `TWIN` and `TWINO` among them: RISC OS 5's BASIC has no Twin
            // to start.
            _ => Err(Error::Mistake.into()),
        }
    }

    /// A line-number constant as a command takes one: digits or a line
    /// reference, a number past the last line a syntax error.
    fn number_constant(&mut self) -> Step<Option<u32>> {
        let b = self.skip_spaces()?;
        if b == token::LINE_NUMBER {
            let bytes = [
                self.byte_at(self.ptr + 1)?,
                self.byte_at(self.ptr + 2)?,
                self.byte_at(self.ptr + 3)?,
            ];
            self.ptr += 4;
            return Ok(Some(u32::from(lineref::decode(bytes))));
        }
        if !b.is_ascii_digit() {
            return Ok(None);
        }
        let mut n: u32 = 0;
        loop {
            let digit = self.peek()?;
            if !digit.is_ascii_digit() {
                break;
            }
            n = n.saturating_mul(10).saturating_add(u32::from(digit - b'0'));
            self.ptr += 1;
        }
        if n > LAST_LINE {
            return Err(Error::Syntax.into());
        }
        Ok(Some(n))
    }

    /// `[start][,step]` as `AUTO` and `RENUMBER` take them.
    fn two_numbers(&mut self, start: u32, step: u32) -> Step<(u32, u32)> {
        let first = self.number_constant()?.unwrap_or(start);
        let second = if self.take(b',')? {
            self.number_constant()?.ok_or(Error::Syntax)?
        } else {
            step
        };
        if second == 0 {
            return Err(Error::NoStep.into());
        }
        Ok((first, second))
    }

    // AUTO.

    /// `AUTO`: each line typed numbered from its start in its steps, until
    /// Escape, an error or a number past the last.
    fn auto_command(&mut self) -> Step<Next> {
        let (start, step) = self.two_numbers(10, 10)?;
        self.os.write_word(reg::AUTO.at(), start)?;
        self.os.write_word(reg::AUTO_STEP.at(), step)?;
        self.mode = Mode::Prompt;
        Ok(Next::Moved)
    }

    // RENUMBER.

    /// `RENUMBER`: the lines numbered from `start` in `step`s, every line
    /// reference following its line; a reference to no line is reported
    /// and left.
    pub(crate) fn renumber(&mut self, start: u32, step: u32) -> Step {
        let lines = self.lines()?;
        if let Some(steps) = lines.len().checked_sub(1) {
            let last = u64::from(start) + steps as u64 * u64::from(step);
            if last > u64::from(LAST_LINE) {
                return Err(Error::RenumberTooHigh.into());
            }
        }
        let mut map = Vec::new();
        map.try_reserve_exact(lines.len())
            .map_err(|_| Error::NoRenumberRoom)?;
        for (i, line) in (0..).zip(&lines) {
            let new = u16::try_from(start + i * step).map_err(|_| Error::RenumberTooHigh)?;
            map.push((line.number, new));
        }
        for (line, &(_, new)) in lines.iter().zip(&map) {
            self.renumber_references(line, new, &map)?;
        }
        for (line, &(_, new)) in lines.iter().zip(&map) {
            self.os.write_bytes(line.at + 1, &new.to_be_bytes())?;
        }
        Ok(())
    }

    /// The line references in `line`, renumbered `new`, changed to their
    /// lines' new numbers.
    fn renumber_references(&mut self, line: &Line, new: u16, map: &[(u16, u16)]) -> Step {
        let mut at = line.at + len32(HEADER);
        let end = line.at + line.length;
        let mut quoted = false;
        while at < end {
            let b = self.byte_at(at)?;
            match b {
                b'"' => quoted = !quoted,
                token::REM | token::DATA if !quoted => return Ok(()),
                token::LINE_NUMBER if !quoted && at + 3 < end => {
                    let bytes = [
                        self.byte_at(at + 1)?,
                        self.byte_at(at + 2)?,
                        self.byte_at(at + 3)?,
                    ];
                    let old = lineref::decode(bytes);
                    match map.binary_search_by_key(&old, |&(old, _)| old) {
                        Ok(index) => {
                            let new = lineref::encode(map[index].1);
                            self.os.write_bytes(at + 1, &new)?;
                        }
                        Err(_) => self.renumber_failed(old, new)?,
                    }
                    at += 3;
                }
                _ => {}
            }
            at += 1;
        }
        Ok(())
    }

    /// "Failed with `old` on line `line`": a reference `RENUMBER` could not
    /// follow, in the line it renumbered `line`.
    fn renumber_failed(&mut self, old: u16, line: u16) -> Step {
        self.output(b"Failed with ")?;
        self.output(super::functions::decimal(i32::from(old)).as_bytes())?;
        self.output(b" on line ")?;
        self.output(super::functions::decimal(i32::from(line)).as_bytes())?;
        self.newline()
    }

    // DELETE.

    /// `DELETE first, last`: the lines from `first` to `last` gone.
    fn delete_lines(&mut self, first: u32, last: u32) -> Step {
        if first > last {
            return Ok(());
        }
        let lines = self.lines()?;
        let mut doomed = lines
            .iter()
            .filter(|l| (first..=last).contains(&u32::from(l.number)));
        let Some(from) = doomed.next() else {
            return Ok(());
        };
        let to = doomed.next_back().unwrap_or(from);
        let start = from.at;
        let stop = to.at + to.length;
        let tail = self.top.checked_sub(stop).ok_or(Error::BadProgram)?;
        self.os.copy_bytes(stop, start, tail)?;
        self.set_top(self.top - (stop - start))?;
        self.clear()
    }

    // LISTO and CRUNCH.

    /// `LISTO`'s or `CRUNCH`'s number, as `which` says.
    pub(crate) fn command_setting(&mut self, which: u32, value: &Value) -> Step<Next> {
        let n = self.int_value(value)?;
        match which {
            setting::LISTO => {
                if !(0..32).contains(&n) {
                    return Err(Error::BadListo.into());
                }
                self.os.write_word(reg::LISTO.at(), n.cast_unsigned())?;
            }
            setting::CRUNCH => self.crunch(n.cast_unsigned())?,
            _ => return Err(Error::StackCorrupt.into()),
        }
        Ok(Next::Done)
    }

    /// `CRUNCH`: spaces, `REM`s, empty statements and empty lines stripped
    /// as its bits ask, a line at a time.
    pub(crate) fn crunch(&mut self, bits: u32) -> Step {
        let lines = self.lines()?;
        let mut out = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let mut text = [0; 256];
            let n = (line.length as usize) - HEADER;
            self.os
                .read_bytes(line.at + len32(HEADER), &mut text[..n])?;
            let crunched = crunch_line(&text[..n], bits, index == 0);
            if bits & CRUNCH_EMPTY_LINES != 0 && crunched.is_empty() {
                continue;
            }
            out.try_reserve(crunched.len() + HEADER)
                .map_err(|_| Error::NoRoom)?;
            let [high, low] = line.number.to_be_bytes();
            let length = u8::try_from(crunched.len() + HEADER).map_err(|_| Error::LineTooLong)?;
            out.extend_from_slice(&[b'\r', high, low, length]);
            out.extend_from_slice(&crunched);
        }
        out.try_reserve(2).map_err(|_| Error::NoRoom)?;
        out.extend_from_slice(&[b'\r', END]);
        self.os.write_bytes(self.page, &out)?;
        self.set_top(self.page + len32(out.len()))?;
        self.clear()
    }

    // Libraries.

    /// A file a command or statement names: `LOAD`, `SAVE`, `TEXTLOAD`,
    /// `TEXTSAVE`, `APPEND` and `INSTALL` as commands, `LIBRARY` and
    /// `INSTALL` as statements.
    pub(crate) fn library(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let name = super::functions::string_of(value)?;
        let word = payload.word(0);
        let [which, ..] = word.to_le_bytes();
        if word & super::program::FILE_COMMAND != 0 {
            return self.file_command(which, name);
        }
        if word != 0 {
            self.install(name)?;
        } else {
            self.load_library(name)?;
        }
        Ok(Next::Done)
    }

    /// `APPEND`, and `INSTALL` as a command.
    pub(crate) fn library_file(&mut self, which: u8, name: &Text) -> Step<Next> {
        match which {
            command::APPEND => self.append(name)?,
            command::INSTALL => self.install(name)?,
            _ => return Err(Error::Mistake.into()),
        }
        Ok(Next::Done)
    }

    /// A library's file loaded at `at`: its length, refused where it is not
    /// a tokenised program.
    fn library_at(&mut self, name: &Text, at: u32, room: u32) -> Step<u32> {
        let (length, _) = self.file_length(name)?;
        if length.saturating_add(4) > room {
            return Err(Error::NoLibraryRoom.into());
        }
        self.load_file_at(name, at + 4)?;
        let end = self.program_extent(at + 4).map_err(|_| Error::BadLibrary)?;
        Ok(end - at)
    }

    /// `LIBRARY`: a library loaded into the heap and linked first among
    /// the transient ones.
    fn load_library(&mut self, name: &Text) -> Step {
        let room = self.sp.saturating_sub(self.fsa).saturating_sub(MARGIN);
        let at = self.fsa;
        let size = self.library_at(name, at, room)?.next_multiple_of(4);
        let at = self.allocate(size, Error::NoLibraryRoom)?;
        let next = self.os.read_word(ws::LIBRARYLIST.at())?;
        self.os.write_word(at, next)?;
        self.os.write_word(ws::LIBRARYLIST.at(), at)?;
        Ok(())
    }

    /// `INSTALL`: a library loaded below `HIMEM`, which it lowers, and
    /// linked first among the installed ones; only with nothing on the
    /// stack.
    fn install(&mut self, name: &Text) -> Step {
        if self.sp != self.himem
            && self
                .top_header()?
                .is_some_and(|h| h.kind != crate::stack::Kind::Guest)
        {
            return Err(Error::CannotInstall.into());
        }
        let (length, _) = self.file_length(name)?;
        let size = length
            .checked_add(4)
            .and_then(|bytes| bytes.checked_next_multiple_of(4))
            .ok_or(Error::NoLibraryRoom)?;
        let held = self.sp;
        let sp = held
            .checked_sub(size)
            .filter(|&sp| sp >= self.fsa.saturating_add(MARGIN))
            .ok_or(Error::NoLibraryRoom)?;
        let at = self.himem - size;
        // The stack moves below the library first, as the library's room is
        // the top of the stack's.
        let in_use = self.himem - held;
        self.os.copy_bytes(held, sp, in_use)?;
        self.sp = sp;
        if let Err(failed) = self.library_at(name, at, size) {
            self.os.copy_bytes(sp, held, in_use)?;
            self.sp = held;
            return Err(failed);
        }
        let next = self.os.read_word(ws::INSTALLLIST.at())?;
        self.os.write_word(at, next)?;
        self.os.write_word(ws::INSTALLLIST.at(), at)?;
        self.himem = at;
        self.os.write_word(ws::HIMEM.at(), at)?;
        Ok(())
    }

    /// `APPEND`: a program's lines added after the last, numbered on from
    /// it.
    fn append(&mut self, name: &Text) -> Step {
        let last = self.lines()?.last().map_or(0, |line| line.number);
        let (length, _) = self.file_length(name)?;
        let staging = length
            .checked_next_multiple_of(4)
            .and_then(|bytes| bytes.checked_add(MARGIN))
            .and_then(|room| self.himem.checked_sub(room))
            .filter(|&at| at >= self.top)
            .ok_or(Error::NoRoom)?;
        self.load_file_at(name, staging)?;
        self.program_extent(staging)
            .map_err(|_| Error::BadProgram)?;
        let mut line = staging;
        let mut number = last;
        let mut incoming = Vec::new();
        loop {
            if self.byte_at(line + 1)? == END {
                break;
            }
            let length = u32::from(self.byte_at(line + 3)?);
            let mut text = [0; 256];
            let n = (length as usize) - HEADER;
            self.os.read_bytes(line + len32(HEADER), &mut text[..n])?;
            number = number
                .checked_add(10)
                .filter(|&n| u32::from(n) <= LAST_LINE)
                .ok_or(Error::RenumberTooHigh)?;
            incoming.try_reserve(1).map_err(|_| Error::NoRoom)?;
            incoming.push((number, text, n));
            line += length;
        }
        for (number, text, n) in &incoming {
            self.store_line(*number, Some(&text[..*n]))?;
        }
        self.clear()
    }

    /// `OVERLAY a$()`: the overlay structure set up from the array of file
    /// names, its room the longest file's.
    pub(crate) fn overlay_statement(&mut self) -> Step<Next> {
        self.skip_spaces()?;
        let name = self.read_name()?.ok_or(Error::NeedStringArray)?;
        if !name.is_array() || name.kind() != super::vars::Type::Str {
            return Err(Error::NeedStringArray.into());
        }
        self.expect(b')', Error::MissingBracket)?;
        let lvalue = self.variable(&name, false)?.ok_or(Error::UnknownArray)?;
        let shape = self.shape(lvalue.address, super::lvalue::Element::Str)?;
        let mut largest = 0;
        for i in 0..shape.total() {
            let sib = self.element_at(&shape, i).address;
            let file = self.read_sib(sib)?;
            let (length, _) = self.file_length(&file)?;
            largest = largest.max(length);
        }
        let room = largest
            .checked_add(4)
            .and_then(|bytes| bytes.checked_next_multiple_of(4))
            .ok_or(Error::NoLibraryRoom)?;
        let structure = self.allocate(
            room.checked_add(16).ok_or(Error::NoLibraryRoom)?,
            Error::NoLibraryRoom,
        )?;
        let first = self.os.read_word(lvalue.address)?;
        self.os.write_word(structure, first)?;
        self.os.write_word(structure + 4, u32::MAX)?;
        self.os.write_word(structure + 8, room)?;
        self.os.write_word(structure + 12, structure + 16)?;
        self.os.write_word(ws::OVERPTR.at(), structure)?;
        Ok(Next::Done)
    }

    /// A program held in memory from `start` to `end`, copied to `PAGE`:
    /// `*BASIC @start,end`.
    pub(crate) fn load_memory(&mut self, start: u32, end: u32) -> Step {
        let length = end - start;
        if self.page.saturating_add(length).saturating_add(MARGIN) > self.himem {
            return Err(Error::NoRoom.into());
        }
        let bytes = self.os.read_vec(start, length as usize)?;
        let staging = self.himem - MARGIN / 2 - length.next_multiple_of(4);
        self.os.write_bytes(staging, &bytes)?;
        self.load_staged(staging, length)
    }

    // The screen editor and help.

    /// `EDIT`: the ARM BASIC Editor, a module of its own, loaded where it is
    /// not resident and started. The rest of the line is the editor's to
    /// read, as genuine BASIC passes it nothing.
    fn edit_command(&mut self) -> Step<Next> {
        self.skip_line()?;
        let ensured = Text::of(EDITOR_ENSURED).ok_or(Error::StringTooLong)?;
        self.command_then(&ensured, Then::StartEditor)
    }

    /// The ARM BASIC Editor started, `EDIT`'s module ensured.
    pub(crate) fn start_editor(&mut self) -> Step<Next> {
        let start = Text::of(EDITOR_START).ok_or(Error::StringTooLong)?;
        self.command_text(&start)
    }
}

/// `CRUNCH`'s bits.
const CRUNCH_LEADING: u32 = 1;
const CRUNCH_INSIDE: u32 = 1 << 1;
const CRUNCH_REMS: u32 = 1 << 2;
const CRUNCH_EMPTY_STATEMENTS: u32 = 1 << 3;
const CRUNCH_EMPTY_LINES: u32 = 1 << 4;

/// One line's text crunched as `bits` asks, as genuine BASIC's is recorded;
/// its `REM`s kept where it is the program's first. A statement's leading
/// spaces go with bit 0 and the rest outside strings with bit 1; bit 2 cuts
/// a `REM` to the line's end, leaving what stood before it; bit 3 drops a
/// statement left empty and its colon.
fn crunch_line(text: &[u8], bits: u32, first: bool) -> Vec<u8> {
    let drop_rems = bits & CRUNCH_REMS != 0 && !first;
    let drop_empty = bits & CRUNCH_EMPTY_STATEMENTS != 0;
    let mut out = Vec::with_capacity(text.len());
    for statement in statements(text) {
        let body = crunched(text, &statement, bits, drop_rems);
        if statement.colon {
            if !(drop_empty && body.is_empty()) {
                out.extend_from_slice(&body);
                out.push(b':');
            }
        } else if drop_empty && body.is_empty() {
            if out.last() == Some(&b':') {
                out.pop();
            }
        } else {
            out.extend_from_slice(&body);
        }
    }
    out
}

/// A statement of a line: its bytes, whether a colon follows them, and
/// where the `REM`, `DATA` or `*` that runs it to the line's end starts.
struct Statement {
    start: usize,
    end: usize,
    colon: bool,
    rest: Option<usize>,
}

/// `text`'s statements, each begun after a colon, `THEN` or `ELSE`, outside
/// strings; a `THEN` or `ELSE` ends the statement it closes.
fn statements(text: &[u8]) -> Vec<Statement> {
    let mut list = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut i = 0;
    while i < text.len() {
        let b = text[i];
        if quoted {
            quoted = b != b'"';
            i += 1;
            continue;
        }
        let rest_of_line = b == token::REM
            || b == token::DATA
            || (b == b'*' && text[start..i].iter().all(|&c| c == b' '));
        if rest_of_line {
            list.push(Statement {
                start,
                end: text.len(),
                colon: false,
                rest: Some(i),
            });
            return list;
        }
        match b {
            b'"' => quoted = true,
            token::LINE_NUMBER => {
                i = (i + 4).min(text.len());
                continue;
            }
            b':' => {
                list.push(Statement {
                    start,
                    end: i,
                    colon: true,
                    rest: None,
                });
                start = i + 1;
            }
            token::THEN | token::ELSE => {
                list.push(Statement {
                    start,
                    end: i + 1,
                    colon: false,
                    rest: None,
                });
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    list.push(Statement {
        start,
        end: text.len(),
        colon: false,
        rest: None,
    });
    list
}

/// `statement` crunched as `bits` asks, its `REM` cut where `drop_rems`
/// says: what a `REM`, `DATA` or `*` runs to is kept whole.
fn crunched(text: &[u8], statement: &Statement, bits: u32, drop_rems: bool) -> Vec<u8> {
    let verbatim = statement.rest.unwrap_or(statement.end);
    let end = if drop_rems && statement.rest.is_some_and(|at| text[at] == token::REM) {
        verbatim
    } else {
        statement.end
    };
    let bytes = &text[statement.start..end];
    let leading = bytes.iter().take_while(|&&b| b == b' ').count();
    let mut out = Vec::with_capacity(bytes.len());
    if bits & CRUNCH_LEADING == 0 {
        out.extend_from_slice(&bytes[..leading]);
    }
    let mut quoted = false;
    let mut i = statement.start + leading;
    while i < verbatim {
        let b = text[i];
        if b == token::LINE_NUMBER && !quoted {
            let after = (i + 4).min(verbatim);
            out.extend_from_slice(&text[i..after]);
            i = after;
            continue;
        }
        quoted ^= b == b'"';
        if quoted || b != b' ' || bits & CRUNCH_INSIDE == 0 {
            out.push(b);
        }
        i += 1;
    }
    out.extend_from_slice(&text[verbatim..end]);
    out
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::crunch_line;
    use crate::token;

    /// Each line of the recorded program, as tokenised, `#` its `REM`, `%`
    /// its `PRINT`, `D` its `DATA`, `I` its `IF` and `T` its `THEN`.
    const LINES: [&[u8]; 6] = [
        b" # first",
        b"   A = 1 :  : B$ = \" x \" : # note",
        b" ",
        b" :",
        b" % A : D 1, 2",
        b" I A T % : # x",
    ];

    fn tokens(text: &[u8]) -> Vec<u8> {
        text.iter()
            .map(|&b| match b {
                b'#' => token::REM,
                b'%' => token::PRINT,
                b'D' => token::DATA,
                b'I' => token::IF,
                b'T' => token::THEN,
                other => other,
            })
            .collect()
    }

    #[test]
    fn crunch_strips_what_each_bit_names_as_recorded() {
        let cases: [(u32, [&[u8]; 6]); 7] = [
            (
                1,
                [
                    b"# first",
                    b"A = 1 ::B$ = \" x \" :# note",
                    b"",
                    b":",
                    b"% A :D 1, 2",
                    b"I A T% :# x",
                ],
            ),
            (
                2,
                [
                    b" # first",
                    b"   A=1:  : B$=\" x \": # note",
                    b" ",
                    b" :",
                    b" %A: D 1, 2",
                    b" IAT %: # x",
                ],
            ),
            (
                4,
                [
                    b" # first",
                    b"   A = 1 :  : B$ = \" x \" : ",
                    b" ",
                    b" :",
                    b" % A : D 1, 2",
                    b" I A T % : ",
                ],
            ),
            (
                8,
                [
                    b" # first",
                    b"   A = 1 :  : B$ = \" x \" : # note",
                    b" ",
                    b" ",
                    b" % A : D 1, 2",
                    b" I A T % : # x",
                ],
            ),
            (
                3,
                [
                    b"# first",
                    b"A=1::B$=\" x \":# note",
                    b"",
                    b":",
                    b"%A:D 1, 2",
                    b"IAT%:# x",
                ],
            ),
            (
                15,
                [
                    b"# first",
                    b"A=1:B$=\" x \"",
                    b"",
                    b"",
                    b"%A:D 1, 2",
                    b"IAT%",
                ],
            ),
            (16, LINES),
        ];
        for (bits, wanted) in cases {
            for (n, (line, want)) in LINES.iter().zip(wanted).enumerate() {
                assert_eq!(
                    crunch_line(&tokens(line), bits, n == 0),
                    tokens(want),
                    "CRUNCH {bits}, line {n}"
                );
            }
        }
    }
}
