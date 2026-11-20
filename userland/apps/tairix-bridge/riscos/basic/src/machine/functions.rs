//! BASIC's functions: the operations on a factor — unary operators,
//! indirection and the functions of one argument — and the functions whose
//! arguments are bracketed, with the keywords that begin an operand.

use tairix_riscos_kernel::os::Regs;
use tairix_riscos_kernel::swi::{os as swi, X};

use super::eval::truth_value;
use super::lvalue::{Element, Lvalue};
use super::next::Next;
use super::{flag, len32, Leave, Machine, Mode, Step};
use crate::five::Five;
use crate::messages::Error;
use crate::six::Arithmetic;
use crate::stack::Kind;
use crate::token;
use crate::value::{Arith, Dialect, Number, Text, Value};
use crate::workspace::{self as ws, reg};

/// An operation on the factor after it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum Unary {
    Neg = 1,
    Plus,
    Not,
    Byte,
    Word,
    Dollar,
    Real,
    BaseByte,
    BaseWord,
    Abs,
    Acs,
    Asn,
    Atn,
    Cos,
    Deg,
    Exp,
    Int,
    Ln,
    Log,
    Rad,
    Sgn,
    Sin,
    Sqr,
    Tan,
    Asc,
    Chr,
    Len,
    Str,
    StrHex,
    Val,
    Eval,
    Inkey,
    InkeyString,
    Usr,
    Adval,
    OpenIn,
    OpenOut,
    OpenUp,
    Bget,
    Eof,
    Ext,
    Ptr,
    GetHash,
}

impl Unary {
    const ALL: [Self; 43] = [
        Self::Neg,
        Self::Plus,
        Self::Not,
        Self::Byte,
        Self::Word,
        Self::Dollar,
        Self::Real,
        Self::BaseByte,
        Self::BaseWord,
        Self::Abs,
        Self::Acs,
        Self::Asn,
        Self::Atn,
        Self::Cos,
        Self::Deg,
        Self::Exp,
        Self::Int,
        Self::Ln,
        Self::Log,
        Self::Rad,
        Self::Sgn,
        Self::Sin,
        Self::Sqr,
        Self::Tan,
        Self::Asc,
        Self::Chr,
        Self::Len,
        Self::Str,
        Self::StrHex,
        Self::Val,
        Self::Eval,
        Self::Inkey,
        Self::InkeyString,
        Self::Usr,
        Self::Adval,
        Self::OpenIn,
        Self::OpenOut,
        Self::OpenUp,
        Self::Bget,
        Self::Eof,
        Self::Ext,
        Self::Ptr,
        Self::GetHash,
    ];

    pub(crate) fn of(byte: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|&op| op as u8 == byte)
    }
}

/// A function whose arguments are bracketed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum Bracketed {
    /// An array element's subscripts: the array's pointer and its element.
    Subscript = 1,
    Instr,
    Left,
    Mid,
    Right,
    String,
    Point,
}

impl Bracketed {
    const ALL: [Self; 7] = [
        Self::Subscript,
        Self::Instr,
        Self::Left,
        Self::Mid,
        Self::Right,
        Self::String,
        Self::Point,
    ];

    pub(crate) fn of(byte: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|&f| f as u8 == byte)
    }

    /// The fewest arguments it takes.
    pub(crate) const fn least(self) -> u32 {
        match self {
            Self::Subscript | Self::Left | Self::Right => 1,
            Self::Instr | Self::Mid | Self::String | Self::Point => 2,
        }
    }

    /// The most.
    pub(crate) const fn most(self) -> u32 {
        match self {
            Self::Subscript => u32::MAX,
            Self::Left | Self::Right | Self::String | Self::Point => 2,
            Self::Instr | Self::Mid => 3,
        }
    }

    /// The error past its most.
    pub(crate) const fn too_many(self) -> Error {
        match self {
            Self::Subscript => Error::SubscriptCount,
            _ => Error::MissingBracket,
        }
    }

    /// The error short of its least.
    pub(crate) const fn too_few(self) -> Error {
        match self {
            Self::Subscript => Error::SubscriptCount,
            _ => Error::MissingComma,
        }
    }
}

