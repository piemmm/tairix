//! SWIs from guest code: results and flags handed back, errors returned or
//! raised, SWIs not known, `OS_CallASWI` and SWI names.

use tairix_riscos_arm::psr::{self, Mode};

use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::os::{Ended, Outcome};
use crate::swi::os::{BYTE, CLAIM, DYNAMIC_AREA, GENERATE_ERROR, READ_MONOTONIC_TIME};
use crate::swi::os::{CALL_A_SWI, SWI_NUMBER_FROM_STRING, SWI_NUMBER_TO_STRING};
use crate::swi::X;
use crate::testkit::{asm, with_application, Session, CODE, DATA, SWI_AT};

const UKSWIV: u32 = 0x18;

fn session() -> Session {
    with_application(&RISC_OS_5, Bits::Bit32)
}

fn registers(low: &[u32]) -> [u32; 13] {
    let mut r = [0; 13];
    r[..low.len()].copy_from_slice(low);
    r
}

#[test]
fn a_swi_hands_back_its_results_with_v_clear_and_returns_past_itself() {
    let mut s = session();
    s.clock.set(1234);
    let (regs, state) = s.user_swi(X | READ_MONOTONIC_TIME, registers(&[7, 8]));
    assert_eq!(regs.r[0], 1234);
    assert_eq!(regs.r[1], 8, "a register the SWI leaves alone");
    assert!(!regs.flag(psr::V));
    assert_eq!(state.pc, SWI_AT + 4);
    assert_eq!(Mode::from_bits(state.cpsr), Some(Mode::User));
}

#[test]
fn an_x_swis_error_comes_back_with_v_set_and_r0_at_its_block() {
    let mut s = session();
    let (regs, state) = s.user_swi(X | DYNAMIC_AREA, registers(&[11]));
    assert!(regs.flag(psr::V));
    assert_eq!(s.peek(regs.r[0]), 0x180);
    assert_eq!(s.error_text(regs.r[0]), b"No such reason code");
    assert_eq!(state.pc, SWI_AT + 4);
}

