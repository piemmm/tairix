//! The `scanf` family as RISC OS 5.30's library reads: C99's directives, but
//! a scanset of single characters with no ranges, `%u` refusing a sign, an
//! integer kept to its low bits, and one character of lookahead, so what a
//! failed conversion took is lost. A floating-point number is read as
//! `strtod` reads one.

use ::alloc::vec::Vec;

use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_softfloat::fpa_words;

use super::format::{length_and_conversion, store, Arguments, Size, VaList, Variadic};
use super::stream::{self, Stream, EOF};
use crate::c::ctype::{self, bit};
use crate::c::fpa::single_bits;
use crate::c::real;
use crate::c::stdlib;
use crate::c::string::ANY_LENGTH;
use crate::kernel::context;
use crate::kernel::statics::Statics;

/// Where a scan's characters come from, one at a time; the last of them can
/// be given back.
trait Input {
    fn next(&mut self, os: &mut Os<'_>) -> Result<Option<u8>, Fail>;
    fn back(&mut self, os: &mut Os<'_>, c: u8) -> Result<(), Fail>;
}

/// A string's characters, to its terminator.
struct FromMemory(u32);

impl Input for FromMemory {
    fn next(&mut self, os: &mut Os<'_>) -> Result<Option<u8>, Fail> {
        let c = os.as_caller().read_byte(self.0)?;
        if c == 0 {
            return Ok(None);
        }
        self.0 = self.0.wrapping_add(1);
        Ok(Some(c))
    }

    fn back(&mut self, _os: &mut Os<'_>, _c: u8) -> Result<(), Fail> {
        self.0 = self.0.wrapping_sub(1);
        Ok(())
    }
}

/// A stream's characters, as `getc` reads them and `ungetc` gives one back.
struct FromStream {
    stream: Stream,
    statics: Statics,
}

impl Input for FromStream {
    fn next(&mut self, os: &mut Os<'_>) -> Result<Option<u8>, Fail> {
        let c = self.stream.getc(os, self.statics)?;
        Ok((c != EOF).then(|| c.to_le_bytes()[0]))
    }

    fn back(&mut self, _os: &mut Os<'_>, c: u8) -> Result<(), Fail> {
        self.stream.unget(u32::from(c));
        Ok(())
    }
}

/// Why a scan stopped short of its format's end.
enum Stop {
    /// Its input ended.
    Input,
    /// A character or a directive did not match.
    Matching,
}

/// A scan in progress: its input, the character looked at and not yet
/// taken, how many have been taken, and the locale's white space.
struct Scanner<'a, I: Input> {
    input: &'a mut I,
    ahead: Option<u8>,
    taken: u32,
    space: [bool; 256],
}

impl<I: Input> Scanner<'_, I> {
    fn peek(&mut self, os: &mut Os<'_>) -> Result<Option<u8>, Fail> {
        if self.ahead.is_none() {
            self.ahead = self.input.next(os)?;
        }
        Ok(self.ahead)
    }

    /// The character looked at taken: counted as `%n`'s `int` counts it,
    /// wrapping past four gigabytes of input.
    fn take(&mut self) {
        self.taken = self
            .taken
            .wrapping_add(u32::from(self.ahead.take().is_some()));
    }

    /// The next character as `map` reads it, taken where it reads and
    /// `width` has room for it.
    fn take_mapped<T>(
        &mut self,
        os: &mut Os<'_>,
        width: &mut u32,
        map: impl Fn(u8) -> Option<T>,
    ) -> Result<Option<T>, Fail> {
        if *width == 0 {
            return Ok(None);
        }
        let Some(value) = self.peek(os)?.and_then(map) else {
            return Ok(None);
        };
        self.take();
        *width -= 1;
        Ok(Some(value))
    }

    fn take_if(
        &mut self,
        os: &mut Os<'_>,
        width: &mut u32,
        wanted: impl Fn(u8) -> bool,
    ) -> Result<Option<u8>, Fail> {
        self.take_mapped(os, width, |c| wanted(c).then_some(c))
    }