/// `OS_Word` 1: the system clock read.
const READ_CLOCK: u32 = 1;
/// `OS_Word` 14 reason 0: the real-time clock as a string.
const READ_TIME_STRING: u32 = 14;
/// `OS_Byte` 134: the text cursor's position.
const CURSOR_POSITION: u32 = 134;
/// `OS_Byte` 128: an ADC channel or buffer's state.
const ADVAL: u32 = 128;
/// `OS_Byte` 129: a key read in a time, or a key scanned.
const INKEY: u32 = 129;
/// `OS_Args` 0, 1 and 2: a file's pointer read, written, and its extent.
const ARGS_POINTER: u32 = 0;
const ARGS_EXTENT: u32 = 2;
/// `OS_Args` 5: whether a file is at its end.
const ARGS_EOF: u32 = 5;
/// `OS_Find`'s reasons BASIC opens files with, as recorded: for input,
/// output and update, through `File$Path`, a missing file answering nought
/// and a directory left to the filing system.
const OPEN_IN: u32 = 0x40;
const OPEN_OUT: u32 = 0x80;
const OPEN_UP: u32 = 0xC0;

impl Machine<'_, '_> {
    /// An operand that begins with a keyword.
    #[expect(clippy::too_many_lines, reason = "one arm per keyword")]
    pub(crate) fn keyword_operand(&mut self) -> Step<Next> {
        let b = self.next()?;
        let factor = |m: &mut Self, op: Unary| -> Step<Next> {
            m.push(Kind::Factor, op as u8, &[0])?;
            Ok(Next::Moved)
        };
        let hashed = |m: &mut Self, op: Unary| -> Step<Next> {
            m.expect(b'#', Error::MissingHash)?;
            m.push(Kind::Factor, op as u8, &[0])?;
            Ok(Next::Moved)
        };
        let bracketed = |m: &mut Self, f: Bracketed| -> Step<Next> {
            m.push(Kind::Arguments, f as u8, &[0])?;
            Ok(Next::Moved)
        };
        match b {
            token::PI => {
                let pi = self.arith.pi();
                self.operand_is(&Value::Real(pi))
            }
            token::TRUE => self.operand_is(&Value::Int(-1)),
            token::FALSE => self.operand_is(&Value::Int(0)),
            token::ERR => {
                let n = self.os.read_word(reg::ERR.at())?;
                self.operand_is(&Value::Int(n.cast_signed()))
            }
            token::ERL => {
                let n = self.os.read_word(reg::ERL.at())?;
                self.operand_is(&Value::Int(n.cast_signed()))
            }
            token::PAGE => self.operand_is(&Value::Int(self.page.cast_signed())),
            token::LOMEM => self.operand_is(&Value::Int(self.lomem.cast_signed())),
            token::HIMEM => self.operand_is(&Value::Int(self.himem.cast_signed())),
            token::END => self.operand_is(&Value::Int(self.fsa.cast_signed())),
            token::TO if self.peek()? == b'P' => {
                self.ptr += 1;
                self.operand_is(&Value::Int(self.top.cast_signed()))
            }
            token::COUNT => {
                let n = self.os.read_word(ws::TALLY.at())?;
                self.operand_is(&Value::Int(n.cast_signed()))
            }
            token::TIME if self.peek()? == b'$' => {
                self.ptr += 1;
                let text = self.time_string()?;
                self.operand_is(&Value::Str(text))
            }
            token::TIME => {
                let time = self.clock()?;
                self.operand_is(&Value::Int(time))
            }
            token::POS | token::VPOS => {
                let (column, row) = self.cursor()?;
                let n = if b == token::POS { column } else { row };
                self.operand_is(&Value::Int(n.cast_signed()))
            }
            token::RND => self.rnd_operand(),
            token::REPORT if self.peek()? == b'$' => {
                self.ptr += 1;
                let text = self.report_text()?;
                self.operand_is(&Value::Str(text))
            }
            token::GET if self.peek()? == b'$' => {
                self.ptr += 1;
                if self.take(b'#')? {
                    return factor(self, Unary::GetHash);
                }
                self.read_key(false)
            }
            token::GET_STRING => {
                if self.take(b'#')? {
                    return factor(self, Unary::GetHash);
                }
                self.read_key(false)
            }
            token::GET => self.read_key(true),
            token::INKEY => factor(self, Unary::Inkey),
            token::INKEY_STRING => factor(self, Unary::InkeyString),
            token::ABS => factor(self, Unary::Abs),
            token::ACS => factor(self, Unary::Acs),
            token::ASN => factor(self, Unary::Asn),
            token::ATN => factor(self, Unary::Atn),
            token::COS => factor(self, Unary::Cos),
            token::DEG => factor(self, Unary::Deg),
            token::EXP => factor(self, Unary::Exp),
            token::INT => factor(self, Unary::Int),
            token::LN => factor(self, Unary::Ln),
            token::LOG => factor(self, Unary::Log),
            token::RAD => factor(self, Unary::Rad),
            token::SGN => factor(self, Unary::Sgn),
            token::SIN => factor(self, Unary::Sin),
            token::SQR => factor(self, Unary::Sqr),
            token::TAN => factor(self, Unary::Tan),
            token::ASC => factor(self, Unary::Asc),
            token::CHR => factor(self, Unary::Chr),
            token::LEN => factor(self, Unary::Len),
            token::STR if self.peek()? == b'~' => {
                self.ptr += 1;
                factor(self, Unary::StrHex)
            }
            token::STR => factor(self, Unary::Str),
            token::VAL => factor(self, Unary::Val),
            token::EVAL => factor(self, Unary::Eval),
            token::USR => factor(self, Unary::Usr),
            token::ADVAL => factor(self, Unary::Adval),
            token::OPENIN => factor(self, Unary::OpenIn),
            token::OPENOUT => factor(self, Unary::OpenOut),
            token::OPENUP => factor(self, Unary::OpenUp),
            token::BGET => hashed(self, Unary::Bget),
            token::EOF => hashed(self, Unary::Eof),
            token::EXT => hashed(self, Unary::Ext),
            token::PTR => hashed(self, Unary::Ptr),
            token::INSTR => bracketed(self, Bracketed::Instr),
            token::LEFT => bracketed(self, Bracketed::Left),
            token::MID => bracketed(self, Bracketed::Mid),
            token::RIGHT => bracketed(self, Bracketed::Right),
            token::STRING => bracketed(self, Bracketed::String),
            token::POINT => bracketed(self, Bracketed::Point),
            token::DIM if self.take(b'(')? => self.dim_operand(),
            token::MOD => self.modulus_operand(),
            token::FN => self.fn_operand(),
            token::STATEMENT if self.peek()? == token::statement::QUIT => {
                self.ptr += 1;
                let quitting = self.flags & flag::QUIT_AT_END != 0;
                self.operand_is(&truth_value(quitting))
            }
            token::FUNCTION => self.prefixed_function(),
            _ => Err(Error::Mistake.into()),
        }
    }