#[test]
fn an_error_without_x_enters_the_error_handler_which_ends_the_program() {
    let mut s = session();
    match s.start_swi(DYNAMIC_AREA, registers(&[11])) {
        Outcome::Ended(Ended::Error { number, block }) => {
            assert_eq!(number, 0x180);
            assert_eq!(s.error_text(block), b"No such reason code");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_swi_the_kernel_has_not_is_not_known_by_its_number() {
    let mut s = session();
    let (regs, _) = s.user_swi(X | 0x63, [0; 13]);
    assert!(regs.flag(psr::V));
    assert_eq!(s.peek(regs.r[0]), 0x1E6);
    assert_eq!(s.error_text(regs.r[0]), b"SWI &63 not known");
}

#[test]
fn a_swi_no_module_provides_goes_through_ukswiv_and_a_claimant_answers_it() {
    let mut s = session();
    let (regs, _) = s.user_swi(X | 0x4_5678, [0; 13]);
    assert!(regs.flag(psr::V));
    assert_eq!(s.error_text(regs.r[0]), b"SWI &45678 not known");
    // A claimant answering the SWI in R11 with R0 = 99.
    s.poke(CODE, &[asm::mov(0, 99), asm::claim()]);
    let (regs, _) = s.user_swi(X | CLAIM, registers(&[UKSWIV, CODE, 0]));
    assert!(!regs.flag(psr::V));
    let (regs, _) = s.user_swi(X | 0x4_5678, [0; 13]);
    assert!(!regs.flag(psr::V));
    assert_eq!(regs.r[0], 99);
}

#[test]
fn call_a_swi_performs_the_swi_r10_names_its_x_bit_deciding() {
    let mut s = session();
    s.clock.set(77);
    let mut r = [0; 13];
    r[10] = READ_MONOTONIC_TIME;
    let (regs, _) = s.user_swi(X | CALL_A_SWI, r);
    assert_eq!(regs.r[0], 77);
    r[10] = X | DYNAMIC_AREA;
    r[0] = 11;
    let (regs, _) = s.user_swi(CALL_A_SWI, r);
    assert!(regs.flag(psr::V), "returned, as the X bit R10 holds says");
    // A chain of them, followed a bounded way.
    r[10] = X | CALL_A_SWI;
    let (regs, _) = s.user_swi(X | CALL_A_SWI, r);
    assert!(regs.flag(psr::V));
    assert_eq!(s.peek(regs.r[0]), 0x1E6);
}

#[test]
fn generate_error_returns_its_block_unchecked_and_a_claimant_s_bad_pointer_is_replaced() {
    let mut s = session();
    s.poke(DATA, &[0x1234]);
    s.write(DATA + 4, b"Mine\0");
    let (regs, _) = s.user_swi(X | GENERATE_ERROR, registers(&[DATA]));
    assert!(regs.flag(psr::V));
    assert_eq!(regs.r[0], DATA);
    // A ByteV claimant failing, V set by an overflowing compare, with an
    // error pointer that cannot be one.
    s.poke(
        CODE,
        &[
            asm::mov(0, 0x8000_0000),
            asm::mov(1, 1),
            asm::cmp(0, 1),
            asm::mov(0, 0x10),
            asm::claim(),
        ],
    );
    let (regs, _) = s.user_swi(X | CLAIM, registers(&[6, CODE, 0]));
    assert!(!regs.flag(psr::V));
    let (regs, _) = s.user_swi(X | BYTE, registers(&[0]));
    assert!(regs.flag(psr::V));
    assert_eq!(s.peek(regs.r[0]), 0x116);
    assert_eq!(
        s.error_text(regs.r[0]),
        b"SWI &6 returned an invalid error pointer"
    );
    // The kernel's own call is checked at the same exit.
    let mut call = crate::os::Regs::default();
    let crate::os::Fail::Error(block) = s
        .kernel
        .os()
        .perform(X | BYTE, &mut call)
        .expect_err("the claimant fails")
    else {
        panic!("an error, not an unwind");
    };
    assert_eq!(s.peek(block), 0x116);
}

#[test]
fn swi_names_and_numbers_convert_both_ways() {
    let mut s = session();
    let (regs, _) = s.user_swi(X | SWI_NUMBER_TO_STRING, registers(&[X | BYTE, DATA, 64]));
    assert!(!regs.flag(psr::V));
    assert_eq!(regs.r[2], 9, "the terminator counted");
    assert_eq!(s.read(DATA, 9), b"XOS_Byte\0");
    let (regs, _) = s.user_swi(X | SWI_NUMBER_TO_STRING, registers(&[0x120 | X, DATA, 64]));
    assert_eq!(s.read(DATA, regs.r[2] as usize), b"XOS_WriteI+\" \"\0");
    s.write(DATA, b"OS_ReadMonotonicTime\0");
    let (regs, _) = s.user_swi(X | SWI_NUMBER_FROM_STRING, registers(&[0, DATA]));
    assert!(!regs.flag(psr::V));
    assert_eq!(regs.r[0], READ_MONOTONIC_TIME);
    s.write(DATA, b"OS_NoSuchThing\0");
    let (regs, _) = s.user_swi(X | SWI_NUMBER_FROM_STRING, registers(&[0, DATA]));
    assert!(regs.flag(psr::V));
    assert_eq!(s.peek(regs.r[0]), 0x1E6);
}

#[test]
fn a_name_too_long_for_its_buffer_is_cut_and_overflow_is_pointed_at_with_v_clear() {
    let mut s = session();
    s.write(DATA, &[0xAA; 8]);
    let (regs, _) = s.user_swi(X | SWI_NUMBER_TO_STRING, registers(&[BYTE, DATA, 4]));
    assert!(!regs.flag(psr::V));
    assert_eq!(regs.r[2], 4);
    assert_eq!(s.read(DATA, 5), b"OS_B\xAA");
    assert_eq!(s.peek(regs.r[0]), 0x1E4);
}

#[test]
fn each_sound_swi_lies_in_its_own_module_s_chunk() {
    use crate::swi::chunk::{SOUND_CHANNELS, SOUND_DMA, SOUND_SCHEDULER};
    use crate::swi::sound::{
        ATTACH_NAMED_VOICE, CONFIGURE, CONTROL, CONTROL_PACKED, ENABLE, Q_BEAT, Q_INIT, Q_TEMPO,
        STEREO,
    };
    let chunk_of = |swi: u32| swi & !(crate::swi::CHUNK - 1);
    for swi in [CONFIGURE, ENABLE, STEREO] {
        assert_eq!(chunk_of(swi), SOUND_DMA, "{swi:#X} is SoundDMA's");
    }
    for swi in [CONTROL_PACKED, CONTROL, ATTACH_NAMED_VOICE] {
        assert_eq!(chunk_of(swi), SOUND_CHANNELS, "{swi:#X} is SoundChannels'");
    }
    for swi in [Q_INIT, Q_TEMPO, Q_BEAT] {
        assert_eq!(
            chunk_of(swi),
            SOUND_SCHEDULER,
            "{swi:#X} is SoundScheduler's"
        );
    }
    let all = [
        CONFIGURE,
        ENABLE,
        STEREO,
        CONTROL_PACKED,
        CONTROL,
        ATTACH_NAMED_VOICE,
        Q_INIT,
        Q_TEMPO,
        Q_BEAT,
    ];
    for (i, a) in all.iter().enumerate() {
        assert!(all[i + 1..].iter().all(|b| b != a), "{a:#X} named twice");
    }
}