    /// Past the input's white space: whether any input is left.
    fn skip_space(&mut self, os: &mut Os<'_>) -> Result<bool, Fail> {
        while let Some(c) = self.peek(os)? {
            if !self.space[usize::from(c)] {
                return Ok(true);
            }
            self.take();
        }
        Ok(false)
    }

    /// An integer in `radix` — nought for the one its prefix names — after a
    /// sign where `signed`: its low 64 bits, none where it has no digit.
    fn integer(
        &mut self,
        os: &mut Os<'_>,
        mut width: u32,
        radix: u32,
        signed: bool,
    ) -> Result<Option<u64>, Fail> {
        let sign = if signed {
            self.take_if(os, &mut width, |c| c == b'-' || c == b'+')?
        } else {
            None
        };
        let mut radix = radix;
        let mut digits = 0u32;
        if (radix == 0 || radix == 16) && self.take_if(os, &mut width, |c| c == b'0')?.is_some() {
            if self
                .take_if(os, &mut width, |c| c.eq_ignore_ascii_case(&b'x'))?
                .is_some()
            {
                radix = 16;
            } else {
                digits = 1;
                if radix == 0 {
                    radix = 8;
                }
            }
        }
        if radix == 0 {
            radix = 10;
        }
        let mut value = 0u64;
        while let Some(d) =
            self.take_mapped(os, &mut width, |c| stdlib::digit(c).filter(|&d| d < radix))?
        {
            value = value
                .wrapping_mul(u64::from(radix))
                .wrapping_add(u64::from(d));
            digits += 1;
        }
        Ok((digits > 0).then(|| {
            if sign == Some(b'-') {
                value.wrapping_neg()
            } else {
                value
            }
        }))
    }

    /// A floating-point number's text, as much as `strtod` would read; none
    /// where that is not a number.
    fn real_text(
        &mut self,
        os: &mut Os<'_>,
        width: u32,
        point: u8,
    ) -> Result<Option<Vec<u8>>, Fail> {
        let mut taken = Taken {
            scanner: self,
            width,
            text: Vec::new(),
        };
        taken.one(os, |c| c == b'+' || c == b'-')?;
        let whole = match taken.peek(os)? {
            Some(b'i' | b'I') => taken.infinity(os)?,
            Some(b'n' | b'N') => taken.word(os, b"nan")? && taken.nan_payload(os)?,
            _ => taken.digits(os, point)?,
        };
        Ok(whole.then_some(taken.text))
    }
}

/// The text a conversion has taken, within its width.
struct Taken<'s, 'a, I: Input> {
    scanner: &'s mut Scanner<'a, I>,
    width: u32,
    text: Vec<u8>,
}

impl<I: Input> Taken<'_, '_, I> {
    /// The next character, where the width has room for it.
    fn peek(&mut self, os: &mut Os<'_>) -> Result<Option<u8>, Fail> {
        if self.width == 0 {
            return Ok(None);
        }
        self.scanner.peek(os)
    }

    /// The next character taken where `wanted`: whether it was.
    fn one(&mut self, os: &mut Os<'_>, wanted: impl Fn(u8) -> bool) -> Result<bool, Fail> {
        let Some(c) = self.scanner.take_if(os, &mut self.width, wanted)? else {
            return Ok(false);
        };
        self.text.try_reserve(1).map_err(|_| os.no_memory())?;
        self.text.push(c);
        Ok(true)
    }

    /// The characters taken while `wanted`: how many.
    fn run(&mut self, os: &mut Os<'_>, wanted: impl Fn(u8) -> bool + Copy) -> Result<u32, Fail> {
        let mut count = 0;
        while self.one(os, wanted)? {
            count += 1;
        }
        Ok(count)
    }

