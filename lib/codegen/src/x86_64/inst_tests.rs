use alloc::vec::Vec;

use super::*;
use crate::x86_64::render::text;

/// `inst` encoded alone decodes, as one instruction of all its bytes, to
/// what it means.
fn agrees(inst: Inst) {
    let bytes = encode(&[inst], 0).unwrap();
    let decoded = tairix_disasm::x86_64::decode(&bytes, 0).unwrap();
    let (name, operands) = text(&inst);
    assert_eq!(
        (
            decoded.mnemonic.as_str(),
            decoded.operands.as_str(),
            decoded.length
        ),
        (name.as_str(), operands.as_str(), bytes.len()),
        "{inst:?} as {bytes:02x?}"
    );
}

const REGS: core::ops::Range<u8> = 0..16;
const WIDTHS: [Bytes; 4] = [Bytes::B1, Bytes::B2, Bytes::B4, Bytes::B8];
const CCS: [Cc; 10] = [
    Cc::B,
    Cc::Ae,
    Cc::E,
    Cc::Ne,
    Cc::Be,
    Cc::A,
    Cc::L,
    Cc::Ge,
    Cc::Le,
    Cc::G,
];
const DISPS: [i32; 9] = [0, 1, -1, 127, -128, 128, -129, i32::MAX, i32::MIN];
const IMMS: [i32; 8] = [0, 1, -1, 127, -128, 128, i32::MAX, i32::MIN];

#[test]
fn register_forms_name_their_registers() {
    for wide in [false, true] {
        for dst in REGS {
            agrees(Inst::Neg { wide, dst });
            agrees(Inst::Not { wide, dst });
            agrees(Inst::Div {
                signed: false,
                wide,
                src: dst,
            });
            agrees(Inst::Div {
                signed: true,
                wide,
                src: dst,
            });
            for src in REGS {
                agrees(Inst::Mov { wide, dst, src });
                agrees(Inst::Imul { wide, dst, src });
                agrees(Inst::Test {
                    wide,
                    a: dst,
                    b: src,
                });
                for op in [Alu::Add, Alu::Or, Alu::And, Alu::Sub, Alu::Xor, Alu::Cmp] {
                    agrees(Inst::Alu {
                        op,
                        wide,
                        dst,
                        src: Src::Reg(src),
                    });
                }
                for cc in CCS {
                    agrees(Inst::Cmov { cc, wide, dst, src });
                }
                agrees(Inst::Movsx {
                    from: Bytes::B1,
                    wide,
                    dst,
                    src,
                });
                agrees(Inst::Movsx {
                    from: Bytes::B2,
                    wide,
                    dst,
                    src,
                });
            }
        }
        agrees(Inst::SignExtend { wide });
    }
    for dst in REGS {
        agrees(Inst::Push(dst));
        agrees(Inst::Pop(dst));
        agrees(Inst::Call(dst));
        for cc in CCS {
            agrees(Inst::Setcc { cc, dst });
        }
        for src in REGS {
            agrees(Inst::Movzx {
                from: Bytes::B1,
                dst,
                src,
            });
            agrees(Inst::Movzx {
                from: Bytes::B2,
                dst,
                src,
            });
            agrees(Inst::Movsx {
                from: Bytes::B4,
                wide: true,
                dst,
                src,
            });
        }
    }
    agrees(Inst::Ret);
}

#[test]
fn bmi2_forms_name_their_registers() {
    for wide in [false, true] {
        for dst in REGS {
            for src in REGS {
                for count in [reg::RAX, reg::RCX, reg::RSP, reg::R8, reg::R15] {
                    for op in [ShiftX::Shl, ShiftX::Shr, ShiftX::Sar] {
                        agrees(Inst::ShiftX {
                            op,
                            wide,
                            dst,
                            src,
                            count,
                        });
                    }
                }
                for imm in [0, 1, 31, 63] {
                    agrees(Inst::Rorx {
                        wide,
                        dst,
                        src,
                        imm,
                    });
                }
            }
        }
    }
}