    /// A function behind the functions' prefix.
    fn prefixed_function(&mut self) -> Step<Next> {
        match self.next()? {
            token::function::SUM => self.sum_operand(),
            token::function::BEAT => {
                let mut regs = Regs::default();
                regs.r[0] = 0;
                self.os.perform(X | BEAT_SWI, &mut regs)?;
                self.operand_is(&Value::Int(regs.r[0].cast_signed()))
            }
            _ => Err(Error::Mistake.into()),
        }
    }

    /// `op` applied to `value`, its own word `word`: the result pushed, or
    /// where the machine leaves to get it, how.
    #[expect(clippy::too_many_lines, reason = "one arm per function")]
    pub(crate) fn apply_unary(
        &mut self,
        op: Unary,
        value: &Value,
        word: u32,
    ) -> Step<Option<Next>> {
        let result = match op {
            Unary::Neg => {
                let n = value.number().ok_or(Error::NeedNumber)?;
                self.arith.negate(n)?.into()
            }
            Unary::Plus => {
                value.number().ok_or(Error::NeedNumber)?;
                *value
            }
            Unary::Not => Value::Int(!self.int_value(value)?),
            Unary::Byte | Unary::Word | Unary::Dollar | Unary::Real => {
                let address = self.int_value(value)?.cast_unsigned();
                let place = match op {
                    Unary::Byte => super::Place::Byte,
                    Unary::Word => super::Place::Int,
                    Unary::Dollar => super::Place::Dollar,
                    _ => super::Place::Real,
                };
                if op == Unary::Dollar && address < DOLLAR_LEAST {
                    return Err(Error::LowDollar.into());
                }
                self.read(Lvalue { address, place })?
            }
            Unary::BaseByte | Unary::BaseWord => {
                let offset = self.int_value(value)?;
                let address = word.wrapping_add_signed(offset);
                let place = if op == Unary::BaseByte {
                    super::Place::Byte
                } else {
                    super::Place::Int
                };
                self.read(Lvalue { address, place })?
            }
            Unary::Abs => {
                let n = value.number().ok_or(Error::NeedNumber)?;
                self.arith.abs(n)?.into()
            }
            Unary::Int => match *value {
                Value::Int(n) => Value::Int(n),
                Value::Real(r) => Value::Int(self.arith.floor(r)?),
                Value::Str(_) => return Err(Error::NeedNumber.into()),
            },
            Unary::Sgn => {
                let n = value.number().ok_or(Error::NeedNumber)?;
                Value::Int(match n {
                    Number::Int(n) => n.signum(),
                    Number::Real(r) if self.arith.is_zero(r) => 0,
                    Number::Real(r) if self.arith.is_negative(r) => -1,
                    Number::Real(_) => 1,
                })
            }
            Unary::Acs => self.real_function(value, Five::acs, Arithmetic::acs)?,
            Unary::Asn => self.real_function(value, Five::asn, Arithmetic::asn)?,
            Unary::Atn => self.real_function(value, Five::atn, Arithmetic::atn)?,
            Unary::Cos => self.real_function(value, Five::cos, Arithmetic::cos)?,
            Unary::Deg => self.real_function(value, Five::deg, Arithmetic::deg)?,
            Unary::Exp => self.real_function(value, Five::exp, Arithmetic::exp)?,
            Unary::Ln => self.real_function(value, Five::ln, Arithmetic::ln)?,
            Unary::Log => self.real_function(value, Five::log, Arithmetic::log)?,
            Unary::Rad => self.real_function(value, Five::rad, Arithmetic::rad)?,
            Unary::Sin => self.real_function(value, Five::sin, Arithmetic::sin)?,
            Unary::Sqr => self.real_function(value, Five::sqrt, Arithmetic::sqrt)?,
            Unary::Tan => self.real_function(value, Five::tan, Arithmetic::tan)?,
            Unary::Asc => {
                let text = string_of(value)?;
                Value::Int(text.as_bytes().first().map_or(-1, |&b| i32::from(b)))
            }
            Unary::Chr => {
                let n = self.int_value(value)?;
                Value::Str(Text::of(&[n.to_le_bytes()[0]]).ok_or(Error::StringTooLong)?)
            }
            Unary::Len => Value::Int(i32::from(string_of(value)?.len())),
            Unary::Str => Value::Str(self.str_of(value)?),
            Unary::StrHex => {
                let n = self.int_value(value)?;
                Value::Str(hex_text(n.cast_unsigned()))
            }
            Unary::Val => self.val(string_of(value)?)?,
            Unary::Eval => return self.eval_string(string_of(value)?).map(Some),
            Unary::Inkey | Unary::InkeyString => {
                let n = self.int_value(value)?;
                return self.inkey(n, op == Unary::InkeyString).map(Some);
            }
            Unary::Usr => {
                let address = self.int_value(value)?.cast_unsigned();
                return self.usr(address).map(Some);
            }
            Unary::Adval => {
                let n = self.int_value(value)?;
                let mut regs = Regs::default();
                regs.r[..2].copy_from_slice(&[ADVAL, n.cast_unsigned()]);
                self.os.perform(X | swi::BYTE, &mut regs)?;
                Value::Int(adval_result(n, regs.r[1], regs.r[2]))
            }
            Unary::OpenIn | Unary::OpenOut | Unary::OpenUp => {
                let reason = match op {
                    Unary::OpenIn => OPEN_IN,
                    Unary::OpenOut => OPEN_OUT,
                    _ => OPEN_UP,
                };
                Value::Int(self.open(reason, string_of(value)?)?.cast_signed())
            }
            Unary::Bget => {
                let handle = self.int_value(value)?.cast_unsigned();
                let mut regs = Regs::default();
                regs.r[1] = handle;
                self.os.perform(X | swi::BGET, &mut regs)?;
                Value::Int(if regs.flag(tairix_riscos_arm::psr::C) {
                    -1
                } else {
                    (regs.r[0] & 0xFF).cast_signed()
                })
            }
            Unary::Eof | Unary::Ext | Unary::Ptr => {
                let handle = self.int_value(value)?.cast_unsigned();
                let reason = match op {
                    Unary::Eof => ARGS_EOF,
                    Unary::Ext => ARGS_EXTENT,
                    _ => ARGS_POINTER,
                };
                let mut regs = Regs::default();
                regs.r[..2].copy_from_slice(&[reason, handle]);
                self.os.perform(X | swi::ARGS, &mut regs)?;
                if op == Unary::Eof {
                    truth_value(regs.r[2] != 0)
                } else {
                    Value::Int(regs.r[2].cast_signed())
                }
            }
            Unary::GetHash => {
                let handle = self.int_value(value)?.cast_unsigned();
                Value::Str(self.get_line(handle)?)
            }
        };
        self.push_value(&result)?;
        Ok(None)
    }

