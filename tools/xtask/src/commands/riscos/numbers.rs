//! The reference harness's part for BASIC's numbers: BASIC V and BASIC VI
//! each evaluate every case [`cases`] draws, and what each answered goes to
//! [`ANSWERS`], which `tairix-riscos-basic`'s replay holds the crate to.
//!
//! An answer names its dialect, `v` or `vi`, the operation and its operands,
//! then `=` and what BASIC gave: a real as the bytes memory holds it, in hex;
//! an integer or a truth in decimal; text quoted; or `E` and the error
//! number, BASIC's own or, for BASIC VI, the floating-point emulator's. A
//! BASIC VI `str` adds the words of the packed decimal store its digits come
//! from.

use std::fmt::Write as _;
use std::path::Path;

use tairix_fuzzseed::Prng;
use tairix_softfloat::{Decimal, Mode, Parts};

use super::{find, quote};

/// The answers the harness records.
pub(super) const ANSWERS: &str =
    "userland/apps/tairix-bridge/riscos/basic/tests/riscos5/numbers.txt";

/// The seed every case is drawn from.
const SEED: u64 = 0x2020_B45C_5E1D;

/// The `!Boot` commands that run each dialect's program.
pub(super) const RUN: [&str; 2] = [
    "BASIC -quit HostFS:$.NumbersV",
    "BASIC64 -quit HostFS:$.NumbersVI",
];

/// Each HostFS file a run leaves.
pub(super) const LEFT: [&str; 4] = ["NumbersV", "NumbersVI", "AnswersV", "AnswersVI"];

/// A dialect, and its value's size in bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dialect {
    V,
    Vi,
}

impl Dialect {
    const fn name(self) -> &'static str {
        match self {
            Self::V => "v",
            Self::Vi => "vi",
        }
    }

    const fn bytes(self) -> usize {
        match self {
            Self::V => 5,
            Self::Vi => 8,
        }
    }
}

/// An operand.
#[derive(Clone)]
enum Operand {
    None,
    /// A real as memory holds it.
    Real(Vec<u8>),
    /// An integer.
    Int(i32),
    /// `@%`'s value.
    Layout(u32),
    /// Text.
    Text(String),
}

/// A case: an operation on up to two operands.
struct Case {
    dialect: Dialect,
    op: &'static str,
    x: Operand,
    y: Operand,
}

/// Every operation, as the programs evaluate it from `X`, `Y`, `N%` and
/// `T$`: a real into `|C%`, or anything else into `R$`.
const OPERATIONS: [(&str, &str); 31] = [
    ("add", "|C%=X+Y"),
    ("sub", "|C%=X-Y"),
    ("mul", "|C%=X*Y"),
    ("div", "|C%=X/Y"),
    ("sqr", "|C%=SQR(X)"),
    ("neg", "|C%=-X"),
    ("abs", "|C%=ABS(X)"),
    ("int", "R$=STR$(INT(X))"),
    ("fix", "I%=X:R$=STR$(I%)"),
    ("float", "|C%=N%"),
    ("imul", "|C%=M%*N%"),
    ("eq", "R$=STR$(X=Y)"),
    ("lt", "R$=STR$(X<Y)"),
    ("gt", "R$=STR$(X>Y)"),
    ("powi", "|C%=X^N%"),
    ("pow", "|C%=X^Y"),
    ("sin", "|C%=SIN(X)"),
    ("cos", "|C%=COS(X)"),
    ("tan", "|C%=TAN(X)"),
    ("atn", "|C%=ATN(X)"),
    ("asn", "|C%=ASN(X)"),
    ("acs", "|C%=ACS(X)"),
    ("exp", "|C%=EXP(X)"),
    ("ln", "|C%=LN(X)"),
    ("log", "|C%=LOG(X)"),
    ("rad", "|C%=RAD(X)"),
    ("deg", "|C%=DEG(X)"),
    ("pi", "|C%=PI"),
    ("val", "|C%=VAL(T$)"),
    ("lit", "|C%=EVAL(T$)"),
    ("str", "@%=N%:R$=CHR$34+STR$(X)+CHR$34:@%=&90A"),
];

