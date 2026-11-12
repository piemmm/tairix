use alloc::vec::Vec;

use super::*;
use crate::conformance;
use crate::model::Fault;
use crate::relax::{relax, Relaxable};
use crate::testgen::{generate, TestCalls, TestMemory, BUFFER_BYTES, HELPER_BASE};
use crate::Isa;

#[test]
fn random_functions_agree_with_the_interpreter() {
    conformance::each(Isa::Aarch64, |target| {
        conformance::random_functions_agree(target, 0xA64, 3000);
    });
}

/// Every machine instruction of every case decodes, in one walk over the
/// code, to what it means, each branch to its label's place.
#[test]
fn compiled_code_decodes_instruction_by_instruction() {
    let mut next = conformance::seeded(0x00DE_C0DE);
    for _ in 0..400 {
        let case = generate(&mut next).unwrap();
        let (lowered, bytes) = build(&case.func).unwrap();
        let layout = relax(&lowered.insts, lowered.labels).unwrap();
        assert_eq!(layout.length, bytes.len());
        for (n, inst) in lowered.insts.iter().enumerate() {
            if matches!(inst, Inst::Bind(_)) {
                continue;
            }
            let at = layout.places[n];
            let decoded = tairix_disasm::aarch64::decode(&bytes[at..], at as u64).unwrap();
            let (name, operands) = render::text(inst);
            match inst.target() {
                Some(label) if layout.forms[n] == 0 => {
                    assert_eq!(decoded.mnemonic, name, "{inst:?}");
                    assert_eq!(decoded.branch_target, Some(layout.labels[label.0] as u64));
                }
                Some(_) => panic!("{inst:?} needed the long form in a small function"),
                None => assert_eq!(
                    (decoded.mnemonic, decoded.operands),
                    (name, operands),
                    "{inst:?}"
                ),
            }
        }
    }
}

#[test]
fn division_is_total_on_the_edges() {
    conformance::each(Isa::Aarch64, conformance::division_is_total);
}

#[test]
fn shifts_and_rotates_count_modulo_the_width() {
    conformance::each(Isa::Aarch64, conformance::shifts_count_modulo_the_width);
}

#[test]
fn immediates_on_the_edge_of_folding_agree() {
    conformance::each(Isa::Aarch64, |target| {
        conformance::immediates_on_their_edges(
            target,
            &[
                0x00FF_F000,
                0x00FF_F001,
                0x0100_0000,
                0xFF,
                0xFF00,
                0x5555_5555,
                0xAAAA_AAAA_AAAA_AAAA,
                0x0F0F_0F0F_0F0F_0F0F,
                0xFFFF_0000,
                0x1_0000_0001,
                0xFFFF_FFFF_FFFF_F000,
            ],
        );
    });
}

#[test]
fn values_outlive_a_call_in_saved_registers_and_slots() {
    // Five thousand slots: beyond a load's 32 KiB immediate reach, and a
    // frame beyond one twelve-bit adjustment.
    conformance::each(Isa::Aarch64, |target| {
        conformance::values_outlive_a_call(target, 5000);
    });
}

#[test]
fn loads_and_stores_reach_their_offsets() {
    conformance::each(Isa::Aarch64, conformance::offsets_on_their_edges);
}

#[test]
fn a_function_that_answers_nothing_reads_no_answer() {
    conformance::each(
        Isa::Aarch64,
        conformance::a_function_that_answers_nothing_reads_no_answer,
    );
}

#[test]
fn compile_answers_the_bytes_the_model_ran() {
    conformance::each(Isa::Aarch64, |target| {
        conformance::compile_is_the_build(target, |func| build(func).unwrap().1);
    });
}

