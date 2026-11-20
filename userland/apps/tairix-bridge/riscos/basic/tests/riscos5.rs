//! The replay: every answer genuine RISC OS 5's BASIC V and BASIC VI gave
//! `cargo xtask riscos --reference` (`tests/riscos5/numbers.txt`), held to
//! the crate.
//!
//! Arithmetic, conversions, text and every refusal agree exactly. A
//! transcendental BASIC V rounds a place or two off agrees within two places
//! and the places its condition number magnifies an argument's last one
//! into, as do the real powers it takes the same way. BASIC VI's are the
//! FPA's: they, and the packed decimal conversions its numbers are read and
//! written through, agree within FPEmulator's error, its own place; its
//! layouts are held exactly to the digits FPEmulator's packed decimal store
//! wrote.

mod common;

use common::unquote;
use tairix_riscos_basic::five::{self, Five};
use tairix_riscos_basic::six::{self, Arithmetic, Packed, Six};
use tairix_riscos_basic::text::{write, Digits, Format, Written};
use tairix_riscos_basic::Fault;

/// The answers.
const ANSWERS: &str = include_str!("riscos5/numbers.txt");

/// RISC OS's floating-point status for an application, under which BASIC VI
/// was recorded.
const TRAPPING: u32 = tairix_riscos_kernel::os::APPLICATION_FPSR;

/// The places a BASIC V transcendental may lie from the correctly rounded
/// value, beyond those its condition number accounts for.
const FIVE_PLACES: f64 = 2.0;

/// The places FPEmulator's double results and packed decimal conversions
/// may lie from the correctly rounded ones.
const FPEMULATOR_PLACES: f64 = 1.0;

/// The double places of a result FPEmulator's error in an argument comes
/// to, per place its condition number magnifies: the two extended places
/// `riscos-fpa` measures, eleven bits below a double's last.
const FPEMULATOR_ARGUMENT_PLACES: f64 = 2.0 / 2048.0;

/// How far above a tie, in a five-byte significand's units, a number BASIC
/// V reads a place low may lie: 2^-6 of a place, the most it was measured
/// losing 0.0135 of one.
const TIE_REACH: u128 = 1 << 89;

/// The significant digits a packed decimal store writes.
const PACKED_DIGITS: u32 = 17;

/// The places of its last digit FPEmulator's packed decimal store may lie
/// from the correctly rounded digits.
const FPEMULATOR_DIGIT_PLACES: u128 = 1;

/// An operand or answer as the file writes it.
#[derive(Clone, Debug, PartialEq)]
enum Item {
    None,
    Hex(Vec<u8>),
    Int(i64),
    Layout(u32),
    Text(Vec<u8>),
    Error(i64),
}

/// One answer: its line, dialect, operation, operands and what BASIC gave,
/// then for a BASIC VI `str` the packed decimal store's words.
struct Answer<'a> {
    line: usize,
    six: bool,
    op: &'a str,
    x: Item,
    y: Item,
    given: Item,
    words: Option<[u32; 3]>,
}

/// The file's tokens: spaces part them but within quotes.
fn tokens(line: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = None;
    let mut quoted = false;
    for (at, c) in line.char_indices() {
        match (c, start) {
            ('"', _) => {
                quoted = !quoted;
                start.get_or_insert(at);
            }
            (' ', Some(from)) if !quoted => {
                tokens.push(&line[from..at]);
                start = None;
            }
            (' ', None) => {}
            (_, None) => start = Some(at),
            _ => {}
        }
    }
    if let Some(from) = start {
        tokens.push(&line[from..]);
    }
    tokens
}

fn item(token: &str, size: usize) -> Item {
    if token == "-" {
        return Item::None;
    }
    if let Some(n) = token.strip_prefix("n=") {
        return Item::Int(n.parse().unwrap());
    }
    if let Some(at) = token.strip_prefix("@%=&") {
        return Item::Layout(u32::from_str_radix(at, 16).unwrap());
    }
    if token.starts_with('"') {
        return Item::Text(unquote(token));
    }
    if token.len() == 2 * size && token.bytes().all(|b| b.is_ascii_hexdigit()) {
        let bytes = (0..size)
            .map(|at| u8::from_str_radix(&token[2 * at..2 * at + 2], 16).unwrap())
            .collect();
        return Item::Hex(bytes);
    }
    if let Some(n) = token.strip_prefix('E') {
        return Item::Error(n.parse().unwrap());
    }
    Item::Int(token.parse().unwrap())
}

