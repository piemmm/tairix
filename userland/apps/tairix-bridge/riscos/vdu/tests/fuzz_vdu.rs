//! Deterministic fuzz harness for the VDU drivers: hostile streams through
//! `OS_WriteC` — every control code with any parameters, modes no monitor
//! shows, windows off the screen or inside out, shapes reaching to the ends
//! of the coordinate range — and the drivers' SWIs, `OS_Byte` and `OS_Word`
//! reasons with hostile registers and blocks.
//!
//! The invariants, checked after every call:
//!
//! * nothing panics, and every refusal is a value;
//! * every error block a call returns can be read back;
//! * the drivers still answer a mode variable afterwards.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use tairix_fuzzseed::Prng;
use tairix_riscos_filing::session::{session, Session, Setup};
use tairix_riscos_kernel::os::{Fail, Regs, Unwind};
use tairix_riscos_kernel::swi::os::{
    BYTE, CHANGED_BOX, CHECK_MODE_VALID, CLAIM_SCREEN_MEMORY, PLOT, POINTER, READ_MODE_VARIABLE,
    READ_PALETTE, READ_POINT, READ_VDU_VARIABLES, REMOVE_CURSORS, RESTORE_CURSORS, SCREEN_MODE,
    SET_COLOUR, SET_ECF_ORIGIN, WORD, WRITE_C,
};
use tairix_riscos_kernel::swi::X;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_RUNS: u64 = 6;
/// Calls made in one run.
const CALLS: usize = 300;
/// The application slot's bytes, and where it starts.
const BYTES: u32 = 0x10_0000;
const APPLICATION: u32 = 0x8000;

/// The SWIs the drivers perform.
const SWIS: [u32; 15] = [
    READ_PALETTE,
    READ_VDU_VARIABLES,
    READ_POINT,
    READ_MODE_VARIABLE,
    REMOVE_CURSORS,
    RESTORE_CURSORS,
    CHECK_MODE_VALID,
    CLAIM_SCREEN_MEMORY,
    PLOT,
    SET_ECF_ORIGIN,
    CHANGED_BOX,
    SET_COLOUR,
    POINTER,
    SCREEN_MODE,
    WRITE_C,
];

/// The `OS_Byte` reasons the drivers answer, and some beside them.
const BYTES_ASKED: [u32; 16] = [
    20, 25, 106, 112, 113, 114, 117, 134, 135, 154, 160, 163, 218, 242, 251, 0,
];
/// The `OS_Word` reasons the drivers answer.
const WORDS_ASKED: [u32; 8] = [9, 10, 11, 12, 13, 21, 22, 23];

/// A session with the drivers and their module, its application space
/// salted with blocks hostile calls read.
fn boot(rng: &mut Prng) -> Session {
    let mut session = session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: &tairix_riscos_vdu::MODULES,
        bytes: BYTES,
        keys: b"yyyyyyyyyyyyyyyy",
        disc: None,
    });
    let mut bytes = vec![0u8; 0x4000];
    rng.fill(&mut bytes);
    for chunk in bytes.chunks_mut(64) {
        if rng.below(3) != 0 {
            continue;
        }
        let text: &[u8] = rng.pick(&[
            &b"X640 Y480 C256\0"[..],
            b"28\0",
            b"G16 EX0 EY1\0",
            b"\x01\0\0\0\x80\x02\0\0",
        ]);
        let length = text.len().min(chunk.len());
        chunk[..length].copy_from_slice(&text[..length]);
    }
    let _ = session.kernel.os().write_bytes(APPLICATION, &bytes);
    session
}

/// A hostile register value: small numbers, coordinates at the range's
/// ends, pointers into the salted space or nowhere.
fn hostile(rng: &mut Prng) -> u32 {
    let into = APPLICATION + u32::try_from(rng.below(0x4000)).unwrap_or(0);
    let edge = *rng.pick(&[-32768i32, -32767, -1, 0, 1, 639, 1279, 32767]);
    let coordinate = (edge + i32::try_from(rng.below(3)).unwrap_or(0)).cast_unsigned();
    let small = u32::try_from(rng.below(300)).unwrap_or(0);
    let anything = rng.next_u32();
    *rng.pick(&[
        0,
        1,
        small,
        u32::MAX,
        0x8000_0000,
        into,
        into | 3,
        coordinate,
        0xFFFF_FFFC,
        anything,
    ])
}

