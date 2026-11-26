//! `stdlib.h`'s conversions, random numbers, integer arithmetic and the
//! searching and sorting, as RISC OS 5.30 answers: `strtol` saturating with
//! `ERANGE`, `rand`'s additive generator, `bsearch`'s probes and `qsort`'s
//! median-of-three quicksort with its closing insertion pass, each compare
//! the client's procedure called as genuine calls it.

use tairix_riscos_kernel::error::BUFFER_OVERFLOW;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::{os as kos, X};

use super::alloc;
use super::ctype::{self, bit};
use super::errno::{self, ERANGE};
use super::statics::{at, RANDOM_WORDS};
use super::stdio::stream;
use super::string::ANY_LENGTH;
use crate::client;
use crate::kernel::os::EXPANDED;
use crate::kernel::statics::Statics;
use crate::kernel::{context, trap};

/// What the integer readers read: the magnitude, whether it passed the
/// most asked, its sign, and where the text it took ends; none where it took
/// none.
struct Parsed {
    magnitude: u64,
    negative: bool,
    overflowed: bool,
    end: u32,
}

/// The value of the digit `b` in any base up to 36.
pub(super) const fn digit(b: u8) -> Option<u32> {
    match b {
        b'0'..=b'9' => Some((b - b'0') as u32),
        b'a'..=b'z' => Some((b - b'a') as u32 + 10),
        b'A'..=b'Z' => Some((b - b'A') as u32 + 10),
        _ => None,
    }
}

/// Where the text at `text` goes on past the white space the locale's table
/// marks.
pub(super) fn past_space(os: &mut Os<'_>, statics: Statics, text: u32) -> Result<u32, Fail> {
    let table = ctype::table(os, statics)?;
    let skipped = os
        .position(text, ANY_LENGTH, |b| {
            table[usize::from(b)] & bit::SPACE == 0
        })?
        .unwrap_or(0);
    Ok(text.wrapping_add(skipped))
}

/// Where an integer's text at `text` goes on past the white space, an
/// optional sign and the white space after it, and whether the sign is a
/// minus.
fn past_space_and_sign(os: &mut Os<'_>, statics: Statics, text: u32) -> Result<(u32, bool), Fail> {
    let at = past_space(os, statics, text)?;
    let sign = os.as_caller().read_byte(at)?;
    if sign == b'-' || sign == b'+' {
        return Ok((past_space(os, statics, at.wrapping_add(1))?, sign == b'-'));
    }
    Ok((at, false))
}

/// The integer at `text` in `base`, nought for the base its prefix names,
/// its magnitude capped at `most`. After the white space and the sign, `0x`
/// commits a base of sixteen or one chosen to hexadecimal digits; any base
/// above one reads the digits and letters below it, but none from one
/// negative.
fn parse(
    os: &mut Os<'_>,
    statics: Statics,
    text: u32,
    base: u32,
    most: u64,
) -> Result<Option<Parsed>, Fail> {
    if base == 1 || base.cast_signed() < 0 {
        return Ok(None);
    }
    let (mut at, negative) = past_space_and_sign(os, statics, text)?;
    let mut base = base;
    if base == 0 || base == 16 {
        let zero = os.as_caller().read_byte(at)? == b'0';
        if zero && matches!(os.as_caller().read_byte(at.wrapping_add(1))?, b'x' | b'X') {
            at = at.wrapping_add(2);
            base = 16;
        } else if base == 0 {
            base = if zero { 8 } else { 10 };
        }
    }
    let (mut magnitude, mut overflowed) = (0u64, false);
    let digits = os
        .as_caller()
        .position(at, ANY_LENGTH, |b| match digit(b) {
            Some(d) if d < base => {
                let next = magnitude
                    .checked_mul(u64::from(base))
                    .and_then(|m| m.checked_add(u64::from(d)));
                match next.filter(|&m| m <= most) {
                    Some(m) => magnitude = m,
                    None => overflowed = true,
                }
                false
            }
            _ => true,
        })?;
    Ok(match digits {
        Some(0) | None => None,
        Some(n) => Some(Parsed {
            magnitude,
            negative,
            overflowed,
            end: at.wrapping_add(n),
        }),
    })
}

