//! Exceptions from guest code: the default handlers' errors, abort traps
//! answering loads and stores, and claims of the processor vectors.

use alloc::format;
use alloc::vec;

use tairix_riscos_arm::psr;

use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::os::{Ended, Outcome};
use crate::swi::os::{ABORT_TRAP, CHANGE_ENVIRONMENT, CLAIM_PROCESSOR_VECTOR};
use crate::swi::X;
use crate::testkit::{asm, with_application, Session, CODE, DATA};

/// Nothing is mapped here.
const NOWHERE: u32 = 0x7000_0000;

fn session() -> Session {
    with_application(&RISC_OS_5, Bits::Bit32)
}

fn registers(low: &[u32]) -> [u32; 13] {
    let mut r = [0; 13];
    r[..low.len()].copy_from_slice(low);
    r
}

/// Runs `code` at `CODE` in User mode with `registers`.
fn run(s: &mut Session, code: &[u32], registers: [u32; 13]) -> Outcome {
    s.poke(CODE, code);
    s.enter(Bits::Bit32, CODE, registers);
    s.resume()
}

#[test]
fn an_aborting_load_with_no_trap_is_an_internal_error_naming_its_address() {
    let mut s = session();
    let spin = asm::b(CODE + 4, CODE + 4);
    match run(&mut s, &[asm::ldr(0, 1, 0), spin], registers(&[0, NOWHERE])) {
        Outcome::Ended(Ended::Error { number, block }) => {
            assert_eq!(number, 0x8000_0002);
            assert_eq!(
                s.error_text(block),
                format!("Data transfer aborted at &{CODE:08X}").as_bytes()
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_undefined_instruction_is_an_internal_error_naming_its_address() {
    let mut s = session();
    match run(&mut s, &[0xE7F0_00F0], [0; 13]) {
        Outcome::Ended(Ended::Error { number, block }) => {
            assert_eq!(number, 0x8000_0000);
            assert_eq!(
                s.error_text(block),
                format!("Undefined instruction executed at &{CODE:08X}").as_bytes()
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_abort_trap_answers_the_loads_and_takes_the_stores_in_its_range() {
    let mut s = session();
    // Loads read &7F000000; stores leave what was stored at DATA.
    let loads = CODE + 0x100;
    s.poke(
        loads,
        &[
            asm::mov(10, 0x7F00_0000),
            asm::str(10, 1, 0),
            asm::mov_pc_lr(),
        ],
    );
    let stores = CODE + 0x140;
    s.poke(
        stores,
        &[asm::ldr(10, 1, 0), asm::str(10, 12, 0), asm::mov_pc_lr()],
    );
    let (regs, _) = s.user_swi(
        X | ABORT_TRAP,
        registers(&[0, NOWHERE, NOWHERE + 0x100, loads, 0]),
    );
    assert!(!regs.flag(psr::V));
    let spin = asm::b(CODE + 4, CODE + 4);
    assert_eq!(
        run(
            &mut s,
            &[asm::ldr(0, 1, 0x10), spin],
            registers(&[0, NOWHERE])
        ),
        Outcome::Budget
    );
    assert_eq!(s.regs().r[0], 0x7F00_0000, "the trap's word loaded");
    s.user_swi(
        X | ABORT_TRAP,
        registers(&[1, NOWHERE, NOWHERE + 0x100, loads, 0]),
    );
    s.user_swi(
        X | ABORT_TRAP,
        registers(&[0, NOWHERE, NOWHERE + 0x100, stores, DATA]),
    );
    assert_eq!(
        run(
            &mut s,
            &[asm::str(2, 1, 0x20), spin],
            registers(&[0, NOWHERE, 0xABCD])
        ),
        Outcome::Budget
    );
    assert_eq!(s.peek(DATA), 0xABCD, "the trap took the store");
    let (regs, _) = s.user_swi(
        X | ABORT_TRAP,
        registers(&[1, NOWHERE, NOWHERE + 0x200, stores, DATA]),
    );
    assert!(regs.flag(psr::V), "no exact match to remove");
}

#[test]
fn a_processor_vector_claim_returns_the_old_entry_and_a_release_must_match() {
    let mut s = session();
    let (regs, _) = s.user_swi(X | CLAIM_PROCESSOR_VECTOR, registers(&[1 | 1 << 8, CODE]));
    assert!(!regs.flag(psr::V));
    let old = regs.r[1];
    assert_ne!(old, 0);
    let (regs, _) = s.user_swi(X | CLAIM_PROCESSOR_VECTOR, registers(&[1, old, DATA]));
    assert!(regs.flag(psr::V));
    assert_eq!(s.peek(regs.r[0]), 0x1A1);
    let (regs, _) = s.user_swi(X | CLAIM_PROCESSOR_VECTOR, registers(&[1, old, CODE]));
    assert!(!regs.flag(psr::V), "released back to the old entry");
    let (regs, _) = s.user_swi(X | CLAIM_PROCESSOR_VECTOR, registers(&[7 | 1 << 8, CODE]));
    assert_eq!(s.peek(regs.r[0]), 0x1A0);
}

#[test]
fn an_enabled_floating_point_trap_raises_fpemulator_s_error() {
    use tairix_riscos_arm::a32;
    use tairix_riscos_arm::cond::Condition;
    use tairix_riscos_arm::fpa::{Precision, Rounding};
    use tairix_riscos_arm::instruction::{
        FpaDyadicOp, FpaMonadicOp, FpaOperand, FpaRegisterOp, Instruction, Op,
    };
    use tairix_riscos_arm::reg::Reg;

    let encode = |op: Op| {
        a32::encode(&Instruction {
            cond: Condition::Al,
            op,
        })
        .expect("encodable")
    };
    let mut s = session();
    // The division-by-zero trap enabled, then 1 / 0.
    let code = [
        asm::mov(0, 0x2_0000),
        encode(Op::FpaRegister {
            op: FpaRegisterOp::Wfs,
            precision: Precision::Single,
            rounding: Rounding::Nearest,
            fn_: 0,
            fm: FpaOperand::Register(0),
            rd: Reg::field(0),
        }),
        encode(Op::FpaMonadic {
            op: FpaMonadicOp::Mvf,
            precision: Precision::Single,
            rounding: Rounding::Nearest,
            fd: 1,
            fm: FpaOperand::Constant(1),
        }),
        encode(Op::FpaDyadic {
            op: FpaDyadicOp::Dvf,
            precision: Precision::Single,
            rounding: Rounding::Nearest,
            fd: 0,
            fn_: 1,
            fm: FpaOperand::Constant(0),
        }),
        asm::b(CODE + 16, CODE + 16),
    ];
    match run(&mut s, &code, [0; 13]) {
        Outcome::Ended(Ended::Error { number, block }) => {
            assert_eq!(number, 0x8000_0202);
            assert_eq!(s.error_text(block), b"Floating point division by zero");
        }
        other => panic!("{other:?}"),
    }
}

/// An undefined instruction in User code under `bits`, its handler
/// recording what it was entered with at `DATA` onward: under 26 bits R15,
/// which holds the PSR, and R14; under 32 the CPSR, R14 and the SPSR.
fn undefined_handler_entry(bits: Bits) -> [u32; 3] {
    let (layout, status) = match bits {
        Bits::Bit26 => (&crate::memory::layout::SHARED, asm::mov_reg(1, 15)),
        Bits::Bit32 => (&RISC_OS_5, asm::mrs(1)),
    };
    let mut s = with_application(layout, bits);
    let handler = CODE + 0x40;
    let spsr = match bits {
        Bits::Bit26 => asm::mov(1, 0),
        Bits::Bit32 => 0xE14F_1000,
    };
    // An exception handler has no workspace in R12: it finds its own.
    s.poke(
        handler,
        &[
            status,
            asm::mov(12, DATA),
            asm::str(1, 12, 0),
            asm::str(14, 12, 4),
            spsr,
            asm::str(1, 12, 8),
            asm::b(handler + 24, handler + 24),
        ],
    );
    s.user_swi(
        X | CHANGE_ENVIRONMENT,
        [1, handler, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    let at = CODE + 0x80;
    s.poke(at, &[asm::set_flags(psr::Z), 0xE7F0_00F0]);
    s.enter(bits, at, [0; 13]);
    s.run(100);
    [s.peek(DATA), s.peek(DATA + 4), s.peek(DATA + 8)]
}

#[test]
fn an_exception_handler_is_entered_in_supervisor_mode_as_the_pre_veneer_leaves_it() {
    let at = CODE + 0x80;
    let [r15, r14, _] = undefined_handler_entry(Bits::Bit26);
    assert_eq!(
        r15 & psr::R15_MODE,
        psr::Mode::Supervisor26.bits(),
        "Supervisor mode"
    );
    assert_ne!(r15 & psr::R15_I, 0, "interrupts off");
    assert_eq!(r14 & psr::R15_PC, at + 8, "past the instruction");
    assert_eq!(r14 & !psr::R15_PC, psr::Z, "with the User PSR beside it");
    let [cpsr, r14, spsr] = undefined_handler_entry(Bits::Bit32);
    assert_eq!(psr::Mode::from_bits(cpsr), Some(psr::Mode::Supervisor));
    assert_eq!(r14, at + 8);
    assert_eq!(psr::Mode::from_bits(spsr), Some(psr::Mode::User));
    assert_ne!(spsr & psr::Z, 0, "the User PSR in SPSR");
}

/// A trap handler that counts its calls in its workspace word, keeps the
/// reason, address and length it was given after the count, and declines.
fn declining(s: &mut Session, at: u32, error: u32) {
    s.poke(
        at,
        &[
            asm::ldr(11, 12, 0),
            asm::add(11, 11, 1),
            asm::str(11, 12, 0),
            asm::str(0, 12, 4),
            asm::str(2, 12, 8),
            asm::str(3, 12, 12),
            asm::mov(0, error),
            asm::set_flags(psr::V),
            asm::mov_pc_lr(),
        ],
    );
}

#[test]
fn a_declining_trap_passes_the_access_to_the_next_whose_range_holds_it() {
    let mut s = session();
    let (first, second, error, count) = (CODE + 0x100, CODE + 0x140, DATA + 0x200, DATA + 0x300);
    s.poke(error, &[0x1234, 0]);
    declining(&mut s, first, error);
    s.poke(
        second,
        &[
            asm::mov(10, 0x7F00_0000),
            asm::str(10, 1, 0),
            asm::mov_pc_lr(),
        ],
    );
    for handler in [first, second] {
        let workspace = if handler == first { count } else { 0 };
        let (regs, _) = s.user_swi(
            X | ABORT_TRAP,
            registers(&[0, NOWHERE, NOWHERE + 0x100, handler, workspace]),
        );
        assert!(!regs.flag(psr::V));
    }
    let spin = asm::b(CODE + 4, CODE + 4);
    let outcome = run(
        &mut s,
        &[asm::ldr(0, 1, 0x10), spin],
        registers(&[0, NOWHERE]),
    );
    assert_eq!(outcome, Outcome::Budget);
    assert_eq!(s.regs().r[0], 0x7F00_0000, "the second trap answered");
    assert_eq!(s.peek(count), 1, "after the first declined");
}

#[test]
fn a_trap_is_asked_only_for_memory_the_page_tables_refuse() {
    let mut s = session();
    let (handler, error, count) = (CODE + 0x100, DATA + 0x200, DATA + 0x300);
    s.poke(error, &[0x1234, 0]);
    declining(&mut s, handler, error);
    s.user_swi(
        X | ABORT_TRAP,
        registers(&[0, DATA, DATA + 0x1000, handler, count]),
    );
    s.poke(DATA + 0x10, &[0x5150]);
    let spin = asm::b(CODE + 4, CODE + 4);
    let outcome = run(&mut s, &[asm::ldr(0, 1, 0x10), spin], registers(&[0, DATA]));
    assert_eq!(outcome, Outcome::Budget);
    assert_eq!(
        (s.regs().r[0], s.peek(count)),
        (0x5150, 0),
        "mapped: not asked"
    );
    // Misaligned under alignment checking: no trap answers for that.
    s.user_swi(
        X | ABORT_TRAP,
        registers(&[0, NOWHERE, NOWHERE + 0x100, handler, count]),
    );
    let outcome = run(&mut s, &[asm::ldr(0, 1, 1), spin], registers(&[0, NOWHERE]));
    assert!(
        matches!(outcome, Outcome::Ended(Ended::Error { .. })),
        "{outcome:?}"
    );
    assert_eq!(s.peek(count), 0, "an alignment fault is not offered");
}

#[test]
fn an_exclusive_load_and_an_instruction_fetch_ask_a_trap_to_map_the_memory() {
    let mut s = session();
    let (handler, error, record) = (CODE + 0x100, DATA + 0x200, DATA + 0x300);
    s.poke(error, &[0x1234, 0]);
    declining(&mut s, handler, error);
    s.user_swi(
        X | ABORT_TRAP,
        registers(&[0, NOWHERE, NOWHERE + 0x100, handler, record]),
    );
    // LDREX R0, [R1].
    let outcome = run(&mut s, &[0xE191_0F9F], registers(&[0, NOWHERE + 8]));
    assert!(
        matches!(outcome, Outcome::Ended(Ended::Error { .. })),
        "{outcome:?}"
    );
    assert_eq!(
        [s.peek(record + 4), s.peek(record + 8), s.peek(record + 12)],
        [2 | 1 << 6, NOWHERE + 8, 4],
        "a map request for User mode's reading"
    );
    s.poke(record, &[0]);
    let jump = [
        asm::mov(1, NOWHERE),
        asm::add(1, 1, 0x20),
        asm::mov_reg(15, 1),
    ];
    let outcome = run(&mut s, &jump, [0; 13]);
    assert!(
        matches!(outcome, Outcome::Ended(Ended::Error { .. })),
        "{outcome:?}"
    );
    assert_eq!(
        [s.peek(record), s.peek(record + 4), s.peek(record + 8)],
        [1, 2 | 1 << 4 | 1 << 6, NOWHERE + 0x20],
        "a map request for User mode's reading and running"
    );
}

#[test]
fn an_abortable_area_s_handler_is_told_its_allocated_end_and_must_do_it_all() {
    use tairix_riscos_arm::cond::Condition;
    let mut s = session();
    let (handler, workspace) = (CODE + 0x100, DATA + 0x300);
    let done = [asm::mov(10, 0x55), asm::str(10, 2, 0), asm::mov_pc_lr()];
    let mut code = vec![
        asm::cmp_imm(0, 5),
        asm::when(Condition::Ne, asm::mov_pc_lr()),
        asm::str(5, 12, 0),
        asm::str(1, 12, 4),
    ];
    code.extend(done);
    s.poke(handler, &code);
    s.write(DATA + 0x400, b"Abortable\0");
    let (regs, _) = s.user_swi(
        X | 0x66,
        registers(&[
            0,
            u32::MAX,
            0,
            u32::MAX,
            1 << 16,
            0x1_0000,
            handler,
            workspace,
            DATA + 0x400,
        ]),
    );
    assert!(!regs.flag(psr::V));
    let base = regs.r[3];
    let spin = asm::b(CODE + 4, CODE + 4);
    let outcome = run(&mut s, &[asm::ldr(0, 1, 0x10), spin], registers(&[0, base]));
    assert_eq!(outcome, Outcome::Budget);
    assert_eq!(s.regs().r[0], 0x55);
    assert_eq!(
        [s.peek(workspace), s.peek(workspace + 4)],
        [base, 1],
        "nothing allocated yet; a User mode load"
    );
    // The handler now says it did none of it.
    s.poke(handler + 16, &[asm::mov(4, 0)]);
    let outcome = run(&mut s, &[asm::ldr(0, 1, 0x10), spin], registers(&[0, base]));
    assert!(
        matches!(outcome, Outcome::Ended(Ended::Error { .. })),
        "{outcome:?}"
    );
}
