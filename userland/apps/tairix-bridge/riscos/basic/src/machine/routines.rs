//! Procedures and functions: the catalogue entry each has once first called,
//! as BASIC V lays it — the next entry, the name after `PROC` or `FN`, each
//! parameter's l-value and type word, a zero, the body's address and -1 —
//! the call that binds the arguments to the parameters, saving each one's
//! value as `LOCAL` does, and the return that puts them back.

use alloc::vec::Vec;

use super::lvalue::{Lvalue, Place};
use super::next::{Delivered, Next, Payload};
use super::statements::cont;
use super::vars::{in_name, Found};
use super::{len32, Machine, Mode, Step};
use crate::messages::Error;
use crate::stack::Kind;
use crate::token;
use crate::value::{Text, Value};
use crate::workspace::{self as ws, FN_HEAD, PROC_HEAD};

/// A routine's kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Routine {
    /// `PROC`.
    Proc,
    /// `FN`.
    Fn,
}

impl Routine {
    const fn head(self) -> u8 {
        match self {
            Self::Proc => PROC_HEAD,
            Self::Fn => FN_HEAD,
        }
    }

    const fn token(self) -> u8 {
        match self {
            Self::Proc => token::PROC,
            Self::Fn => token::FN,
        }
    }

    const fn frame(self) -> Kind {
        match self {
            Self::Proc => Kind::Proc,
            Self::Fn => Kind::Fn,
        }
    }
}

/// What a parameter type word's low half, the parameter's own type, has
/// added for a `RETURN` parameter, as recorded; the bit no type has alone.
const RETURNED: u32 = 0x500;
const RETURNED_MARK: u32 = 0x400;
/// The low half's flag for an array.
const ARRAY: u32 = 0x8000;

/// A parameter: its l-value, and whether it returns its value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Parameter {
    pub(crate) lvalue: Lvalue,
    pub(crate) returned: bool,
}

/// The type word an entry keeps for a parameter kept as `place`.
fn type_word(place: Place, returned: bool, dialect: crate::value::Dialect) -> u32 {
    let own = match place {
        Place::Array(element) => ARRAY | 0x100 | element.place().call_type(dialect),
        other => other.call_type(dialect),
    };
    let own = if returned { own | RETURNED } else { own };
    let save = match place {
        Place::Array(_) => 4,
        other => other.call_type(dialect),
    };
    save << 16 | own
}

/// The place and return a type word names. An array's own type has
/// [`RETURNED`]'s low bit already, so its high bit alone marks a return.
fn of_type_word(word: u32, dialect: crate::value::Dialect) -> Option<(Place, bool)> {
    let own = word & 0xFFFF;
    let returned = own & RETURNED_MARK != 0;
    let base = match (returned, own & ARRAY != 0) {
        (false, _) => own,
        (true, true) => own & !RETURNED_MARK,
        (true, false) => own & !RETURNED,
    };
    let place = if base & ARRAY != 0 {
        match Place::of_call_type(base & !ARRAY, dialect)? {
            place @ Place::Array(_) => place,
            _ => return None,
        }
    } else {
        Place::of_call_type(base, dialect)?
    };
    Some((place, returned))
}

/// The most parameters a routine takes: as many as a line could name.
const PARAMETERS_MOST: usize = 127;

/// An array pointer made `LOCAL` but not yet dimensioned, below the least a
/// dimensioned one holds: its `DIM` takes its block from the stack.
pub(crate) const LOCAL_ARRAY: u32 = 1;

impl Machine<'_, '_> {
    /// A call of the `routine` named by the program's text at `start` for
    /// `length` bytes, its arguments read from the pointer; it returns to
    /// `return_to`, or where its arguments end.
    pub(crate) fn begin_call(
        &mut self,
        routine: Routine,
        start: u32,
        length: u32,
        return_to: Option<u32>,
    ) -> Step<Next> {
        let entry = self.routine_entry(routine, start, length)?;
        let count = self.parameter_count(entry)?;
        let payload = Payload::of(&[routine as u32, entry, 0, count, return_to.unwrap_or(0)]);
        if self.peek()? == b'(' {
            self.ptr += 1;
            if count == 0 {
                return Err(Error::BadArguments.into());
            }
            return self.next_argument(payload);
        }
        if count != 0 {
            return Err(Error::BadArguments.into());
        }
        self.call(routine, entry, 0, return_to)
    }

