//! The routine table `CALL` hands code in R14: BASIC's own routines for
//! called code, each as the BBC BASIC Reference Manual documents its
//! registers — `VARIND`, `STOREA`, `STSTORE`, `LVBLNK`, `CREATE`, `EXPR`,
//! `MATCH`, `TOKENADDR`, and BASIC V's floating-point routines on its
//! expanded four-word real.
//!
//! `EXPR` and an `LVBLNK` of an array's element evaluate BASIC, which may
//! call a function and wait on input, so they run the machine: their
//! caller's registers are kept on the stack and the machine returns to it
//! when done. An error any of them meets is BASIC's, ending the `CALL` as on
//! RISC OS.

use tairix_riscos_kernel::os::{Fail, Os, Regs};

use crate::five::Five;
use crate::machine::{self, Lvalue, Machine, Place, Trap};
use crate::messages::Error;
use crate::value::{Arith, Dialect, Text, Value};
use crate::workspace as ws;

/// `VARIND`'s and `EXPR`'s value types.
const STRING: u32 = 0;
const INTEGER: u32 = 0x4000_0000;
const FLOAT: u32 = 0x8000_0000;

/// A routine's work on the machine, `f` given it loaded and its registers.
fn with_machine<T>(
    os: &mut Os<'_>,
    f: impl FnOnce(&mut Machine<'_, '_>) -> machine::Step<T>,
) -> Result<T, Fail> {
    let mut m = match Machine::load(os) {
        Ok(m) => m,
        Err(trap) => return Err(crate::start::fatal(os, trap)),
    };
    let done = f(&mut m);
    match done {
        Ok(value) => {
            m.save().map_err(|trap| crate::start::fatal(m.os, trap))?;
            Ok(value)
        }
        Err(trap) => Err(raise(&mut m, trap)),
    }
}

/// An error a routine met: BASIC's error path run and control gone where
/// it leads, from User mode; otherwise the error handed back with V set.
fn raise(m: &mut Machine<'_, '_>, trap: Trap) -> Fail {
    if let Trap::Unwind(unwind) = trap {
        return Fail::Unwind(unwind);
    }
    if !m.os.at_outermost() {
        return match m.block_of(trap) {
            Ok(block) => Fail::Error(block),
            Err(Trap::Unwind(unwind)) => Fail::Unwind(unwind),
            Err(_) => m.os.no_memory(),
        };
    }
    let leave = match m
        .error(trap)
        .and_then(|raised| raised.map_or_else(|| m.slice(), Ok))
    {
        Ok(leave) => leave,
        Err(unwind) => return Fail::Unwind(unwind),
    };
    if let Err(trap) = m.save() {
        return crate::start::fatal(m.os, trap);
    }
    match crate::module::depart(m.os, leave) {
        Ok(()) => m.os.no_memory(),
        Err(fail) => fail,
    }
}

/// A value as `VARIND` and `EXPR` return one: a string in the string
/// accumulator with R2 its end, an integer in R0, a real in R0 to R3 for
/// BASIC V and F0 for BASIC VI; R9 its type.
pub(crate) fn give(m: &mut Machine<'_, '_>, value: &Value, regs: &mut Regs) -> machine::Step {
    match *value {
        Value::Str(ref text) => {
            let at = ws::STRACC.at();
            m.os.write_bytes(at, text.as_bytes())?;
            regs.r[2] = at + u32::from(text.len());
            regs.r[9] = STRING;
        }
        Value::Int(n) => {
            regs.r[0] = n.cast_unsigned();
            regs.r[9] = INTEGER;
        }
        Value::Real(r) => {
            match m.dialect() {
                Dialect::V => {
                    let [r0, r1, r3] = expand(Arith::as_five(r));
                    regs.r[0] = r0;
                    regs.r[1] = r1;
                    regs.r[3] = r3;
                }
                Dialect::VI => m.os.set_fpa_double(0, Arith::as_six(r).to_bits()),
            }
            regs.r[9] = FLOAT;
        }
    }
    Ok(())
}