    /// A real function of `value`, each dialect's.
    fn real_function(
        &mut self,
        value: &Value,
        five: fn(Five) -> Result<Five, crate::Fault>,
        six: fn(&mut Arithmetic, crate::six::Six) -> Result<crate::six::Six, crate::Fault>,
    ) -> Step<Value> {
        let n = value.number().ok_or(Error::NeedNumber)?;
        Ok(Value::Real(self.arith.function(n, five, six)?))
    }

    /// The bracketed function `function` called with the `count` values above
    /// its record at `marker`.
    pub(crate) fn call_bracketed(
        &mut self,
        function: Bracketed,
        marker: u32,
        count: u32,
    ) -> Step<Next> {
        let mut args = [Value::Int(0); 3];
        if function == Bracketed::Subscript {
            return self.element(marker, count);
        }
        let n = count as usize;
        for slot in args
            .get_mut(..n)
            .ok_or(Error::StackCorrupt)?
            .iter_mut()
            .rev()
        {
            *slot = self.pop_value()?;
        }
        self.pop_record(Kind::Arguments, &mut [0])?;
        let result = match function {
            Bracketed::Instr => {
                let haystack = string_of(&args[0])?;
                let needle = string_of(&args[1])?;
                let start = if n == 3 { self.int_value(&args[2])? } else { 1 };
                Value::Int(instr(haystack.as_bytes(), needle.as_bytes(), start))
            }
            Bracketed::Left | Bracketed::Right => {
                let text = string_of(&args[0])?;
                let length = text.as_bytes().len();
                let take = if n == 2 {
                    count_of(self.int_value(&args[1])?)
                } else if function == Bracketed::Left {
                    length.saturating_sub(1)
                } else {
                    1
                };
                let take = take.min(length);
                Value::Str(if function == Bracketed::Left {
                    text.slice(0, take)
                } else {
                    text.slice(length - take, take)
                })
            }
            Bracketed::Mid => {
                let text = string_of(&args[0])?;
                let start = count_of(self.int_value(&args[1])?);
                let count = if n == 3 {
                    count_of(self.int_value(&args[2])?)
                } else {
                    usize::MAX
                };
                Value::Str(text.slice(start.max(1) - 1, count))
            }
            Bracketed::String => {
                let times = self.int_value(&args[0])?;
                let unit = string_of(&args[1])?;
                let mut out = Text::EMPTY;
                for _ in 0..times.max(0) {
                    out.push_bytes(unit.as_bytes())
                        .ok_or(Error::StringTooLong)?;
                }
                Value::Str(out)
            }
            Bracketed::Point => {
                let x = self.int_value(&args[0])?;
                let y = self.int_value(&args[1])?;
                Value::Int(self.point(x, y)?)
            }
            Bracketed::Subscript => return Err(Error::StackCorrupt.into()),
        };
        self.operand_is(&result)
    }

