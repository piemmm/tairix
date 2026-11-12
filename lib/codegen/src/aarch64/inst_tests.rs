use alloc::string::String;
use alloc::vec::Vec;

use super::*;
use crate::aarch64::render::text;

/// `inst` encoded alone decodes, as one word, to what it means.
fn agrees(inst: Inst) {
    let bytes = encode(&[inst], 0).unwrap();
    assert_eq!(bytes.len(), 4, "{inst:?}");
    let decoded = tairix_disasm::aarch64::decode(&bytes, 0).unwrap();
    let (name, operands) = text(&inst);
    assert_eq!(
        (decoded.mnemonic.as_str(), decoded.operands.as_str()),
        (name.as_str(), operands.as_str()),
        "{inst:?} as {bytes:02x?}"
    );
}

const CONDS: [Cond; 10] = [
    Cond::Eq,
    Cond::Ne,
    Cond::Hs,
    Cond::Lo,
    Cond::Hi,
    Cond::Ls,
    Cond::Ge,
    Cond::Lt,
    Cond::Gt,
    Cond::Le,
];
const SIZES: [Size; 4] = [Size::B, Size::H, Size::W, Size::X];

/// The registers worth naming: the first, each scratch, the platform's,
/// the frame and link registers, and 31.
const REGS: [u8; 9] = [0, 7, 15, 16, 17, 19, 29, 30, 31];

/// Every combination of width and the registers worth naming.
fn each_register(mut check: impl FnMut(bool, u8, u8, u8)) {
    for wide in [false, true] {
        for rd in REGS {
            for rn in REGS {
                for rm in [0, 17, 31] {
                    check(wide, rd, rn, rm);
                }
            }
        }
    }
}

const LOGIC: [Logic; 4] = [Logic::And, Logic::Orr, Logic::Eor, Logic::Orn];
const DP2: [Dp2; 6] = [
    Dp2::Udiv,
    Dp2::Sdiv,
    Dp2::Lslv,
    Dp2::Lsrv,
    Dp2::Asrv,
    Dp2::Rorv,
];

#[test]
fn arithmetic_and_logic_name_their_registers() {
    each_register(|wide, rd, rn, rm| {
        for (sub, flags) in [(false, false), (false, true), (true, false), (true, true)] {
            agrees(Inst::AddReg {
                sub,
                flags,
                wide,
                rd,
                rn,
                rm,
            });
        }
        for op in LOGIC {
            agrees(Inst::Logic {
                op,
                wide,
                rd,
                rn,
                rm,
            });
        }
        for op in DP2 {
            agrees(Inst::Dp2 {
                op,
                wide,
                rd,
                rn,
                rm,
            });
        }
    });
}

#[test]
fn multiply_and_select_name_their_registers() {
    each_register(|wide, rd, rn, rm| {
        for (sub, ra) in [(false, 31), (true, 31), (false, 5), (true, 5)] {
            agrees(Inst::Madd {
                sub,
                wide,
                rd,
                rn,
                rm,
                ra,
            });
        }
        for cond in CONDS {
            for inc in [false, true] {
                agrees(Inst::Csel {
                    inc,
                    wide,
                    rd,
                    rn,
                    rm,
                    cond,
                });
            }
        }
    });
}

#[test]
fn stack_and_control_forms_name_their_registers() {
    for rd in REGS {
        for rn in REGS {
            for sub in [false, true] {
                agrees(Inst::AddSp {
                    sub,
                    rd,
                    rn,
                    rm: 16,
                });
            }
        }
        agrees(Inst::Blr(rd));
    }
    agrees(Inst::Ret);
    for (rt, rt2) in [(reg::FP, reg::LR), (27, reg::ZR), (19, 20)] {
        agrees(Inst::PushPair { rt, rt2 });
        agrees(Inst::PopPair { rt, rt2 });
    }
}