/// The cases, BASIC V's then BASIC VI's.
fn cases() -> Vec<Case> {
    let mut rng = Prng::new(SEED);
    let mut cases = Vec::new();
    let mut draw = Draw {
        rng: &mut rng,
        cases: &mut cases,
        dialect: Dialect::V,
    };
    five_arithmetic(&mut draw);
    five_conversions(&mut draw);
    five_powers(&mut draw);
    five_functions(&mut draw);
    five_text(&mut draw);
    draw.dialect = Dialect::Vi;
    six_arithmetic(&mut draw);
    six_functions(&mut draw);
    six_text(&mut draw);
    cases
}

/// The cases being drawn, each of the one dialect.
struct Draw<'a> {
    rng: &'a mut Prng,
    cases: &'a mut Vec<Case>,
    dialect: Dialect,
}

impl Draw<'_> {
    fn add(&mut self, op: &'static str, x: Operand, y: Operand) {
        self.cases.push(Case {
            dialect: self.dialect,
            op,
            x,
            y,
        });
    }

    /// A random value below `n`.
    fn below(&mut self, n: u32) -> u32 {
        u32::try_from(self.rng.next_u64() % u64::from(n.max(1))).unwrap_or(0)
    }

    fn coin(&mut self) -> bool {
        self.rng.next_u64() & 1 != 0
    }

    /// A random BASIC V real with an exponent byte in `exponents`.
    fn five(&mut self, exponents: core::ops::RangeInclusive<u8>) -> Operand {
        let span = u32::from(exponents.end() - exponents.start()) + 1;
        let exponent = exponents.start() + u8::try_from(self.below(span)).unwrap_or(0);
        let negative = self.coin();
        five(negative, self.rng.next_u32(), exponent)
    }

    /// A random double, its exponent within `exponents`.
    fn six(&mut self, exponents: core::ops::RangeInclusive<u16>) -> Operand {
        let span = u32::from(exponents.end() - exponents.start()) + 1;
        let exponent = u64::from(exponents.start() + u16::try_from(self.below(span)).unwrap_or(0));
        let sign = u64::from(self.coin());
        six(sign << 63 | exponent << 52 | (self.rng.next_u64() & ((1 << 52) - 1)))
    }

    /// A random positive real of the dialect's, for a logarithm.
    fn positive(&mut self) -> Operand {
        let drawn = match self.dialect {
            Dialect::V => self.five(0x01..=0xFE),
            Dialect::Vi => self.six(0x001..=0x7FE),
        };
        let Operand::Real(mut bytes) = drawn else {
            return drawn;
        };
        bytes[3] &= 0x7F;
        Operand::Real(bytes)
    }

    /// `count` random decimal digits.
    fn digits(&mut self, count: u32) -> String {
        (0..count)
            .map(|_| char::from(b'0' + u8::try_from(self.below(10)).unwrap_or(0)))
            .collect()
    }
}

/// BASIC V's real of sign `negative`, mantissa `mantissa` (its top bit
/// standing for the sign) and exponent byte `exponent`.
fn five(negative: bool, mantissa: u32, exponent: u8) -> Operand {
    let word = (mantissa & 0x7FFF_FFFF) | if negative { 0x8000_0000 } else { 0 };
    let mut bytes = word.to_le_bytes().to_vec();
    bytes.push(exponent);
    Operand::Real(bytes)
}

/// BASIC VI's double of these bits, as memory holds it: the high word first.
fn six(bits: u64) -> Operand {
    let bytes = bits.to_be_bytes();
    let (high, low) = bytes.split_at(4);
    Operand::Real(high.iter().rev().chain(low.iter().rev()).copied().collect())
}

/// A real as text its dialect reads: cases name such values as text BASIC
/// reads, never as a host float.
fn text(text: &str) -> Operand {
    Operand::Text(text.into())
}

