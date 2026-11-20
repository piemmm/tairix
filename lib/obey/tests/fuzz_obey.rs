//! Deterministic fuzz harness for RISC OS's command-line machinery, which
//! reads strings, expressions, templates and Obey files a guest program or a
//! foreign disc supplies. For any input:
//!
//! 1. Nothing panics, and every result is within its bound: GSTrans within
//!    its buffer, an expression's string within its most, a substitution
//!    within its limit, `OS_ReadArgs` within its buffer, a variable read
//!    within its longest, and no reading examines more variables than its
//!    budget, wildcards over long names among them.
//! 2. Any bytes, written in GSTrans's escapes, GSTrans back to themselves.
//! 3. A fully bracketed integer expression evaluates as wrapping 32-bit
//!    arithmetic does.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from the
//! same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use tairix_fuzzseed::Prng;
use tairix_obey::args::substitute;
use tairix_obey::command::{read_only, Context, Observer};
use tairix_obey::expr::{evaluate, MOST_STRING};
use tairix_obey::gstrans::{gstrans, Flags};
use tairix_obey::readargs::{read_args, Arg};
use tairix_obey::vars::read_expanded;
use tairix_obey::{Answer, Budget, Error, Store, Value, Variables};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 300;

/// Bytes a command line is made of, often the ones that mean something, and
/// often whole references, wildcards among them.
fn hostile(rng: &mut Prng, len: usize) -> Vec<u8> {
    const MEANINGFUL: &[u8] = b"|<>\"%*!#$&()+-/=:,. _?@AaZz09\r\n\t\x00\x01\x7F\xE9";
    const REFERENCES: [&[u8]; 6] = [b"<A*>", b"<*>", b"<Long*>", b"<#*#>", b"<N>", b"<Obey$Dir>"];
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        match rng.below(6) {
            0 => out.push(rng.next_u8()),
            1 => out.extend_from_slice(rng.pick(&REFERENCES)),
            _ => out.push(*rng.pick(MEANINGFUL)),
        }
    }
    out
}

/// A name as long as the store takes, `Long` and its number first.
fn long_name(n: usize) -> Vec<u8> {
    let mut name = format!("Long{n}").into_bytes();
    name.resize(NAMES, b'x');
    name
}

/// The longest name the fuzzed store takes.
const NAMES: usize = 96;

/// A [`Store`] that counts the variables its lookups examine.
struct Counted {
    store: Store,
    examined: usize,
}

impl Variables for Counted {
    fn lookup(&mut self, pattern: &[u8]) -> Result<Option<Value>, Error> {
        self.examined += self.store.cost(pattern);
        self.store.lookup(pattern)
    }

    fn exists(&mut self, pattern: &[u8]) -> Result<bool, Error> {
        self.store.exists(pattern)
    }

    fn cost(&self, pattern: &[u8]) -> usize {
        self.store.cost(pattern)
    }

    fn assign(&mut self, pattern: &[u8], value: Value) -> Result<(), Error> {
        self.store.assign(pattern, value)
    }

    fn delete(&mut self, pattern: &[u8]) -> Result<bool, Error> {
        self.examined += self.store.cost(pattern);
        self.store.delete(pattern)
    }

    fn unset(&mut self, pattern: &[u8]) -> Result<usize, Error> {
        self.examined += self.store.cost(pattern);
        self.store.unset(pattern)
    }
}

/// `read` with a budget of `budget`, held to examining no more variables
/// than the budget allows.
fn within<T>(
    vars: &mut Counted,
    budget: usize,
    read: impl FnOnce(&mut Counted, &mut Budget) -> T,
) -> T {
    vars.examined = 0;
    let mut reading = Budget::new(budget);
    let answer = read(vars, &mut reading);
    assert!(
        vars.examined <= budget,
        "{} variables examined on a budget of {budget}",
        vars.examined
    );
    assert!(
        vars.examined <= budget - reading.left(),
        "a lookup the budget was not charged for"
    );
    answer
}

/// A store of a few variables, macros among them that name one another, and
/// long names a wildcard must match against.
fn variables(rng: &mut Prng) -> Counted {
    let mut vars = Store::with_room(16, 256, NAMES).unwrap();
    for n in 0..rng.below(8) {
        let _ = vars.assign(&long_name(n), Value::String(b"x".to_vec()));
    }
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
    Counted {
        store: vars,
        examined: 0,
    }
}

struct Ignored;

impl Observer for Ignored {
    fn icon_sprites(&mut self, _: &[u8], _: &mut dyn Variables, _: &mut Budget) {}
}

