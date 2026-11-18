//! Software vectors from guest code: claims and releases, claimants passing
//! the call on and claiming it in either profile, `OS_CallAVector`, and a
//! chain changed while it is called.

use tairix_riscos_arm::psr;

use crate::memory::layout::{RISC_OS_5, SHARED};
use crate::memory::Bits;
use crate::swi::os::{ADD_TO_VECTOR, BYTE, CALL_A_VECTOR, CLAIM, RELEASE};
use crate::swi::X;
use crate::testkit::{asm, with_application, Session, CODE, DATA};

const BYTE_V: u32 = 6;
/// A vector no kernel default claims, for `OS_CallAVector`.
const SPARE_V: u32 = 0x40;

fn registers(low: &[u32]) -> [u32; 13] {
    let mut r = [0; 13];
    r[..low.len()].copy_from_slice(low);
    r
}

/// A claimant counting its calls in the word its R12 names, then passing
/// the call on.
fn counter() -> [u32; 4] {
    [
        asm::ldr(11, 12, 0),
        asm::add(11, 11, 1),
        asm::str(11, 12, 0),
        asm::mov_pc_lr(),
    ]
}

/// Makes `code` a claimant at `at` of `vector` with workspace `workspace`,
/// by `swi`.
fn install(s: &mut Session, swi: u32, vector: u32, at: u32, workspace: u32) -> bool {
    let (regs, _) = s.user_swi(X | swi, registers(&[vector, at, workspace]));
    !regs.flag(psr::V)
}

#[test]
fn claim_removes_its_duplicates_add_to_vector_keeps_them_and_release_takes_the_newest() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    s.poke(CODE, &counter());
    assert!(install(&mut s, CLAIM, BYTE_V, CODE, DATA));
    assert!(install(&mut s, CLAIM, BYTE_V, CODE, DATA));
    s.user_swi(X | BYTE, registers(&[4, 0]));
    assert_eq!(s.peek(DATA), 1, "one claim after two identical ones");
    assert!(install(&mut s, ADD_TO_VECTOR, BYTE_V, CODE, DATA));
    s.user_swi(X | BYTE, registers(&[4, 0]));
    assert_eq!(s.peek(DATA), 3, "added beside it");
    assert!(install(&mut s, RELEASE, BYTE_V, CODE, DATA));
    s.user_swi(X | BYTE, registers(&[4, 0]));
    assert_eq!(s.peek(DATA), 4);
    assert!(install(&mut s, RELEASE, BYTE_V, CODE, DATA));
    s.user_swi(X | BYTE, registers(&[4, 0]));
    assert_eq!(s.peek(DATA), 4, "both released");
    let (regs, _) = s.user_swi(X | RELEASE, registers(&[BYTE_V, CODE, DATA]));
    assert!(regs.flag(psr::V));
    assert_eq!(s.peek(regs.r[0]), 0x1A1);
    let (regs, _) = s.user_swi(X | CLAIM, registers(&[96, CODE, DATA]));
    assert!(regs.flag(psr::V));
    assert_eq!(s.peek(regs.r[0]), 0x1A0);
}

#[test]
fn a_claimant_passes_on_what_it_changed_and_one_that_claims_answers_alone() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    // OS_Byte 4 sets the cursor editing state, returning the old one.
    s.poke(CODE, &[asm::mov(1, 2), asm::mov_pc_lr()]);
    assert!(install(&mut s, CLAIM, BYTE_V, CODE, 0));
    let (regs, _) = s.user_swi(X | BYTE, registers(&[4, 1]));
    assert_eq!(regs.r[1], 0, "the default ran, with the claimant's R1");
    assert!(install(&mut s, RELEASE, BYTE_V, CODE, 0));
    let (regs, _) = s.user_swi(X | BYTE, registers(&[4, 0]));
    assert_eq!(regs.r[1], 2, "the default stored what it was passed");
    s.poke(CODE + 0x40, &[asm::mov(1, 0x99), asm::claim()]);
    assert!(install(&mut s, CLAIM, BYTE_V, CODE + 0x40, 0));
    let (regs, _) = s.user_swi(X | BYTE, registers(&[4, 1]));
    assert_eq!(regs.r[1], 0x99);
    assert!(install(&mut s, RELEASE, BYTE_V, CODE + 0x40, 0));
    let (regs, _) = s.user_swi(X | BYTE, registers(&[4, 0]));
    assert_eq!(regs.r[1], 0, "the claim kept the default from storing 1");
}