    /// `word`'s letters taken, in either case: whether all of them were.
    fn word(&mut self, os: &mut Os<'_>, word: &[u8]) -> Result<bool, Fail> {
        for &letter in word {
            if !self.one(os, |c| c.eq_ignore_ascii_case(&letter))? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// `inf` or `infinity`: whether whole.
    fn infinity(&mut self, os: &mut Os<'_>) -> Result<bool, Fail> {
        if !self.word(os, b"inf")? {
            return Ok(false);
        }
        if matches!(self.peek(os)?, Some(b'i' | b'I')) {
            return self.word(os, b"inity");
        }
        Ok(true)
    }

    /// `nan`'s optional `(n-char-sequence)`: whether whole.
    fn nan_payload(&mut self, os: &mut Os<'_>) -> Result<bool, Fail> {
        if !self.one(os, |c| c == b'(')? {
            return Ok(true);
        }
        self.run(os, |c| c.is_ascii_alphanumeric() || c == b'_')?;
        self.one(os, |c| c == b')')
    }

    /// A number's digits, decimal or after `0x` hexadecimal, at most one
    /// `point` among them, and its exponent: whether they make a number.
    fn digits(&mut self, os: &mut Os<'_>, point: u8) -> Result<bool, Fail> {
        let (mut radix, mut marker, mut digits) = (10, b'e', 0);
        if self.one(os, |c| c == b'0')? {
            digits = 1;
            if self.one(os, |c| c.eq_ignore_ascii_case(&b'x'))? {
                (radix, marker, digits) = (16, b'p', 0);
            }
        }
        let in_radix = move |c: u8| stdlib::digit(c).is_some_and(|d| d < radix);
        digits += self.run(os, in_radix)?;
        if self.one(os, |c| c == point)? {
            digits += self.run(os, in_radix)?;
        }
        if digits == 0 {
            return Ok(false);
        }
        if !self.one(os, |c| c.eq_ignore_ascii_case(&marker))? {
            return Ok(true);
        }
        self.one(os, |c| c == b'+' || c == b'-')?;
        Ok(self.run(os, |c| c.is_ascii_digit())? > 0)
    }
}

/// A scanset's characters after `[` at `at`: which characters it takes, and
/// where the format goes on past it; none where it has no closing `]`.
fn scanset(os: &mut Os<'_>, mut at: u32) -> Result<Option<([bool; 256], u32)>, Fail> {
    let complement = os.as_caller().read_byte(at)? == b'^';
    if complement {
        at = at.wrapping_add(1);
    }
    let mut set = [complement; 256];
    let mut first = true;
    loop {
        let c = os.as_caller().read_byte(at)?;
        at = at.wrapping_add(1);
        match c {
            0 => return Ok(None),
            b']' if !first => return Ok(Some((set, at))),
            _ => set[usize::from(c)] = !complement,
        }
        first = false;
    }
}

/// A scan's conversions: those made, and those of them assigned.
#[derive(Default)]
struct Counts {
    converted: u32,
    assigned: u32,
}

/// The format at `format` scanned from `input`, its targets from `args`:
/// how many were assigned, or `EOF` where the input ended before the first
/// conversion.
fn scan(
    os: &mut Os<'_>,
    statics: Statics,
    input: &mut impl Input,
    format: u32,
    args: &mut impl Arguments,
) -> Result<u32, Fail> {
    let space = ctype::table(os, statics)?.map(|class| class & bit::SPACE != 0);
    let point = real::decimal_point(os, statics)?;
    let mut scanner = Scanner {
        input,
        ahead: None,
        taken: 0,
        space,
    };
    let mut counts = Counts::default();
    let stop = directives(os, &mut scanner, format, args, point, &mut counts)?;
    if let Some(c) = scanner.ahead {
        scanner.input.back(os, c)?;
    }
    Ok(match stop {
        Some(Stop::Input) if counts.converted == 0 => EOF,
        _ => counts.assigned,
    })
}

/// The format's directives worked in turn: why the scan stopped short, if it
/// did.
fn directives<I: Input>(
    os: &mut Os<'_>,
    scanner: &mut Scanner<'_, I>,
    format: u32,
    args: &mut impl Arguments,
    point: u8,
    counts: &mut Counts,
) -> Result<Option<Stop>, Fail> {
    let mut at = format;
    loop {
        let c = os.as_caller().read_byte(at)?;
        at = at.wrapping_add(1);
        let stop = match c {
            0 => return Ok(None),
            b'%' => {
                let (stop, past) = conversion(os, scanner, at, args, point, counts)?;
                at = past;
                stop
            }
            _ if scanner.space[usize::from(c)] => {
                scanner.skip_space(os)?;
                None
            }
            _ => literal(os, scanner, c)?,
        };
        if stop.is_some() {
            return Ok(stop);
        }
    }
}

/// An ordinary character of the format matched.
fn literal<I: Input>(
    os: &mut Os<'_>,
    scanner: &mut Scanner<'_, I>,
    c: u8,
) -> Result<Option<Stop>, Fail> {
    match scanner.peek(os)? {
        None => Ok(Some(Stop::Input)),
        Some(got) if got == c => {
            scanner.take();
            Ok(None)
        }
        Some(_) => Ok(Some(Stop::Matching)),
    }
}

/// The next target's address, none where its conversion is suppressed.
fn target(
    os: &mut Os<'_>,
    args: &mut impl Arguments,
    suppressed: bool,
) -> Result<Option<u32>, Fail> {
    if suppressed {
        return Ok(None);
    }
    args.word(os).map(Some)
}

/// The directive after a `%` at `at` worked: why the scan stops there, if it
/// does, and where the format goes on.
fn conversion<I: Input>(
    os: &mut Os<'_>,
    scanner: &mut Scanner<'_, I>,
    mut at: u32,
    args: &mut impl Arguments,
    point: u8,
    counts: &mut Counts,
) -> Result<(Option<Stop>, u32), Fail> {
    let mut c = os.as_caller().read_byte(at)?;
    at = at.wrapping_add(1);
    let suppressed = c == b'*';
    if suppressed {
        c = os.as_caller().read_byte(at)?;
        at = at.wrapping_add(1);
    }
    let mut width = 0u32;
    while c.is_ascii_digit() {
        width = width.saturating_mul(10).saturating_add(u32::from(c - b'0'));
        c = os.as_caller().read_byte(at)?;
        at = at.wrapping_add(1);
    }
    let width = (width != 0).then_some(width);
    let (length, letter, mut at) = length_and_conversion(os, c, at)?;
    if !matches!(letter, b'c' | b'[' | b'n') && !scanner.skip_space(os)? {
        return Ok((Some(Stop::Input), at));
    }
    let stop = match letter {
        b'%' => return Ok((literal(os, scanner, b'%')?, at)),
        b'n' => {
            if let Some(pointer) = target(os, args, suppressed)? {
                store(os, pointer, length.size, u64::from(scanner.taken))?;
            }
            return Ok((None, at));
        }
        b'c' => {
            let pointer = target(os, args, suppressed)?;
            characters(os, scanner, pointer, width.unwrap_or(1))?
        }
        b's' | b'[' => {
            let set = if letter == b'[' {
                let Some((set, past)) = scanset(os, at)? else {
                    return Ok((Some(Stop::Matching), at));
                };
                at = past;
                set
            } else {
                scanner.space.map(|space| !space)
            };
            let pointer = target(os, args, suppressed)?;
            string(os, scanner, pointer, &set, width.unwrap_or(ANY_LENGTH))?
        }
        b'd' | b'i' | b'u' | b'o' | b'x' | b'X' | b'p' => {
            let radix = match letter {
                b'i' => 0,
                b'o' => 8,
                b'x' | b'X' | b'p' => 16,
                _ => 10,
            };
            match scanner.integer(os, width.unwrap_or(ANY_LENGTH), radix, letter != b'u')? {
                Some(value) => {
                    if let Some(pointer) = target(os, args, suppressed)? {
                        let size = if letter == b'p' {
                            Size::Word
                        } else {
                            length.size
                        };
                        store(os, pointer, size, value)?;
                    }
                    None
                }
                None => Some(Stop::Matching),
            }
        }
        b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'a' | b'A' => {
            let number = match scanner.real_text(os, width.unwrap_or(ANY_LENGTH), point)? {
                Some(text) => real::parse(os, &mut text.as_slice(), point)?,
                None => None,
            };
            match number {
                Some(number) => {
                    if let Some(pointer) = target(os, args, suppressed)? {
                        if length.long {
                            os.as_caller()
                                .write_words(pointer, &fpa_words(number.bits))?;
                        } else {
                            os.as_caller()
                                .write_word(pointer, single_bits(f64::from_bits(number.bits)))?;
                        }
                    }
                    None
                }
                None => Some(Stop::Matching),
            }
        }
        _ => Some(Stop::Matching),
    };
    if stop.is_none() {
        counts.converted += 1;
        counts.assigned += u32::from(!suppressed);
    }
    Ok((stop, at))
}

/// `%c`: `count` characters, stored unterminated at `pointer`.
fn characters<I: Input>(
    os: &mut Os<'_>,
    scanner: &mut Scanner<'_, I>,
    pointer: Option<u32>,
    count: u32,
) -> Result<Option<Stop>, Fail> {
    for n in 0..count {
        let Some(c) = scanner.peek(os)? else {
            return Ok(Some(Stop::Input));
        };
        scanner.take();
        if let Some(pointer) = pointer {
            os.as_caller().write_byte(pointer.wrapping_add(n), c)?;
        }
    }
    Ok(None)
}

/// `%s` and `%[`: the characters in `set`, at least one and at most `width`,
/// stored terminated at `pointer`.
fn string<I: Input>(
    os: &mut Os<'_>,
    scanner: &mut Scanner<'_, I>,
    pointer: Option<u32>,
    set: &[bool; 256],
    mut width: u32,
) -> Result<Option<Stop>, Fail> {
    let mut count = 0u32;
    while let Some(c) = scanner.take_if(os, &mut width, |c| set[usize::from(c)])? {
        if let Some(pointer) = pointer {
            os.as_caller().write_byte(pointer.wrapping_add(count), c)?;
        }
        count += 1;
    }
    if count == 0 {
        return Ok(Some(if scanner.peek(os)?.is_none() {
            Stop::Input
        } else {
            Stop::Matching
        }));
    }
    if let Some(pointer) = pointer {
        os.as_caller().write_byte(pointer.wrapping_add(count), 0)?;
    }
    Ok(None)
}

/// `scanf(format, ...)`: from `stdin`.
pub(in crate::c) fn scanf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let mut args = Variadic::of(os, regs, 1);
    regs.r[0] = from_stream(os, statics, stream::stdin(statics), regs.r[0], &mut args)?;
    Ok(())
}

/// `fscanf(stream, format, ...)`.
pub(in crate::c) fn fscanf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let mut args = Variadic::of(os, regs, 2);
    regs.r[0] = from_stream(os, statics, regs.r[0], regs.r[1], &mut args)?;
    Ok(())
}

