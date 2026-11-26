//! The `printf` family's conversions as RISC OS 5.30's library writes them:
//! C99's flags, widths, precisions and lengths; an unknown conversion written
//! as its letter alone; a floating-point value's seventeen significant digits,
//! correctly rounded, rounded on half to even to the precision asked, with
//! noughts past them; and in the `_printf` family, which takes no floating
//! point, a floating-point conversion written as the string at address
//! nought.

use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_softfloat::{Binary, Decimal, Mode, Unpacked};

use crate::c::fpa::{biased_exponent, BIAS, FRACTION};
use crate::c::string::ANY_LENGTH;

/// Where a format's arguments come from: the caller's words in turn.
pub(crate) trait Arguments {
    fn word(&mut self, os: &mut Os<'_>) -> Result<u32, Fail>;

    /// Where the words not yet taken lie, one after another in memory.
    fn rest(&mut self, os: &mut Os<'_>) -> Result<u32, Fail>;
}

/// Where a format's text goes.
pub(crate) trait Sink {
    fn put(&mut self, os: &mut Os<'_>, bytes: &[u8]) -> Result<(), Fail>;

    /// How many of the bytes to come it keeps; the rest it only counts.
    fn keeps(&self) -> usize {
        usize::MAX
    }

    /// `bytes` more it only counts.
    fn count(&mut self, _bytes: usize) {}
}

/// The variadic words of a call: the argument registers from the first
/// variadic one, then the caller's stack.
pub(crate) struct Variadic {
    registers: [u32; 4],
    next: usize,
    stack: u32,
}

impl Variadic {
    pub(crate) fn of(os: &mut Os<'_>, regs: &Regs, first: usize) -> Self {
        Self {
            registers: [regs.r[0], regs.r[1], regs.r[2], regs.r[3]],
            next: first,
            stack: os.registers().r[13],
        }
    }
}

impl Arguments for Variadic {
    fn word(&mut self, os: &mut Os<'_>) -> Result<u32, Fail> {
        let word = if let Some(&word) = self.registers.get(self.next) {
            word
        } else {
            let index = u32::try_from(self.next - self.registers.len()).unwrap_or(u32::MAX);
            os.as_caller()
                .read_word(self.stack.wrapping_add(index.wrapping_mul(4)))?
        };
        self.next += 1;
        Ok(word)
    }

    /// The registers not yet taken stored just below the caller's stack, so
    /// they and the stacked words run on unbroken.
    fn rest(&mut self, os: &mut Os<'_>) -> Result<u32, Fail> {
        let left = self.registers.get(self.next..).unwrap_or_default();
        let spilled = u32::try_from(4 * left.len()).unwrap_or(0);
        let at = self.stack.wrapping_sub(spilled);
        os.as_caller().write_words(at, left)?;
        let past =
            u32::try_from(self.next.saturating_sub(self.registers.len())).unwrap_or(u32::MAX);
        Ok(at.wrapping_add(past.wrapping_mul(4)))
    }
}

/// A `va_list`'s words: from the argument pointer the list holds.
pub(crate) struct VaList {
    list: u32,
    pointer: u32,
}

impl VaList {
    pub(crate) fn of(os: &mut Os<'_>, list: u32) -> Result<Self, Fail> {
        Ok(Self {
            list,
            pointer: os.as_caller().read_word(list)?,
        })
    }

    /// The list left past the words taken.
    pub(crate) fn advanced(&self, os: &mut Os<'_>) -> Result<(), Fail> {
        os.as_caller().write_word(self.list, self.pointer)
    }
}

impl Arguments for VaList {
    fn word(&mut self, os: &mut Os<'_>) -> Result<u32, Fail> {
        let word = os.as_caller().read_word(self.pointer)?;
        self.pointer = self.pointer.wrapping_add(4);
        Ok(word)
    }

    fn rest(&mut self, _os: &mut Os<'_>) -> Result<u32, Fail> {
        Ok(self.pointer)
    }
}

/// Whether a family converts floating point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Floats {
    Converted,
    /// As the `_printf` family: each such conversion takes a double's words
    /// and writes the string at address nought.
    Absent,
}

/// The most of the string at address nought a float-less conversion writes.
const ZERO_PAGE_TEXT: usize = 1 << 16;
/// The most a width or precision is taken as: C's `INT_MAX`.
const MOST_FIELD: usize = i32::MAX as usize;
/// The most of a format's text, or of a string it writes, held at once.
const TEXT_RUN: usize = 4096;
/// A floating-point value's significant digits, as the library writes them.
const SIGNIFICANT_DIGITS: u32 = 17;
const SIGNIFICANT: usize = SIGNIFICANT_DIGITS as usize;
/// `%p`'s digits.
const POINTER_DIGITS: usize = 8;
/// A run of padding written at a time.
const SPACES: [u8; 64] = [b' '; 64];
const ZEROS: [u8; 64] = [b'0'; 64];

/// An integer argument's size.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Size {
    Byte,
    Halfword,
    Word,
    Doubleword,
}

impl Size {
    const fn bits(self) -> u32 {
        match self {
            Self::Byte => 8,
            Self::Halfword => 16,
            Self::Word => 32,
            Self::Doubleword => 64,
        }
    }