fn hostile_everything(rng: &mut Prng) {
    let mut vars = variables(rng);
    let len = rng.below(80);
    let text = hostile(rng, len);
    let budget = rng.below(64);
    let flags = Flags::from_bits(rng.next_u32());
    let room = rng.below(64);
    let mut buffer = vec![0u8; room];
    let done = within(&mut vars, budget, |vars, budget| {
        gstrans(&text, flags, vars, budget, &mut buffer)
    });
    assert!(done.next.is_none_or(|next| next <= text.len() + 1));

    match within(&mut vars, 64, |vars, budget| evaluate(&text, vars, budget)) {
        Ok(Answer::String(s)) => assert!(s.len() <= MOST_STRING),
        Ok(Answer::Integer(_)) | Err(_) => {}
    }

    let longest = rng.below(64);
    let len = rng.below(12);
    let pattern = hostile(rng, len);
    if let Ok(Some(value)) = within(&mut vars, budget, |vars, budget| {
        read_expanded(vars, &pattern, budget, longest)
    }) {
        assert!(value.len() <= longest);
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
    if let Ok(read) = within(&mut vars, 64, |vars, budget| {
        read_args(&template, &args, vars, budget, 0x8000, capacity)
    }) {
        assert!(read.buffer.len() <= capacity);
        assert_eq!(read.left, capacity - used(&read.args, &read.buffer, 0x8000));
    }

    let len = rng.below(200);
    let file = hostile(rng, len);
    let _ = within(&mut vars, 32, |vars, budget| {
        let mut cx = Context {
            vars,
            longest: 128,
            observer: &mut Ignored,
            budget,
        };
        read_only(&file, b"\0", b"", &mut cx)
    });
}

/// The bytes a reading's buffer counts as used: its address words, then up
/// to the end of the value laid last, an `/E` value found by its word, and
/// three more for each `/E` integer, which counts eight bytes wherever it
/// lies but lays five, the next value following them. What follows the last
/// value — a `/G` value's terminator, what is left of the text an `/E` or
/// `/G` value was copied from — is written but not counted.
fn used(args: &[Arg], buffer: &[u8], base: u32) -> usize {
    let integers = args
        .iter()
        .filter(|arg| matches!(arg, Arg::Evaluated(Answer::Integer(_))))
        .count();
    let laid = args
        .iter()
        .enumerate()
        .filter_map(|(entry, arg)| match arg {
            Arg::Text(at) => Some(at.end + 1),
            Arg::Translated(at) => Some(at.end),
            Arg::Evaluated(answer) => {
                let word = buffer.get(entry * 4..entry * 4 + 4)?;
                let address = u32::from_le_bytes(word.try_into().ok()?);
                let start = usize::try_from(address.checked_sub(base)?).ok()?;
                Some(match answer {
                    Answer::Integer(_) => start + 5,
                    Answer::String(bytes) => start + 3 + bytes.len(),
                })
            }
            Arg::Absent | Arg::Switch => None,
        })
        .fold(args.len() * 4, usize::max);
    laid + 3 * integers
}

/// `bytes` written in GSTrans's escapes, so that GSTrans reads them back.
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
    let mut vars = Store::with_room(1, 1, 1).unwrap();
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
    let mut vars = Store::with_room(1, 1, 1).unwrap();
    assert_eq!(
        evaluate(text.as_bytes(), &mut vars, &mut Budget::new(0)),
        Ok(Answer::Integer(value)),
        "{text}"
    );
}

/// Regression: a `/G` value GSTrans makes shorter than its text leaves the
/// text's tail in the buffer, and the room left counts none of it.
#[test]
fn a_translation_shorter_than_its_text_leaves_its_tail_uncounted() {
    let mut store = Store::with_room(16, 256, NAMES).unwrap();
    store
        .assign(&long_name(0), Value::String(b"x".to_vec()))
        .unwrap();
    let mut vars = Counted { store, examined: 0 };
    let read = within(&mut vars, 64, |vars, budget| {
        read_args(b"a/G", b"+=><Long*>", vars, budget, 0x8000, 47)
    })
    .unwrap();
    assert_eq!(read.args, [Arg::Translated(6..10)]);
    assert!(read.buffer.len() > 11, "the text's tail stays");
    assert_eq!(read.left, 37);
    assert_eq!(read.left, 47 - used(&read.args, &read.buffer, 0x8000));
}

/// Regression: an `/E` integer laid before another value still counts the
/// three bytes it never writes, wherever it lies.
#[test]
fn an_integer_laid_before_another_value_counts_eight_bytes() {
    let store = Store::with_room(16, 256, NAMES).unwrap();
    let mut vars = Counted { store, examined: 0 };
    let read = within(&mut vars, 64, |vars, budget| {
        read_args(b"n/E,a", b"1 z", vars, budget, 0x8000, 64)
    })
    .unwrap();
    assert_eq!(read.args[1], Arg::Text(13..14), "laid five bytes on");
    assert_eq!(read.left, 64 - 18);
    assert_eq!(read.left, 64 - used(&read.args, &read.buffer, 0x8000));
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