    /// The argument `payload`'s count of them says comes next, read as its
    /// parameter wants: a value, or an l-value to return to.
    fn next_argument(&mut self, payload: Payload) -> Step<Next> {
        let entry = payload.word(1);
        let index = payload.word(2);
        let (_, place, returned) = self.parameter(entry, index)?;
        if returned || matches!(place, Place::Array(_)) {
            return self.lvalue(cont::ARGUMENT, payload);
        }
        self.evaluate(cont::ARGUMENT, payload)
    }

    /// An argument delivered: kept on the stack, then the next one or the
    /// call.
    pub(crate) fn argument(&mut self, payload: Payload, delivered: &Delivered) -> Step<Next> {
        let routine = if payload.word(0) == Routine::Proc as u32 {
            Routine::Proc
        } else {
            Routine::Fn
        };
        let entry = payload.word(1);
        let index = payload.word(2);
        let count = payload.word(3);
        if index >= count || count as usize > PARAMETERS_MOST {
            return Err(Error::StackCorrupt.into());
        }
        match *delivered {
            Delivered::Value(ref value) => self.push_value(value)?,
            Delivered::Lvalue(lvalue) => {
                let place = lvalue.place.call_type(self.dialect());
                self.push(Kind::Local, 0, &[lvalue.address, place])?;
            }
        }
        let index = index + 1;
        if index < count {
            self.expect(b',', Error::BadArguments)?;
            return self.next_argument(payload.with(2, index));
        }
        self.expect(b')', Error::MissingBracket)?;
        let return_to = Some(payload.word(4)).filter(|&at| at != 0);
        self.call(routine, entry, count, return_to)
    }

    /// The catalogue entry of the routine named at `start`, made from its
    /// `DEF` the first time it is called.
    fn routine_entry(&mut self, routine: Routine, start: u32, length: u32) -> Step<u32> {
        let mut name = [0; super::NAME_MOST];
        let n = length as usize;
        let name = name.get_mut(..n).ok_or(Error::BadCall)?;
        self.os.read_bytes(start, name)?;
        let head = Self::head(routine.head())?;
        match self.find(head, name)? {
            Found::Entry(entry) => Ok(entry),
            Found::Missing(link) => self.define(routine, name, link),
        }
    }

    /// The entry made for the routine `name` from its `DEF`, linked from
    /// `link`: its parameters made variables, as BASIC V makes them.
    fn define(&mut self, routine: Routine, name: &[u8], link: u32) -> Step<u32> {
        let def = self.find_def(routine, name)?.ok_or(Error::NoSuchRoutine)?;
        let saved = (self.ptr, self.line);
        self.ptr = def.after_name;
        self.line = def.line;
        let parameters = self.read_parameters();
        let body = self.ptr;
        self.ptr = saved.0;
        self.line = saved.1;
        let parameters = parameters?;
        let rest = len32(name.len());
        let name_words = (4 + rest + 1).next_multiple_of(4);
        let size = name_words + 8 * len32(parameters.len()) + 12;
        let entry = self.allocate(size, Error::NoRoom)?;
        self.os.fill(entry, size, 0)?;
        self.os.write_bytes(entry + 4, name)?;
        let mut at = entry + name_words;
        let dialect = self.dialect();
        for parameter in &parameters {
            self.os.write_word(at, parameter.lvalue.address)?;
            self.os.write_word(
                at + 4,
                type_word(parameter.lvalue.place, parameter.returned, dialect),
            )?;
            at += 8;
        }
        self.os.write_word(at, 0)?;
        self.os.write_word(at + 4, body)?;
        self.os.write_word(at + 8, u32::MAX)?;
        self.os.write_word(link, entry)?;
        Ok(entry)
    }

