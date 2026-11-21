//! Deterministic fuzz harness for BASIC's numbers. A program's text is a
//! stranger's: `VAL` reads any string, and a program's literals any bytes.
//! The invariants:
//!
//! * reading any bytes as a number never panics, takes no more text than
//!   there is, and yields digits only;
//! * every BASIC V and BASIC VI operation and function on any operands, and
//!   BASIC VI's under any floating-point status, answers or faults without a
//!   panic;
//! * any value laid out under any `@%` writes a bounded text without a
//!   panic, and `STR$`'s own layout reads back as the value it wrote.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use tairix_fuzzseed::Prng;
use tairix_riscos_basic::five::{self, Five};
use tairix_riscos_basic::six::{self, Arithmetic, Six};
use tairix_riscos_basic::text::{write, Format, Written};
use tairix_riscos_basic::STRING_MOST;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 2_000;

/// The longest text any layout writes: a sign, a dialect's most digits with
/// their noughts, a point and an exponent.
const LONGEST_TEXT: usize = 64;

/// Text a number may be read from: mostly number-shaped, sometimes any bytes.
fn text(rng: &mut Prng) -> Vec<u8> {
    const SHAPED: &[u8] = b"0123456789.E+- ";
    let length = rng.below(STRING_MOST + 1);
    let any = rng.below(4) == 0;
    (0..length)
        .map(|_| {
            if any {
                u8::try_from(rng.next_u32() >> 24).unwrap_or(0)
            } else {
                SHAPED[rng.below(SHAPED.len())]
            }
        })
        .collect()
}

fn five_of(rng: &mut Prng) -> Five {
    let [a, b, c, d] = rng.next_u32().to_le_bytes();
    Five::from_bytes([a, b, c, d, u8::try_from(rng.next_u32() >> 24).unwrap_or(0)])
}

fn six_of(rng: &mut Prng) -> Six {
    Six::from_bits(rng.next_u64())
}

fn check_reading(rng: &mut Prng) {
    let text = text(rng);
    for digits in [five::EXPONENT_DIGITS, six::EXPONENT_DIGITS] {
        let number = Written::val(&text, digits);
        assert!(number.length <= text.len(), "read past the text");
        assert!(number.digits().all(|d| d <= 9), "a digit above nine");
        if let Some(literal) = Written::literal(&text, digits) {
            assert!(literal.length <= text.len());
        }
        let _ = Five::read(&number);
        let _ = Arithmetic::new(rng.next_u32()).read(&number);
    }
}

fn check_five(rng: &mut Prng) {
    let (x, y) = (five_of(rng), five_of(rng));
    let n = i32::from_le_bytes(rng.next_u32().to_le_bytes()) >> rng.below(32);
    let _ = (
        x.add(y),
        x.sub(y),
        x.mul(y),
        x.div(y),
        x.sqrt(),
        x.to_int(),
        x.floor(),
    );
    let _ = (x.sin(), x.cos(), x.tan(), x.atn(), x.asn(), x.acs());
    let _ = (
        x.exp(),
        x.ln(),
        x.log(),
        x.rad(),
        x.deg(),
        x.pow(y),
        x.pow_int(n),
    );
    let at = rng.next_u32();
    let mut out = String::new();
    write(&x, Format::of(at), &mut out).expect("a String takes any text");
    assert!(out.len() <= LONGEST_TEXT, "{out:?} under {at:#X}");
    // STR$'s own layout gives every digit a value needs.
    let mut own = String::new();
    write(&x, Format::of_str(0), &mut own).expect("a String takes any text");
    let read = Five::read(&Written::val(own.as_bytes(), five::EXPONENT_DIGITS));
    assert_eq!(read, Ok(x), "{x:?} written {own:?}");
}

fn check_six(rng: &mut Prng) {
    let mut a = Arithmetic::new(rng.next_u32());
    let (x, y) = (six_of(rng), six_of(rng));
    let _ = (
        a.add(x, y),
        a.sub(x, y),
        a.mul(x, y),
        a.div(x, y),
        a.pow(x, y),
    );
    let _ = (
        a.negate(x),
        a.abs(x),
        a.sqrt(x),
        a.sin(x),
        a.cos(x),
        a.tan(x),
    );
    let _ = (a.asn(x), a.acs(x), a.atn(x), a.exp(x), a.ln(x), a.log(x));
    let _ = (a.rad(x), a.deg(x), a.to_int(x), a.floor(x), a.compare(x, y));
    let mut quiet = Arithmetic::new(0);
    let Ok(digits) = quiet.digits(x) else { return };
    let at = rng.next_u32();
    let mut out = String::new();
    write(&digits, Format::of(at), &mut out).expect("a String takes any text");
    assert!(out.len() <= LONGEST_TEXT, "{out:?} under {at:#X}");
    if x.to_bits() & 0x7FF0_0000_0000_0000 == 0x7FF0_0000_0000_0000 {
        return;
    }
    let mut own = String::new();
    write(&digits, Format::of_str(0), &mut own).expect("a String takes any text");
    let read = quiet.read(&Written::val(own.as_bytes(), six::EXPONENT_DIGITS));
    // A negative zero is written with its sign, and read as positive zero.
    let expected = if x.is_zero() { Six::ZERO } else { x };
    assert_eq!(read, Ok(expected), "{x:?} written {own:?}");
}

#[test]
fn any_number_read_operated_on_or_written_keeps_its_invariants() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_number_read_operated_on_or_written_keeps_its_invariants",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        check_reading(&mut rng);
        check_five(&mut rng);
        check_six(&mut rng);
        iteration += 1;
        match deadline {
            None if iteration >= SMOKE_ITERATIONS => break,
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            _ => {}
        }
    }
}