/// The integer a reader answers: signed or not, a word or two.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Integer {
    Signed(u32),
    Unsigned(u32),
}

impl Integer {
    /// The largest magnitude the type holds, a signed one's below nought.
    const fn most(self) -> u64 {
        match self {
            Self::Signed(bits) => 1 << (bits - 1),
            Self::Unsigned(bits) => u64::MAX >> (64 - bits),
        }
    }
}

/// Whether a reader's overflow sets `errno`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Overflow {
    Error,
    Quiet,
}

/// The `integer` at `text` in `base`, `endptr` where not nought made where
/// its text ends; nought where there is none. Past the type's range it is
/// the limit its sign passed, `ERANGE` where `overflow` asks; an unsigned
/// one with a minus sign is negated.
fn read_integer(
    os: &mut Os<'_>,
    statics: Statics,
    [text, endptr, base]: [u32; 3],
    integer: Integer,
    overflow: Overflow,
) -> Result<u64, Fail> {
    let most = integer.most();
    let (value, end) = match parse(os, statics, text, base, most)? {
        Some(parsed) => {
            let past = match integer {
                Integer::Signed(_) => {
                    parsed.overflowed || (!parsed.negative && parsed.magnitude == most)
                }
                Integer::Unsigned(_) => parsed.overflowed,
            };
            let value = match (integer, past) {
                (Integer::Signed(_), true) if parsed.negative => most.wrapping_neg(),
                (Integer::Signed(_), true) => most - 1,
                (Integer::Unsigned(_), true) => most,
                (_, false) if parsed.negative => parsed.magnitude.wrapping_neg(),
                (_, false) => parsed.magnitude,
            };
            if past && overflow == Overflow::Error {
                errno::set(os, statics, ERANGE)?;
            }
            (value, parsed.end)
        }
        None => (0, text),
    };
    set_end(os, endptr, end)?;
    Ok(value)
}

/// A word's integer type, and a long long's.
const WORD: u32 = 32;
const LONG_LONG: u32 = 64;

/// `endptr`, where not nought, made `end`.
pub(super) fn set_end(os: &mut Os<'_>, endptr: u32, end: u32) -> Result<(), Fail> {
    if endptr != 0 {
        os.as_caller().write_word(endptr, end)?;
    }
    Ok(())
}

/// `value`'s low word.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a word's integer read wider"
)]
const fn low_word(value: u64) -> u32 {
    value as u32
}

/// `value` answered in R0 and R1, its low word first.
fn answer_long(regs: &mut Regs, value: u64) {
    regs.r[0] = low_word(value);
    regs.r[1] = low_word(value >> 32);
}

/// `strtol(nptr, endptr, base)`.
pub(super) fn strtol(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let value = read_integer(
        os,
        statics,
        [regs.r[0], regs.r[1], regs.r[2]],
        Integer::Signed(WORD),
        Overflow::Error,
    )?;
    regs.r[0] = low_word(value);
    Ok(())
}

/// `strtoul(nptr, endptr, base)`.
pub(super) fn strtoul(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let value = read_integer(
        os,
        statics,
        [regs.r[0], regs.r[1], regs.r[2]],
        Integer::Unsigned(WORD),
        Overflow::Error,
    )?;
    regs.r[0] = low_word(value);
    Ok(())
}

/// `atoi(nptr)` and `atol(nptr)`: `strtol` in base ten.
pub(super) fn atoi(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[1] = 0;
    regs.r[2] = 10;
    strtol(os, regs)
}

/// `strtoll(nptr, endptr, base)`, and `strtoimax`.
pub(crate) fn strtoll(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let args = [regs.r[0], regs.r[1], regs.r[2]];
    let value = read_integer(
        os,
        statics,
        args,
        Integer::Signed(LONG_LONG),
        Overflow::Error,
    )?;
    answer_long(regs, value);
    Ok(())
}

/// `strtoull(nptr, endptr, base)`, and `strtoumax`.
pub(crate) fn strtoull(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let args = [regs.r[0], regs.r[1], regs.r[2]];
    let value = read_integer(
        os,
        statics,
        args,
        Integer::Unsigned(LONG_LONG),
        Overflow::Error,
    )?;
    answer_long(regs, value);
    Ok(())
}