/// `sscanf(s, format, ...)`.
pub(in crate::c) fn sscanf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let mut args = Variadic::of(os, regs, 2);
    regs.r[0] = scan(
        os,
        statics,
        &mut FromMemory(regs.r[0]),
        regs.r[1],
        &mut args,
    )?;
    Ok(())
}

/// `vfscanf(stream, format, arg)`: the list left past the pointers taken.
pub(crate) fn vfscanf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let mut args = VaList::of(os, regs.r[2])?;
    regs.r[0] = from_stream(os, statics, regs.r[0], regs.r[1], &mut args)?;
    args.advanced(os)
}

/// `vscanf(format, arg)`: from `stdin`.
pub(crate) fn vscanf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let mut args = VaList::of(os, regs.r[1])?;
    regs.r[0] = from_stream(os, statics, stream::stdin(statics), regs.r[0], &mut args)?;
    args.advanced(os)
}

/// `vsscanf(s, format, arg)`.
pub(crate) fn vsscanf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let mut args = VaList::of(os, regs.r[2])?;
    regs.r[0] = scan(
        os,
        statics,
        &mut FromMemory(regs.r[0]),
        regs.r[1],
        &mut args,
    )?;
    args.advanced(os)
}

/// The format at `format` scanned from the stream at `at`.
fn from_stream(
    os: &mut Os<'_>,
    statics: Statics,
    at: u32,
    format: u32,
    args: &mut impl Arguments,
) -> Result<u32, Fail> {
    let mut input = FromStream {
        stream: Stream::load(os, at)?,
        statics,
    };
    let scanned = scan(os, statics, &mut input, format, args)?;
    input.stream.store(os)?;
    Ok(scanned)
}

#[cfg(test)]
mod tests;