    /// `value`'s low bits as this size holds them.
    const fn narrowed(self, value: u64) -> u64 {
        value & u64::MAX >> (64 - self.bits())
    }

    /// `value`'s low bits as this size holds them, signed: whether negative,
    /// and the magnitude.
    const fn sign_and_magnitude(self, value: u64) -> (bool, u64) {
        let shift = 64 - self.bits();
        let signed = (value << shift).cast_signed() >> shift;
        (signed < 0, signed.unsigned_abs())
    }
}

/// A directive's length modifiers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Length {
    pub(crate) size: Size,
    /// An `l` or `L` among them, which makes a scanned floating-point target
    /// a double.
    pub(crate) long: bool,
}

/// The length modifiers from `c`, the format read on from `at`: them, the
/// conversion letter after them, and the address past that letter.
pub(crate) fn length_and_conversion(
    os: &mut Os<'_>,
    mut c: u8,
    mut at: u32,
) -> Result<(Length, u8, u32), Fail> {
    let mut length = Length {
        size: Size::Word,
        long: false,
    };
    loop {
        match c {
            b'h' if length.size == Size::Halfword => length.size = Size::Byte,
            b'h' => length.size = Size::Halfword,
            b'l' if length.size == Size::Word => {
                length.long = true;
                if os.as_caller().read_byte(at)? == b'l' {
                    at = at.wrapping_add(1);
                    length.size = Size::Doubleword;
                }
            }
            b'j' => length.size = Size::Doubleword,
            b'L' => length.long = true,
            b'z' | b't' => {}
            _ => return Ok((length, c, at)),
        }
        c = os.as_caller().read_byte(at)?;
        at = at.wrapping_add(1);
    }
}

/// `value`'s low bytes stored at `pointer` in `size`.
pub(crate) fn store(os: &mut Os<'_>, pointer: u32, size: Size, value: u64) -> Result<(), Fail> {
    let bytes = value.to_le_bytes();
    os.as_caller()
        .write_bytes(pointer, &bytes[..(size.bits() / 8) as usize])
}

/// A conversion's flags, width and precision.
#[derive(Clone, Copy, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the flags, each set by a character of its own"
)]
struct Spec {
    left: bool,
    plus: bool,
    space: bool,
    alternate: bool,
    zero: bool,
    width: usize,
    precision: Option<usize>,
}

/// A part of a conversion's text: bytes, a run of noughts, or the guest's
/// bytes from an address.
#[derive(Clone, Copy)]
enum Piece<'a> {
    Bytes(&'a [u8]),
    Zeros(usize),
    Guest(u32, usize),
}

impl Piece<'_> {
    fn len(self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes.len(),
            Self::Zeros(count) | Self::Guest(_, count) => count,
        }
    }
}