#[test]
fn a_26_bit_claimant_passes_on_and_claims_as_its_profile_does() {
    let mut s = with_application(&SHARED, Bits::Bit26);
    s.poke(CODE, &counter());
    s.poke(CODE + 0x40, &[asm::mov(1, 0x42), asm::claim_26()]);
    assert!(install(&mut s, CLAIM, BYTE_V, CODE, DATA));
    let (regs, _) = s.user_swi(X | BYTE, registers(&[4, 3]));
    assert!(!regs.flag(psr::V));
    assert_eq!(s.peek(DATA), 1);
    assert_eq!(regs.r[1], 0, "passed on to the default");
    assert!(install(&mut s, CLAIM, BYTE_V, CODE + 0x40, 0));
    let (regs, state) = s.user_swi(X | BYTE, registers(&[4, 0]));
    assert_eq!(regs.r[1], 0x42);
    assert_eq!(s.peek(DATA), 1, "the older claimant not called");
    assert_eq!(state.pc, crate::testkit::SWI_AT + 4);
}

#[test]
fn call_a_vector_hands_back_every_flag_the_claimant_left() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    let (regs, _) = s.user_swi(
        X | CALL_A_VECTOR,
        registers(&[1, 2, 3, 4, 5, 6, 7, 8, 9, SPARE_V]),
    );
    assert!(
        !regs.flag(psr::V),
        "a vector with no default claims and clears V"
    );
    assert_eq!(regs.r[0], 1);
    // CMP R0, R1 with R0 = R1 sets Z and C.
    s.poke(
        CODE,
        &[asm::mov(0, 5), asm::mov(1, 5), asm::cmp(0, 1), asm::claim()],
    );
    assert!(install(&mut s, CLAIM, SPARE_V, CODE, 0));
    let mut r = [0; 13];
    r[9] = SPARE_V;
    let (regs, _) = s.user_swi(X | CALL_A_VECTOR, r);
    assert!(regs.flag(psr::Z) && regs.flag(psr::C));
    assert!(!regs.flag(psr::N) && !regs.flag(psr::V));
    assert_eq!(regs.r[0], 5);
}

#[test]
fn a_claimant_releasing_itself_mid_call_leaves_the_older_ones_called() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    s.poke(CODE, &counter());
    // The newer claimant releases itself, then passes on.
    let releasing = CODE + 0x40;
    s.poke(
        releasing,
        &[
            asm::push(1 << 14),
            asm::mov(0, BYTE_V),
            asm::mov(1, releasing & 0xFF),
            asm::add(1, 1, releasing & !0xFF),
            asm::mov(2, 0),
            asm::swi(X | RELEASE),
            asm::pull(1 << 14),
            asm::mov(0, 4),
            asm::mov(1, 0),
            asm::mov_pc_lr(),
        ],
    );
    assert!(install(&mut s, CLAIM, BYTE_V, CODE, DATA));
    assert!(install(&mut s, CLAIM, BYTE_V, releasing, 0));
    s.user_swi(X | BYTE, registers(&[4, 0]));
    assert_eq!(s.peek(DATA), 1, "the older claimant still called");
    s.user_swi(X | BYTE, registers(&[4, 0]));
    assert_eq!(s.peek(DATA), 2, "and the released one gone");
}

#[test]
fn a_device_vector_is_claimed_and_released_by_its_registers() {
    const CLAIM_DEVICE: u32 = 0x4B;
    const RELEASE_DEVICE: u32 = 0x4C;
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    let call = |s: &mut Session, swi: u32, r: [u32; 5]| {
        let (regs, _) = s.user_swi(
            X | swi,
            [r[0], r[1], r[2], r[3], r[4], 0, 0, 0, 0, 0, 0, 0, 0],
        );
        if regs.flag(psr::V) {
            Err(s.peek(regs.r[0]))
        } else {
            Ok(())
        }
    };
    assert_eq!(call(&mut s, CLAIM_DEVICE, [3, 0x9000, 1, 0, 0]), Ok(()));
    assert_eq!(
        call(&mut s, CLAIM_DEVICE, [3, 0x9000, 1, 0, 0]),
        Ok(()),
        "the same again"
    );
    assert_eq!(call(&mut s, RELEASE_DEVICE, [3, 0x9000, 1, 0, 0]), Ok(()));
    assert_eq!(
        call(&mut s, RELEASE_DEVICE, [3, 0x9000, 1, 0, 0]),
        Err(0x1A3),
        "it was there once"
    );
    assert_eq!(
        call(&mut s, CLAIM_DEVICE, [13, 0x9000, 1, 0x3000, 4]),
        Ok(())
    );
    assert_eq!(
        call(&mut s, RELEASE_DEVICE, [13, 0x9000, 1, 0x3000, 8]),
        Err(0x1A3),
        "an expansion card's mask counts"
    );
    assert_eq!(
        call(&mut s, RELEASE_DEVICE, [13, 0x9000, 1, 0x3000, 4]),
        Ok(())
    );
    assert_eq!(
        call(&mut s, CLAIM_DEVICE, [27, 0x9000, 1, 0, 0]),
        Err(0x1A2),
        "no such device"
    );
}
