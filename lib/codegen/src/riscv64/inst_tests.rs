use alloc::string::String;
use alloc::vec::Vec;

use super::*;
use crate::riscv64::render::text;

/// `inst` encoded alone decodes, as one word, to what it means.
fn agrees(inst: Inst) {
    let bytes = encode(&[inst], 0).unwrap();
    assert_eq!(bytes.len(), 4, "{inst:?}");
    let decoded = tairix_disasm::riscv64::decode(&bytes, 0).unwrap();
    let (name, operands) = text(&inst);
    assert_eq!(
        (
            decoded.mnemonic.as_str(),
            decoded.operands.as_str(),
            decoded.length
        ),
        (name.as_str(), operands.as_str(), 4),
        "{inst:?} as {bytes:02x?}"
    );
}

const REGS: [u8; 8] = [0, 1, 2, 5, 8, 10, 28, 31];
const OPS: [Op; 15] = [
    Op::Add,
    Op::Sub,
    Op::Sll,
    Op::Slt,
    Op::Sltu,
    Op::Xor,
    Op::Srl,
    Op::Sra,
    Op::Or,
    Op::And,
    Op::Mul,
    Op::Div,
    Op::Divu,
    Op::Rem,
    Op::Remu,
];

/// Every pair of the registers worth naming.
fn each_pair(mut check: impl FnMut(u8, u8)) {
    for rd in REGS {
        for rs1 in REGS {
            check(rd, rs1);
        }
    }
}

#[test]
fn register_forms_name_their_registers() {
    each_pair(|rd, rs1| {
        for rs2 in [0, 7, 31] {
            for op in OPS {
                for word in [false, true] {
                    if !word || op.has_word() {
                        agrees(Inst::Reg {
                            op,
                            word,
                            rd,
                            rs1,
                            rs2,
                        });
                    }
                }
            }
        }
    });
}

const ARITH: [ImmOp; 6] = [
    ImmOp::Addi,
    ImmOp::Slti,
    ImmOp::Sltiu,
    ImmOp::Xori,
    ImmOp::Ori,
    ImmOp::Andi,
];

#[test]
fn immediate_forms_mean_what_they_hold() {
    each_pair(|rd, rs1| {
        let imm = |op, word, imm| Inst::Imm {
            op,
            word,
            rd,
            rs1,
            imm,
        };
        for op in ARITH {
            for n in [0, 1, -1, 2047, -2048, 255] {
                agrees(imm(op, false, n));
            }
        }
        for n in [0, 1, -1, 2047, -2048] {
            agrees(imm(ImmOp::Addi, true, n));
        }
        for op in [ImmOp::Slli, ImmOp::Srli, ImmOp::Srai] {
            for n in [0, 1, 31, 32, 63] {
                agrees(imm(op, false, n));
            }
            for n in [0, 1, 31] {
                agrees(imm(op, true, n));
            }
        }
    });
    for rd in REGS {
        for imm in [0, 1, 0x7FFFF, 0x80000, 0xFFFFF] {
            agrees(Inst::Lui { rd, imm });
        }
    }
}

#[test]
fn zbb_forms_mean_what_they_hold() {
    each_pair(|rd, rs1| {
        for word in [false, true] {
            for op in [Op::Rol, Op::Ror] {
                for rs2 in [0, 7, 31] {
                    agrees(Inst::Reg {
                        op,
                        word,
                        rd,
                        rs1,
                        rs2,
                    });
                }
            }
            let widest = if word { 31 } else { 63 };
            for imm in [0, 1, 31, widest] {
                agrees(Inst::Imm {
                    op: ImmOp::Rori,
                    word,
                    rd,
                    rs1,
                    imm,
                });
            }
        }
        for ext in [Ext::SextB, Ext::SextH, Ext::ZextH] {
            agrees(Inst::Ext { ext, rd, rs1 });
        }
    });
    let refused = [64, -1].map(|imm| Inst::Imm {
        op: ImmOp::Rori,
        word: false,
        rd: 1,
        rs1: 1,
        imm,
    });
    let word = Inst::Imm {
        op: ImmOp::Rori,
        word: true,
        rd: 1,
        rs1: 1,
        imm: 32,
    };
    for inst in refused.into_iter().chain([word]) {
        assert_eq!(encode(&[inst], 0), Err(CompileError::Malformed), "{inst:?}");
    }
}

#[test]
fn memory_and_jump_forms_mean_what_they_hold() {
    each_pair(|rd, rs1| {
        for offset in [0, 8, -8, 2047, -2048] {
            for size in [Size::B, Size::H, Size::W, Size::D] {
                agrees(Inst::Store {
                    size,
                    rs2: rd,
                    rs1,
                    offset,
                });
                for signed in [true, false] {
                    if signed || size != Size::D {
                        agrees(Inst::Load {
                            size,
                            signed,
                            rd,
                            rs1,
                            offset,
                        });
                    }
                }
            }
            agrees(Inst::Jalr { rd, rs1, offset });
        }
    });
}

#[test]
fn fields_out_of_range_are_refused() {
    let bad = [
        Inst::Imm {
            op: ImmOp::Addi,
            word: false,
            rd: 1,
            rs1: 1,
            imm: 2048,
        },
        Inst::Imm {
            op: ImmOp::Slli,
            word: false,
            rd: 1,
            rs1: 1,
            imm: 64,
        },
        Inst::Imm {
            op: ImmOp::Slli,
            word: true,
            rd: 1,
            rs1: 1,
            imm: 32,
        },
        Inst::Imm {
            op: ImmOp::Xori,
            word: true,
            rd: 1,
            rs1: 1,
            imm: 1,
        },
        Inst::Reg {
            op: Op::And,
            word: true,
            rd: 1,
            rs1: 1,
            rs2: 1,
        },
        Inst::Lui {
            rd: 1,
            imm: 1 << 20,
        },
        Inst::Load {
            size: Size::D,
            signed: false,
            rd: 1,
            rs1: 1,
            offset: 0,
        },
        Inst::Store {
            size: Size::B,
            rs2: 32,
            rs1: 1,
            offset: 0,
        },
    ];
    for inst in bad {
        assert_eq!(encode(&[inst], 0), Err(CompileError::Malformed), "{inst:?}");
    }
}