    /// The parameters after a `DEF`'s name at the pointer, each made a
    /// variable: the pointer left at the body.
    fn read_parameters(&mut self) -> Step<Vec<Parameter>> {
        let mut parameters = Vec::new();
        if self.skip_spaces()? != b'(' {
            return Ok(parameters);
        }
        self.ptr += 1;
        loop {
            let returned = self.skip_spaces()? == token::RETURN;
            if returned {
                self.ptr += 1;
                self.skip_spaces()?;
            }
            let name = self.read_name()?.ok_or(Error::BadArguments)?;
            let lvalue = if name.is_array() {
                self.expect(b')', Error::BadArguments)?;
                self.variable(&name, true)?
            } else {
                self.variable(&name, true)?
            }
            .ok_or(Error::BadArguments)?;
            if parameters.len() >= PARAMETERS_MOST {
                return Err(Error::BadArguments.into());
            }
            parameters.try_reserve(1).map_err(|_| Error::NoRoom)?;
            parameters.push(Parameter { lvalue, returned });
            if self.take(b')')? {
                return Ok(parameters);
            }
            self.expect(b',', Error::BadArguments)?;
        }
    }

    /// Where an entry's parameters begin: past its link and its name.
    fn parameters_at(&mut self, entry: u32) -> Step<u32> {
        let mut at = entry.wrapping_add(4);
        for _ in 0..super::NAME_MOST {
            let b = self.os.read_byte(at)?;
            at += 1;
            if b == 0 {
                return Ok(at.next_multiple_of(4));
            }
        }
        Err(Error::StackCorrupt.into())
    }

    /// How many parameters the entry at `entry` lists.
    fn parameter_count(&mut self, entry: u32) -> Step<u32> {
        let mut at = self.parameters_at(entry)?;
        let mut count = 0;
        while self.os.read_word(at)? != 0 {
            count += 1;
            at += 8;
            if count as usize > PARAMETERS_MOST {
                return Err(Error::StackCorrupt.into());
            }
        }
        Ok(count)
    }

    /// The `index`th parameter of the entry at `entry`: its l-value's
    /// address, its place and whether it returns.
    fn parameter(&mut self, entry: u32, index: u32) -> Step<(u32, Place, bool)> {
        let at = self.parameter_at(entry, index)?;
        let address = self.os.read_word(at)?;
        let word = self.os.read_word(at + 4)?;
        let (place, returned) = of_type_word(word, self.dialect()).ok_or(Error::StackCorrupt)?;
        Ok((address, place, returned))
    }

    /// The body an entry names.
    fn body(&mut self, entry: u32, count: u32) -> Step<u32> {
        let at = self.parameter_at(entry, count)?;
        Ok(self.os.read_word(at.wrapping_add(4))?)
    }

    /// Where the entry at `entry` keeps its `index`th parameter, or past its
    /// last the word before its body.
    fn parameter_at(&mut self, entry: u32, index: u32) -> Step<u32> {
        if index as usize > PARAMETERS_MOST {
            return Err(Error::StackCorrupt.into());
        }
        Ok(self.parameters_at(entry)?.wrapping_add(8 * index))
    }

