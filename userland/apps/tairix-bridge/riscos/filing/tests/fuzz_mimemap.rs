//! Deterministic fuzz harness for MimeMap: `MimeMap_Translate` on hostile
//! registers — forms in and out of range, numbers past a type, strings with
//! no terminator, pointers at memory there is none of — and `*MimeMap` on
//! hostile tails.
//!
//! The invariants:
//!
//! * nothing panics, every refusal is a value, and the only unwind is the
//!   abort a pointer at no memory takes;
//! * a translation that answers in a string writes it, zero-terminated, at
//!   the R3 it was given and nowhere else;
//! * the session still answers after every call.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz` extends it to a wall-clock
//! budget.

mod common;

use common::{command, read};
use tairix_fuzzseed::Prng;
use tairix_riscos_kernel::os::{Fail, Os, Regs, Unwind};
use tairix_riscos_kernel::swi::mime_map::TRANSLATE;
use tairix_riscos_kernel::swi::os::READ_MONOTONIC_TIME;
use tairix_riscos_kernel::swi::X;

/// Runs a plain `cargo test` makes.
const SMOKE_RUNS: u64 = 32;
/// Calls in one run.
const STEPS: usize = 200;

/// What a hostile string is made of.
const PIECES: [&[u8]; 14] = [
    b"text/plain",
    b"TEXT/HTML",
    b"txt",
    b".zip",
    b"&FFF",
    b"&",
    b"Text",
    b"Data",
    b"/",
    b".",
    b" ",
    b"\r",
    b"\0",
    b"application/x-no-such-type",
];

fn hostile_string(rng: &mut Prng) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..rng.below(6) {
        out.extend_from_slice(PIECES[rng.below(PIECES.len())]);
    }
    if rng.below(4) == 0 {
        out.extend((0..rng.below(400)).map(|_| rng.next_u8()));
    }
    out
}

/// A register value: a form, a small number, a pointer at a hostile string,
/// one off the end of memory, or anything.
fn hostile(os: &mut Os<'_>, rng: &mut Prng) -> u32 {
    match rng.below(6) {
        0 => u32::try_from(rng.below(6)).unwrap_or(0),
        1 => u32::try_from(rng.below(0x1100)).unwrap_or(0),
        2 | 3 => {
            let bytes = hostile_string(rng);
            common::block(os, &bytes)
        }
        4 => 0xFFFF_FFF0 + u32::try_from(rng.below(16)).unwrap_or(0),
        _ => rng.next_u32(),
    }
}

/// A call's outcome: an answer, a refusal, or the abort a pointer at no
/// memory takes.
fn answered_as_a_value(outcome: Result<(), Fail>) {
    match outcome {
        Ok(()) | Err(Fail::Error(_) | Fail::Unwind(Unwind::Transfer)) => {}
        Err(Fail::Unwind(unwind)) => panic!("unwound: {unwind:?}"),
    }
}

#[test]
fn any_translation_or_line_answers_as_a_value() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_translation_or_line_answers_as_a_value",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let mut kernel = common::kernel();
        let mut os = kernel.os();
        for _ in 0..STEPS {
            if rng.below(4) == 0 {
                let mut line = b"MimeMap ".to_vec();
                line.extend(hostile_string(&mut rng).into_iter().filter(|&b| b != 0));
                answered_as_a_value(command(&mut os, &line));
            } else {
                let buffer = os.rma_claim(320).expect("room");
                os.write_bytes(buffer, &[0xEE; 320]).expect("written");
                let into = if rng.below(8) == 0 {
                    hostile(&mut os, &mut rng)
                } else {
                    buffer + 32
                };
                let mut regs = Regs::default();
                regs.r[..4].copy_from_slice(&[
                    hostile(&mut os, &mut rng),
                    hostile(&mut os, &mut rng),
                    hostile(&mut os, &mut rng),
                    into,
                ]);
                let to = regs.r[2];
                let answered = os.perform(X | TRANSLATE, &mut regs);
                let wrote = answered.is_ok() && into == buffer + 32 && (1..=3).contains(&to);
                answered_as_a_value(answered);
                if wrote {
                    let after = read(&mut os, buffer, 320);
                    let end = after[32..]
                        .iter()
                        .position(|&b| b == 0)
                        .expect("terminated");
                    assert!(after[..32].iter().all(|&b| b == 0xEE), "nothing before R3");
                    assert!(
                        after[33 + end..].iter().all(|&b| b == 0xEE),
                        "nothing after"
                    );
                }
            }
            let mut regs = Regs::default();
            assert!(os.perform(X | READ_MONOTONIC_TIME, &mut regs).is_ok());
        }
        runs += 1;
    }
}
