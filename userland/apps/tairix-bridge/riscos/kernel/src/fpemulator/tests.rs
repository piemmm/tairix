//! FPEmulator held to what RISC OS 5.30's answered: its SWIs, contexts laid
//! out and switched as genuine's are, and the dumps an enabled trap leaves.

use alloc::vec::Vec;

use tairix_riscos_arm::a32;
use tairix_riscos_arm::cond::Condition;
use tairix_riscos_arm::fpa::{Format, Precision, Rounding};
use tairix_riscos_arm::instruction::{
    CoIndex, FpaDyadicOp, FpaOperand, FpaRegisterOp, FpaTransferOp, Instruction, Op,
};
use tairix_riscos_arm::reg::Reg;

use super::*;
use crate::memory::layout::{RISC_OS_5, SHARED};
use crate::memory::Bits;
use crate::os::{Ended, Outcome};
use crate::swi::fp_emulator::{
    ABORT, ACTIVATE_CONTEXT, CHANGE_CONTEXT, CONTEXT_LENGTH, DEACTIVATE_CONTEXT, EXCEPTION_DUMP,
    INIT_CONTEXT, LOAD_CONTEXT, SAVE_CONTEXT, VERSION as VERSION_SWI,
};
use crate::swi::os::{CHANGE_ENVIRONMENT, MODULE, SWI_NUMBER_TO_STRING};
use crate::swi::X;
use crate::testkit::{asm, with_application, Session, CODE, DATA};

const CONTEXT: u32 = DATA;
const AREA: u32 = DATA + 0x100;
const DOUBLES: u32 = DATA + 0x200;
const NAME: u32 = DATA + 0x300;

fn session() -> Session {
    with_application(&RISC_OS_5, Bits::Bit32)
}

fn registers(low: &[u32]) -> [u32; 13] {
    let mut r = [0; 13];
    r[..low.len()].copy_from_slice(low);
    r
}

/// SWI `number` with `low` from R0 on: R0 after.
fn answer(s: &mut Session, number: u32, low: &[u32]) -> u32 {
    s.user_swi(number, registers(low)).0.r[0]
}

fn words(s: &mut Session, at: u32, n: u32) -> Vec<u32> {
    (0..n).map(|k| s.peek(at + 4 * k)).collect()
}

fn encode(op: Op) -> u32 {
    a32::encode(&Instruction {
        cond: Condition::Al,
        op,
    })
    .expect("encodable")
}

fn double(fd: u8, rn: u32, offset: u8, op: FpaTransferOp) -> u32 {
    encode(Op::FpaTransfer {
        op,
        format: Format::Double,
        fd,
        rn: Reg::field(rn),
        up: true,
        index: CoIndex::Offset,
        offset,
    })
}

fn divide(fd: u8, fn_: u8, fm: u8) -> u32 {
    encode(Op::FpaDyadic {
        op: FpaDyadicOp::Dvf,
        precision: Precision::Double,
        rounding: Rounding::Nearest,
        fd,
        fn_,
        fm: FpaOperand::Register(fm),
    })
}

fn rfs(rd: u32) -> u32 {
    encode(Op::FpaRegister {
        op: FpaRegisterOp::Rfs,
        precision: Precision::Single,
        rounding: Rounding::Nearest,
        fn_: 0,
        fm: FpaOperand::Register(0),
        rd: Reg::field(rd),
    })
}

/// `code` run at `CODE` in User mode with `low` from R0 on, a spin after it.
fn run(s: &mut Session, code: &[u32], low: &[u32]) -> Outcome {
    let spin = CODE + 4 * u32::try_from(code.len()).expect("short");
    s.poke(CODE, &[code, &[asm::b(spin, spin)]].concat());
    s.enter(Bits::Bit32, CODE, registers(low));
    s.resume()
}