/// `atoll(nptr)`: `strtoll` in base ten, `errno` left as it is.
pub(crate) fn atoll(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let value = read_integer(
        os,
        statics,
        [regs.r[0], 0, 10],
        Integer::Signed(LONG_LONG),
        Overflow::Quiet,
    )?;
    answer_long(regs, value);
    Ok(())
}

/// `llabs(j)`, and `imaxabs`: the most negative value is its own.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(crate) fn llabs(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let j = (u64::from(regs.r[1]) << 32 | u64::from(regs.r[0])).cast_signed();
    answer_long(regs, j.wrapping_abs().cast_unsigned());
    Ok(())
}

/// `lldiv(numer, denom)`, and `imaxdiv`: the quotient toward nought and the
/// remainder, each low word first, written to the structure R0 addresses —
/// the numerator in R1 and R2, the denominator in R3 and on the stack — R0
/// left the quotient's low word, as RISC OS 5.30 leaves it; a divisor of
/// nought a trap.
pub(crate) fn lldiv(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let result = regs.r[0];
    let numer = (u64::from(regs.r[2]) << 32 | u64::from(regs.r[1])).cast_signed();
    let [high] = client::stacked(os)?;
    let denom = (u64::from(high) << 32 | u64::from(regs.r[3])).cast_signed();
    if denom == 0 {
        return trap::at_call(os, trap::DIVIDE_BY_ZERO);
    }
    let (quotient, remainder) = (
        numer.wrapping_div(denom).cast_unsigned(),
        numer.wrapping_rem(denom).cast_unsigned(),
    );
    let words = [quotient, remainder].map(|v| [low_word(v), low_word(v >> 32)]);
    os.as_caller().write_words(result, words.as_flattened())?;
    regs.r[0] = words[0][0];
    Ok(())
}

/// The generator's multiplier and increment, which `srand` fills its state
/// with, and the places its two indexes start at, counted from one.
const MULTIPLIER: u32 = 69069;
const INCREMENT: u32 = 0x66D6_19E1;
const LAG: u32 = 23;
/// The largest value `rand` gives.
const RAND_MAX: u32 = 0x7FFF_FFFF;

/// The generator seeded with `seed`, as `srand` seeds it.
pub(crate) fn seed(os: &mut Os<'_>, statics: Statics, seed: u32) -> Result<(), Fail> {
    let mut state = [0; RANDOM_WORDS as usize];
    let mut x = seed;
    for word in &mut state {
        x = x.wrapping_mul(MULTIPLIER).wrapping_add(INCREMENT);
        *word = x.wrapping_add(x >> 16);
    }
    statics.write(os, at::RANDOM_SEEDED, 1)?;
    statics.write(os, at::RANDOM_SEED, seed.wrapping_add(seed >> 16))?;
    os.as_caller()
        .write_words(statics.field(at::RANDOM_STATE), &state)?;
    os.as_caller()
        .write_words(statics.field(at::RANDOM_INDEXES), &[LAG, RANDOM_WORDS])
}

/// `srand(seed)`.
pub(super) fn srand(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    seed(os, statics, regs.r[0])
}

/// `rand()`: the sum of the words at the two indexes, stored at the second,
/// both indexes then stepping down, past one to the last.
pub(super) fn rand(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let mut indexes = [0; 2];
    os.as_caller()
        .read_words(statics.field(at::RANDOM_INDEXES), &mut indexes)?;
    let place = |index: u32| at::RANDOM_STATE + 4 * (index.wrapping_sub(1) % RANDOM_WORDS);
    let [j, k] = indexes;
    let sum = statics
        .read(os, place(j))?
        .wrapping_add(statics.read(os, place(k))?);
    statics.write(os, place(k), sum)?;
    let step = |index: u32| if index <= 1 { RANDOM_WORDS } else { index - 1 };
    os.as_caller()
        .write_words(statics.field(at::RANDOM_INDEXES), &[step(j), step(k)])?;
    regs.r[0] = sum & RAND_MAX;
    Ok(())
}

/// `abs(j)` and `labs(j)`: the most negative value is its own.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(super) fn abs(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = regs.r[0].cast_signed().wrapping_abs().cast_unsigned();
    Ok(())
}