    /// The routine called: its `count` arguments on top bound to its
    /// parameters, each parameter's value saved first, and its body run, to
    /// return to `return_to` or the pointer.
    fn call(
        &mut self,
        routine: Routine,
        entry: u32,
        count: u32,
        return_to: Option<u32>,
    ) -> Step<Next> {
        let mut arguments: Vec<Argument> = Vec::new();
        arguments
            .try_reserve(count as usize)
            .map_err(|_| Error::NoRoom)?;
        for _ in 0..count {
            let header = self.top_header()?.ok_or(Error::StackCorrupt)?;
            let argument = if header.kind == Kind::Local {
                let mut words = [0; 2];
                self.pop_record(Kind::Local, &mut words)?;
                let place =
                    Place::of_call_type(words[1], self.dialect()).ok_or(Error::StackCorrupt)?;
                Argument::Lvalue(Lvalue {
                    address: words[0],
                    place,
                })
            } else {
                Argument::Value(self.pop_value()?)
            };
            arguments.push(argument);
        }
        arguments.reverse();
        let body = self.body(entry, count)?;
        let def_line = self.line_of(body)?;
        let back = return_to.unwrap_or(self.ptr);
        self.push(
            routine.frame(),
            0,
            &[back, self.line, self.flags & super::flag::RUNNING],
        )?;
        for (index, argument) in (0..).zip(&arguments) {
            let (address, place, returned) = self.parameter(entry, index)?;
            let formal = Lvalue { address, place };
            match (argument, place) {
                (Argument::Lvalue(actual), Place::Array(element)) => {
                    let Place::Array(actual_element) = actual.place else {
                        return Err(Error::BadArrayArgument.into());
                    };
                    if actual_element != element {
                        return Err(Error::ArrayParameterType.into());
                    }
                    let pointer = self.os.read_word(actual.address)?;
                    self.save_local(formal, None)?;
                    self.os.write_word(formal.address, pointer)?;
                }
                (Argument::Lvalue(actual), _) if returned => {
                    let value = self.read(*actual)?;
                    self.save_local(formal, Some(*actual))?;
                    self.write(formal, &value)?;
                }
                (Argument::Value(value), _) if !returned => {
                    self.save_local(formal, None)?;
                    self.write(formal, value)?;
                }
                _ => return Err(Error::BadReturnArgument.into()),
            }
        }
        self.ptr = body;
        self.line = def_line;
        self.flags |= super::flag::RUNNING;
        self.mode = Mode::Statement;
        Ok(Next::Moved)
    }

    /// The value of `formal` saved on the stack, as `LOCAL` saves it, and it
    /// emptied; `returned`, an argument to return its value to on the way
    /// back.
    pub(crate) fn save_local(&mut self, formal: Lvalue, returned: Option<Lvalue>) -> Step {
        let dialect = self.dialect();
        let (actual, actual_place) =
            returned.map_or((0, u32::MAX), |a| (a.address, a.place.call_type(dialect)));
        let place = formal.place.call_type(dialect);
        let mut saved = [0; 2];
        match formal.place {
            Place::Int | Place::Array(_) => saved[0] = self.os.read_word(formal.address)?,
            Place::Real => {
                let mut bytes = [0; 8];
                let n = dialect.real_bytes() as usize;
                self.os.read_bytes(formal.address, &mut bytes[..n])?;
                saved[0] = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                saved[1] = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
            }
            Place::Sib => {
                saved[0] = self.os.read_word(formal.address)?;
                saved[1] = u32::from(self.os.read_byte(formal.address + 4)?);
            }
            Place::Byte | Place::Dollar => return Err(Error::BadArguments.into()),
        }
        self.push(
            Kind::Local,
            1,
            &[
                formal.address,
                place,
                saved[0],
                saved[1],
                actual,
                actual_place,
            ],
        )?;
        match formal.place {
            Place::Sib => {
                self.os.write_word(formal.address, 0)?;
                self.os.write_byte(formal.address + 4, 0)?;
            }
            Place::Array(_) => self.os.write_word(formal.address, LOCAL_ARRAY)?,
            _ => {
                let zero = Value::Int(0);
                self.write(formal, &zero)?;
            }
        }
        Ok(())
    }