fn answers() -> Vec<Answer<'static>> {
    let mut answers = Vec::new();
    for (index, line) in ANSWERS.lines().enumerate() {
        if line.starts_with('#') {
            continue;
        }
        let tokens = tokens(line);
        let [dialect, op, first, second, "=", given, rest @ ..] = tokens.as_slice() else {
            panic!("line {}: {line}", index + 1);
        };
        let six = *dialect == "vi";
        let size = if six { six::BYTES } else { five::BYTES };
        let words = match rest {
            [high, middle, low] => {
                Some([high, middle, low].map(|w| u32::from_str_radix(w, 16).unwrap()))
            }
            _ => None,
        };
        // An integer or a truth answers in decimal, which a run of digits as
        // long as a real's hex would otherwise read as.
        let given = match (*op, given.strip_prefix('E')) {
            ("int" | "fix" | "eq" | "lt" | "gt", None) => Item::Int(given.parse().unwrap()),
            _ => item(given, size),
        };
        answers.push(Answer {
            line: index + 1,
            six,
            op,
            x: item(first, size),
            y: item(second, size),
            given,
            words,
        });
    }
    answers
}

fn five_of(item: &Item) -> Five {
    match item {
        Item::Hex(bytes) => Five::from_bytes(bytes.as_slice().try_into().unwrap()),
        Item::Text(text) => Five::read(&Written::val(text, five::EXPONENT_DIGITS)).unwrap(),
        other => panic!("not a real: {other:?}"),
    }
}

fn six_of(item: &Item) -> Six {
    match item {
        Item::Hex(bytes) => Six::from_bytes(bytes.as_slice().try_into().unwrap()),
        Item::Text(text) => Arithmetic::new(TRAPPING)
            .read(&Written::val(text, six::EXPONENT_DIGITS))
            .unwrap(),
        other => panic!("not a real: {other:?}"),
    }
}

fn int_of(item: &Item) -> i32 {
    match item {
        Item::Int(n) => i32::try_from(*n).unwrap(),
        other => panic!("not an integer: {other:?}"),
    }
}

/// An error as the answers file numbers it: BASIC's, or for a
/// floating-point exception the floating-point emulator's, as a signed word.
fn error_number(fault: Fault) -> i64 {
    match fault {
        Fault::Floating(exception) => i64::from(i32::from_le_bytes(
            tairix_riscos_kernel::error::floating_point(exception)
                .number
                .to_le_bytes(),
        )),
        other => i64::from(other.number().unwrap()),
    }
}

fn five_f64(v: Five) -> f64 {
    let bytes = v.to_bytes();
    if v.is_zero() {
        return 0.0;
    }
    let word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let mantissa = f64::from(word | 0x8000_0000) / 4_294_967_296.0;
    let magnitude = mantissa * 2f64.powi(i32::from(bytes[4]) - 128);
    if word >> 31 != 0 {
        -magnitude
    } else {
        magnitude
    }
}

fn six_f64(v: Six) -> f64 {
    f64::from_bits(v.to_bits())
}

/// How far `kind` magnifies a relative change of its arguments.
fn condition(kind: &str, x: f64, y: f64) -> f64 {
    let kappa = match kind {
        "sin" => x * x.cos() / x.sin(),
        "cos" => x * x.sin() / x.cos(),
        "tan" => x * (1.0 + x.tan().powi(2)) / x.tan(),
        "atn" => x / ((1.0 + x * x) * x.atan()),
        "asn" => x / ((1.0 - x * x).sqrt() * x.asin()),
        "acs" => x / ((1.0 - x * x).sqrt() * x.acos()),
        "exp" => x,
        "ln" | "log" => 1.0 / x.ln(),
        "pow" | "powi" => y * (1.0 + x.abs().ln().abs()),
        _ => 0.0,
    };
    if kappa.is_finite() {
        kappa.abs()
    } else {
        f64::INFINITY
    }
}

/// Whether `ours` lies within `places` places of `recorded`, each in
/// `ulp`'s units at `recorded`.
fn within(ours: f64, recorded: f64, places: f64, ulp: impl Fn(f64) -> f64) -> bool {
    (ours - recorded).abs() <= places * ulp(recorded)
}