/// F1 and F2 loaded with the doubles whose high words are `f1` and `f2`.
fn load_doubles(s: &mut Session, f1: u32, f2: u32) {
    s.poke(DOUBLES, &[f1, 0, f2, 0]);
    let code = [
        double(1, 0, 0, FpaTransferOp::Ldf),
        double(2, 0, 2, FpaTransferOp::Ldf),
    ];
    assert_eq!(run(s, &code, &[DOUBLES]), Outcome::Budget);
}

/// The FPSR, as `RFS` reads it.
fn status(s: &mut Session) -> u32 {
    assert_eq!(run(s, &[rfs(0)], &[]), Outcome::Budget);
    s.kernel.os().current_registers()[0]
}

const FRESH: u32 = 0x0107_0000;
const ONE_AND_A_HALF: [u32; 3] = [0x3FFF, 0xC000_0000, 0];
const TWO: [u32; 3] = [0x4000, 0x8000_0000, 0];

#[test]
fn the_version_names_and_context_length_are_genuine_s() {
    let mut s = session();
    assert_eq!(answer(&mut s, VERSION_SWI, &[]), 439);
    assert_eq!(answer(&mut s, CONTEXT_LENGTH, &[]), 136);
    let names = [
        "Version",
        "DeactivateContext",
        "ActivateContext",
        "ChangeContext",
        "ContextLength",
        "InitContext",
        "ExceptionDump",
        "Abort",
        "LoadContext",
        "SaveContext",
        "10",
    ];
    for (offset, name) in (0..).zip(names) {
        s.user_swi(SWI_NUMBER_TO_STRING, registers(&[CHUNK + offset, NAME, 64]));
        let text = s
            .kernel
            .os()
            .read_until(NAME, 64, |b| b == 0)
            .expect("a name")
            .0;
        assert_eq!(text, [&b"FPEmulator_"[..], name.as_bytes()].concat());
    }
    s.write(NAME, b"FPEmulator\0");
    let (regs, _) = s.user_swi(MODULE, registers(&[18, NAME]));
    let base = regs.r[3];
    let help_at = base + s.peek(base + 20);
    let help = s
        .kernel
        .os()
        .read_until(help_at, 64, |b| b == 0)
        .expect("help")
        .0;
    assert!(
        help.starts_with(b"FPEmulator\t4.39 (") && help.ends_with(b") (1.13M)"),
        "{}",
        help.escape_ascii()
    );
}

#[test]
fn the_default_context_is_current_and_fresh_and_so_is_the_dump() {
    let mut s = session();
    assert_eq!(answer(&mut s, DEACTIVATE_CONTEXT, &[]), DEFAULT);
    s.user_swi(ACTIVATE_CONTEXT, registers(&[DEFAULT]));
    assert_eq!(
        status(&mut s),
        FRESH,
        "the invalid operation, division by zero and overflow traps enabled"
    );
    assert_eq!(
        answer(&mut s, SAVE_CONTEXT, &[CURRENT, AREA, 0]),
        0,
        "nothing pending"
    );
    let fresh: Vec<u32> = [FRESH].into_iter().chain([0; 24]).collect();
    assert_eq!(words(&mut s, AREA, 25), fresh);
    let dump = answer(&mut s, EXCEPTION_DUMP, &[]);
    assert_eq!(words(&mut s, dump, 25), fresh);
}