    /// The saved value on top put back, then a returned parameter's value
    /// given to its argument: after, so an argument that is the parameter's
    /// own variable keeps what the routine left in it, as recorded.
    pub(crate) fn restore_local(&mut self) -> Step {
        let mut words = [0; 6];
        self.pop_record(Kind::Local, &mut words)?;
        let dialect = self.dialect();
        let place = Place::of_call_type(words[1], dialect).ok_or(Error::StackCorrupt)?;
        let formal = Lvalue {
            address: words[0],
            place,
        };
        let returned = if words[5] == u32::MAX {
            None
        } else {
            let actual_place = Place::of_call_type(words[5], dialect).ok_or(Error::StackCorrupt)?;
            let actual = Lvalue {
                address: words[4],
                place: actual_place,
            };
            Some((actual, self.read(formal)?))
        };
        self.put_back(formal, words[2], words[3])?;
        if let Some((actual, value)) = returned {
            self.write(actual, &value)?;
        }
        Ok(())
    }

    /// `formal`'s value saved as `first` and `second` put back.
    fn put_back(&mut self, formal: Lvalue, first: u32, second: u32) -> Step {
        match formal.place {
            Place::Int | Place::Array(_) => self.os.write_word(formal.address, first)?,
            Place::Real => {
                let saved = (u64::from(second) << 32 | u64::from(first)).to_le_bytes();
                let n = self.dialect().real_bytes() as usize;
                self.os.write_bytes(formal.address, &saved[..n])?;
            }
            Place::Sib => {
                self.assign_sib(formal.address, &Text::EMPTY)?;
                self.os.write_word(formal.address, first)?;
                self.os
                    .write_byte(formal.address + 4, (second & 0xFF) as u8)?;
            }
            Place::Byte | Place::Dollar => return Err(Error::StackCorrupt.into()),
        }
        Ok(())
    }

    /// The routine's return: every record above its frame unwound, each
    /// saved value put back, then its frame popped and its caller's place
    /// taken up again.
    pub(crate) fn return_from(&mut self, routine: Routine) -> Step {
        loop {
            let header = self.top_header()?.ok_or(match routine {
                Routine::Proc => Error::NotInProcedure,
                Routine::Fn => Error::NotInFunction,
            })?;
            match header.kind {
                Kind::Local if header.aux == 1 => self.restore_local()?,
                Kind::LocalError => self.restore_local_error()?,
                Kind::LocalData => self.restore_local_data()?,
                kind if kind == routine.frame() => break,
                Kind::Proc | Kind::Fn | Kind::Expression | Kind::Continuation => {
                    return Err(match routine {
                        Routine::Proc => Error::NotInProcedure,
                        Routine::Fn => Error::NotInFunction,
                    }
                    .into())
                }
                _ => {
                    self.drop_top()?;
                }
            }
        }
        let mut words = [0; 3];
        self.pop_record(routine.frame(), &mut words)?;
        self.ptr = words[0];
        self.line = words[1];
        self.flags = (self.flags & !super::flag::RUNNING) | (words[2] & super::flag::RUNNING);
        Ok(())
    }

    /// The `DEF` of `routine` `name`, in the program or a library.
    fn find_def(&mut self, routine: Routine, name: &[u8]) -> Step<Option<Def>> {
        if let Some(def) = self.find_def_in(self.page, routine, name)? {
            return Ok(Some(def));
        }
        for list in [ws::INSTALLLIST.at(), ws::LIBRARYLIST.at()] {
            let mut library = self.os.read_word(list)?;
            let mut hops = 0;
            while library != 0 && hops < LIBRARIES_MOST {
                if let Some(def) = self.find_def_in(library.wrapping_add(4), routine, name)? {
                    return Ok(Some(def));
                }
                library = self.os.read_word(library)?;
                hops += 1;
            }
        }
        Ok(None)
    }