/// The format at `format` written to `sink`: the bytes written.
pub(crate) fn format(
    os: &mut Os<'_>,
    format: u32,
    args: &mut impl Arguments,
    sink: &mut impl Sink,
    floats: Floats,
) -> Result<u32, Fail> {
    let mut written = 0usize;
    let mut at = format;
    loop {
        let mut met = 0;
        let (text, ended) = os.as_caller().read_until(at, TEXT_RUN, |b| {
            met = b;
            b == 0 || b == b'%'
        })?;
        if !text.is_empty() {
            sink.put(os, &text)?;
            written = written.saturating_add(text.len());
            at = at.wrapping_add(len32(text.len()));
        }
        os.yield_point()?;
        if !ended {
            continue;
        }
        if met == 0 {
            break;
        }
        at = at.wrapping_add(1);
        let (spec, length, conversion, next) = directive(os, at, args)?;
        at = next;
        if conversion == 0 {
            break;
        }
        let wrote = convert(
            os,
            args,
            sink,
            floats,
            spec,
            length.size,
            conversion,
            written,
        )?;
        written = written.saturating_add(wrote);
    }
    Ok(len32(written))
}

/// The directive after a `%` at `at`: its spec, its argument's length, its
/// conversion letter — nought where the format ends first — and where the
/// format goes on.
fn directive(
    os: &mut Os<'_>,
    mut at: u32,
    args: &mut impl Arguments,
) -> Result<(Spec, Length, u8, u32), Fail> {
    let mut spec = Spec::default();
    let next = |at: &mut u32, os: &mut Os<'_>| -> Result<u8, Fail> {
        let b = os.as_caller().read_byte(*at)?;
        *at = at.wrapping_add(1);
        Ok(b)
    };
    let mut c = next(&mut at, os)?;
    loop {
        match c {
            b'-' => spec.left = true,
            b'+' => spec.plus = true,
            b' ' => spec.space = true,
            b'#' => spec.alternate = true,
            b'0' => spec.zero = true,
            _ => break,
        }
        c = next(&mut at, os)?;
    }
    let field = |value: usize, digit: u8| {
        value
            .saturating_mul(10)
            .saturating_add(usize::from(digit - b'0'))
            .min(MOST_FIELD)
    };
    if c == b'*' {
        let width = args.word(os)?.cast_signed();
        spec.left |= width < 0;
        spec.width = (width.unsigned_abs() as usize).min(MOST_FIELD);
        c = next(&mut at, os)?;
    } else {
        while c.is_ascii_digit() {
            spec.width = field(spec.width, c);
            c = next(&mut at, os)?;
        }
    }
    if c == b'.' {
        c = next(&mut at, os)?;
        if c == b'*' {
            let precision = args.word(os)?.cast_signed();
            spec.precision = usize::try_from(precision).ok();
            c = next(&mut at, os)?;
        } else {
            let mut precision = 0;
            while c.is_ascii_digit() {
                precision = field(precision, c);
                c = next(&mut at, os)?;
            }
            spec.precision = Some(precision);
        }
    }
    let (length, conversion, at) = length_and_conversion(os, c, at)?;
    Ok((spec, length, conversion, at))
}