/// A place of BASIC V's at `v`: the unit of its mantissa's last bit.
fn five_ulp(v: f64) -> f64 {
    let mut ulp = 2f64.powi(-160);
    while ulp * 4_294_967_296.0 <= v.abs() {
        ulp *= 2.0;
    }
    ulp
}

fn six_ulp(v: f64) -> f64 {
    if v == 0.0 {
        return f64::from_bits(1);
    }
    let bits = v.abs().to_bits();
    f64::from_bits(bits + 1) - f64::from_bits(bits)
}

/// BASIC V's real path of a power: not by squares.
fn real_power(op: &str, x: &Item, y: &Item) -> bool {
    match op {
        "powi" => !(-64..=63).contains(&int_of(y)),
        "pow" => {
            let y = five_of(y);
            let n = y.to_int().ok();
            !n.is_some_and(|n| (-63..=63).contains(&n) && Five::from_int(n) == y)
        }
        _ => {
            let _ = x;
            false
        }
    }
}

fn five_case(answer: &Answer<'_>) -> Result<(), String> {
    let (x, y) = (&answer.x, &answer.y);
    let real = |r: Result<Five, Fault>| r.map(|v| Item::Hex(v.to_bytes().to_vec()));
    let integer = |r: Result<i32, Fault>| r.map(|n| Item::Int(i64::from(n)));
    let truth = |b: bool| Ok(Item::Int(if b { -1 } else { 0 }));
    let ours = match answer.op {
        "add" => real(five_of(x).add(five_of(y))),
        "sub" => real(five_of(x).sub(five_of(y))),
        "mul" => real(five_of(x).mul(five_of(y))),
        "div" => real(five_of(x).div(five_of(y))),
        "sqr" => real(five_of(x).sqrt()),
        "int" => integer(five_of(x).floor()),
        "fix" => integer(five_of(x).to_int()),
        "float" => real(Ok(Five::from_int(int_of(y)))),
        "imul" => {
            let product = i64::from(int_of(x)) * i64::from(int_of(y));
            real(Ok(
                i32::try_from(product).map_or_else(|_| Five::from_i64(product), Five::from_int)
            ))
        }
        "eq" => truth(five_of(x) == five_of(y)),
        "lt" => truth(five_of(x) < five_of(y)),
        "gt" => truth(five_of(x) > five_of(y)),
        "powi" => real(five_of(x).pow_int(int_of(y))),
        "pow" => real(five_of(x).pow(five_of(y))),
        "sin" => real(five_of(x).sin()),
        "cos" => real(five_of(x).cos()),
        "tan" => real(five_of(x).tan()),
        "atn" => real(five_of(x).atn()),
        "asn" => real(five_of(x).asn()),
        "acs" => real(five_of(x).acs()),
        "exp" => real(five_of(x).exp()),
        "ln" => real(five_of(x).ln()),
        "log" => real(five_of(x).log()),
        "rad" => real(five_of(x).rad()),
        "deg" => real(five_of(x).deg()),
        "pi" => real(Ok(Five::PI)),
        "val" | "lit" => {
            let Item::Text(text) = x else {
                return Err("no text".into());
            };
            let number = if answer.op == "val" {
                Some(Written::val(text, five::EXPONENT_DIGITS))
            } else {
                Written::literal(text, five::EXPONENT_DIGITS)
            };
            match number {
                Some(number) => real(Five::read(&number)),
                None => return Err("no literal".into()),
            }
        }
        "str" => {
            let Item::Layout(at) = y else {
                return Err("no layout".into());
            };
            let mut text = String::new();
            write(&five_of(x), Format::of_str(*at), &mut text).map_err(|e| e.to_string())?;
            Ok(Item::Text(text.into_bytes()))
        }
        other => return Err(format!("no operation {other}")),
    };
    let ours = ours.map_err(error_number).unwrap_or_else(Item::Error);
    if ours == answer.given {
        return Ok(());
    }
    if matches!(answer.op, "val" | "lit") && read_below_a_tie(x, &ours, &answer.given) {
        return Ok(());
    }
    let transcendental = matches!(
        answer.op,
        "sin" | "cos" | "tan" | "atn" | "asn" | "acs" | "exp" | "ln" | "log"
    ) || real_power(answer.op, x, y);
    if transcendental {
        let y = match answer.op {
            "powi" => f64::from(int_of(y)),
            "pow" => five_f64(five_of(y)),
            _ => 0.0,
        };
        let places = FIVE_PLACES + condition(answer.op, five_f64(five_of(x)), y);
        let value = |bytes: &[u8]| five_f64(Five::from_bytes(bytes.try_into().unwrap()));
        match (&ours, &answer.given) {
            (Item::Hex(ours), Item::Hex(recorded))
                if within(value(ours), value(recorded), places, five_ulp) =>
            {
                return Ok(());
            }
            // Too big, where BASIC V's own approximation lies within its
            // error of the largest value.
            (Item::Error(20), Item::Hex(recorded)) => {
                let largest = five_f64(Five::from_bytes([0xFF, 0xFF, 0xFF, 0x7F, 0xFF]));
                if within(largest, value(recorded).abs(), places, five_ulp) {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
    Err(format!("ours {ours:?}"))
}

/// Whether BASIC V read `text` a place below ours, its value lying on a
/// tie or within [`TIE_REACH`] of a place above one: BASIC V reads with a
/// little less than every bit, and loses only there.
fn read_below_a_tie(text: &Item, ours: &Item, recorded: &Item) -> bool {
    let (Item::Text(text), Item::Hex(ours), Item::Hex(recorded)) = (text, ours, recorded) else {
        return false;
    };
    let magnitude = |bytes: &[u8]| {
        let word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        (bytes[4], word | 0x8000_0000, word >> 31)
    };
    let ((ours_exponent, ours_mantissa, ours_sign), (exponent, mantissa, sign)) =
        (magnitude(ours), magnitude(recorded));
    let one_below = ours_sign == sign
        && if ours_mantissa == 0x8000_0000 {
            exponent + 1 == ours_exponent && mantissa == u32::MAX
        } else {
            exponent == ours_exponent && mantissa + 1 == ours_mantissa
        };
    let number = Written::val(text, five::EXPONENT_DIGITS);
    let Ok(exact) =
        tairix_softfloat::decimal::exact_digits(number.negative, number.digits(), number.exponent)
    else {
        return false;
    };
    let exact = exact.normalized(true);
    // The mantissa is the significand's top 32 bits; a place is its last.
    let place = 1u128 << 95;
    let below = exact.sig & (place - 1);
    one_below && below >= place / 2 && below - place / 2 < TIE_REACH
}

fn six_case(answer: &Answer<'_>) -> Result<(), String> {
    let mut a = Arithmetic::new(TRAPPING);
    let (x, y) = (&answer.x, &answer.y);
    let real = |r: Result<Six, Fault>| r.map(|v| Item::Hex(v.to_bytes().to_vec()));
    let integer = |r: Result<i32, Fault>| r.map(|n| Item::Int(i64::from(n)));
    let truth = |c: Result<Option<core::cmp::Ordering>, Fault>, want: core::cmp::Ordering| {
        c.map(|c| Item::Int(if c == Some(want) { -1 } else { 0 }))
    };
    let ours = match answer.op {
        "add" => real(a.add(six_of(x), six_of(y))),
        "sub" => real(a.sub(six_of(x), six_of(y))),
        "mul" => real(a.mul(six_of(x), six_of(y))),
        "div" => real(a.div(six_of(x), six_of(y))),
        "sqr" => real(a.sqrt(six_of(x))),
        "neg" => real(a.negate(six_of(x))),
        "abs" => real(a.abs(six_of(x))),
        "int" => integer(a.floor(six_of(x))),
        "fix" => integer(a.to_int(six_of(x))),
        "float" => real(a.from_int(int_of(y))),
        "eq" => truth(a.compare(six_of(x), six_of(y)), core::cmp::Ordering::Equal),
        "lt" => truth(a.compare(six_of(x), six_of(y)), core::cmp::Ordering::Less),
        "gt" => truth(
            a.compare(six_of(x), six_of(y)),
            core::cmp::Ordering::Greater,
        ),
        "powi" => {
            let n = a.from_int(int_of(y)).unwrap();
            real(a.pow(six_of(x), n))
        }
        "pow" => real(a.pow(six_of(x), six_of(y))),
        "sin" => real(a.sin(six_of(x))),
        "cos" => real(a.cos(six_of(x))),
        "tan" => real(a.tan(six_of(x))),
        "atn" => real(a.atn(six_of(x))),
        "asn" => real(a.asn(six_of(x))),
        "acs" => real(a.acs(six_of(x))),
        "exp" => real(a.exp(six_of(x))),
        "ln" => real(a.ln(six_of(x))),
        "log" => real(a.log(six_of(x))),
        "rad" => real(a.rad(six_of(x))),
        "deg" => real(a.deg(six_of(x))),
        "pi" => real(Ok(Six::PI)),
        "val" => {
            let Item::Text(text) = x else {
                return Err("no text".into());
            };
            real(a.read(&Written::val(text, six::EXPONENT_DIGITS)))
        }
        "str" => return six_str(answer, &mut a),
        other => return Err(format!("no operation {other}")),
    };
    let ours = ours.map_err(error_number).unwrap_or_else(Item::Error);
    if ours == answer.given {
        return Ok(());
    }
    let fpemulator = matches!(
        answer.op,
        "sin"
            | "cos"
            | "tan"
            | "atn"
            | "asn"
            | "acs"
            | "exp"
            | "ln"
            | "log"
            | "pow"
            | "powi"
            | "val"
    );
    if let (true, Item::Hex(ours), Item::Hex(recorded)) = (fpemulator, &ours, &answer.given) {
        let ours = six_f64(Six::from_bytes(ours.as_slice().try_into().unwrap()));
        let recorded = six_f64(Six::from_bytes(recorded.as_slice().try_into().unwrap()));
        let y = match answer.op {
            "powi" => f64::from(int_of(y)),
            "pow" => six_f64(six_of(y)),
            _ => 0.0,
        };
        let argument = if answer.op == "val" {
            0.0
        } else {
            six_f64(six_of(x))
        };
        let places =
            FPEMULATOR_PLACES + FPEMULATOR_ARGUMENT_PLACES * condition(answer.op, argument, y);
        if within(ours, recorded, places, six_ulp) {
            return Ok(());
        }
    }
    Err(format!("ours {ours:?}"))
}

/// A BASIC VI `str`: its layout of FPEmulator's packed digits exactly, and
/// ours within its error of them.
fn six_str(answer: &Answer<'_>, a: &mut Arithmetic) -> Result<(), String> {
    let (Item::Layout(at), Some(words), Item::Text(recorded)) =
        (&answer.y, answer.words, &answer.given)
    else {
        return Err("a str answer without its layout, digits or text".into());
    };
    let theirs = Packed::from_words(words).ok_or("FPEmulator's digits unreadable")?;
    let mut text = String::new();
    write(&theirs, Format::of_str(*at), &mut text).map_err(|e| e.to_string())?;
    if text.as_bytes() != recorded.as_slice() {
        return Err(format!("FPEmulator's digits laid out as {text:?}"));
    }
    let ours = a.digits(six_of(&answer.x)).map_err(|f| format!("{f:?}"))?;
    // Both significands at the lesser's last place: they differ there by
    // FPEmulator's place at most.
    let close = match (ours.rounded(PACKED_DIGITS), theirs.rounded(PACKED_DIGITS)) {
        (None, None) => ours == theirs,
        (Some((a, ea)), Some((b, eb))) => {
            let (a, b) = match ea - eb {
                0 => (a, b),
                1 => (a * 10, b),
                -1 => (a, b * 10),
                _ => return Err(format!("our digits {ours:?}, FPEmulator's {theirs:?}")),
            };
            a.abs_diff(b) <= FPEMULATOR_DIGIT_PLACES
        }
        _ => false,
    };
    if close {
        Ok(())
    } else {
        Err(format!("our digits {ours:?}, FPEmulator's {theirs:?}"))
    }
}

#[test]
fn every_answer_basic_gave_is_ours() {
    let answers = answers();
    assert!(
        answers.len() > 7000,
        "the recording holds {} answers",
        answers.len()
    );
    let failures: Vec<String> = answers
        .iter()
        .filter_map(|answer| {
            let outcome = if answer.six {
                six_case(answer)
            } else {
                five_case(answer)
            };
            outcome.err().map(|why| {
                format!(
                    "line {}: {} {} {:?} {:?} = {:?}: {why}",
                    answer.line,
                    if answer.six { "vi" } else { "v" },
                    answer.op,
                    answer.x,
                    answer.y,
                    answer.given
                )
            })
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} answers differ:\n{}",
        failures.len(),
        answers.len(),
        failures
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