#[test]
fn immediates_take_their_shortest_form_and_mean_what_they_hold() {
    for wide in [false, true] {
        for dst in REGS {
            for imm in IMMS {
                for op in [Alu::Add, Alu::Or, Alu::And, Alu::Sub, Alu::Xor, Alu::Cmp] {
                    agrees(Inst::Alu {
                        op,
                        wide,
                        dst,
                        src: Src::Imm(imm),
                    });
                }
            }
            for op in [Shift::Rol, Shift::Ror, Shift::Shl, Shift::Shr, Shift::Sar] {
                agrees(Inst::Shift {
                    op,
                    wide,
                    dst,
                    count: Count::Cl,
                });
                for n in [1, 7, 31, 63] {
                    agrees(Inst::Shift {
                        op,
                        wide,
                        dst,
                        count: Count::Imm(n),
                    });
                }
            }
            for imm in [
                0,
                1,
                0x7FFF_FFFF,
                0xFFFF_FFFF,
                0x1_0000_0000,
                u64::MAX,
                0xFFFF_FFFF_8000_0000,
                0x1234_5678_9ABC_DEF0,
            ] {
                agrees(Inst::MovImm { wide, dst, imm });
            }
        }
    }
    let short = encode(
        &[Inst::Alu {
            op: Alu::Add,
            wide: true,
            dst: reg::RAX,
            src: Src::Imm(-8),
        }],
        0,
    )
    .unwrap();
    assert_eq!(short.len(), 4, "an immediate a byte holds takes a byte");
    let short = encode(
        &[Inst::MovImm {
            wide: true,
            dst: reg::RAX,
            imm: 7,
        }],
        0,
    )
    .unwrap();
    assert_eq!(
        short.len(),
        5,
        "a 64-bit constant a 32-bit one holds zero-extends"
    );
}

#[test]
fn memory_operands_reach_every_base_and_displacement() {
    for base in REGS {
        for disp in DISPS {
            for bytes in WIDTHS {
                for src in REGS {
                    agrees(Inst::Store {
                        bytes,
                        src,
                        base,
                        disp,
                    });
                }
                for imm in IMMS {
                    agrees(Inst::StoreImm {
                        bytes,
                        imm,
                        base,
                        disp,
                    });
                }
                for dst in [reg::RAX, reg::RSI, reg::R9, reg::R15] {
                    for signed in [false, true] {
                        for wide in [false, true] {
                            if bytes == Bytes::B8 && !wide {
                                continue;
                            }
                            agrees(Inst::Load {
                                bytes,
                                signed,
                                wide,
                                dst,
                                base,
                                disp,
                            });
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn jumps_reach_their_labels_both_ways() {
    let insts = [
        Inst::Bind(Label(0)),
        Inst::Jcc(Cc::Ne, Label(1)),
        Inst::Mov {
            wide: true,
            dst: reg::RAX,
            src: reg::RBX,
        },
        Inst::Jmp(Label(0)),
        Inst::Bind(Label(1)),
        Inst::Jcc(Cc::L, Label(0)),
        Inst::Jmp(Label(1)),
        Inst::Ret,
    ];
    let bytes = encode(&insts, 2).unwrap();
    let mut at = 0usize;
    let mut places = [None; 2];
    let mut jumps = Vec::new();
    for inst in &insts {
        match *inst {
            Inst::Bind(label) => places[label.0] = Some(at as u64),
            other => {
                let decoded = tairix_disasm::x86_64::decode(&bytes[at..], at as u64).unwrap();
                assert_eq!(decoded.mnemonic, text(&other).0);
                if let Inst::Jmp(label) | Inst::Jcc(_, label) = other {
                    assert_eq!(
                        decoded.length,
                        if matches!(other, Inst::Jmp(_)) { 5 } else { 6 }
                    );
                    jumps.push((label, decoded.branch_target));
                }
                at += decoded.length;
            }
        }
    }
    assert_eq!(at, bytes.len());
    for (label, target) in jumps {
        assert_eq!(target, places[label.0]);
    }
}

#[test]
fn a_jump_to_a_label_never_bound_is_refused() {
    assert_eq!(
        encode(&[Inst::Jmp(Label(0))], 1),
        Err(CompileError::Malformed)
    );
    assert_eq!(
        encode(&[Inst::Bind(Label(3))], 1),
        Err(CompileError::Malformed)
    );
}