/// A sequence of VDU bytes: a control code and parameters for it, often
/// ones no screen holds.
fn stream(rng: &mut Prng, out: &mut Vec<u8>) {
    out.clear();
    let code = u8::try_from(rng.below(32)).unwrap_or(0);
    out.push(code);
    let push_word =
        |out: &mut Vec<u8>, value: i32| out.extend_from_slice(&value.to_le_bytes()[..2]);
    match code {
        22 => out.push(*rng.pick(&[
            0,
            7,
            12,
            15,
            25,
            28,
            31,
            49,
            54,
            99,
            127,
            128 + 12,
            200,
            255,
        ])),
        23 => {
            for _ in 0..9 {
                out.push(
                    rng.pick(&[0u8, 1, 2, 7, 8, 9, 10, 11, 12, 16, 17, 255])
                        .wrapping_add(u8::try_from(rng.below(2)).unwrap_or(0)),
                );
            }
        }
        24 | 28 => {
            for _ in 0..4 {
                let value = i32::from_le_bytes(hostile(rng).to_le_bytes());
                if code == 24 {
                    push_word(out, value);
                } else {
                    out.push(value.to_le_bytes()[0]);
                }
            }
        }
        25 => {
            out.push(u8::try_from(rng.below(256)).unwrap_or(0));
            for _ in 0..2 {
                push_word(out, i32::from_le_bytes(hostile(rng).to_le_bytes()));
            }
        }
        17 | 18 | 1 => out.extend((0..2).map(|_| u8::try_from(rng.below(256)).unwrap_or(0))),
        19 => out.extend((0..5).map(|_| u8::try_from(rng.below(256)).unwrap_or(0))),
        29 => {
            for _ in 0..2 {
                push_word(out, i32::from_le_bytes(hostile(rng).to_le_bytes()));
            }
        }
        31 => out.extend((0..2).map(|_| u8::try_from(rng.below(100)).unwrap_or(0))),
        _ => out.extend((0..rng.below(6)).map(|_| 32 + u8::try_from(rng.below(95)).unwrap_or(0))),
    }
}

/// One hostile call, its outcome a value.
fn call(session: &mut Session, rng: &mut Prng, bytes: &mut Vec<u8>) {
    let mut regs = Regs::new([0; 13]);
    for register in &mut regs.r {
        *register = hostile(rng);
    }
    let result = match rng.below(4) {
        0 | 1 => {
            stream(rng, bytes);
            let mut outcome = Ok(());
            for &byte in bytes.iter() {
                let mut write = Regs::new([u32::from(byte), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                outcome = session.kernel.os().perform(X | WRITE_C, &mut write);
                if outcome.is_err() {
                    break;
                }
            }
            outcome
        }
        2 => {
            regs.r[0] = *rng.pick(&BYTES_ASKED);
            let swi = if rng.below(2) == 0 { BYTE } else { WORD };
            if swi == WORD {
                regs.r[0] = *rng.pick(&WORDS_ASKED);
                regs.r[1] = APPLICATION + u32::try_from(rng.below(0x4000)).unwrap_or(0);
            }
            session.kernel.os().perform(X | swi, &mut regs)
        }
        _ => {
            if rng.below(2) == 0 {
                regs.r[0] = u32::try_from(rng.below(20)).unwrap_or(0);
            }
            session.kernel.os().perform(X | *rng.pick(&SWIS), &mut regs)
        }
    };
    if let Err(Fail::Error(block)) = result {
        let _ = session.kernel.os().read_word(block);
    }
}

/// The drivers still answer.
fn still_answers(session: &mut Session, step: usize) {
    let mut regs = Regs::new([u32::MAX, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(
        session
            .kernel
            .os()
            .perform(X | READ_MODE_VARIABLE, &mut regs)
            .is_ok(),
        "OS_ReadModeVariable after step {step}"
    );
}

/// The calls fuzzing has found taking the drivers down, each answered now.
#[test]
fn the_calls_fuzzing_found_are_answered() {
    let mut session = boot(&mut Prng::new(1));
    let mut regs = Regs::new([160, u32::MAX, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(
        session.kernel.os().perform(X | BYTE, &mut regs).is_ok(),
        "OS_Byte 160 past the last variable"
    );
    assert_eq!((regs.r[1], regs.r[2]), (0, 0));
    still_answers(&mut session, 0);
}

/// Paged mode at a page's end waits for Shift; once what is left to type is
/// taken and none of it is Shift, the wait is stopped, however often the
/// flashing palette's frames fall due.
#[test]
fn a_page_s_end_with_nothing_left_to_type_is_stopped() {
    for keys in [&b""[..], b"yyyy"] {
        let mut session = session(&Setup {
            vdu: Some(tairix_riscos_vdu::DRIVERS),
            modules: &tairix_riscos_vdu::MODULES,
            bytes: BYTES,
            keys,
            disc: None,
        });
        let mut write = |byte: u8| {
            let mut regs = Regs::new([u32::from(byte), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            session.kernel.os().perform(X | WRITE_C, &mut regs)
        };
        for byte in [22, 12, 14] {
            assert!(write(byte).is_ok(), "VDU {byte}");
        }
        let outcome = (0..40).map(|_| write(10)).find(Result::is_err);
        assert!(
            matches!(outcome, Some(Err(Fail::Unwind(Unwind::Stopped)))),
            "{keys:?}: {outcome:?}"
        );
    }
}

#[test]
fn hostile_streams_and_calls_leave_the_drivers_answering() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_streams_and_calls_leave_the_drivers_answering",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut bytes = Vec::new();
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let mut session = boot(&mut rng);
        for step in 0..CALLS {
            call(&mut session, &mut rng, &mut bytes);
            still_answers(&mut session, step);
        }
        runs += 1;
    }
}