/// A value of `kind` taken as `STOREA` takes one.
fn take(m: &mut Machine<'_, '_>, kind: u32, regs: &Regs) -> machine::Step<Value> {
    Ok(match kind {
        STRING => Value::Str(text_between(m, ws::STRACC.at(), regs.r[2])?),
        INTEGER => Value::Int(regs.r[0].cast_signed()),
        FLOAT => match m.dialect() {
            Dialect::V => Value::Real(Arith::of_five(compact([regs.r[0], regs.r[1], regs.r[3]])?)),
            Dialect::VI => {
                let bits = m.os.fpa_double(0).map_err(Trap::Floating)?;
                Value::Real(Arith::of_six(crate::six::Six::from_bits(bits)))
            }
        },
        _ => return Err(Error::NeedNumber.into()),
    })
}

/// The l-value a register pair names, as `CALL`'s list types it.
fn lvalue_of(m: &Machine<'_, '_>, address: u32, kind: u32) -> machine::Step<Lvalue> {
    let place = Place::of_call_type(kind, m.dialect()).ok_or(Error::NeedNumber)?;
    Ok(Lvalue { address, place })
}

/// BASIC V's real as the routines' four words give it: the mantissa with
/// its top bit, the exponent, and the sign; R2 is not used.
pub(crate) fn expand(real: Five) -> [u32; 3] {
    if real.is_zero() {
        return [0, 0, 0];
    }
    let [m0, m1, m2, m3, exponent] = real.to_bytes();
    let mantissa = u32::from_le_bytes([m0, m1, m2, m3]);
    [mantissa | 1 << 31, u32::from(exponent), mantissa & 1 << 31]
}

/// The real the routines' four words give: "Number too big" past the
/// largest exponent, zero below the least.
pub(crate) fn compact([mantissa, exponent, sign]: [u32; 3]) -> Result<Five, Error> {
    if mantissa & 1 << 31 == 0 {
        return Ok(Five::ZERO);
    }
    let exponent = match u8::try_from(exponent.cast_signed()) {
        Ok(exponent) => exponent,
        Err(_) if exponent.cast_signed() < 0 => return Ok(Five::ZERO),
        Err(_) => return Err(Error::TooBig),
    };
    let bits = (mantissa & !(1 << 31)) | (sign & 1 << 31);
    let [m0, m1, m2, m3] = bits.to_le_bytes();
    Ok(Five::from_bytes([m0, m1, m2, m3, exponent]))
}

/// `VARIND`: the value at R0, of R9's type.
pub(crate) fn varind(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    with_machine(os, |m| {
        let lvalue = lvalue_of(m, regs.r[0], regs.r[9])?;
        let value = m.read(lvalue)?;
        give(m, &value, regs)
    })
}

/// `STOREA`: R9's kind of value stored at R4, of R5's type.
pub(crate) fn storea(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    with_machine(os, |m| {
        let value = take(m, regs.r[9], regs)?;
        let lvalue = lvalue_of(m, regs.r[4], regs.r[5])?;
        m.write(lvalue, &value)
    })
}

/// `STSTORE`: the string from R3 to R2 kept in the string whose block is
/// at R4.
pub(crate) fn ststore(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    with_machine(os, |m| {
        let text = text_between(m, regs.r[3], regs.r[2])?;
        m.assign_sib(regs.r[4], &text)
    })
}

/// The string from `start` to `end`: "String too long" past BASIC's longest,
/// which an end below the start is too.
fn text_between(m: &mut Machine<'_, '_>, start: u32, end: u32) -> machine::Step<Text> {
    let length = usize::try_from(end.wrapping_sub(start))
        .ok()
        .filter(|&length| length <= crate::value::STRING_MOST)
        .ok_or(Error::StringTooLong)?;
    let mut bytes = [0; crate::value::STRING_MOST];
    m.os.read_bytes(start, &mut bytes[..length])?;
    Ok(Text::of(&bytes[..length]).ok_or(Error::StringTooLong)?)
}

/// `LVBLNK`: the variable named at R11 looked up — Z clear with its
/// l-value in R0 and R9; Z set where it is missing, C set where the text
/// cannot be a name, with what `CREATE` needs.
pub(crate) fn lvblnk(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let caller = os.current_registers();
    if os.at_outermost() && names_element(os, regs.r[11])? {
        let mut regs = *regs;
        let at = regs.r[11];
        return crate::module::slice(os, |m| {
            m.begin_routine(&mut regs, caller, machine::RoutineCall::Lvalue, at)
        });
    }
    with_machine(os, |m| m.lvblnk(regs))
}