/// Sums and differences over every exponent gap the rules turn on; products,
/// quotients, products lying exactly half way, roots.
fn five_arithmetic(draw: &mut Draw<'_>) {
    for op in ["add", "sub"] {
        for _ in 0..400 {
            let x = draw.five(0x70..=0x90);
            let Operand::Real(ref bytes) = x else {
                continue;
            };
            let gap = u8::try_from(draw.below(42)).unwrap_or(0);
            let exponent = bytes[4].saturating_sub(gap);
            let negative = draw.coin();
            let y = five(negative, draw.rng.next_u32(), exponent);
            draw.add(op, x, y);
        }
    }
    for op in ["mul", "div"] {
        for _ in 0..250 {
            let (x, y) = (draw.five(0x60..=0xA0), draw.five(0x60..=0xA0));
            draw.add(op, x, y);
        }
    }
    // Odd factors whose shifts add to 31.
    for _ in 0..80 {
        let shift = 1 + draw.below(30);
        let mut pick = |shift: u32| {
            let least = 1u32 << (31 - shift);
            ((least + draw.below(least)) | 1) << shift
        };
        let (a, b) = (pick(shift), pick(31 - shift));
        draw.add("mul", five(false, a, 0x81), five(true, b, 0x7F));
    }
    for _ in 0..150 {
        let x = draw.five(0x01..=0xFE);
        draw.add("sqr", x, Operand::None);
    }
}

/// Integers from reals, about and past each bound; integers to reals, and
/// integer products overflowing into reals; comparisons.
fn five_conversions(draw: &mut Draw<'_>) {
    for value in [
        "2147483647.5",
        "2147483648",
        "-2147483648",
        "-2147483648.5",
        "-2147483649",
        "-2147483647.5",
        "-2147483647",
        "0.5",
        "-0.5",
        "1E10",
        "-1E10",
        "2.5",
        "-2.5",
        "0",
        "1E-30",
    ] {
        for op in ["int", "fix"] {
            draw.add(op, text(value), Operand::None);
        }
    }
    for _ in 0..120 {
        let x = draw.five(0x70..=0xA0);
        draw.add("int", x.clone(), Operand::None);
        draw.add("fix", x, Operand::None);
    }
    for n in [0, 1, -1, i32::MAX, i32::MIN, 12_345_678, -987_654_321] {
        draw.add("float", Operand::None, Operand::Int(n));
    }
    for _ in 0..150 {
        let a = i32::from_le_bytes(draw.rng.next_u32().to_le_bytes()) >> draw.below(31);
        let b = i32::from_le_bytes(draw.rng.next_u32().to_le_bytes()) >> draw.below(31);
        draw.add("imul", Operand::Int(a), Operand::Int(b));
    }
    for op in ["eq", "lt", "gt"] {
        for _ in 0..50 {
            let x = draw.five(0x7E..=0x82);
            let y = if draw.below(4) == 0 {
                x.clone()
            } else {
                draw.five(0x7E..=0x82)
            };
            draw.add(op, x, y);
        }
    }
}

/// Integer powers each side of the squares' bounds, and real ones at each
/// fault their parts meet.
fn five_powers(draw: &mut Draw<'_>) {
    for n in (-70..=70).chain([100, 127, 128, 200, -128, -129, -130, 1000]) {
        let x = draw.five(0x7F..=0x81);
        draw.add("powi", x, Operand::Int(n));
    }
    for (x, n) in [
        ("0", -1),
        ("0", 0),
        ("0", 64),
        ("0", -64),
        ("-2", 63),
        ("-2", 64),
        ("-2", -64),
        ("2", 127),
        ("2", 128),
    ] {
        draw.add("powi", text(x), Operand::Int(n));
    }
    for _ in 0..150 {
        let (x, y) = (draw.five(0x7A..=0x84), draw.five(0x7C..=0x83));
        draw.add("pow", x, y);
    }
    for (x, y) in [
        ("0", "0.5"),
        ("0", "-0.5"),
        ("0", "-64"),
        ("0", "1.5"),
        ("-2", "0.5"),
        ("-2", "-63.5"),
        ("-2", "-64"),
        ("10", "38.5"),
        ("2", "129"),
        ("2", "130"),
        ("2", "-200"),
        ("0.25", "-63.5"),
        ("4", "0.5"),
    ] {
        draw.add("pow", text(x), text(y));
    }
}