#[test]
fn immediates_and_fields_mean_what_they_hold() {
    for wide in [false, true] {
        for rd in REGS {
            for hw in 0..if wide { 4 } else { 2 } {
                for imm in [0, 1, 0x8000, 0xFFFF] {
                    agrees(Inst::Movz { wide, rd, imm, hw });
                    agrees(Inst::Movk { wide, rd, imm, hw });
                    agrees(Inst::Movn { wide, rd, imm, hw });
                }
            }
            for imm in [0, 1, 0xFFF] {
                for (sub, flags, high) in [
                    (false, false, false),
                    (true, true, true),
                    (true, false, true),
                    (false, true, false),
                ] {
                    agrees(Inst::AddImm {
                        sub,
                        flags,
                        wide,
                        rd,
                        rn: reg::SP,
                        imm,
                        high,
                    });
                    agrees(Inst::AddImm {
                        sub,
                        flags,
                        wide,
                        rd,
                        rn: 3,
                        imm,
                        high,
                    });
                }
            }
            let bits = if wide { 64 } else { 32 };
            for signed in [false, true] {
                for (immr, imms) in [
                    (0, 7),
                    (0, 15),
                    (0, 31),
                    (1, bits - 1),
                    (bits - 3, bits - 4),
                    (5, bits - 1),
                ] {
                    agrees(Inst::Bfm {
                        signed,
                        wide,
                        rd,
                        rn: 9,
                        immr,
                        imms,
                    });
                }
            }
            for lsb in [0, 1, bits - 1] {
                agrees(Inst::Extr {
                    wide,
                    rd,
                    rn: 4,
                    rm: 4,
                    lsb,
                });
            }
        }
    }
}

#[test]
fn every_logical_immediate_round_trips_through_the_decoder() {
    // Every encoding the architecture allows: its value, found by the
    // decoder, encodes back to the same value.
    for wide in [false, true] {
        for n in [false, true] {
            if n && !wide {
                continue;
            }
            for immr in 0..64u8 {
                for imms in 0..64u8 {
                    let word = 0x3200_03E0
                        | u32::from(wide) << 31
                        | u32::from(n) << 22
                        | u32::from(immr) << 16
                        | u32::from(imms) << 10;
                    let decoded = tairix_disasm::aarch64::decode(&word.to_le_bytes(), 0).unwrap();
                    let Some(text) =
                        decoded
                            .operands
                            .strip_prefix(if wide { "x0,xzr,#" } else { "w0,wzr,#" })
                    else {
                        continue;
                    };
                    let value = u64::from_str_radix(text.trim_start_matches("0x"), 16).unwrap();
                    let mask = bitmask(value, wide)
                        .unwrap_or_else(|| panic!("{value:#x} has an encoding"));
                    agrees(Inst::LogicImm {
                        op: Logic::Orr,
                        wide,
                        rd: 0,
                        rn: reg::ZR,
                        mask,
                    });
                    let again = encode(
                        &[Inst::LogicImm {
                            op: Logic::Orr,
                            wide,
                            rd: 0,
                            rn: reg::ZR,
                            mask,
                        }],
                        0,
                    )
                    .unwrap();
                    let back = tairix_disasm::aarch64::decode(&again, 0).unwrap();
                    assert_eq!(back.operands, decoded.operands);
                }
            }
        }
    }
    assert!(
        bitmask(0xFFFF_FFFF_0000_0001, true).is_some(),
        "a run round the top"
    );
    for value in [0, u64::MAX, 0x1234, 0xFFFF_FFFF_0000_0101] {
        assert_eq!(bitmask(value, true), None, "{value:#x}");
    }
    assert_eq!(bitmask(0xFFFF_FFFF, false), None, "all ones of 32 bits");
    assert!(bitmask(0xFFFF_FFFF, true).is_some());
}

#[test]
fn memory_operands_reach_their_offsets() {
    for size in SIZES {
        for rn in [0, 16, reg::SP] {
            for offset in [
                Offset::Scaled(0),
                Offset::Scaled(1),
                Offset::Scaled(4095),
                Offset::Unscaled(-256),
                Offset::Unscaled(255),
                Offset::Unscaled(-1),
                Offset::Index(17),
                Offset::Index(reg::ZR),
            ] {
                for rt in [0, 16, reg::ZR] {
                    agrees(Inst::Store {
                        size,
                        rt,
                        rn,
                        offset,
                    });
                    for (signed, wide) in
                        [(false, false), (false, true), (true, false), (true, true)]
                    {
                        if size == Size::X && !wide {
                            continue;
                        }
                        agrees(Inst::Load {
                            size,
                            signed,
                            wide,
                            rt,
                            rn,
                            offset,
                        });
                    }
                }
            }
        }
    }
}