/// Whether the text at `at` names an array's element: a name, its suffix,
/// then a bracket.
fn names_element(os: &mut Os<'_>, at: u32) -> Result<bool, Fail> {
    let mut at = at;
    if !machine::starts_name(os.read_byte(at)?) {
        return Ok(false);
    }
    while machine::in_name(os.read_byte(at)?) {
        at += 1;
    }
    if matches!(os.read_byte(at)?, b'%' | b'$') {
        at += 1;
    }
    Ok(os.read_byte(at)? == b'(')
}

/// `CREATE`: the variable `LVBLNK` could not find made.
pub(crate) fn create(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    with_machine(os, |m| m.create_named(regs))
}

/// `EXPR`: the expression at R11 evaluated.
pub(crate) fn expr(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if !os.at_outermost() {
        return Err(os.error(tairix_riscos_kernel::error::BAD_PARAMETERS));
    }
    let caller = os.current_registers();
    let mut regs = *regs;
    let at = regs.r[11];
    crate::module::slice(os, |m| {
        m.begin_routine(&mut regs, caller, machine::RoutineCall::Expr, at)
    })
}

/// `MATCH`: the text at R1 tokenised to R2.
pub(crate) fn matcher(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    with_machine(os, |m| m.match_text(regs))
}

/// `TOKENADDR`: the text a token stands for.
pub(crate) fn tokenaddr(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    with_machine(os, |m| m.token_address(regs))
}

/// The five-byte real at R9.
fn five_at(m: &mut Machine<'_, '_>, at: u32) -> machine::Step<Five> {
    let mut bytes = [0; 5];
    m.os.read_bytes(at, &mut bytes)?;
    Ok(Five::from_bytes(bytes))
}

/// The routines' four-word real in R0 to R3.
fn four(regs: &Regs) -> machine::Step<Five> {
    Ok(compact([regs.r[0], regs.r[1], regs.r[3]])?)
}

/// `x` given back in R0 to R3.
fn set_four(regs: &mut Regs, x: Five) {
    let [r0, r1, r3] = expand(x);
    regs.r[0] = r0;
    regs.r[1] = r1;
    regs.r[3] = r3;
}

/// A routine of BASIC V alone.
fn basic_v(m: &Machine<'_, '_>) -> machine::Step {
    if m.dialect() == Dialect::V {
        Ok(())
    } else {
        Err(Error::Mistake.into())
    }
}

/// `FSTA`: R0 to R3 stored at R9.
pub(crate) fn fsta(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    with_machine(os, |m| {
        basic_v(m)?;
        let x = four(regs)?;
        Ok(m.os.write_bytes(regs.r[9], &x.to_bytes())?)
    })
}

/// `FLDA`: the real at R9 loaded into R0 to R3.
pub(crate) fn flda(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    with_machine(os, |m| {
        basic_v(m)?;
        let x = five_at(m, regs.r[9])?;
        set_four(regs, x);
        Ok(())
    })
}

/// One of the arithmetic routines: `op` of R0 to R3 and the real at R9.
fn arithmetic(
    os: &mut Os<'_>,
    regs: &mut Regs,
    op: fn(Five, Five) -> Result<Five, crate::Fault>,
    memory_first: bool,
) -> Result<(), Fail> {
    with_machine(os, |m| {
        basic_v(m)?;
        let held = four(regs)?;
        let memory = five_at(m, regs.r[9])?;
        let result = if memory_first {
            op(memory, held)?
        } else {
            op(held, memory)?
        };
        set_four(regs, result);
        Ok(())
    })
}

/// `FADD`.
pub(crate) fn fadd(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    arithmetic(os, regs, Five::add, false)
}

/// `FSUB`: the real at R9 less R0 to R3.
pub(crate) fn fsub(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    arithmetic(os, regs, Five::sub, true)
}

/// `FMUL`.
pub(crate) fn fmul(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    arithmetic(os, regs, Five::mul, false)
}