/// Each function over its domain, and at each bound it refuses.
fn five_functions(draw: &mut Draw<'_>) {
    let functions: [(&str, core::ops::RangeInclusive<u8>, &[&str]); 9] = [
        (
            "sin",
            0x60..=0x97,
            &["8388607.99", "8388608", "-8388608", "1E30", "0"],
        ),
        (
            "cos",
            0x60..=0x97,
            &["8388607.99", "8388608", "-8388608", "1E30", "0"],
        ),
        (
            "tan",
            0x60..=0x97,
            &["8388607.99", "8388608", "-8388608", "1E30", "0"],
        ),
        ("atn", 0x50..=0xB0, &["0"]),
        ("asn", 0x60..=0x80, &["1", "-1", "0", "1.0000001", "-2"]),
        ("acs", 0x60..=0x80, &["1", "-1", "0", "1.0000001", "-2"]),
        (
            "exp",
            0x60..=0x86,
            &["88", "88.03", "89.49", "89.5", "100", "-89.5", "-1E10", "0"],
        ),
        ("rad", 0x70..=0x90, &["0"]),
        ("deg", 0x70..=0x90, &["0", "1E38"]),
    ];
    for (op, exponents, bounds) in functions {
        for _ in 0..110 {
            let x = draw.five(exponents.clone());
            draw.add(op, x, Operand::None);
        }
        for bound in bounds {
            draw.add(op, text(bound), Operand::None);
        }
    }
    for op in ["ln", "log"] {
        for _ in 0..110 {
            let x = draw.positive();
            draw.add(op, x, Operand::None);
        }
        for bound in ["1", "0", "-1", "1E-38", "1E38"] {
            draw.add(op, text(bound), Operand::None);
        }
    }
    draw.add("pi", Operand::None, Operand::None);
}

/// The `@%` values each layout is recorded under: every form and its digits
/// at and past their bounds, a decimal comma, and `STR$` told and not told
/// to use the layout.
const LAYOUTS: [u32; 20] = [
    0x0100_0A0A,
    0x0101_0A0A,
    0x0102_0A0A,
    0x0100_0300,
    0x0101_0300,
    0x0102_0300,
    0x0100_0000,
    0x0101_0000,
    0x0102_0000,
    0x0100_1400,
    0x0101_1400,
    0x0102_1400,
    0x0101_0100,
    0x0100_0100,
    0x0180_0400,
    0x0181_0400,
    0x0182_0400,
    0x01C1_0400,
    0x0104_0A0A,
    0x90A,
];

/// Decimal text for BASIC V to read: digits with and without a point and an
/// exponent, every form `VAL` stops at, and long runs every digit counts in;
/// then values written in every layout.
fn five_text(draw: &mut Draw<'_>) {
    let mut texts: Vec<String> = [
        "1E123",
        "1E0001",
        "1E-0001",
        "1E",
        "1E+",
        "1E-",
        "-",
        "+",
        "",
        "&FF",
        "%101",
        "E5",
        "1.2.3",
        "  12",
        "12  ",
        "12abc",
        ".5",
        "5.",
        "- 5",
        "+-5",
        "1e5",
        "1E38",
        "1.7E38",
        "1.71E38",
        "1E39",
        "1E-38",
        "1E-39",
        "1.5E-39",
        "1E-40",
        "4294967297",
        "4294967296.9999999999999999999999999",
        "00000000000000000000000000001",
        "123456789012345678901234567890",
    ]
    .iter()
    .map(|t| (*t).to_string())
    .collect();
    for _ in 0..300 {
        let count = 1 + draw.below(16);
        let digits = draw.digits(count);
        let point = usize::try_from(draw.below(count + 1)).unwrap_or(0);
        let mut written = String::new();
        if draw.coin() {
            written.push('-');
        }
        written.push_str(&digits[..point]);
        if point < digits.len() {
            written.push('.');
            written.push_str(&digits[point..]);
        }
        if draw.coin() {
            let _ = write!(written, "E{}", i64::from(draw.below(80)) - 40);
        }
        texts.push(written);
    }
    texts.extend(near_ties(draw));
    for written in texts {
        draw.add("val", Operand::Text(written.clone()), Operand::None);
        if written.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
            draw.add("lit", Operand::Text(written), Operand::None);
        }
    }
    for _ in 0..200 {
        let x = draw.five(0x60..=0xB0);
        draw.add("str", x, Operand::Layout(0x90A));
    }
    for _ in 0..40 {
        let x = draw.five(0x6A..=0xA4);
        for layout in LAYOUTS {
            draw.add("str", x.clone(), Operand::Layout(layout));
        }
    }
    for value in [
        "0",
        "1",
        "0.5",
        "0.01",
        "1E11",
        "1E20",
        "12345678901.5",
        "999999999.5",
        "-0.004",
        "1.5",
    ] {
        for layout in LAYOUTS {
            draw.add("str", text(value), Operand::Layout(layout));
        }
    }
}