    /// The element the subscripts above the record at `marker` pick of the
    /// array the record names: its value as an operand, or its l-value for
    /// the statement waiting under the record where the record's flag says.
    fn element(&mut self, marker: u32, count: u32) -> Step<Next> {
        let pointer = self.record_word(marker, 1)?;
        let element = match self.record_word(marker, 2)? {
            0 => Element::Int,
            1 => Element::Real,
            _ => Element::Str,
        };
        let wants_lvalue = self.record_word(marker, 3)? != 0;
        let mut subscripts = [0_i32; SUBSCRIPTS_MOST];
        let n = usize::try_from(count).map_err(|_| Error::SubscriptCount)?;
        if n > SUBSCRIPTS_MOST {
            return Err(Error::SubscriptCount.into());
        }
        for slot in subscripts[..n].iter_mut().rev() {
            *slot = self.pop_int()?;
        }
        self.pop_record(Kind::Arguments, &mut [0, 0, 0, 0])?;
        let lvalue = self.element_lvalue(pointer, element, &subscripts[..n])?;
        if wants_lvalue {
            return self.deliver_lvalue(lvalue);
        }
        let value = self.read(lvalue)?;
        self.after_variable(&value)
    }

    /// The l-value of the element `subscripts` pick of the array whose block
    /// is at `pointer`: each subscript within its size, as many as the array
    /// has dimensions.
    pub(crate) fn element_lvalue(
        &mut self,
        pointer: u32,
        element: Element,
        subscripts: &[i32],
    ) -> Step<Lvalue> {
        if pointer < UNDIMENSIONED_BELOW {
            return Err(Error::Undimensioned.into());
        }
        let mut position: u32 = 0;
        for (i, &subscript) in subscripts.iter().enumerate() {
            let size = self.os.read_word(pointer + 4 * len32(i))?;
            if size == 0 {
                return Err(Error::SubscriptCount.into());
            }
            let index = u32::try_from(subscript)
                .ok()
                .filter(|&s| s < size)
                .ok_or(Error::SubscriptRange)?;
            position = position
                .checked_mul(size)
                .and_then(|p| p.checked_add(index))
                .ok_or(Error::SubscriptRange)?;
        }
        if self.os.read_word(pointer + 4 * len32(subscripts.len()))? != 0 {
            return Err(Error::SubscriptCount.into());
        }
        let first = pointer + 4 * len32(subscripts.len()) + 8;
        let size = element.place().bytes(self.dialect());
        let address = position
            .checked_mul(size)
            .and_then(|offset| first.checked_add(offset))
            .ok_or(Error::SubscriptRange)?;
        Ok(Lvalue {
            address,
            place: element.place(),
        })
    }

