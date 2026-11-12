use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::conformance;
use crate::model::Fault;
use crate::testgen::{generate, TestCalls, TestMemory};
use crate::{Isa, Target};

#[test]
fn random_functions_agree_with_the_interpreter() {
    conformance::each(Isa::X86_64, |target| {
        conformance::random_functions_agree(target, 0x8664, 3000);
    });
}

/// Every machine instruction of every case decodes, in one walk over the
/// code, to what it means, each jump to its label's place.
#[test]
fn compiled_code_decodes_instruction_by_instruction() {
    conformance::each(Isa::X86_64, |target| {
        let mut next = conformance::seeded(0x00DE_C0DE);
        for _ in 0..400 {
            let case = generate(&mut next).unwrap();
            let (lowered, bytes) = build(&case.func, target).unwrap();
            let mut at = 0usize;
            let mut places = vec![None; lowered.labels];
            let mut jumps = Vec::new();
            for inst in &lowered.insts {
                if let Inst::Bind(label) = *inst {
                    places[label.0] = Some(at as u64);
                    continue;
                }
                let decoded = tairix_disasm::x86_64::decode(&bytes[at..], at as u64).unwrap();
                let (name, operands) = render::text(inst);
                assert_eq!(decoded.mnemonic, name, "{inst:?}");
                if let Inst::Jmp(label) | Inst::Jcc(_, label) = *inst {
                    jumps.push((label, decoded.branch_target));
                } else {
                    assert_eq!(decoded.operands, operands, "{inst:?}");
                }
                at += decoded.length;
            }
            assert_eq!(at, bytes.len());
            for (label, place) in jumps {
                assert_eq!(place, places[label.0]);
            }
        }
    });
}

#[test]
fn division_is_total_on_the_edges() {
    conformance::each(Isa::X86_64, conformance::division_is_total);
}

#[test]
fn shifts_and_rotates_count_modulo_the_width() {
    conformance::each(Isa::X86_64, conformance::shifts_count_modulo_the_width);
}

#[test]
fn immediates_on_the_edge_of_folding_agree() {
    conformance::each(Isa::X86_64, |target| {
        conformance::immediates_on_their_edges(
            target,
            &[0x7F, 0x80, 0xFFFF_FF80, 0xFFFF_FFFF_FFFF_FF7F],
        );
    });
}

#[test]
fn values_outlive_a_call_in_saved_registers_and_slots() {
    conformance::each(Isa::X86_64, |target| {
        conformance::values_outlive_a_call(target, 600);
    });
}

#[test]
fn loads_and_stores_reach_their_offsets() {
    conformance::each(Isa::X86_64, conformance::offsets_on_their_edges);
}

#[test]
fn a_function_that_answers_nothing_reads_no_answer() {
    conformance::each(
        Isa::X86_64,
        conformance::a_function_that_answers_nothing_reads_no_answer,
    );
}

/// With BMI2 a shift by a register count takes the count where it lies and
/// a rotate by a constant is one instruction; without it, neither form
/// appears.
#[test]
fn bmi2_shifts_take_any_count_register() {
    use crate::ir::{BinOp, Block, Builder, Type};
    for (op, constant) in [
        (BinOp::Shl, false),
        (BinOp::Shr, false),
        (BinOp::Sar, false),
        (BinOp::Rotl, true),
        (BinOp::Rotr, true),
    ] {
        for ty in [Type::I32, Type::I64] {
            let mut b = Builder::new(&[ty, ty], Some(ty));
            let p = b.params(Block::ENTRY).to_vec();
            let count = if constant { b.iconst(ty, 5) } else { p[1] };
            let shifted = b.binary(op, p[0], count);
            b.ret(Some(shifted));
            let func = b.finish().unwrap();
            let bmi2 = |target| {
                let (lowered, _) = build(&func, target).unwrap();
                let insts = lowered.insts;
                (
                    insts
                        .iter()
                        .any(|inst| matches!(inst, Inst::ShiftX { .. } | Inst::Rorx { .. })),
                    insts.iter().any(|inst| matches!(inst, Inst::Shift { .. })),
                )
            };
            assert_eq!(
                bmi2(Target::X86_64.with(tairix_abi::cpufeatures::CpuFeature::Bmi2)),
                (true, false),
                "{op:?} {ty:?}"
            );
            assert_eq!(bmi2(Target::X86_64), (false, true), "{op:?} {ty:?}");
        }
    }
}