/// Decimal text a little below and a little above a tie between two of
/// BASIC V's reals: where its reading, which keeps a little less than every
/// bit, can lose a place.
fn near_ties(draw: &mut Draw<'_>) -> Vec<String> {
    let mut texts = Vec::new();
    for _ in 0..60 {
        // The tie (2m + 1) / 2^33 of a mantissa m, scaled.
        let mantissa = u64::from(draw.rng.next_u32() | 0x8000_0000);
        let tie = Parts {
            negative: false,
            exp: i32::try_from(draw.below(80)).unwrap_or(0) - 40,
            sig: (2 * mantissa + 1) << 30,
        };
        let digits = 10 + draw.below(9);
        for mode in [Mode::Zero, Mode::Up] {
            if let Some((decimal, _)) = Decimal::of(tie, digits, mode) {
                texts.push(format!("{}E{}", decimal.significand, decimal.exponent));
            }
        }
    }
    texts
}

/// BASIC VI's arithmetic about its range's ends, its zeros by their bits
/// (it reads `-0` as positive zero), conversions and comparisons.
fn six_arithmetic(draw: &mut Draw<'_>) {
    for op in ["add", "sub", "mul", "div"] {
        for _ in 0..80 {
            let (x, y) = (draw.six(0x3C0..=0x440), draw.six(0x3C0..=0x440));
            draw.add(op, x, y);
        }
    }
    for value in ["0", "1E308", "-1E308", "1E-308", "5E-324"] {
        for (op, y) in [
            ("mul", "10"),
            ("div", "0.1"),
            ("div", "0"),
            ("add", "1.7976931348623157E308"),
        ] {
            draw.add(op, text(value), text(y));
        }
    }
    for _ in 0..60 {
        let x = draw.six(0x001..=0x7FE);
        draw.add("sqr", x, Operand::None);
    }
    for value in ["-1", "0", "2"] {
        draw.add("sqr", text(value), Operand::None);
    }
    let negative_zero = 1 << 63;
    for op in ["sqr", "neg", "abs"] {
        for bits in [0, negative_zero, negative_zero | 1, 0x4004_0000_0000_0000] {
            draw.add(op, six(bits), Operand::None);
        }
    }
    for value in [
        "2147483647.9",
        "2147483648",
        "-2147483648.9",
        "-2147483649",
        "1E10",
        "-0.5",
        "0.5",
        "1E300",
    ] {
        for op in ["int", "fix"] {
            draw.add(op, text(value), Operand::None);
        }
    }
    for n in [0, 1, -1, i32::MAX, i32::MIN] {
        draw.add("float", Operand::None, Operand::Int(n));
    }
    for op in ["eq", "lt", "gt"] {
        for _ in 0..20 {
            let x = draw.six(0x3FE..=0x401);
            let y = if draw.coin() {
                x.clone()
            } else {
                draw.six(0x3FE..=0x401)
            };
            draw.add(op, x, y);
        }
        draw.add(op, six(0), six(negative_zero));
    }
}

