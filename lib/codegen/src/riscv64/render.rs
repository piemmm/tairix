//! Each machine instruction as the RV64 decoder renders it, written from
//! what the instruction means rather than from its bits, so the encoding can
//! be checked by an independent decoder.

use alloc::format;
use alloc::string::String;

use super::inst::{Cond, Ext, ImmOp, Inst, Op, Size};

const XREG: [&str; 32] = [
    "zero", "ra", "sp", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2", "a3", "a4",
    "a5", "a6", "a7", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11", "t3", "t4",
    "t5", "t6",
];

fn x(n: u8) -> &'static str {
    XREG[usize::from(n)]
}

fn word(name: &str, word: bool) -> String {
    if word {
        format!("{name}w")
    } else {
        String::from(name)
    }
}

const fn reg_name(op: Op) -> &'static str {
    match op {
        Op::Add => "add",
        Op::Sub => "sub",
        Op::Sll => "sll",
        Op::Slt => "slt",
        Op::Sltu => "sltu",
        Op::Xor => "xor",
        Op::Srl => "srl",
        Op::Sra => "sra",
        Op::Or => "or",
        Op::And => "and",
        Op::Mul => "mul",
        Op::Div => "div",
        Op::Divu => "divu",
        Op::Rem => "rem",
        Op::Remu => "remu",
        Op::Rol => "rol",
        Op::Ror => "ror",
    }
}

const fn imm_name(op: ImmOp) -> &'static str {
    match op {
        ImmOp::Addi => "addi",
        ImmOp::Slti => "slti",
        ImmOp::Sltiu => "sltiu",
        ImmOp::Xori => "xori",
        ImmOp::Ori => "ori",
        ImmOp::Andi => "andi",
        ImmOp::Slli => "slli",
        ImmOp::Srli => "srli",
        ImmOp::Srai => "srai",
        ImmOp::Rori => "rori",
    }
}

/// `inst`'s mnemonic and operands; a jump's target is left to the caller,
/// which checks it apart.
pub(crate) fn text(inst: &Inst) -> (String, String) {
    match *inst {
        Inst::Reg {
            op,
            word: w,
            rd,
            rs1,
            rs2,
        } => (
            word(reg_name(op), w),
            format!("{},{},{}", x(rd), x(rs1), x(rs2)),
        ),
        Inst::Imm {
            op,
            word: w,
            rd,
            rs1,
            imm,
        } => (word(imm_name(op), w), format!("{},{},{imm}", x(rd), x(rs1))),
        Inst::Lui { rd, imm } => ("lui".into(), format!("{},{imm:#x}", x(rd))),
        Inst::Ext { ext, rd, rs1 } => {
            let name = match ext {
                Ext::SextB => "sext.b",
                Ext::SextH => "sext.h",
                Ext::ZextH => "zext.h",
            };
            (name.into(), format!("{},{}", x(rd), x(rs1)))
        }
        Inst::Load {
            size,
            signed,
            rd,
            rs1,
            offset,
        } => {
            let name = match (size, signed) {
                (Size::B, true) => "lb",
                (Size::H, true) => "lh",
                (Size::W, true) => "lw",
                (Size::D, _) => "ld",
                (Size::B, false) => "lbu",
                (Size::H, false) => "lhu",
                (Size::W, false) => "lwu",
            };
            (name.into(), format!("{},{offset}({})", x(rd), x(rs1)))
        }
        Inst::Store {
            size,
            rs2,
            rs1,
            offset,
        } => {
            let name = match size {
                Size::B => "sb",
                Size::H => "sh",
                Size::W => "sw",
                Size::D => "sd",
            };
            (name.into(), format!("{},{offset}({})", x(rs2), x(rs1)))
        }
        Inst::Jalr { rd, rs1, offset } => {
            ("jalr".into(), format!("{},{offset}({})", x(rd), x(rs1)))
        }
        Inst::Branch { cond, rs1, rs2, .. } => {
            let name = match cond {
                Cond::Eq => "beq",
                Cond::Ne => "bne",
                Cond::Lt => "blt",
                Cond::Ge => "bge",
                Cond::Ltu => "bltu",
                Cond::Geu => "bgeu",
            };
            (name.into(), format!("{},{}", x(rs1), x(rs2)))
        }
        Inst::Jump(_) => ("jal".into(), "zero".into()),
        Inst::Bind(_) => (String::new(), String::new()),
    }
}