/// `div(numer, denom)` and `ldiv`: the quotient toward nought and the
/// remainder written to the structure R0 addresses, R0 left the quotient
/// as RISC OS 5.30 leaves it; a divisor of nought a trap.
pub(super) fn div(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (result, numer, denom) = (regs.r[0], regs.r[1].cast_signed(), regs.r[2].cast_signed());
    if denom == 0 {
        return trap::at_call(os, trap::DIVIDE_BY_ZERO);
    }
    let quotient = numer.wrapping_div(denom).cast_unsigned();
    os.as_caller().write_words(
        result,
        &[quotient, numer.wrapping_rem(denom).cast_unsigned()],
    )?;
    regs.r[0] = quotient;
    Ok(())
}

/// The room `getenv` first reads a value into, and the most it grows to.
const VALUE_ROOM: u32 = 256;
const VALUE_MOST: u32 = 256 * 1024;

/// `getenv(name)`: the variable's value expanded, in one buffer of the
/// library's each call reuses; nought where it is not set.
pub(super) fn getenv(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let name = regs.r[0];
    let mut room = VALUE_ROOM;
    regs.r[0] = loop {
        let Some(buffer) = alloc::kept_buffer(os, statics, at::GETENV, room)? else {
            break 0;
        };
        let mut read = stream::registers(&[name, buffer, room - 1, 0, EXPANDED]);
        match os.perform(X | kos::READ_VAR_VAL, &mut read) {
            Ok(()) => {
                os.as_caller()
                    .write_byte(buffer.wrapping_add(read.r[2]), 0)?;
                break buffer;
            }
            Err(Fail::Error(block))
                if room < VALUE_MOST
                    && os.as_caller().read_word(block)? == BUFFER_OVERFLOW.number =>
            {
                room *= 2;
            }
            Err(Fail::Error(_)) => break 0,
            Err(unwind) => return Err(unwind),
        }
    };
    Ok(())
}

/// `system(string)`: whether there is a command line interpreter for a null
/// string, which there is; else every output stream flushed, so what the
/// program wrote comes before what the command writes, and the command run
/// as `_kernel_system` runs it, its answer.
pub(super) fn system(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[0] == 0 {
        regs.r[0] = 1;
        return Ok(());
    }
    let statics = context::statics_at(os, regs.r[10])?;
    stream::flush_all(os, statics)?;
    regs.r[1] = 0;
    crate::kernel::system::system(os, regs)
}

/// What a client's comparison is called with: its procedure, and the
/// caller's stack and frame it runs on.
struct Comparison {
    procedure: u32,
    sp: u32,
    sl: u32,
    fp: u32,
}

impl Comparison {
    fn of(os: &mut Os<'_>, procedure: u32) -> Self {
        let caller = os.registers();
        Self {
            procedure,
            sp: caller.r[13],
            sl: caller.r[10],
            fp: caller.r[11],
        }
    }

    /// How the client orders `a` and `b`.
    fn order(&self, os: &mut Os<'_>, a: u32, b: u32) -> Result<i32, Fail> {
        let mut regs = Regs::default();
        regs.r[0] = a;
        regs.r[1] = b;
        regs.r[10] = self.sl;
        regs.r[11] = self.fp;
        client::call(os, self.procedure, &mut regs, self.sp)?;
        Ok(regs.r[0].cast_signed())
    }
}

/// `bsearch(key, base, nmemb, size, compar)`: probing the middle of what is
/// left, its upper half's first when the count is even.
pub(super) fn bsearch(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (key, base, count, size) = (regs.r[0], regs.r[1], regs.r[2], regs.r[3]);
    let [procedure] = client::stacked(os)?;
    let comparison = Comparison::of(os, procedure);
    let (mut lo, mut left) = (0u32, count);
    while left > 0 {
        let half = left / 2;
        let element = base.wrapping_add(size.wrapping_mul(lo + half));
        match comparison.order(os, key, element)? {
            0 => {
                regs.r[0] = element;
                return Ok(());
            }
            order if order < 0 => left = half,
            _ => {
                lo += half + 1;
                left -= half + 1;
            }
        }
    }
    regs.r[0] = 0;
    Ok(())
}

/// The array `qsort` sorts.
struct Elements {
    base: u32,
    size: u32,
}

/// The most of two elements swapped at once.
const SWAP_CHUNK: u32 = 256;