/// BASIC VI's functions, each the FPA's, and their refusals; powers.
fn six_functions(draw: &mut Draw<'_>) {
    for op in [
        "sin", "cos", "tan", "atn", "asn", "acs", "exp", "ln", "log", "rad", "deg",
    ] {
        for _ in 0..40 {
            let x = match op {
                "asn" | "acs" => draw.six(0x3C0..=0x3FE),
                "exp" => draw.six(0x3C0..=0x408),
                "ln" | "log" => draw.positive(),
                _ => draw.six(0x3C0..=0x41F),
            };
            draw.add(op, x, Operand::None);
        }
    }
    for (op, value) in [
        ("sin", "1E15"),
        ("sin", "8388608"),
        ("cos", "1E20"),
        ("tan", "1E300"),
        ("asn", "2"),
        ("acs", "-2"),
        ("ln", "0"),
        ("ln", "-1"),
        ("log", "0"),
        ("exp", "709.8"),
        ("exp", "-746"),
        ("deg", "1E308"),
        ("atn", "1E300"),
    ] {
        draw.add(op, text(value), Operand::None);
    }
    for _ in 0..40 {
        let (x, y) = (draw.six(0x3FC..=0x402), draw.six(0x3F8..=0x401));
        draw.add("pow", x, y);
    }
    for (x, y) in [
        ("0", "-1"),
        ("0", "0.5"),
        ("0", "0"),
        ("-2", "0.5"),
        ("-2", "3"),
        ("2", "1024"),
        ("2", "-1080"),
        ("10", "309"),
    ] {
        draw.add("pow", text(x), text(y));
    }
    for n in [2, 3, 17, 63, 64, 65, -64, 100, 1000] {
        let x = draw.six(0x3FE..=0x400);
        draw.add("powi", x, Operand::Int(n));
    }
    draw.add("pi", Operand::None, Operand::None);
}

/// Decimal text for BASIC VI to read — three-digit exponents, the range's
/// ends and runs past the 19 digits it reads — and values in every layout.
fn six_text(draw: &mut Draw<'_>) {
    for value in [
        "1E300",
        "1E-300",
        "1E308",
        "1E309",
        "1E-320",
        "1E-330",
        "1E0001",
        "1E-0001",
        "1.5E+100",
        "4.9E-324",
        "2.4E-324",
        "2.5E-324",
        "1.7976931348623157E308",
        "1.7976931348623159E308",
        "1E",
        "1E-",
        "-0",
        "- 5",
        "  -3.5",
        "0.99999999999999999999",
        "0.9999999999999999999",
    ] {
        draw.add("val", text(value), Operand::None);
    }
    for _ in 0..150 {
        let count = 1 + draw.below(24);
        let digits = draw.digits(count);
        let exponent = i64::from(draw.below(600)) - 300;
        draw.add(
            "val",
            Operand::Text(format!("{}.{}E{exponent}", &digits[..1], &digits[1..])),
            Operand::None,
        );
    }
    for _ in 0..200 {
        let x = draw.six(0x2C0..=0x540);
        draw.add("str", x, Operand::Layout(0x90A));
    }
    for _ in 0..20 {
        let x = draw.six(0x3A0..=0x460);
        for layout in LAYOUTS {
            draw.add("str", x.clone(), Operand::Layout(layout));
        }
    }
    let values = [
        six(1 << 63),
        text("0"),
        text("5E-324"),
        text("1E-308"),
        text("1.7976931348623157E308"),
        text("1E100"),
        text("1E18"),
        text("645519784491519100"),
        text("99999.99999999999"),
    ];
    for x in values {
        for layout in [
            0x90A,
            0x0100_0A0A,
            0x0101_0A0A,
            0x0102_0A0A,
            0x0101_0000,
            0x0101_1400,
            0x0102_1100,
        ] {
            draw.add("str", x.clone(), Operand::Layout(layout));
        }
    }
}

/// Each dialect's program: every case of it as `DATA`, evaluated and its
/// answer written a line each, an error's as `E` and its number.
pub(super) fn staged() -> Vec<(&'static str, Vec<u8>)> {
    let cases = cases();
    [
        (Dialect::V, "NumbersV", "AnswersV"),
        (Dialect::Vi, "NumbersVI", "AnswersVI"),
    ]
    .into_iter()
    .map(|(dialect, program, answers)| {
        (
            program,
            self::program(dialect, answers, &cases).into_bytes(),
        )
    })
    .collect()
}