    /// The text cursor's column and row, as `POS` and `VPOS` read them.
    pub(crate) fn cursor(&mut self) -> Step<(u32, u32)> {
        let mut regs = Regs::default();
        regs.r[0] = CURSOR_POSITION;
        self.os.perform(X | swi::BYTE, &mut regs)?;
        Ok((regs.r[1], regs.r[2]))
    }

    /// `STR$` of `value`: `@%`'s layout where its flag asks.
    pub(crate) fn str_of(&mut self, value: &Value) -> Step<Text> {
        let at = self.os.read_word(ws::AT_PERCENT.at())?;
        let format = crate::text::Format::of_str(at);
        self.number_text(value, format)
    }

    /// `value` as text under `format`.
    /// `value` as text under `format`: an integer laid out as the real it
    /// is, as BASIC V lays one out.
    pub(crate) fn number_text(&mut self, value: &Value, format: crate::text::Format) -> Step<Text> {
        let real = match *value {
            Value::Int(n) => self.arith.real(n)?,
            Value::Real(r) => r,
            Value::Str(_) => return Err(Error::NeedNumber.into()),
        };
        let mut out = TextWriter(Text::EMPTY);
        let written = match self.dialect() {
            Dialect::V => crate::text::write(&Arith::as_five(real), format, &mut out),
            Dialect::VI => {
                let digits = self.arith.six().digits(Arith::as_six(real))?;
                crate::text::write(&digits, format, &mut out)
            }
        };
        written.map_err(|_| Error::StringTooLong)?;
        Ok(out.0)
    }

    /// `VAL` of `text`: a number read as a literal is, an integer where it
    /// is one.
    fn val(&mut self, text: &Text) -> Step<Value> {
        let digits = match self.dialect() {
            Dialect::V => crate::five::EXPONENT_DIGITS,
            Dialect::VI => crate::six::EXPONENT_DIGITS,
        };
        let written = crate::text::Written::val(text.as_bytes(), digits);
        if let Some(n) = self.integer_of(&written) {
            return Ok(Value::Int(n));
        }
        Ok(Value::Real(self.read_real(&written)?))
    }

