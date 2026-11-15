//! Deterministic fuzz harness for RISC OS's command-line machinery, which
//! reads strings, expressions, templates and Obey files a guest program or a
//! foreign disc supplies. For any input:
//!
//! 1. Nothing panics, and every result is within its bound: GSTrans within
//!    its buffer, an expression's string within its most, a substitution
//!    within its limit, `OS_ReadArgs` within its buffer, and no reading past
//!    its budget.
//! 2. Any bytes, escaped as `*Show` writes a value, GSTrans back to themselves.
//! 3. A fully bracketed integer expression evaluates as wrapping 32-bit
//!    arithmetic does.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from the
//! same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use tairix_fuzzseed::Prng;
use tairix_obey::args::substitute;
use tairix_obey::command::{read_only, Observer};
use tairix_obey::expr::{evaluate, MOST_STRING};
use tairix_obey::gstrans::{gstrans, Flags};
use tairix_obey::readargs::read_args;
use tairix_obey::{Answer, Budget, Store, Value, Variables};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 300;

/// Bytes a command line is made of, often the ones that mean something.
fn hostile(rng: &mut Prng, len: usize) -> Vec<u8> {
    const MEANINGFUL: &[u8] = b"|<>\"%*!#$&()+-/=:,. _?@AaZz09\r\n\t\x00\x01\x7F\xE9";
    (0..len)
        .map(|_| match rng.below(3) {
            0 => rng.next_u8(),
            _ => *rng.pick(MEANINGFUL),
        })
        .collect()
}

/// A store of a few variables, macros among them that name one another.
fn variables(rng: &mut Prng) -> Store {
    let mut vars = Store::with_room(8, 256).unwrap();
    for name in [&b"A"[..], b"B", b"Obey$Dir", b"N", b"Alias$Set"] {
        let len = rng.below(24);
        let value = match rng.below(3) {
            0 => Value::String(hostile(rng, len)),
            1 => Value::Number(rng.next_u32().cast_signed()),
            _ => Value::Macro(
                b"<A><B>x"
                    .iter()
                    .copied()
                    .chain(hostile(rng, len))
                    .filter(|&b| b >= b' ')
                    .collect(),
            ),
        };
        let _ = vars.assign(name, value);
    }
    vars
}

struct Ignored;

impl Observer for Ignored {
    fn icon_sprites(&mut self, _: &[u8], _: &mut Store, _: &mut Budget) {}
}

fn hostile_everything(rng: &mut Prng) {
    let mut vars = variables(rng);
    let len = rng.below(80);
    let text = hostile(rng, len);
    let budget_size = rng.below(64);
    let mut budget = Budget::new(budget_size);
    let flags = Flags::from_bits(rng.next_u32());
    let room = rng.below(64);
    let mut buffer = vec![0u8; room];
    let done = gstrans(&text, flags, &mut vars, &mut budget, &mut buffer);
    assert!(done.stored <= buffer.len());
    assert!(done.next <= text.len() + 1);
    assert!(budget.left() <= budget_size);

    match evaluate(&text, &mut vars, &mut Budget::new(64)) {
        Ok(Answer::String(s)) => assert!(s.len() <= MOST_STRING),
        Ok(Answer::Integer(_)) | Err(_) => {}
    }

    let limit = rng.below(128);
    let len = rng.below(40);
    let args = hostile(rng, len);
    if let Ok(out) = substitute(&args, &text, rng.below(2) == 0, limit) {
        assert!(out.len() <= limit);
    }

    let len = rng.below(24);
    let template = hostile(rng, len);
    let capacity = rng.below(96);
    if let Ok(read) = read_args(
        &template,
        &args,
        &mut vars,
        &mut Budget::new(64),
        0x8000,
        capacity,
    ) {
        assert!(read.buffer.len() <= capacity);
        assert!(read.left <= capacity);
    }

    let len = rng.below(200);
    let file = hostile(rng, len);
    let mut store = Store::with_room(8, 128).unwrap();
    let mut reading = Budget::new(32);
    let _ = read_only(&file, b"\0", &mut store, &mut Ignored, &mut reading);
    assert!(reading.left() <= 32);
}

/// `bytes` written so that GSTrans reads them back, as `*Show` writes a value.
fn escaped(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for &b in bytes {
        if b >= 0x80 {
            out.extend_from_slice(b"|!");
        }
        match b & 0x7F {
            0x7F => out.extend_from_slice(b"|?"),
            c @ 0..=0x1F => out.extend_from_slice(&[b'|', c + 0x40]),
            // A space too, as GSInit skips one that leads.
            c @ (b'|' | b'"' | b'<' | b' ') => out.extend_from_slice(&[b'|', c]),
            c => out.push(c),
        }
    }
    out
}

fn escapes_round_trip(rng: &mut Prng) {
    let len = rng.below(48);
    let bytes: Vec<u8> = (0..len).map(|_| rng.next_u8()).collect();
    let text = escaped(&bytes);
    let mut vars = Store::with_room(1, 1).unwrap();
    let mut buffer = vec![0u8; text.len() + 1];
    let done = gstrans(
        &text,
        Flags::NO_QUOTES,
        &mut vars,
        &mut Budget::new(0),
        &mut buffer,
    );
    assert_eq!(done.ended, Ok(Some(0)), "{bytes:02X?}");
    assert_eq!(&buffer[..done.stored], &bytes[..], "{text:?}");
}

/// A fully bracketed expression over `+ - *` and its value.
fn bracketed(rng: &mut Prng, depth: u32) -> (String, i32) {
    if depth == 0 || rng.below(3) == 0 {
        let n = rng.next_u32() >> rng.below(32);
        return (n.to_string(), n.cast_signed());
    }
    let (a, x) = bracketed(rng, depth - 1);
    let (b, y) = bracketed(rng, depth - 1);
    let (op, value) = match rng.below(3) {
        0 => ("+", x.wrapping_add(y)),
        1 => ("-", x.wrapping_sub(y)),
        _ => ("*", x.wrapping_mul(y)),
    };
    (format!("({a}{op}{b})"), value)
}

fn arithmetic_agrees(rng: &mut Prng) {
    let (text, value) = bracketed(rng, 5);
    let mut vars = Store::with_room(1, 1).unwrap();
    assert_eq!(
        evaluate(text.as_bytes(), &mut vars, &mut Budget::new(0)),
        Ok(Answer::Integer(value)),
        "{text}"
    );
}

#[test]
fn hostile_command_lines_stay_bounded_escapes_round_trip_and_arithmetic_agrees() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_command_lines_stay_bounded_escapes_round_trip_and_arithmetic_agrees",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            hostile_everything(&mut rng);
            escapes_round_trip(&mut rng);
            arithmetic_agrees(&mut rng);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