fn program(dialect: Dialect, answers: &str, cases: &[Case]) -> String {
    let (fetch, size) = match dialect {
        Dialect::V => ("!A%=M%:A%?4=E%:X=|A%:!Y%=N%:Y%?4=F%:Y=|Y%", 5),
        Dialect::Vi => ("!A%=M%:A%!4=E%:X=|A%:!Y%=N%:Y%!4=F%:Y=|Y%", 8),
    };
    let mut lines: Vec<String> = vec![
        format!(
            "DIM A% 15,B% 15,C% 15,Y% 15,Q% 15:P$=\"\":O%=OPENOUT\"HostFS:$.{answers}\":ON ERROR BPUT#O%,P$+\"E\"+STR$ERR:@%=&90A"
        ),
        // LDFD F0,[R0]; STFP F0,[R1]; MOV PC,R14: the packed decimal store
        // BASIC VI's digits come from.
        "Q%!0=&ED908100:Q%!4=&EDC18100:Q%!8=&E1A0F00E".into(),
        "REPEAT:READ K$,M%,E%,N%,F%,T$:IF K$=\"end\" THEN CLOSE#O%:QUIT".into(),
        format!("{fetch}:R$=\"\":P$=\"\""),
        // Operands given as text are BASIC's own reading of it, which the
        // answer records.
        "IF T$<>\"\" AND K$<>\"val\" AND K$<>\"lit\" THEN X=VAL(T$):IF INSTR(T$,\" \") THEN Y=VAL(MID$(T$,INSTR(T$,\" \")+1))".into(),
        format!(
            "IF T$<>\"\" AND K$<>\"val\" AND K$<>\"lit\" THEN |A%=X:|B%=Y:P$=\"x=\"+FNb(A%,{size})+\" y=\"+FNb(B%,{size})+\" \""
        ),
    ];
    for (op, evaluate) in OPERATIONS {
        lines.push(format!("IF K$=\"{op}\" THEN {evaluate}"));
    }
    if dialect == Dialect::Vi {
        lines.push(
            "IF K$=\"str\" THEN |A%=X:CALL Q%:R$+=\" \"+FNw(B%,0)+\" \"+FNw(B%,4)+\" \"+FNw(B%,8)"
                .into(),
        );
    }
    lines.extend([
        format!("IF R$=\"\" THEN R$=FNb(C%,{size})"),
        "BPUT#O%,P$+R$".into(),
        "UNTIL FALSE".into(),
        "DEF FNb(M%,L%):LOCAL Q%,T$:FOR Q%=0 TO L%-1:T$+=RIGHT$(\"0\"+STR$~(M%?Q%),2):NEXT:=T$"
            .into(),
        "DEF FNw(M%,O%)=RIGHT$(\"0000000\"+STR$~(M%!O%),8)".into(),
    ]);
    for case in cases.iter().filter(|case| case.dialect == dialect) {
        lines.push(data(case));
    }
    lines.push("DATA end,0,0,0,0,\"\"".into());
    let mut text = String::new();
    for (line, number) in lines.iter().zip((10..).step_by(10)) {
        let _ = writeln!(text, "{number} {line}");
    }
    text
}

/// A case's `DATA`: the operation, the words of `X`'s and `Y`'s bytes, or an
/// integer or layout in `N%`, and text in `T$`, a `pow` of two texts both.
fn data(case: &Case) -> String {
    let words = |operand: &Operand| match operand {
        Operand::Real(bytes) => {
            let word = |at: usize| {
                i32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
            };
            let second = if bytes.len() == 8 {
                word(4)
            } else {
                i32::from(bytes[4])
            };
            (word(0), second)
        }
        Operand::Int(n) => (*n, 0),
        Operand::Layout(at) => (i32::from_le_bytes(at.to_le_bytes()), 0),
        Operand::None | Operand::Text(_) => (0, 0),
    };
    let (first, second) = (words(&case.x), words(&case.y));
    let text = match (&case.x, &case.y) {
        (Operand::Text(x), Operand::Text(y)) => format!("{x} {y}"),
        (Operand::Text(x), _) | (_, Operand::Text(x)) => x.clone(),
        _ => String::new(),
    };
    format!(
        "DATA {},{},{},{},{},\"{text}\"",
        case.op, first.0, first.1, second.0, second.1
    )
}