    /// The system clock's centiseconds: `TIME`.
    pub(crate) fn clock(&mut self) -> Step<i32> {
        let block = ws::STRACC.at();
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[READ_CLOCK, block]);
        self.os.perform(X | swi::WORD, &mut regs)?;
        Ok(self.os.read_word(block)?.cast_signed())
    }

    /// `TIME$`: the real-time clock as RISC OS writes it.
    fn time_string(&mut self) -> Step<Text> {
        let block = ws::STRACC.at();
        self.os.write_byte(block, 0)?;
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[READ_TIME_STRING, block]);
        self.os.perform(X | swi::WORD, &mut regs)?;
        let mut text = self.read_dollar(block)?;
        if text.as_bytes().last() == Some(&b'\0') {
            text = text.slice(0, text.as_bytes().len() - 1);
        }
        Ok(text)
    }

    /// The last error's text: `REPORT$`.
    pub(crate) fn report_text(&mut self) -> Step<Text> {
        let block = reg::ERROR.at();
        let mut text = Text::EMPTY;
        for i in 0..crate::value::STRING_MOST {
            let b = self.os.read_byte(block + 4 + len32(i))?;
            if b == 0 {
                break;
            }
            text.push_bytes(&[b]).ok_or(Error::StringTooLong)?;
        }
        Ok(text)
    }

    /// A file opened by `OS_Find`'s `reason`, named `name`: its handle, zero
    /// where it is not there to open.
    fn open(&mut self, reason: u32, name: &Text) -> Step<u32> {
        let at = ws::STRACC.at();
        self.os.write_bytes(at, name.as_bytes())?;
        self.os.write_byte(at + u32::from(name.len()), b'\r')?;
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[reason, at]);
        self.os.perform(X | swi::FIND, &mut regs)?;
        Ok(regs.r[0])
    }

    /// `GET$#`: the next line of a file, to its CR, LF or zero.
    fn get_line(&mut self, handle: u32) -> Step<Text> {
        let mut text = Text::EMPTY;
        loop {
            let mut regs = Regs::default();
            regs.r[1] = handle;
            self.os.perform(X | swi::BGET, &mut regs)?;
            let b = (regs.r[0] & 0xFF) as u8;
            if regs.flag(tairix_riscos_arm::psr::C) || matches!(b, b'\r' | b'\n' | 0) {
                return Ok(text);
            }
            text.push_bytes(&[b]).ok_or(Error::StringTooLong)?;
        }
    }

    /// `POINT(x, y)`: the colour at a graphics point, -1 off the screen.
    fn point(&mut self, x: i32, y: i32) -> Step<i32> {
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[x.cast_unsigned(), y.cast_unsigned()]);
        self.os.perform(X | READ_POINT, &mut regs)?;
        Ok(if regs.r[4] == 0 {
            regs.r[2].cast_signed()
        } else {
            -1
        })
    }

    /// `GET` and `GET$`: a key read from User mode, the machine resumed with
    /// it.
    fn read_key(&mut self, number: bool) -> Step<Next> {
        self.push(Kind::Guest, Guest::Key as u8, &[u32::from(number)])?;
        self.mode = Mode::Away;
        Ok(Next::Leave(Leave::Swi {
            number: swi::READ_C,
            regs: [0; 10],
            flags: 0,
        }))
    }

    /// `INKEY(n)` and `INKEY$(n)`: a key waited for `n` centiseconds, or a
    /// key scanned where `n` is negative.
    fn inkey(&mut self, n: i32, string: bool) -> Step<Next> {
        self.push(
            Kind::Guest,
            Guest::Inkey as u8,
            &[u32::from(!string), n.cast_unsigned()],
        )?;
        self.mode = Mode::Away;
        let n = n.cast_unsigned();
        let mut regs = [0; 10];
        regs[..3].copy_from_slice(&[INKEY, n & 0xFF, (n >> 8) & 0xFF]);
        Ok(Next::Leave(Leave::Swi {
            number: swi::BYTE,
            regs,
            flags: 0,
        }))
    }

    /// `RND`, `RND(n)`.
    fn rnd_operand(&mut self) -> Step<Next> {
        if self.peek()? != b'(' {
            let n = self.random()?;
            return self.operand_is(&Value::Int(n));
        }
        self.ptr += 1;
        self.push(
            Kind::Continuation,
            crate::machine::statements::cont::RND,
            &[],
        )?;
        self.push(Kind::Expression, 0, &[])?;
        Ok(Next::Moved)
    }

    /// `FN`: a function's name and its arguments, then its call.
    fn fn_operand(&mut self) -> Step<Next> {
        let start = self.ptr;
        while super::vars::in_name(self.peek()?) {
            self.ptr += 1;
        }
        let length = self.ptr - start;
        if length == 0 {
            return Err(Error::BadCall.into());
        }
        self.begin_call(super::routines::Routine::Fn, start, length, None)
    }
}

/// What the machine left for User mode to do, which it resumes after.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum Guest {
    /// `GET` or `GET$`: a key read.
    Key = 1,
    /// `INKEY`: a key read in a time, or scanned.
    Inkey,
    /// `USR`: code entered, its R0 the value.
    Usr,
    /// `CALL`: code entered.
    Call,
    /// `SYS`: a SWI performed.
    Sys,
    /// `OSCLI` or a `*` line: a command run.
    Command,
    /// `INPUT`: a line read.
    Input,
    /// The prompt: a line read.
    Prompt,
    /// A floating-point exception raised through the OS, its block kept for
    /// an `ErrorV` claimant that swallows it.
    Raise,
}

impl Guest {
    const ALL: [Self; 9] = [
        Self::Key,
        Self::Inkey,
        Self::Usr,
        Self::Call,
        Self::Sys,
        Self::Command,
        Self::Input,
        Self::Prompt,
        Self::Raise,
    ];