#[test]
fn fields_out_of_range_are_refused() {
    let bad = [
        Inst::AddImm {
            sub: false,
            flags: false,
            wide: true,
            rd: 0,
            rn: 0,
            imm: 4096,
            high: false,
        },
        Inst::Movz {
            wide: false,
            rd: 0,
            imm: 1,
            hw: 2,
        },
        Inst::Load {
            size: Size::X,
            signed: false,
            wide: true,
            rt: 0,
            rn: 0,
            offset: Offset::Unscaled(256),
        },
        Inst::Store {
            size: Size::B,
            rt: 0,
            rn: 0,
            offset: Offset::Scaled(4096),
        },
        Inst::Bfm {
            signed: false,
            wide: false,
            rd: 0,
            rn: 0,
            immr: 32,
            imms: 0,
        },
        Inst::AddReg {
            sub: false,
            flags: false,
            wide: true,
            rd: 32,
            rn: 0,
            rm: 0,
        },
    ];
    for inst in bad {
        assert_eq!(encode(&[inst], 0), Err(CompileError::Malformed), "{inst:?}");
    }
}

/// The decoded instructions of `insts`, each jump's target checked against
/// its label's place.
fn decodes(insts: &[Inst], labels: usize) -> Vec<(String, String)> {
    let bytes = encode(insts, labels).unwrap();
    let layout = relax(insts, labels).unwrap();
    let mut out = Vec::new();
    for (n, inst) in insts.iter().enumerate() {
        let at = layout.places[n];
        if let Some(label) = inst.target() {
            let place = layout.labels[label.0] as u64;
            let first = tairix_disasm::aarch64::decode(&bytes[at..], at as u64).unwrap();
            if layout.forms[n] == 0 {
                assert_eq!(first.branch_target, Some(place), "{inst:?}");
                out.push((first.mnemonic, first.operands));
            } else {
                // The inverted branch skips the unconditional one.
                assert_eq!(first.branch_target, Some(at as u64 + 8), "{inst:?}");
                let second =
                    tairix_disasm::aarch64::decode(&bytes[at + 4..], at as u64 + 4).unwrap();
                assert_eq!(
                    (second.mnemonic.as_str(), second.branch_target),
                    ("b", Some(place))
                );
                out.push((first.mnemonic, first.operands));
            }
        } else if !matches!(inst, Inst::Bind(_)) {
            let decoded = tairix_disasm::aarch64::decode(&bytes[at..], at as u64).unwrap();
            out.push((decoded.mnemonic, decoded.operands));
        }
    }
    out
}

#[test]
fn branches_reach_their_labels_and_relax_beyond_a_mebibyte() {
    let near = [
        Inst::Bind(Label(0)),
        Inst::BCond(Cond::Lt, Label(1)),
        Inst::Cbz {
            wide: false,
            rt: 3,
            label: Label(0),
        },
        Inst::B(Label(0)),
        Inst::Bind(Label(1)),
        Inst::Ret,
    ];
    let text = decodes(&near, 2);
    assert_eq!(text[0].0, "b.lt");
    assert!(text[1].1.starts_with("w3,"));
    let pad = (1usize << 20) / 4;
    let mut far = Vec::from([
        Inst::BCond(Cond::Ne, Label(0)),
        Inst::Cbz {
            wide: true,
            rt: 9,
            label: Label(0),
        },
    ]);
    far.extend(core::iter::repeat_n(Inst::Ret, pad));
    far.push(Inst::Bind(Label(0)));
    far.push(Inst::BCond(Cond::Hi, Label(1)));
    far.extend(core::iter::repeat_n(Inst::Ret, 4));
    far.push(Inst::Bind(Label(1)));
    let text = decodes(&far, 2);
    assert_eq!(text[0].0, "b.eq", "inverted, over the far branch");
    assert_eq!(text[1].0, "cbnz");
    let last = text.len() - 5;
    assert_eq!(text[last].0, "b.hi", "a near one stays short");
    let bytes = encode(&far, 2).unwrap();
    assert_eq!(bytes.len(), 8 + 8 + pad * 4 + 4 + 16);
}

#[test]
fn a_branch_beyond_every_form_is_refused() {
    assert!(Inst::B(Label(0)).reaches(0, (1 << 27) - 4));
    assert!(!Inst::B(Label(0)).reaches(0, 1 << 27));
    assert!(Inst::BCond(Cond::Eq, Label(0)).reaches(0, -(1 << 20)));
    assert!(!Inst::BCond(Cond::Eq, Label(0)).reaches(0, 1 << 20));
    assert!(
        Inst::BCond(Cond::Eq, Label(0)).reaches(1, 1 << 27),
        "its branch a word on"
    );
    assert!(
        !Inst::Cbz {
            wide: false,
            rt: 0,
            label: Label(0)
        }
        .reaches(0, 2),
        "a word apart"
    );
    assert_eq!(
        encode(&[Inst::B(Label(0))], 1),
        Err(CompileError::Malformed),
        "unbound"
    );
}