#[test]
fn a_context_keeps_its_state_across_a_switch_laid_out_as_genuine_s() {
    let mut s = session();
    s.poke(CONTEXT, &[0xDEAD_BEEF; 34]);
    s.user_swi(INIT_CONTEXT, registers(&[CONTEXT]));
    let fresh: Vec<u32> = [0; 32].into_iter().chain([FRESH, 0]).collect();
    assert_eq!(
        words(&mut s, CONTEXT, 34),
        fresh,
        "every register nought, then the FPSR"
    );
    assert_eq!(answer(&mut s, CHANGE_CONTEXT, &[CONTEXT]), DEFAULT);
    load_doubles(&mut s, 0x3FF8_0000, 0x4000_0000);
    assert_eq!(answer(&mut s, CHANGE_CONTEXT, &[DEFAULT]), CONTEXT);
    let kept: Vec<u32> = [
        [0; 4],
        [0x3FFF, 0xC000_0000, 0, 0],
        [0x4000, 0x8000_0000, 0, 0],
    ]
    .into_iter()
    .flatten()
    .chain([0; 20])
    .chain([FRESH, 0])
    .collect();
    assert_eq!(
        words(&mut s, CONTEXT, 34),
        kept,
        "each register's three words and a nought"
    );
    assert_eq!(answer(&mut s, SAVE_CONTEXT, &[CONTEXT, AREA, 0]), 0);
    let saved: Vec<u32> = [FRESH]
        .into_iter()
        .chain([0; 3])
        .chain(ONE_AND_A_HALF)
        .chain(TWO)
        .chain([0; 15])
        .collect();
    assert_eq!(words(&mut s, AREA, 25), saved);
    assert_eq!(status(&mut s), FRESH, "the default's own");
    assert_eq!(answer(&mut s, SAVE_CONTEXT, &[DEFAULT, AREA, 0]), 0);
    assert_eq!(
        words(&mut s, AREA, 4),
        [FRESH, 0, 0, 0],
        "the default's registers untouched"
    );
}

#[test]
fn loading_a_context_keeps_the_system_id() {
    let mut s = session();
    s.user_swi(INIT_CONTEXT, registers(&[CONTEXT]));
    let mut area = [0; 25];
    area[0] = 0x0001_0000;
    area[1..4].copy_from_slice(&[0x4000, 0xC000_0000, 0]);
    s.poke(AREA, &area);
    s.user_swi(LOAD_CONTEXT, registers(&[CONTEXT, AREA]));
    s.user_swi(CHANGE_CONTEXT, registers(&[CONTEXT]));
    assert_eq!(status(&mut s), 0x0101_0000);
    assert_eq!(
        run(&mut s, &[double(0, 0, 0, FpaTransferOp::Stf)], &[DOUBLES]),
        Outcome::Budget
    );
    assert_eq!(words(&mut s, DOUBLES, 2), [0x4008_0000, 0], "three");
    let mut current = [0; 25];
    current[0] = 0x0000_0001;
    s.poke(AREA, &current);
    s.user_swi(LOAD_CONTEXT, registers(&[CURRENT, AREA]));
    assert_eq!(
        status(&mut s),
        0x0100_0001,
        "the current one's straight into the FPA"
    );
}

#[test]
fn abort_finds_no_partly_done_instruction() {
    let mut s = session();
    s.user_swi(INIT_CONTEXT, registers(&[CONTEXT]));
    assert_eq!(answer(&mut s, ABORT, &[CONTEXT, 0, 0]), 0);
    assert_eq!(answer(&mut s, ABORT, &[CURRENT, 0, 0x8000]), 0);
}

#[test]
fn an_enabled_trap_dumps_both_register_sets_then_raises_its_error() {
    let mut s = session();
    load_doubles(&mut s, 0x3FF0_0000, 0);
    let at = CODE;
    match run(
        &mut s,
        &[divide(0, 1, 2)],
        &[DOUBLES, 0x1111, 0x2222, 0x3333],
    ) {
        Outcome::Ended(Ended::Error { number, .. }) => assert_eq!(number, 0x8000_0202),
        other => panic!("{other:?}"),
    }
    let dump = answer(&mut s, EXCEPTION_DUMP, &[]);
    let fp: Vec<u32> = [FRESH]
        .into_iter()
        .chain([0; 3])
        .chain([0x3FFF, 0x8000_0000, 0])
        .chain([0; 18])
        .collect();
    assert_eq!(
        words(&mut s, dump, 25),
        fp,
        "no flag for the trapped exception, F0 unwritten"
    );
    let (regs, _) = s.user_swi(CHANGE_ENVIRONMENT, registers(&[13, 0, 0, 0]));
    let integer = words(&mut s, regs.r[1], 17);
    assert_eq!(integer[..4], [DOUBLES, 0x1111, 0x2222, 0x3333]);
    assert_eq!(integer[15], at + 4, "the PC past the instruction");
    assert_eq!(integer[16] & 0x1F, 0x10, "User mode");
}