    pub(crate) fn of(byte: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|&g| g as u8 == byte)
    }
}

/// `OS_ReadPoint`: a graphics point's colour, R4 non-zero off the screen.
const READ_POINT: u32 = 0x32;
/// `Sound_QBeat`, which `BEAT` reads.
const BEAT_SWI: u32 = 0x4_0186;

/// The most subscripts an array has.
pub(crate) const SUBSCRIPTS_MOST: usize = 64;

/// An array's pointer below this is one not yet dimensioned.
pub(crate) const UNDIMENSIONED_BELOW: u32 = 16;

/// The least address `$` reads or writes at: page zero's start is refused.
pub(crate) const DOLLAR_LEAST: u32 = 0x100;

/// A string function's count or position: a negative one is the unsigned
/// count it is, so "all of it", as BASIC V reads one.
fn count_of(n: i32) -> usize {
    usize::try_from(n.cast_unsigned()).unwrap_or(usize::MAX)
}

/// The string `value` is: "Type mismatch: string needed" for a number.
pub(crate) fn string_of(value: &Value) -> Step<&Text> {
    match value {
        Value::Str(text) => Ok(text),
        _ => Err(Error::NeedString.into()),
    }
}

/// Where `needle` first appears in `haystack` from the `start`th byte, one
/// the first, zero for nowhere; an empty needle is found where it starts.
pub(crate) fn instr(haystack: &[u8], needle: &[u8], start: i32) -> i32 {
    let from = usize::try_from(start.max(1) - 1).unwrap_or(0);
    if from > haystack.len() {
        return 0;
    }
    if needle.is_empty() {
        return i32::try_from(from + 1).unwrap_or(0);
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map_or(0, |at| i32::try_from(from + at + 1).unwrap_or(0))
}

/// `n` in hexadecimal, its leading noughts dropped.
pub(crate) fn hex_text(n: u32) -> Text {
    let mut text = Text::EMPTY;
    let digits = (32 - n.leading_zeros()).div_ceil(4).max(1);
    for shift in (0..digits).rev() {
        let d = (n >> (4 * shift)) & 0xF;
        let _ = text.push_bytes(&[b"0123456789ABCDEF"[d as usize]]);
    }
    text
}

/// `n`'s decimal digits, its sign before them.
pub(crate) fn decimal(n: i32) -> Text {
    let mut digits = [0; 11];
    let mut at = digits.len();
    let mut m = n.unsigned_abs();
    loop {
        at -= 1;
        digits[at] = b'0' + (m % 10) as u8;
        m /= 10;
        if m == 0 {
            break;
        }
    }
    if n < 0 {
        at -= 1;
        digits[at] = b'-';
    }
    Text::of(&digits[at..]).unwrap_or(Text::EMPTY)
}

/// `ADVAL(n)`'s value from `OS_Byte` 128's: a buffer's count, or an ADC
/// channel's reading.
const fn adval_result(n: i32, low: u32, high: u32) -> i32 {
    if n < 0 {
        low.cast_signed()
    } else {
        (low & 0xFF | (high & 0xFF) << 8).cast_signed()
    }
}

/// A [`Text`] written through [`core::fmt::Write`].
struct TextWriter(Text);

impl core::fmt::Write for TextWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.push_bytes(s.as_bytes()).ok_or(core::fmt::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::{decimal, hex_text, instr};

    #[test]
    fn instr_counts_from_one_and_finds_an_empty_needle_where_it_starts() {
        assert_eq!(instr(b"hello", b"l", 1), 3);
        assert_eq!(instr(b"hello", b"l", 4), 4);
        assert_eq!(instr(b"hello", b"z", 1), 0);
        assert_eq!(instr(b"hello", b"", 2), 2);
        assert_eq!(instr(b"hello", b"o", 9), 0);
        assert_eq!(instr(b"hello", b"h", -3), 1, "a start below one is one");
    }

    #[test]
    fn hex_drops_leading_noughts_but_keeps_one() {
        assert_eq!(hex_text(0).as_bytes(), b"0");
        assert_eq!(hex_text(0xFF).as_bytes(), b"FF");
        assert_eq!(hex_text(0xDEAD_BEEF).as_bytes(), b"DEADBEEF");
    }

    #[test]
    fn integers_are_written_with_their_sign() {
        assert_eq!(decimal(0).as_bytes(), b"0");
        assert_eq!(decimal(-17).as_bytes(), b"-17");
        assert_eq!(decimal(i32::MIN).as_bytes(), b"-2147483648");
    }
}