impl Elements {
    fn at(&self, n: u32) -> u32 {
        self.base.wrapping_add(self.size.wrapping_mul(n))
    }

    fn swap(&self, os: &mut Os<'_>, first: u32, second: u32) -> Result<(), Fail> {
        if first == second {
            return Ok(());
        }
        let (first_at, second_at) = (self.at(first), self.at(second));
        let (mut left, mut right) = ([0; SWAP_CHUNK as usize], [0; SWAP_CHUNK as usize]);
        let mut done = 0;
        while done < self.size {
            let span = (self.size - done).min(SWAP_CHUNK);
            let (left, right) = (&mut left[..span as usize], &mut right[..span as usize]);
            os.as_caller()
                .read_bytes(first_at.wrapping_add(done), left)?;
            os.as_caller()
                .read_bytes(second_at.wrapping_add(done), right)?;
            os.as_caller()
                .write_bytes(first_at.wrapping_add(done), right)?;
            os.as_caller()
                .write_bytes(second_at.wrapping_add(done), left)?;
            done += span;
        }
        Ok(())
    }
}

/// A range RISC OS 5.30 leaves to the closing insertion pass.
const SMALL: u32 = 10;
/// The ranges a sort can have waiting: each is at most half of the one
/// before it, so a 32-bit count needs no more.
const PENDING_MOST: usize = 32;

/// Elements `first` on, `count` of them.
#[derive(Clone, Copy)]
struct Range {
    first: u32,
    count: u32,
}

impl Range {
    const NONE: Self = Self { first: 0, count: 0 };
}

/// `qsort(base, nmemb, size, compar)`. The scans run as genuine's do, past
/// the range for a comparison that orders inconsistently.
pub(super) fn qsort(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let elements = Elements {
        base: regs.r[0],
        size: regs.r[2],
    };
    let count = regs.r[1];
    let comparison = Comparison::of(os, regs.r[3]);
    let order =
        |os: &mut Os<'_>, a: u32, b: u32| comparison.order(os, elements.at(a), elements.at(b));
    let mut pending = [Range::NONE; PENDING_MOST];
    let mut waiting = 0;
    let mut next = Some(Range { first: 0, count }).filter(|range| range.count > SMALL);
    while let Some(range) = next.take() {
        let (lo, hi) = (range.first, range.first + range.count - 1);
        let mid = lo + range.count / 2;
        if order(os, lo, mid)? > 0 {
            elements.swap(os, lo, mid)?;
        }
        if order(os, mid, hi)? > 0 {
            if order(os, lo, hi)? > 0 {
                elements.swap(os, lo, hi)?;
            }
            elements.swap(os, mid, hi)?;
        }
        let pivot = hi - 1;
        elements.swap(os, mid, pivot)?;
        let (mut i, mut j) = (lo + 1, hi - 2);
        loop {
            while order(os, i, pivot)? < 0 {
                i = i.wrapping_add(1);
            }
            while order(os, j, pivot)? > 0 {
                j = j.wrapping_sub(1);
            }
            if i >= j {
                break;
            }
            elements.swap(os, i, j)?;
            i = i.wrapping_add(1);
            j = j.wrapping_sub(1);
        }
        elements.swap(os, i, pivot)?;
        // Within its range unless the comparison ordered inconsistently;
        // a partition that left it is sorted no further.
        let (left, right) = if (lo + 1..=hi + 1).contains(&i) {
            let left = Range {
                first: lo,
                count: i - lo - 1,
            };
            let right = Range {
                first: i,
                count: hi - i + 1,
            };
            (left, right)
        } else {
            (Range::NONE, Range::NONE)
        };
        let (smaller, larger) = if left.count <= right.count {
            (left, right)
        } else {
            (right, left)
        };
        if larger.count > SMALL && waiting < PENDING_MOST {
            pending[waiting] = larger;
            waiting += 1;
        }
        next = if smaller.count > SMALL {
            Some(smaller)
        } else if waiting > 0 {
            waiting -= 1;
            Some(pending[waiting])
        } else {
            None
        };
    }
    for i in 1..count {
        let mut j = i;
        while j > 0 && order(os, j - 1, i)? > 0 {
            j -= 1;
        }
        for k in (j..i).rev() {
            elements.swap(os, k, k + 1)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