/// `FDIV`: the real at R9 over R0 to R3.
pub(crate) fn fdiv(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    arithmetic(os, regs, Five::div, true)
}

/// `FLOAT`: R0's integer as R0 to R3.
pub(crate) fn float(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    with_machine(os, |m| {
        basic_v(m)?;
        set_four(regs, Five::from_int(regs.r[0].cast_signed()));
        Ok(())
    })
}

/// `FIX`: R0 to R3 truncated to an integer in R0.
pub(crate) fn fix(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    with_machine(os, |m| {
        basic_v(m)?;
        let x = four(regs)?;
        regs.r[0] = x.to_int()?.cast_unsigned();
        Ok(())
    })
}

/// `FSQRT`.
pub(crate) fn fsqrt(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    with_machine(os, |m| {
        basic_v(m)?;
        let x = four(regs)?;
        set_four(regs, x.sqrt()?);
        Ok(())
    })
}

impl Machine<'_, '_> {
    /// The error `trap` stands for in a block of its own, for a caller the
    /// routine cannot leave.
    pub(crate) fn block_of(&mut self, trap: Trap) -> machine::Step<u32> {
        match trap {
            Trap::Os(block) => Ok(block),
            Trap::Basic(error) => {
                let block = ws::reg::ERROR.at();
                self.os.write_word(block, error.number())?;
                self.os.write_bytes(block + 4, error.text().as_bytes())?;
                self.os
                    .write_byte(block + 4 + super::machine::len32(error.text().len()), 0)?;
                Ok(block)
            }
            Trap::Floating(exception) => {
                match self
                    .os
                    .error(tairix_riscos_kernel::error::floating_point(exception))
                {
                    Fail::Error(block) => Ok(block),
                    Fail::Unwind(unwind) => Err(Trap::Unwind(unwind)),
                }
            }
            Trap::Unwind(unwind) => Err(Trap::Unwind(unwind)),
        }
    }

    /// `LVBLNK` of a plain name, at once; an element's is the machine's.
    fn lvblnk(&mut self, regs: &mut Regs) -> machine::Step {
        let saved = self.ptr;
        self.ptr = regs.r[11];
        let start = self.ptr;
        let name = self.read_name()?;
        let Some(name) = name else {
            self.ptr = saved;
            regs.set_flag(tairix_riscos_arm::psr::Z, true);
            regs.set_flag(tairix_riscos_arm::psr::C, true);
            return Ok(());
        };
        let found = self.variable(&name, false)?;
        let end = self.ptr;
        self.ptr = saved;
        match found {
            Some(lvalue) if !name.is_array() => {
                regs.r[0] = lvalue.address;
                regs.r[9] = lvalue.place.call_type(self.dialect());
                regs.set_flag(tairix_riscos_arm::psr::Z, false);
            }
            _ => {
                let bytes = name.as_bytes();
                regs.r[10] = u32::from(bytes[0]);
                regs.r[3] = u32::from(bytes.get(1).copied().unwrap_or(0));
                regs.r[4] = start + 1;
                regs.r[11] = end;
                regs.r[9] = 0;
                regs.set_flag(tairix_riscos_arm::psr::Z, true);
                regs.set_flag(tairix_riscos_arm::psr::C, false);
            }
        }
        Ok(())
    }

    /// `CREATE`: the name `LVBLNK` left in R10 and from R4 to R11 made a
    /// variable, its l-value returned as `LVBLNK` returns one.
    fn create_named(&mut self, regs: &mut Regs) -> machine::Step {
        let first = (regs.r[10] & 0xFF) as u8;
        let (start, end) = (regs.r[4], regs.r[11]);
        let length = end.checked_sub(start).ok_or(Error::NoSuchVariable)? as usize;
        let mut bytes = [0; super::machine::NAME_MOST];
        if length + 1 > bytes.len() {
            return Err(Error::NoSuchVariable.into());
        }
        bytes[0] = first;
        self.os.read_bytes(start, &mut bytes[1..=length])?;
        let name = machine::Name::of(&bytes[..=length]).ok_or(Error::NoSuchVariable)?;
        let lvalue = self.variable(&name, true)?.ok_or(Error::NoSuchVariable)?;
        regs.r[0] = lvalue.address;
        regs.r[9] = lvalue.place.call_type(self.dialect());
        regs.set_flag(tairix_riscos_arm::psr::Z, false);
        Ok(())
    }