/// The instructions `insts` encode to, walked by the decoder: every jump's
/// final transfer lands on its label.
fn walk(insts: &[Inst], labels: usize) -> Vec<String> {
    let bytes = encode(insts, labels).unwrap();
    let layout = relax(insts, labels).unwrap();
    let mut names = Vec::new();
    for (n, inst) in insts.iter().enumerate() {
        let at = layout.places[n];
        let decode = |at: usize| tairix_disasm::riscv64::decode(&bytes[at..], at as u64).unwrap();
        match inst.target() {
            None if matches!(inst, Inst::Bind(_)) => {}
            None => names.push(decode(at).mnemonic),
            Some(label) => {
                let place = layout.labels[label.0] as u64;
                let first = decode(at);
                names.push(first.mnemonic.clone());
                match (inst, layout.forms[n]) {
                    (_, 0) => assert_eq!(first.branch_target, Some(place), "{inst:?}"),
                    (Inst::Branch { .. }, 1) => {
                        assert_eq!(
                            first.branch_target,
                            Some(at as u64 + 8),
                            "the inverted skip"
                        );
                        assert_eq!(decode(at + 4).branch_target, Some(place));
                    }
                    (Inst::Branch { .. }, _) => {
                        assert_eq!(
                            first.branch_target,
                            Some(at as u64 + 12),
                            "the inverted skip"
                        );
                        far_lands(&bytes, at + 4, place);
                    }
                    _ => far_lands(&bytes, at, place),
                }
            }
        }
    }
    names
}

/// `auipc t0` then `jalr zero` at `at` go to `place`.
fn far_lands(bytes: &[u8], at: usize, place: u64) {
    let auipc = tairix_disasm::riscv64::decode(&bytes[at..], at as u64).unwrap();
    let jalr = tairix_disasm::riscv64::decode(&bytes[at + 4..], at as u64 + 4).unwrap();
    assert_eq!(auipc.mnemonic, "auipc");
    let hi = i64::from_str_radix(auipc.operands.trim_start_matches("t0,0x"), 16).unwrap();
    let hi = (hi << 44) >> 44;
    let lo: i64 = jalr
        .operands
        .trim_start_matches("zero,")
        .trim_end_matches("(t0)")
        .parse()
        .unwrap();
    assert_eq!(
        (i64::try_from(at).unwrap() + (hi << 12) + lo).cast_unsigned(),
        place
    );
}

#[test]
fn branches_relax_through_each_reach() {
    let near = [
        Inst::Bind(Label(0)),
        Inst::Branch {
            cond: Cond::Lt,
            rs1: 10,
            rs2: 11,
            label: Label(1),
        },
        Inst::Jump(Label(0)),
        Inst::Bind(Label(1)),
        Inst::Jalr {
            rd: 0,
            rs1: 1,
            offset: 0,
        },
    ];
    assert_eq!(walk(&near, 2), ["blt", "jal", "jalr"]);
    let pad = |n: usize| {
        core::iter::repeat_n(
            Inst::Imm {
                op: ImmOp::Addi,
                word: false,
                rd: 0,
                rs1: 0,
                imm: 0,
            },
            n,
        )
    };
    // Past a branch's four kibibytes: over a `jal`.
    let mut mid = Vec::from([Inst::Branch {
        cond: Cond::Geu,
        rs1: 5,
        rs2: 0,
        label: Label(0),
    }]);
    mid.extend(pad(1100));
    mid.push(Inst::Bind(Label(0)));
    assert_eq!(walk(&mid, 1)[0], "bltu");
    // Past a `jal`'s mebibyte: over `auipc` and `jalr`.
    let mut far = Vec::from([
        Inst::Branch {
            cond: Cond::Eq,
            rs1: 5,
            rs2: 6,
            label: Label(0),
        },
        Inst::Jump(Label(0)),
    ]);
    far.extend(pad((1 << 20) / 4 + 10));
    far.push(Inst::Bind(Label(0)));
    far.push(Inst::Jump(Label(1)));
    far.push(Inst::Bind(Label(1)));
    let names = walk(&far, 2);
    assert_eq!(names[0], "bne");
    assert_eq!(names[1], "auipc");
    assert_eq!(
        names.last().map(String::as_str),
        Some("jal"),
        "a near one stays short"
    );
}

#[test]
fn reaches_are_the_encodings_own() {
    let branch = Inst::Branch {
        cond: Cond::Eq,
        rs1: 0,
        rs2: 0,
        label: Label(0),
    };
    assert!(branch.reaches(0, 4094) && branch.reaches(0, -4096));
    assert!(!branch.reaches(0, 4096) && !branch.reaches(0, 3));
    assert!(branch.reaches(1, 4 + (1 << 20) - 2) && !branch.reaches(1, 4 + (1 << 20)));
    assert!(branch.reaches(2, 4 + (1 << 31) - 0x802) && !branch.reaches(2, 4 + (1 << 31) - 0x800));
    let jump = Inst::Jump(Label(0));
    assert!(jump.reaches(0, -(1 << 20)) && !jump.reaches(0, 1 << 20));
    assert!(jump.reaches(1, -(1 << 31) - 0x800) && !jump.reaches(1, -(1 << 31) - 0x802));
}