    /// The `DEF` of `routine` `name` among the lines from `start`.
    fn find_def_in(&mut self, start: u32, routine: Routine, name: &[u8]) -> Step<Option<Def>> {
        let mut line = start;
        for _ in 0..LINES_MOST {
            if self.byte_at(line)? != b'\r' {
                return Err(Error::BadProgram.into());
            }
            if self.byte_at(line + 1)? == tairix_openbasic::program::END {
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
            if self.byte_at(at)? == token::DEF {
                at += 1;
                while self.byte_at(at)? == b' ' {
                    at += 1;
                }
                if self.byte_at(at)? == routine.token() {
                    at += 1;
                    let mut matched = true;
                    for &b in name {
                        if self.byte_at(at)? != b {
                            matched = false;
                            break;
                        }
                        at += 1;
                    }
                    if matched && !in_name(self.byte_at(at)?) {
                        return Ok(Some(Def {
                            line,
                            after_name: at,
                        }));
                    }
                }
            }
            line += length;
            self.os.yield_point()?;
        }
        Err(Error::BadProgram.into())
    }

    /// The line holding `at`: its CR's address, zero outside the program.
    pub(crate) fn line_of(&mut self, at: u32) -> Step<u32> {
        let mut line = self.page;
        for _ in 0..LINES_MOST {
            if self.byte_at(line)? != b'\r'
                || self.byte_at(line + 1)? == tairix_openbasic::program::END
            {
                break;
            }
            let length = u32::from(self.byte_at(line + 3)?);
            if length < 4 {
                break;
            }
            if at >= line && at < line + length {
                return Ok(line);
            }
            line += length;
        }
        for list in [ws::INSTALLLIST.at(), ws::LIBRARYLIST.at()] {
            let mut library = self.os.read_word(list)?;
            let mut hops = 0;
            while library != 0 && hops < LIBRARIES_MOST {
                let mut line = library.wrapping_add(4);
                for _ in 0..LINES_MOST {
                    if self.byte_at(line)? != b'\r'
                        || self.byte_at(line + 1)? == tairix_openbasic::program::END
                    {
                        break;
                    }
                    let length = u32::from(self.byte_at(line + 3)?);
                    if length < 4 {
                        break;
                    }
                    if at >= line && at < line + length {
                        return Ok(line);
                    }
                    line += length;
                }
                library = self.os.read_word(library)?;
                hops += 1;
            }
        }
        Ok(0)
    }
}

/// A `DEF` found: its line, and where its name ends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Def {
    line: u32,
    after_name: u32,
}

/// An argument as the call binds it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "a string is carried inline, so binding one never allocates"
)]
enum Argument {
    Value(Value),
    Lvalue(Lvalue),
}

/// The most lines a walk of a program visits: more than memory holds.
const LINES_MOST: u32 = 1 << 24;

/// The most libraries a list holds before the walk stops: a list the
/// program overwrote cannot hold it.
const LIBRARIES_MOST: u32 = 4096;

#[cfg(test)]
mod tests {
    use super::{of_type_word, type_word};
    use crate::machine::lvalue::{Element, Place};
    use crate::value::Dialect;

    #[test]
    fn type_words_are_basic_v_s_as_recorded() {
        let v = Dialect::V;
        assert_eq!(type_word(Place::Real, false, v), 0x5_0005);
        assert_eq!(type_word(Place::Int, false, v), 0x4_0004);
        assert_eq!(type_word(Place::Sib, false, v), 0x80_0080);
        assert_eq!(type_word(Place::Array(Element::Real), false, v), 0x4_8105);
        assert_eq!(type_word(Place::Array(Element::Int), false, v), 0x4_8104);
        assert_eq!(type_word(Place::Array(Element::Str), false, v), 0x4_8180);
        assert_eq!(type_word(Place::Real, true, v), 0x5_0505);
    }

    #[test]
    fn a_type_word_names_its_place_back() {
        for dialect in [Dialect::V, Dialect::VI] {
            for place in [
                Place::Real,
                Place::Int,
                Place::Sib,
                Place::Array(Element::Real),
                Place::Array(Element::Int),
                Place::Array(Element::Str),
            ] {
                for returned in [false, true] {
                    let word = type_word(place, returned, dialect);
                    assert_eq!(
                        of_type_word(word, dialect),
                        Some((place, returned)),
                        "{word:#X}"
                    );
                }
            }
        }
    }
}