#[test]
fn constants_take_their_fewest_instructions() {
    let count = |wide: bool, imm: u64| {
        let mut arm = Arm::default();
        arm.constant(wide, 0, imm);
        arm.insts.len()
    };
    assert_eq!(count(true, 0), 1);
    assert_eq!(count(true, u64::MAX), 1, "movn");
    assert_eq!(count(true, 0xFFFF_0000), 1, "movz, shifted");
    assert_eq!(count(true, 0x5555_5555_5555_5555), 1, "a logical immediate");
    assert_eq!(count(true, 0xFFFF_FFFF_FFFF_1234), 1, "movn, inverted");
    assert_eq!(count(true, 0x1234_0000_5678), 2);
    assert_eq!(count(true, 0x1234_5678_9ABC_DEF0), 4);
    assert_eq!(count(false, 0xFFFF_FFFF), 1);
    assert_eq!(count(false, 0x8000_0001), 1, "a run round the top");
    assert_eq!(count(false, 0x8001_0001), 2);
}

/// `insts` run on the model, with no arguments.
fn run(insts: &[Inst]) -> Result<Option<u64>, Fault> {
    let mut memory = TestMemory {
        buffer: alloc::vec![0; BUFFER_BYTES],
    };
    sim::run(
        insts,
        &[],
        &mut memory,
        &mut TestCalls::default(),
        100,
        true,
    )
}

const fn set(rd: u8, imm: u16) -> Inst {
    Inst::Movz {
        wide: true,
        rd,
        imm,
        hw: 0,
    }
}

const fn sp_by(sub: bool, imm: u16) -> Inst {
    Inst::AddImm {
        sub,
        flags: false,
        wide: true,
        rd: reg::SP,
        rn: reg::SP,
        imm,
        high: false,
    }
}

const fn sp_word(store: bool, rt: u8, offset: i16) -> Inst {
    let offset = Offset::Unscaled(offset);
    if store {
        Inst::Store {
            size: Size::X,
            rt,
            rn: reg::SP,
            offset,
        }
    } else {
        Inst::Load {
            size: Size::X,
            signed: false,
            wide: true,
            rt,
            rn: reg::SP,
            offset,
        }
    }
}

/// A call of the helper the test table puts third.
fn call() -> Vec<Inst> {
    let helper = HELPER_BASE + 48;
    Vec::from([
        set(reg::X16, crate::bits::low16(helper)),
        Inst::Movk {
            wide: true,
            rd: reg::X16,
            imm: crate::bits::low16(helper >> 16),
            hw: 1,
        },
        Inst::Blr(reg::X16),
    ])
}

#[test]
fn the_model_refuses_what_was_never_defined() {
    assert_eq!(run(&[Inst::Ret]), Err(Fault::Undefined(0)));
    let unset = [
        Inst::BCond(Cond::Eq, Label(0)),
        Inst::Bind(Label(0)),
        Inst::Ret,
    ];
    assert_eq!(run(&unset), Err(Fault::Flags));
}

#[test]
fn the_model_refuses_what_the_convention_keeps() {
    assert_eq!(run(&[set(sim::X18, 1)]), Err(Fault::Reserved(sim::X18)));
    assert_eq!(
        run(&[set(0, 1), set(19, 1), Inst::Ret]),
        Err(Fault::Clobbered(19))
    );
}

#[test]
fn the_model_refuses_a_misaligned_stack_or_a_missing_record() {
    assert_eq!(
        run(&[sp_by(true, 8), sp_word(false, 0, 0)]),
        Err(Fault::Misaligned)
    );
    assert_eq!(run(&call()), Err(Fault::Frame));
}

#[test]
fn a_call_leaves_nothing_below_the_stack_pointer() {
    let record = [
        Inst::PushPair {
            rt: reg::FP,
            rt2: reg::LR,
        },
        Inst::AddImm {
            sub: false,
            flags: false,
            wide: true,
            rd: reg::FP,
            rn: reg::SP,
            imm: 0,
            high: false,
        },
    ];
    let mut code = Vec::from(record);
    code.extend([set(0, 5), sp_word(true, 0, -8)]);
    code.extend(call());
    code.extend([sp_word(false, 0, -8), Inst::Ret]);
    // The link register is the call's until the record gives it back.
    assert_eq!(run(&code), Err(Fault::Clobbered(reg::LR)));
    let last = code.len() - 1;
    code.insert(
        last,
        Inst::PopPair {
            rt: reg::FP,
            rt2: reg::LR,
        },
    );
    assert_eq!(run(&code), Ok(Some(0xCCCC_CCCC_CCCC_CCCC)));
}