/// One conversion written: the bytes it wrote.
#[expect(clippy::too_many_arguments, reason = "a conversion's whole context")]
fn convert(
    os: &mut Os<'_>,
    args: &mut impl Arguments,
    sink: &mut impl Sink,
    floats: Floats,
    spec: Spec,
    size: Size,
    conversion: u8,
    written: usize,
) -> Result<usize, Fail> {
    match conversion {
        b'd' | b'i' | b'u' | b'o' | b'x' | b'X' => {
            let value = integer(os, args, size)?;
            emit_integer(os, sink, spec, size, conversion, value)
        }
        b'p' => {
            let value = u64::from(args.word(os)?);
            let spec = Spec {
                precision: Some(spec.precision.unwrap_or(POINTER_DIGITS)),
                alternate: false,
                plus: false,
                space: false,
                ..spec
            };
            emit_integer(os, sink, spec, Size::Word, b'x', value)
        }
        b'c' => {
            let byte = args.word(os)?.to_le_bytes()[0];
            emit(os, sink, spec, false, b"", &[Piece::Bytes(&[byte])])
        }
        b's' => {
            let pointer = args.word(os)?;
            // A precision bounds what is read: nothing past it need exist.
            let most = spec.precision.map_or(ANY_LENGTH, len32);
            let length = os
                .as_caller()
                .position(pointer, most, |b| b == 0)?
                .unwrap_or(most);
            let text = Piece::Guest(pointer, length as usize);
            emit(os, sink, spec, false, b"", &[text])
        }
        b'n' => {
            let pointer = args.word(os)?;
            let count = u64::try_from(written).unwrap_or(u64::MAX);
            store(os, pointer, size, count)?;
            Ok(0)
        }
        b'%' => emit(os, sink, spec, false, b"", &[Piece::Bytes(b"%")]),
        b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'a' | b'A' => {
            let high = args.word(os)?;
            let low = args.word(os)?;
            match floats {
                Floats::Converted => {
                    let bits = u64::from(high) << 32 | u64::from(low);
                    floating(os, sink, spec, conversion, bits)
                }
                Floats::Absent => {
                    let text = os.as_caller().read_until(0, ZERO_PAGE_TEXT, |b| b == 0)?.0;
                    sink.put(os, &text)?;
                    Ok(text.len())
                }
            }
        }
        other => {
            sink.put(os, &[other])?;
            Ok(1)
        }
    }
}

/// The next integer argument of `size`, as 64 bits: a word's own bits, or
/// a double word's, its low word first.
fn integer(os: &mut Os<'_>, args: &mut impl Arguments, size: Size) -> Result<u64, Fail> {
    let low = u64::from(args.word(os)?);
    Ok(match size {
        Size::Doubleword => u64::from(args.word(os)?) << 32 | low,
        _ => low,
    })
}

/// `value` written as `%*.*d` writes it: at least `digits` digits after its
/// sign, right-aligned in `width`.
pub(crate) fn decimal(
    os: &mut Os<'_>,
    sink: &mut impl Sink,
    value: i32,
    digits: usize,
    width: usize,
) -> Result<(), Fail> {
    let spec = Spec {
        width,
        precision: Some(digits),
        ..Spec::default()
    };
    let word = u64::from(value.cast_unsigned());
    emit_integer(os, sink, spec, Size::Word, b'd', word).map(drop)
}

/// An integer conversion written.
fn emit_integer(
    os: &mut Os<'_>,
    sink: &mut impl Sink,
    spec: Spec,
    size: Size,
    conversion: u8,
    value: u64,
) -> Result<usize, Fail> {
    let signed = matches!(conversion, b'd' | b'i');
    let (negative, magnitude) = if signed {
        size.sign_and_magnitude(value)
    } else {
        (false, size.narrowed(value))
    };
    let base = match conversion {
        b'o' => 8,
        b'x' | b'X' => 16,
        _ => 10,
    };
    let mut buffer = [0u8; 24];
    let digits = if spec.precision == Some(0) && magnitude == 0 {
        &buffer[..0]
    } else {
        digits_in(magnitude, base, conversion == b'X', &mut buffer)
    };
    let prefix: &[u8] = if signed {
        if negative {
            b"-"
        } else if spec.plus {
            b"+"
        } else if spec.space {
            b" "
        } else {
            b""
        }
    } else if spec.alternate {
        match conversion {
            b'o' => b"0",
            b'x' if magnitude != 0 => b"0x",
            b'X' if magnitude != 0 => b"0X",
            _ => b"",
        }
    } else {
        b""
    };
    let zeros = spec
        .precision
        .map_or(0, |precision| precision.saturating_sub(digits.len()));
    let zero_padding = spec.zero && spec.precision.is_none();
    emit(
        os,
        sink,
        spec,
        zero_padding,
        prefix,
        &[Piece::Zeros(zeros), Piece::Bytes(digits)],
    )
}

