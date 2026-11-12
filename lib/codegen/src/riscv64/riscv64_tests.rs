use alloc::vec::Vec;

use super::*;
use crate::conformance;
use crate::model::Fault;
use crate::relax::{relax, Relaxable};
use crate::testgen::{generate, TestCalls, TestMemory, BUFFER_BYTES, HELPER_BASE};
use crate::{Isa, Target};

#[test]
fn random_functions_agree_with_the_interpreter() {
    conformance::each(Isa::Riscv64, |target| {
        conformance::random_functions_agree(target, 0x5C64, 3000);
    });
}

/// Every machine instruction of every case decodes, in one walk over the
/// code, to what it means; every jump's first word is the one its form
/// begins with.
#[test]
fn compiled_code_decodes_instruction_by_instruction() {
    conformance::each(Isa::Riscv64, |target| {
        let mut next = conformance::seeded(0x00DE_C0DE);
        for _ in 0..400 {
            let case = generate(&mut next).unwrap();
            let (lowered, bytes) = build(&case.func, target).unwrap();
            let layout = relax(&lowered.insts, lowered.labels).unwrap();
            assert_eq!(layout.length, bytes.len());
            for (n, inst) in lowered.insts.iter().enumerate() {
                if matches!(inst, Inst::Bind(_)) {
                    continue;
                }
                let at = layout.places[n];
                let decoded = tairix_disasm::riscv64::decode(&bytes[at..], at as u64).unwrap();
                let (name, operands) = render::text(inst);
                match (inst.target(), inst, layout.forms[n]) {
                    (Some(label), _, 0) => {
                        assert_eq!(decoded.mnemonic, name, "{inst:?}");
                        assert_eq!(decoded.branch_target, Some(layout.labels[label.0] as u64));
                    }
                    (Some(_), Inst::Branch { .. }, _) => assert_eq!(
                        decoded.branch_target,
                        Some(at as u64 + 4 * (layout.forms[n] as u64 + 1))
                    ),
                    (Some(_), _, _) => assert_eq!(decoded.mnemonic, "auipc"),
                    (None, _, _) => assert_eq!(
                        (decoded.mnemonic, decoded.operands),
                        (name, operands),
                        "{inst:?}"
                    ),
                }
            }
        }
    });
}

#[test]
fn division_is_total_on_the_edges() {
    conformance::each(Isa::Riscv64, conformance::division_is_total);
}

#[test]
fn shifts_and_rotates_count_modulo_the_width() {
    conformance::each(Isa::Riscv64, conformance::shifts_count_modulo_the_width);
}

/// With Zbb a rotation and a 16-bit or signed 8-bit extension is one
/// instruction; without it, none of Zbb's appears.
#[test]
fn zbb_rotates_and_extends_in_one_instruction() {
    use crate::ir::{BinOp, Block, Builder, Type, UnOp};
    let zbb = Target::RISCV64.with(tairix_abi::cpufeatures::CpuFeature::Zbb);
    let lowered = |func: &crate::ir::Function, target| build(func, target).unwrap().0.insts;
    let uses_zbb = |insts: &[Inst]| {
        insts.iter().any(|inst| {
            matches!(
                inst,
                Inst::Ext { .. }
                    | Inst::Reg {
                        op: Op::Rol | Op::Ror,
                        ..
                    }
                    | Inst::Imm {
                        op: ImmOp::Rori,
                        ..
                    }
            )
        })
    };
    for ty in [Type::I32, Type::I64] {
        for (op, constant) in [
            (BinOp::Rotl, false),
            (BinOp::Rotr, false),
            (BinOp::Rotl, true),
            (BinOp::Rotr, true),
        ] {
            let mut b = Builder::new(&[ty, ty], Some(ty));
            let p = b.params(Block::ENTRY).to_vec();
            let count = if constant { b.iconst(ty, 5) } else { p[1] };
            let rotated = b.binary(op, p[0], count);
            b.ret(Some(rotated));
            let func = b.finish().unwrap();
            assert!(uses_zbb(&lowered(&func, zbb)), "{op:?} {ty:?}");
            assert!(!uses_zbb(&lowered(&func, Target::RISCV64)), "{op:?} {ty:?}");
        }
        for op in [UnOp::Uext16, UnOp::Sext8, UnOp::Sext16] {
            let mut b = Builder::new(&[ty], Some(ty));
            let p = b.params(Block::ENTRY)[0];
            let extended = b.unary(op, p);
            b.ret(Some(extended));
            let func = b.finish().unwrap();
            assert!(uses_zbb(&lowered(&func, zbb)), "{op:?} {ty:?}");
            assert!(!uses_zbb(&lowered(&func, Target::RISCV64)), "{op:?} {ty:?}");
        }
    }
}

#[test]
fn immediates_on_the_edge_of_folding_agree() {
    conformance::each(Isa::Riscv64, |target| {
        conformance::immediates_on_their_edges(
            target,
            &[
                0x7FF,
                0x800,
                0xFFFF_F800,
                0xFFFF_F7FF,
                0xFFFF_FFFF_FFFF_F800,
                0xFFFF_FFFF_FFFF_F7FF,
                0x8000_0800,
                0x7FFF_F800,
                0x1234_5678_9ABC_DEF0,
            ],
        );
    });
}