/// Whichever successor is laid out next is fallen into: a branch is one
/// conditional jump, the comparison inverted where it must be.
#[test]
fn a_branch_falls_into_the_block_laid_out_next() {
    use crate::ir::{Block, Builder, Cond, Type};
    let mut b = Builder::new(&[Type::I32], Some(Type::I32));
    let x = b.params(Block::ENTRY)[0];
    let (then, other) = (b.block(&[]), b.block(&[]));
    let zero = b.iconst(Type::I32, 0);
    let c = b.icmp(Cond::Eq, x, zero);
    b.branch(c, (then, &[]), (other, &[]));
    for (block, answer) in [(then, 1), (other, 2)] {
        b.switch_to(block);
        let answer = b.iconst(Type::I32, answer);
        b.ret(Some(answer));
    }
    let func = b.finish().unwrap();
    let (lowered, _) = build(&func, Target::X86_64).unwrap();
    let jumps = lowered
        .insts
        .iter()
        .filter(|inst| matches!(inst, Inst::Jmp(_)))
        .count();
    assert_eq!(jumps, 0, "{:?}", lowered.insts);
}

#[test]
fn compile_answers_the_bytes_the_model_ran() {
    conformance::each(Isa::X86_64, |target| {
        conformance::compile_is_the_build(target, |func| build(func, target).unwrap().1);
    });
}

/// `insts` run on the model, with no arguments.
fn run(insts: &[Inst]) -> Result<Option<u64>, Fault> {
    let mut memory = TestMemory {
        buffer: vec![0; crate::testgen::BUFFER_BYTES],
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

const fn set(dst: u8, imm: u64) -> Inst {
    Inst::MovImm {
        wide: true,
        dst,
        imm,
    }
}

const HELPER: u64 = crate::testgen::HELPER_BASE + 48;

#[test]
fn the_model_refuses_what_was_never_defined() {
    assert_eq!(run(&[Inst::Ret]), Err(Fault::Undefined(reg::RAX)));
    let unset = [Inst::Jcc(Cc::E, Label(0)), Inst::Bind(Label(0)), Inst::Ret];
    assert_eq!(run(&unset), Err(Fault::Flags));
}

#[test]
fn the_model_refuses_a_frame_left_wrong() {
    let pushed = [set(reg::RAX, 1), Inst::Push(reg::RAX), Inst::Ret];
    assert_eq!(run(&pushed), Err(Fault::Clobbered(reg::RSP)));
    let changed = [set(reg::RBX, 1), set(reg::RAX, 1), Inst::Ret];
    assert_eq!(run(&changed), Err(Fault::Clobbered(reg::RBX)));
}

#[test]
fn the_model_refuses_a_call_the_convention_forbids() {
    let misaligned = [set(reg::R11, HELPER), Inst::Call(reg::R11)];
    assert_eq!(run(&misaligned), Err(Fault::Misaligned));
    let aligned = Inst::Alu {
        op: Alu::Sub,
        wide: true,
        dst: reg::RSP,
        src: Src::Imm(8),
    };
    let unrecorded = [aligned, set(reg::R11, HELPER), Inst::Call(reg::R11)];
    assert_eq!(run(&unrecorded), Err(Fault::Frame));
}

#[test]
fn the_model_divides_as_the_processor_does() {
    let zero_rdx = Inst::Alu {
        op: Alu::Xor,
        wide: false,
        dst: reg::RDX,
        src: Src::Reg(reg::RDX),
    };
    let divide = Inst::Div {
        signed: false,
        wide: true,
        src: reg::RCX,
    };
    let by_zero = [set(reg::RAX, 1), set(reg::RCX, 0), zero_rdx, divide];
    assert_eq!(run(&by_zero), Err(Fault::Divide));
}

#[test]
fn a_call_leaves_nothing_below_the_stack_pointer() {
    let below = |load| Inst::Load {
        bytes: Bytes::B8,
        signed: false,
        wide: true,
        dst: load,
        base: reg::RSP,
        disp: -8,
    };
    let code = [
        Inst::Push(reg::RBP),
        Inst::Mov {
            wide: true,
            dst: reg::RBP,
            src: reg::RSP,
        },
        set(reg::RAX, 5),
        Inst::Store {
            bytes: Bytes::B8,
            src: reg::RAX,
            base: reg::RSP,
            disp: -8,
        },
        set(reg::R11, HELPER),
        Inst::Call(reg::R11),
        below(reg::RAX),
        Inst::Pop(reg::RBP),
        Inst::Ret,
    ];
    assert_eq!(run(&code), Ok(Some(0xCCCC_CCCC_CCCC_CCCC)));
}