/// A count as a signed one, past `i64`'s range its largest.
fn signed(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

/// The digit `value`, below sixteen, as a letter.
fn letter(value: u64, upper: bool) -> u8 {
    let letters: &[u8; 16] = if upper {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    usize::try_from(value)
        .ok()
        .and_then(|n| letters.get(n))
        .copied()
        .unwrap_or(b'?')
}

/// `value`'s digits in `base`, written at the end of `buffer`.
fn digits_in(mut value: u64, base: u64, upper: bool, buffer: &mut [u8; 24]) -> &[u8] {
    let mut start = buffer.len();
    loop {
        start -= 1;
        buffer[start] = letter(value % base, upper);
        value /= base;
        if value == 0 {
            break;
        }
    }
    &buffer[start..]
}

/// `count` bytes of `run`'s byte written, those the sink only counts
/// counted at once.
fn repeated(
    os: &mut Os<'_>,
    sink: &mut impl Sink,
    run: &[u8; 64],
    count: usize,
) -> Result<(), Fail> {
    let kept = count.min(sink.keeps());
    let mut left = kept;
    while left > 0 {
        let step = left.min(run.len());
        sink.put(os, &run[..step])?;
        left -= step;
        os.yield_point()?;
    }
    sink.count(count - kept);
    Ok(())
}

/// The `length` bytes of guest memory at `at` written, a run at a time,
/// those the sink only counts counted without being read.
fn copied(os: &mut Os<'_>, sink: &mut impl Sink, at: u32, length: usize) -> Result<(), Fail> {
    let kept = length.min(sink.keeps());
    let mut run = [0; TEXT_RUN];
    let mut done = 0;
    while done < kept {
        let part = &mut run[..(kept - done).min(TEXT_RUN)];
        os.as_caller()
            .read_bytes(at.wrapping_add(len32(done)), part)?;
        sink.put(os, part)?;
        done += part.len();
        os.yield_point()?;
    }
    sink.count(length - kept);
    Ok(())
}

/// A conversion's text — `prefix` then `pieces` — padded to the spec's width
/// with spaces, or with noughts after the prefix where `zero_padding`: the
/// bytes written.
fn emit(
    os: &mut Os<'_>,
    sink: &mut impl Sink,
    spec: Spec,
    zero_padding: bool,
    prefix: &[u8],
    pieces: &[Piece<'_>],
) -> Result<usize, Fail> {
    let length = prefix.len() + pieces.iter().map(|piece| piece.len()).sum::<usize>();
    let pad = spec.width.saturating_sub(length);
    if !spec.left && !zero_padding {
        repeated(os, sink, &SPACES, pad)?;
    }
    sink.put(os, prefix)?;
    if !spec.left && zero_padding {
        repeated(os, sink, &ZEROS, pad)?;
    }
    for &piece in pieces {
        match piece {
            Piece::Bytes(bytes) => sink.put(os, bytes)?,
            Piece::Zeros(count) => repeated(os, sink, &ZEROS, count)?,
            Piece::Guest(at, length) => copied(os, sink, at, length)?,
        }
    }
    if spec.left {
        repeated(os, sink, &SPACES, pad)?;
    }
    Ok(length + pad)
}

/// A finite value's significant digits, most significant first, and the
/// power of ten the first stands at; none for nought.
#[derive(Clone, Copy)]
struct Digits {
    figures: [u8; SIGNIFICANT],
    len: usize,
    exponent: i64,
}

impl Digits {
    const NONE: Self = Self {
        figures: [0; SIGNIFICANT],
        len: 0,
        exponent: 0,
    };

    /// `parts`' seventeen significant digits, correctly rounded.
    fn of(parts: tairix_softfloat::Parts) -> Self {
        let Some((decimal, _)) = Decimal::of(parts, SIGNIFICANT_DIGITS, Mode::Nearest) else {
            return Self::NONE;
        };
        let mut figures = [0; SIGNIFICANT];
        let mut rest = decimal.significand;
        for digit in figures.iter_mut().rev() {
            *digit = u8::try_from(rest % 10).unwrap_or(0);
            rest /= 10;
        }
        Self {
            figures,
            len: SIGNIFICANT,
            exponent: i64::from(decimal.exponent) + i64::from(SIGNIFICANT_DIGITS) - 1,
        }
    }

    /// The digit standing `index` places after the first, nought past the
    /// ones kept.
    fn at(&self, index: i64) -> u8 {
        usize::try_from(index)
            .ok()
            .filter(|&n| n < self.len)
            .map_or(0, |n| self.figures[n])
    }

    /// Rounded to `keep` significant digits, half to even, as the library
    /// rounds its seventeen: the first one more place up where rounding
    /// carried past it; none where it rounded to nought.
    fn rounded(self, keep: i64) -> Self {
        let Ok(keep) = usize::try_from(keep) else {
            return Self {
                exponent: self.exponent,
                ..Self::NONE
            };
        };
        if self.len == 0 || keep >= self.len {
            return self;
        }
        let dropped = self.figures[keep];
        let beyond = self.figures[keep + 1..self.len].iter().any(|&d| d != 0);
        let odd = keep > 0 && self.figures[keep - 1] % 2 == 1;
        let up = dropped > 5 || (dropped == 5 && (beyond || odd));
        let mut out = Self { len: keep, ..self };
        if !up {
            return if keep == 0 {
                Self {
                    exponent: self.exponent,
                    ..Self::NONE
                }
            } else {
                out
            };
        }
        for digit in out.figures[..keep].iter_mut().rev() {
            if *digit < 9 {
                *digit += 1;
                return out;
            }
            *digit = 0;
        }
        out.figures[0] = 1;
        out.len = keep.max(1);
        out.exponent += 1;
        out
    }

    /// The place of the last digit that is not nought, if any.
    fn last_nonzero(&self) -> Option<usize> {
        self.figures[..self.len].iter().rposition(|&d| d != 0)
    }
}

/// A floating-point conversion of the double `bits` written.
fn floating(
    os: &mut Os<'_>,
    sink: &mut impl Sink,
    spec: Spec,
    conversion: u8,
    bits: u64,
) -> Result<usize, Fail> {
    let upper = conversion.is_ascii_uppercase();
    let (negative, value) = match Binary::DOUBLE.unpack(bits) {
        Unpacked::Nan { negative, .. } => (negative, None),
        Unpacked::Infinity { negative } => (negative, Some(None)),
        Unpacked::Zero { negative } => (negative, Some(Some(Digits::NONE))),
        Unpacked::Finite { parts, .. } => (parts.negative, Some(Some(Digits::of(parts)))),
    };
    let sign: &[u8] = if negative {
        b"-"
    } else if spec.plus {
        b"+"
    } else if spec.space {
        b" "
    } else {
        b""
    };
    let special = |name: &'static [u8; 3], upper_name: &'static [u8; 3]| {
        if upper {
            upper_name
        } else {
            name
        }
    };
    let digits = match value {
        None => {
            return emit(
                os,
                sink,
                spec,
                spec.zero,
                sign,
                &[Piece::Bytes(special(b"nan", b"NAN"))],
            )
        }
        Some(None) => {
            return emit(
                os,
                sink,
                spec,
                spec.zero,
                sign,
                &[Piece::Bytes(special(b"inf", b"INF"))],
            )
        }
        Some(Some(digits)) => digits,
    };
    match conversion.to_ascii_lowercase() {
        b'a' => hexadecimal(os, sink, spec, upper, sign, bits),
        b'e' => {
            let precision = spec.precision.unwrap_or(6);
            let rounded = digits.rounded(signed(precision) + 1);
            exponential(os, sink, spec, upper, sign, &rounded, precision)
        }
        b'f' => {
            let precision = spec.precision.unwrap_or(6);
            let rounded = digits.rounded(digits.exponent + 1 + signed(precision));
            fixed(os, sink, spec, sign, &rounded, precision)
        }
        _ => general(os, sink, spec, upper, sign, digits),
    }
}

/// `%e`'s text of `digits`, already rounded: `d.ddde±xx`.
fn exponential(
    os: &mut Os<'_>,
    sink: &mut impl Sink,
    spec: Spec,
    upper: bool,
    sign: &[u8],
    digits: &Digits,
    precision: usize,
) -> Result<usize, Fail> {
    let lead = [b'0' + digits.at(0)];
    let shown = precision.min(SIGNIFICANT - 1);
    let mut fraction = [0u8; SIGNIFICANT];
    for (n, place) in fraction[..shown].iter_mut().enumerate() {
        *place = b'0' + digits.at(signed(n) + 1);
    }
    let point: &[u8] = if precision > 0 || spec.alternate {
        b"."
    } else {
        b""
    };
    let exponent = if digits.len == 0 { 0 } else { digits.exponent };
    let mut exponent_text = [0u8; 24];
    let exponent_text = exponent_written(exponent, upper, &mut exponent_text);
    let pieces = [
        Piece::Bytes(&lead),
        Piece::Bytes(point),
        Piece::Bytes(&fraction[..shown]),
        Piece::Zeros(precision - shown),
        Piece::Bytes(exponent_text),
    ];
    emit(os, sink, spec, spec.zero, sign, &pieces)
}

/// An exponent as `e±xx`, at least two digits.
fn exponent_written(exponent: i64, upper: bool, buffer: &mut [u8; 24]) -> &[u8] {
    let mut digits = [0u8; 24];
    let written = digits_in(exponent.unsigned_abs(), 10, false, &mut digits);
    let mut length = 0;
    for b in [
        if upper { b'E' } else { b'e' },
        if exponent < 0 { b'-' } else { b'+' },
    ] {
        buffer[length] = b;
        length += 1;
    }
    if written.len() < 2 {
        buffer[length] = b'0';
        length += 1;
    }
    buffer[length..length + written.len()].copy_from_slice(written);
    &buffer[..length + written.len()]
}

/// `%f`'s text of `digits`, already rounded, with `precision` places.
fn fixed(
    os: &mut Os<'_>,
    sink: &mut impl Sink,
    spec: Spec,
    sign: &[u8],
    digits: &Digits,
    precision: usize,
) -> Result<usize, Fail> {
    let exponent = digits.exponent;
    let mut whole = [0u8; SIGNIFICANT];
    let (whole_digits, whole_zeros) = if digits.len == 0 || exponent < 0 {
        whole[0] = b'0';
        (1, 0)
    } else {
        let count = usize::try_from(exponent + 1).unwrap_or(usize::MAX);
        let shown = count.min(digits.len);
        for (n, place) in whole[..shown].iter_mut().enumerate() {
            *place = b'0' + digits.figures[n];
        }
        (shown, count - shown)
    };
    let leading = usize::try_from(-(exponent + 1)).unwrap_or(0).min(precision);
    let mut fraction = [0u8; SIGNIFICANT];
    let mut shown = 0;
    while leading + shown < precision && shown < SIGNIFICANT {
        let index = exponent + 1 + signed(leading + shown);
        if index >= signed(digits.len) {
            break;
        }
        fraction[shown] = b'0' + digits.at(index);
        shown += 1;
    }
    let point: &[u8] = if precision > 0 || spec.alternate {
        b"."
    } else {
        b""
    };
    let pieces = [
        Piece::Bytes(&whole[..whole_digits]),
        Piece::Zeros(whole_zeros),
        Piece::Bytes(point),
        Piece::Zeros(leading),
        Piece::Bytes(&fraction[..shown]),
        Piece::Zeros(precision - leading - shown),
    ];
    emit(os, sink, spec, spec.zero, sign, &pieces)
}

/// `%g`'s text of `digits`: `%e`'s or `%f`'s by the exponent, trailing
/// noughts and a bare point left off unless `#` asks for them.
fn general(
    os: &mut Os<'_>,
    sink: &mut impl Sink,
    spec: Spec,
    upper: bool,
    sign: &[u8],
    digits: Digits,
) -> Result<usize, Fail> {
    let precision = spec.precision.unwrap_or(6).max(1);
    let rounded = digits.rounded(signed(precision));
    let exponent = if rounded.len == 0 {
        0
    } else {
        rounded.exponent
    };
    let fixed_style = exponent >= -4 && exponent < signed(precision);
    let mut places = if fixed_style {
        usize::try_from(signed(precision) - 1 - exponent).unwrap_or(0)
    } else {
        precision - 1
    };
    if !spec.alternate {
        let needed = rounded.last_nonzero().map_or(0, |last| {
            let after_point = if fixed_style {
                signed(last) - exponent
            } else {
                signed(last)
            };
            usize::try_from(after_point).unwrap_or(0)
        });
        places = places.min(needed);
    }
    if fixed_style {
        fixed(os, sink, spec, sign, &rounded, places)
    } else {
        exponential(os, sink, spec, upper, sign, &rounded, places)
    }
}

/// A double's fraction bits, as hexadecimal digits.
const FRACTION_DIGITS: usize = 13;

/// `%a`'s text of the double `bits`: its exact hexadecimal digits, rounded
/// half to even where a precision asks fewer.
fn hexadecimal(
    os: &mut Os<'_>,
    sink: &mut impl Sink,
    spec: Spec,
    upper: bool,
    sign: &[u8],
    bits: u64,
) -> Result<usize, Fail> {
    let fraction = bits & FRACTION;
    let biased = i64::from(biased_exponent(bits));
    let (mut lead, exponent) = match (biased, fraction) {
        (0, 0) => (0u64, 0),
        (0, _) => (0, 1 - i64::from(BIAS)),
        _ => (1, biased - i64::from(BIAS)),
    };
    let mut fraction = fraction;
    let places = match spec.precision {
        Some(precision) if precision < FRACTION_DIGITS => {
            let dropped = 4 * u32::try_from(FRACTION_DIGITS - precision).unwrap_or(0);
            let whole = (lead << 52 | fraction) >> dropped;
            let rest = (lead << 52 | fraction) & ((1u64 << dropped) - 1);
            let half = 1u64 << (dropped - 1);
            let up = rest > half || (rest == half && whole & 1 == 1);
            let rounded = whole + u64::from(up);
            lead = rounded >> (52 - dropped);
            fraction = (rounded << dropped) & FRACTION;
            precision
        }
        Some(precision) => precision,
        None => FRACTION_DIGITS - (fraction.trailing_zeros().min(52) / 4) as usize,
    };
    let mut digits = [0u8; FRACTION_DIGITS];
    let shown = places.min(FRACTION_DIGITS);
    for (n, digit) in digits[..shown].iter_mut().enumerate() {
        *digit = letter(fraction >> (48 - 4 * n) & 0xF, upper);
    }
    let mut lead_text = [0u8; 24];
    let lead_text = digits_in(lead, 16, upper, &mut lead_text);
    let point: &[u8] = if places > 0 || spec.alternate {
        b"."
    } else {
        b""
    };
    let mut exponent_text = [0u8; 24];
    let mut exponent_digits = [0u8; 24];
    let written = digits_in(exponent.unsigned_abs(), 10, false, &mut exponent_digits);
    exponent_text[0] = if upper { b'P' } else { b'p' };
    exponent_text[1] = if exponent < 0 { b'-' } else { b'+' };
    exponent_text[2..2 + written.len()].copy_from_slice(written);
    let exponent_text = &exponent_text[..2 + written.len()];
    let mut prefix = [0u8; 3];
    prefix[..sign.len()].copy_from_slice(sign);
    prefix[sign.len()..sign.len() + 2].copy_from_slice(if upper { b"0X" } else { b"0x" });
    let pieces = [
        Piece::Bytes(lead_text),
        Piece::Bytes(point),
        Piece::Bytes(&digits[..shown]),
        Piece::Zeros(places - shown),
        Piece::Bytes(exponent_text),
    ];
    emit(
        os,
        sink,
        spec,
        spec.zero,
        &prefix[..sign.len() + 2],
        &pieces,
    )
}

#[cfg(test)]
mod tests;