#[test]
fn with_no_context_an_fpa_instruction_is_undefined() {
    let mut s = session();
    assert_eq!(answer(&mut s, CHANGE_CONTEXT, &[NO_CONTEXT]), DEFAULT);
    match run(&mut s, &[rfs(0)], &[]) {
        Outcome::Ended(Ended::Error { number, .. }) => {
            assert_eq!(number, 0x8000_0000, "undefined instruction");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(answer(&mut s, DEACTIVATE_CONTEXT, &[]), NO_CONTEXT);
    assert_eq!(answer(&mut s, CHANGE_CONTEXT, &[DEFAULT]), NO_CONTEXT);
    assert_eq!(status(&mut s), FRESH, "the default back");
}

#[test]
fn with_no_context_the_other_profile_s_fpa_instructions_are_undefined_too() {
    for (deactivating, other) in [(Bits::Bit26, Bits::Bit32), (Bits::Bit32, Bits::Bit26)] {
        let mut s = with_application(&SHARED, Bits::Bit26);
        let spin = CODE + 4;
        s.poke(CODE, &[asm::swi(X | CHANGE_CONTEXT), asm::b(spin, spin)]);
        s.enter(deactivating, CODE, registers(&[NO_CONTEXT]));
        assert_eq!(s.resume(), Outcome::Budget);
        assert_eq!(
            s.regs().r[0],
            DEFAULT,
            "deactivated on the {deactivating:?} processor"
        );
        s.poke(CODE, &[rfs(0), asm::b(spin, spin)]);
        s.enter(other, CODE, registers(&[]));
        match s.resume() {
            Outcome::Ended(Ended::Error { number, .. }) => {
                assert_eq!(number, 0x8000_0000, "undefined on the {other:?} processor");
            }
            outcome => panic!("{outcome:?} on the {other:?} processor"),
        }
    }
}

#[test]
fn a_context_s_state_round_trips_through_the_saved_form() {
    let context = Context {
        registers: [
            [0x3FFF, 0xC000_0000, 0],
            [0x4000, 0x8000_0000, 0],
            [0x8000_3FFE, 0x8000_0000, 0],
            [0; 3],
            [0; 3],
            [0; 3],
            [0; 3],
            [0x4001, 0xE000_0000, 0],
        ],
        fpsr: 0x0107_0010,
    };
    assert_eq!(Context::from_saved(&context.saved()), context);
}

#[test]
fn an_unknown_swi_is_out_of_range() {
    let mut s = session();
    let (regs, _) = s.user_swi(X | (CHUNK + 10), registers(&[]));
    assert!(regs.flag(tairix_riscos_arm::psr::V));
    let text = s.error_text(regs.r[0]);
    assert!(
        text.windows(10).any(|word| word == b"FPEmulator"),
        "{}",
        text.escape_ascii()
    );
}

#[test]
fn a_divide_by_zero_with_the_trap_off_answers_infinity_and_flags_it() {
    let mut s = session();
    load_doubles(&mut s, 0x3FF0_0000, 0);
    let fresh_without_division = FRESH & !Exception::DivisionByZero.enable();
    let wfs = encode(Op::FpaRegister {
        op: FpaRegisterOp::Wfs,
        precision: Precision::Single,
        rounding: Rounding::Nearest,
        fn_: 0,
        fm: FpaOperand::Register(0),
        rd: Reg::field(0),
    });
    let code = [wfs, divide(0, 1, 2), rfs(0)];
    assert_eq!(
        run(&mut s, &code, &[fresh_without_division]),
        Outcome::Budget
    );
    let flagged = fresh_without_division | Exception::DivisionByZero.flag();
    assert_eq!(
        s.kernel.os().current_registers()[0],
        flagged,
        "the flag set"
    );
}