    /// `MATCH`: R1's text to its CR or LF tokenised to R2, under R3's mode
    /// and R4's line-number switch; R5 the warnings.
    fn match_text(&mut self, regs: &mut Regs) -> machine::Step {
        use tairix_openbasic::tokenise::{tokenise, Start};
        let source = regs.r[1];
        let mut text = alloc::vec::Vec::new();
        for i in 0..super::machine::NAME_MOST * 2 {
            let b = self.os.read_byte(source + machine::len32(i))?;
            if b == b'\r' || b == b'\n' {
                break;
            }
            text.try_reserve(1).map_err(|_| Error::NoRoom)?;
            text.push(b);
        }
        let start = match (regs.r[3] & 1, regs.r[4] != 0) {
            (0, false) => Start::Statement,
            (0, true) => Start::Line,
            (_, false) => Start::Expression,
            (_, true) => Start::LineNumbers,
        };
        let mut out = alloc::vec::Vec::new();
        let tokenised = tokenise(tairix_openbasic::Dialect::BasicV, &text, start, &mut out)
            .map_err(|_| Error::NoRoom)?;
        let destination = regs.r[2];
        self.os.write_bytes(destination, &out)?;
        self.os
            .write_byte(destination + machine::len32(out.len()), b'\r')?;
        regs.r[1] = source + machine::len32(text.len()) + 1;
        regs.r[2] = destination + machine::len32(out.len()) + 1;
        let warnings = tokenised.warnings;
        regs.r[5] = u32::from(warnings.quote)
            | u32::from(warnings.line_number) << 8
            | u32::from(warnings.brackets) << 12;
        Ok(())
    }

    /// `TOKENADDR`: R1 the text R0's token stands for in BASIC's token
    /// table, R0 the table; R12 past the second byte of a two-byte token.
    fn token_address(&mut self, regs: &mut Regs) -> machine::Step {
        let token = (regs.r[0] & 0xFF) as u8;
        let second = if crate::token::table::is_prefix(token) {
            let b = self.os.read_byte(regs.r[12])?;
            regs.r[12] += 1;
            Some(b)
        } else {
            None
        };
        let table =
            crate::module::token_table_address(self.os, self.dialect()).ok_or(Error::Mistake)?;
        let offset = crate::token::table::offset_of(token, second).ok_or(Error::Mistake)?;
        regs.r[0] = table;
        regs.r[1] = table + offset;
        Ok(())
    }
}

/// The numeric-type flags a value gives `EXPR`'s caller: Z for a string, N
/// for a real.
pub(crate) fn expr_flags(value: &Value) -> u32 {
    use tairix_riscos_arm::psr;
    match value {
        Value::Str(_) => psr::Z,
        Value::Real(_) => psr::N,
        Value::Int(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{compact, expand};
    use crate::five::Five;

    #[test]
    fn the_four_word_real_carries_every_five_byte_one_and_zero() {
        for n in [1, -1, 2, 100, -12345, i32::MAX, i32::MIN] {
            let x = Five::from_int(n);
            assert_eq!(compact(expand(x)), Ok(x), "{n}");
        }
        assert_eq!(expand(Five::ZERO), [0, 0, 0]);
        assert_eq!(compact([0, 0, 0]), Ok(Five::ZERO));
        let [mantissa, exponent, sign] = expand(Five::from_int(-2));
        assert_eq!(mantissa & 1 << 31, 1 << 31, "normalised");
        assert_eq!(exponent, 0x82);
        assert_eq!(sign, 1 << 31);
    }

    #[test]
    fn a_four_word_real_past_the_exponent_s_range_is_too_big_or_zero() {
        assert_eq!(
            compact([1 << 31, 0x100, 0]),
            Err(crate::messages::Error::TooBig)
        );
        assert_eq!(compact([1 << 31, u32::MAX, 0]), Ok(Five::ZERO));
    }
}