/// The answers file: the recording's provenance, then each case and what
/// BASIC answered it, from the files the programs left in `hostfs`.
pub(super) fn answers(provenance: &[String], hostfs: &Path) -> Result<String, String> {
    let cases = cases();
    let mut out = tairix_riscos_kernel::corpus::answers_head(provenance);
    for (dialect, name) in [(Dialect::V, "AnswersV"), (Dialect::Vi, "AnswersVI")] {
        let path = find(hostfs, name).ok_or_else(|| format!("riscos: BASIC left no {name}"))?;
        let bytes = std::fs::read(&path).map_err(|e| format!("riscos: {}: {e}", path.display()))?;
        let answered: Vec<&[u8]> = bytes
            .strip_suffix(b"\n")
            .unwrap_or(&bytes)
            .split(|&b| b == b'\n')
            .collect();
        let asked: Vec<&Case> = cases
            .iter()
            .filter(|case| case.dialect == dialect)
            .collect();
        if answered.len() != asked.len() {
            return Err(format!(
                "riscos: {name} answered {} of {} cases",
                answered.len(),
                asked.len()
            ));
        }
        for (case, answer) in asked.into_iter().zip(answered) {
            let (read, answer) = read_operands(answer);
            let named = |given: &Operand, read: Option<&str>| match (given, read) {
                (Operand::Text(_), Some(bytes)) => bytes.to_string(),
                _ => operand(given, dialect),
            };
            let x = named(&case.x, read.map(|(x, _)| x));
            let y = named(&case.y, read.map(|(_, y)| y));
            let _ = writeln!(
                out,
                "{} {} {x} {y} = {}",
                dialect.name(),
                case.op,
                answer_text(answer)
            );
        }
    }
    Ok(out)
}

/// The operands BASIC read from text, `x=` and `y=` ahead of an answer,
/// and the answer after them.
fn read_operands(answer: &[u8]) -> (Option<(&str, &str)>, &[u8]) {
    let Some(rest) = answer.strip_prefix(b"x=") else {
        return (None, answer);
    };
    let mut parts = rest.splitn(3, |&b| b == b' ');
    let (Some(x), Some(y), Some(answer)) = (parts.next(), parts.next(), parts.next()) else {
        return (None, answer);
    };
    let (Ok(x), Some(Ok(y))) = (
        std::str::from_utf8(x),
        y.strip_prefix(b"y=").map(std::str::from_utf8),
    ) else {
        return (None, answer);
    };
    (Some((x, y)), answer)
}

/// An operand as an answer names it; `-` for none.
fn operand(operand: &Operand, dialect: Dialect) -> String {
    match operand {
        Operand::None => "-".into(),
        Operand::Real(bytes) if bytes.len() == dialect.bytes() => {
            bytes.iter().fold(String::new(), |mut hex, b| {
                let _ = write!(hex, "{b:02X}");
                hex
            })
        }
        Operand::Real(_) => "?".into(),
        Operand::Int(n) => format!("n={n}"),
        Operand::Layout(at) => format!("@%=&{at:X}"),
        Operand::Text(text) => quote(text.as_bytes()),
    }
}

/// What BASIC answered: text it quoted stays quoted, escaped as the answers
/// file escapes it.
fn answer_text(answer: &[u8]) -> String {
    match answer.strip_prefix(b"\"") {
        Some(rest) => match rest.iter().rposition(|&b| b == b'"') {
            Some(end) => {
                let words = std::str::from_utf8(&rest[end + 1..]).unwrap_or("");
                format!("{}{words}", quote(&rest[..end]))
            }
            None => quote(answer),
        },
        None => String::from_utf8_lossy(answer).into_owned(),
    }
}