#[test]
fn comparisons_against_constants_agree_on_their_edges() {
    use crate::ir::{Block, Builder, Cond as C, Type};
    use crate::testgen::{agree, Case, BUFFER, TABLE};
    let conds = [
        C::Eq,
        C::Ne,
        C::Slt,
        C::Sle,
        C::Sgt,
        C::Sge,
        C::Ult,
        C::Ule,
        C::Ugt,
        C::Uge,
    ];
    let constants = [
        0u64,
        1,
        2046,
        2047,
        2048,
        u64::MAX,
        u64::MAX - 1,
        0xFFFF_FFFF,
        0xFFFF_F800,
        0xFFFF_FFFF_FFFF_F800,
        0x8000_0000,
    ];
    for ty in [Type::I32, Type::I64] {
        for cond in conds {
            for &c in &constants {
                for &a in &[
                    0u64,
                    1,
                    2047,
                    2048,
                    u64::MAX,
                    0xFFFF_FFFF,
                    0x8000_0000,
                    0x7FFF_FFFF,
                ] {
                    let mut b = Builder::new(&[Type::I64, Type::I64, ty], Some(Type::I32));
                    let x = b.params(Block::ENTRY)[2];
                    let k = b.iconst(ty, c);
                    let r = b.icmp(cond, x, k);
                    b.ret(Some(r));
                    let case = Case {
                        func: b.finish().unwrap(),
                        args: alloc::vec![BUFFER, TABLE, ty.wrap(a)],
                        memory: TestMemory {
                            buffer: alloc::vec![0; BUFFER_BYTES],
                        },
                    };
                    assert_eq!(
                        agree(Target::RISCV64, &case, 10),
                        Ok(()),
                        "{ty:?} {a:#x} {cond:?} {c:#x}"
                    );
                }
            }
        }
    }
}

#[test]
fn values_outlive_a_call_in_saved_registers_and_slots() {
    // Past 255 slots, each beyond a load's twelve-bit reach.
    conformance::each(Isa::Riscv64, |target| {
        conformance::values_outlive_a_call(target, 1000);
    });
}

#[test]
fn loads_and_stores_reach_their_offsets() {
    conformance::each(Isa::Riscv64, conformance::offsets_on_their_edges);
}

#[test]
fn a_function_that_answers_nothing_reads_no_answer() {
    conformance::each(
        Isa::Riscv64,
        conformance::a_function_that_answers_nothing_reads_no_answer,
    );
}

#[test]
fn compile_answers_the_bytes_the_model_ran() {
    conformance::each(Isa::Riscv64, |target| {
        conformance::compile_is_the_build(target, |func| build(func, target).unwrap().1);
    });
}

#[test]
fn constants_take_their_fewest_instructions() {
    let count = |imm: i64| {
        let mut risc = Risc::default();
        risc.constant(10, imm);
        risc.insts.len()
    };
    assert_eq!(count(0), 1);
    assert_eq!(count(-2048), 1);
    assert_eq!(count(0x1000), 1, "lui alone");
    assert_eq!(count(0x7FFF_FFFF), 2);
    assert_eq!(count(i64::from(i32::MIN)), 1);
    assert_eq!(count(i64::MIN), 2, "one, shifted");
    assert_eq!(count(0x1_0000_0000), 2);
    assert!(count(0x1234_5678_9ABC_DEF0) <= 8);
    assert!(count(i64::MAX) <= 8, "the low bits' borrow wraps");
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

const fn addi(rd: u8, rs1: u8, imm: i16) -> Inst {
    Inst::Imm {
        op: ImmOp::Addi,
        word: false,
        rd,
        rs1,
        imm,
    }
}

const fn word(store: bool, reg: u8, offset: i16) -> Inst {
    if store {
        Inst::Store {
            size: Size::D,
            rs2: reg,
            rs1: reg::SP,
            offset,
        }
    } else {
        Inst::Load {
            size: Size::D,
            signed: true,
            rd: reg,
            rs1: reg::SP,
            offset,
        }
    }
}

const RET: Inst = Inst::Jalr {
    rd: reg::ZERO,
    rs1: reg::RA,
    offset: 0,
};

/// A call of the helper the test table puts third.
fn call() -> Vec<Inst> {
    let mut risc = Risc::default();
    risc.constant(reg::T0, i64::try_from(HELPER_BASE + 48).unwrap());
    risc.insts.push(Inst::Jalr {
        rd: reg::RA,
        rs1: reg::T0,
        offset: 0,
    });
    risc.insts
}

#[test]
fn the_model_refuses_what_was_never_defined_or_is_kept() {
    assert_eq!(run(&[RET]), Err(Fault::Undefined(reg::A0)));
    assert_eq!(
        run(&[addi(sim::GP, reg::ZERO, 1)]),
        Err(Fault::Reserved(sim::GP))
    );
    let changed = [addi(9, reg::ZERO, 1), addi(reg::A0, reg::ZERO, 1), RET];
    assert_eq!(run(&changed), Err(Fault::Clobbered(9)));
}

#[test]
fn the_model_refuses_a_misaligned_stack_or_a_missing_record() {
    assert_eq!(run(&call()), Err(Fault::Frame));
    let mut misaligned = Vec::from([addi(reg::SP, reg::SP, -8)]);
    misaligned.extend(call());
    assert_eq!(run(&misaligned), Err(Fault::Misaligned));
}

#[test]
fn a_call_leaves_nothing_below_the_stack_pointer() {
    let mut code = Vec::from([
        addi(reg::SP, reg::SP, -16),
        word(true, reg::RA, 8),
        word(true, reg::S0, 0),
        addi(reg::S0, reg::SP, 16),
        addi(reg::A0, reg::ZERO, 5),
        word(true, reg::A0, -8),
    ]);
    code.extend(call());
    code.extend([
        word(false, reg::A0, -8),
        word(false, reg::RA, 8),
        word(false, reg::S0, 0),
        addi(reg::SP, reg::SP, 16),
        RET,
    ]);
    assert_eq!(run(&code), Ok(Some(0xCCCC_CCCC_CCCC_CCCC)));
}
