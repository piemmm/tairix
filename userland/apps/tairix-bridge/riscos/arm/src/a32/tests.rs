use alloc::format;
use alloc::string::String;

use super::table::{self, Row};
use super::*;
use crate::encoding::form::Kind;
use crate::encoding::layout::Field;
use crate::instruction::*;
use crate::isa::{Feature, Isa};
use crate::profile::BIT26;
use crate::reg::{Reg, RegList};
use tairix_fuzzseed::Prng;

/// The 26-bit profile's instruction set, ARMv4 as the SA-110 has it,
/// without the FPA claiming coprocessors 1 and 2.
fn armv4() -> Isa {
    BIT26.isa.without(Feature::Fpa)
}

fn text(word: u32, isa: Isa, address: u32) -> String {
    let instruction = decode(word, isa).expect("decodes");
    format!("{}", instruction.text(Set::A32, address))
}

#[test]
fn every_row_spells_each_bit_once() {
    for row in table::all() {
        let layout = &row.layout;
        let mut seen = layout.mask | layout.should_mask | layout.ignored;
        assert_eq!(layout.mask & layout.should_mask, 0, "{row:?}");
        assert_eq!((layout.mask | layout.should_mask) & layout.ignored, 0);
        for (n, &field) in layout.fields.iter().enumerate() {
            assert_eq!(seen & field, 0, "field {n} overlaps in {row:?}");
            seen |= field;
        }
        assert_eq!(seen, u32::MAX, "{row:?}");
        assert_eq!(layout.bits & !layout.mask, 0);
        assert_eq!(layout.should_bits & !layout.should_mask, 0);
    }
}

/// Whether two rows can match one word, conditional and unconditional rows
/// never doing so.
fn overlap(a: &Row, b: &Row) -> bool {
    let both = a.layout.mask & b.layout.mask;
    (a.layout.bits ^ b.layout.bits) & both == 0 && a.unconditional() == b.unconditional()
}

/// Whether `earlier` is a coprocessor's own encoding and `later` one of the
/// generic forms every coprocessor number shares: the coprocessor claims its
/// number's words from them, however the two fix the bits besides.
fn claims_its_number(earlier: &Row, later: &Row) -> bool {
    let number = later.layout.field(Field::Cp);
    number != 0 && earlier.layout.mask & number == number
}

#[test]
fn no_row_is_shadowed_by_an_earlier_less_specific_one() {
    for rows in table::GROUPS {
        for (i, earlier) in rows.iter().enumerate() {
            for later in &rows[i + 1..] {
                if overlap(earlier, later) && !claims_its_number(earlier, later) {
                    assert_eq!(
                        later.layout.mask & !earlier.layout.mask,
                        0,
                        "{earlier:?} shadows the more specific {later:?}"
                    );
                }
            }
        }
    }
}

/// Whether every word `later` matches `earlier` matches too.
fn covers(earlier: &Row, later: &Row) -> bool {
    earlier.layout.mask & !later.layout.mask == 0 && overlap(earlier, later)
}

#[test]
fn no_floating_point_or_simd_row_is_wholly_covered_by_an_earlier_one() {
    for rows in table::SIMDFP {
        for (i, earlier) in rows.iter().enumerate() {
            for later in &rows[i + 1..] {
                assert!(!covers(earlier, later), "{earlier:?} covers {later:?}");
            }
        }
    }
}

#[test]
fn the_floating_point_and_simd_rows_meet_the_others_only_on_the_numbers_they_claim() {
    for (main, vector) in table::GROUPS.iter().zip(table::SIMDFP) {
        for row in vector {
            // Every coprocessor number the row's fixed bits allow is claimed.
            let fixed = row.layout.mask & 0xF00;
            let claimed = (0..16u32)
                .filter(|number| (number << 8) & fixed == row.layout.bits & fixed)
                .all(|number| Isa::ALL.claims_coprocessor(number));
            for other in main.iter().filter(|other| overlap(other, row)) {
                assert!(
                    other.kind.generic_coprocessor() && claimed,
                    "{row:?} meets {other:?}"
                );
            }
        }
    }
}

#[test]
fn every_row_names_bits_27_to_25_and_sits_in_their_group() {
    for group in 0..8u32 {
        for row in table::rows(group as usize) {
            assert_eq!(row.layout.mask & 0x0E00_0000, 0x0E00_0000, "{row:?}");
            assert_eq!((row.layout.bits >> 25) & 7, group, "{row:?}");
        }
    }
}

/// A word in `row`'s encoding with its fields drawn at random.
fn drawn(rng: &mut Prng, row: &Row) -> u32 {
    let free = !(row.layout.mask | row.layout.should_mask);
    row.layout.bits | row.layout.should_bits | (rng.next_u32() & free)
}

#[test]
fn every_row_decodes_its_own_words_or_a_row_that_refines_it() {
    let mut rng = Prng::new(0xA32_0001);
    for row in table::all() {
        let mut decoded = 0;
        for _ in 0..64 {
            let mut word = drawn(&mut rng, row);
            if !row.unconditional() && word >> 28 == 0xF {
                word &= 0xEFFF_FFFF;
            }
            if let Some(instruction) = decode(word, Isa::ALL) {
                decoded += 1;
                let kind = instruction.op.kind();
                if kind != row.kind {
                    let claimed = table::all()
                        .find(|other| other.kind == kind && other.layout.matches(word))
                        .expect("a row decoded it");
                    assert!(
                        overlap(claimed, row),
                        "{word:08X}: {row:?} lost to an unrelated {claimed:?}"
                    );
                }
            }
        }
        assert!(decoded > 0, "no word of {row:?} decodes");
    }
}

#[test]
fn each_decoded_word_encodes_to_itself_made_canonical() {
    let mut rng = Prng::new(0xA32_0002);
    for _ in 0..200_000 {
        let word = rng.next_u32();
        for isa in [Isa::ALL, armv4()] {
            let Some(decoded) = decode_detail(word, isa) else {
                continue;
            };
            let instruction = decoded.instruction;
            let Ok(encoded) = encode(&instruction) else {
                // ARMv4 reads a few encodings later architectures gave to
                // other instructions; those have no encoding of their own.
                assert_eq!(isa, armv4(), "{word:08X}: {instruction:?}");
                assert_ne!(
                    decode(word, Isa::ALL.with(Feature::ConditionNever)),
                    Some(instruction)
                );
                continue;
            };
            let row = table::all()
                .find(|row| {
                    row.kind == instruction.op.kind()
                        && isa.has(row.since)
                        && row.layout.matches(word)
                })
                .expect("a row");
            let layout = &row.layout;
            let canonical = (word & !(layout.should_mask | layout.ignored)) | layout.should_bits;
            assert_eq!(encoded, canonical, "{word:08X} {encoded:08X}");
            assert_eq!(
                decoded.canonical,
                word & layout.should_mask == layout.should_bits,
                "{word:08X}"
            );
        }
    }
}

#[test]
fn every_kind_has_rows_that_agree_on_their_conditionality() {
    for row in table::all() {
        let unconditional = table::unconditional(row.kind);
        for other in table::all().filter(|other| other.kind == row.kind) {
            assert_eq!(
                other.unconditional(),
                unconditional,
                "{row:?} and {other:?} disagree"
            );
        }
    }
}

#[test]
fn the_condition_field_is_a_field_of_every_conditional_row() {
    for row in table::all() {
        let fixed = row.layout.mask >> 28 == 0xF;
        if !fixed {
            assert_eq!(
                row.layout.field(Field::Cond),
                0xF000_0000,
                "{row:?} has no condition field"
            );
        }
    }
}

#[test]
fn instructions_disassemble_as_basic_and_the_debugger_spell_them() {
    let cases: &[(u32, &str)] = &[
        (0xE3A0_0001, "MOV     R0,#1"),
        (0xE081_0002, "ADD     R0,R1,R2"),
        (0xE1B0_F00E, "MOVS    PC,R14"),
        (0xE59F_1004, "LDR     R1,[PC,#4]"),
        (0x05D2_1003, "LDREQB  R1,[R2,#3]"),
        (0xE51F_1004, "LDR     R1,[PC,#-4]"),
        (0xE792_1103, "LDR     R1,[R2,R3,LSL #2]"),
        (0xE4B2_1004, "LDRT    R1,[R2],#4"),
        (0xE5B2_1004, "LDR     R1,[R2,#4]!"),
        (0xE92D_400F, "STMFD   R13!,{R0-R3,R14}"),
        (0xE8BD_800F, "LDMFD   R13!,{R0-R3,PC}"),
        (0xE8D0_8003, "LDMIA   R0,{R0,R1,PC}^"),
        (0xEF00_0011, "SWI     &000011"),
        (0xE12F_FF1E, "BX      R14"),
        (0xE000_0291, "MUL     R0,R1,R2"),
        (0xE021_3392, "MLA     R1,R2,R3,R3"),
        (0xE081_0392, "UMULL   R0,R1,R2,R3"),
        (0xE6EF_0071, "UXTB    R0,R1"),
        (0xE6AF_0471, "SXTB    R0,R1,ROR #8"),
        (0xE7DF_0F1F, "BFC     R0,#30,#2"),
        (0xF57F_F05F, "DMB     SY"),
        (0xF57F_F040, "SSBB"),
        (0xF57F_F044, "PSSBB"),
        (0xF57F_F04F, "DSB     SY"),
        (0xE320_F003, "WFI"),
        (0xE7F0_00F0, "UDF     #0"),
        (0xE1A0_0000, "MOV     R0,R0"),
        (0xE10F_0000, "MRS     R0,CPSR"),
        (0xE129_F000, "MSR     CPSR_fc,R0"),
        (0xE1A0_1102, "MOV     R1,R2,LSL #2"),
        (0xE1A0_1062, "MOV     R1,R2,RRX"),
        (0xE1A0_1312, "MOV     R1,R2,LSL R3"),
        (0xE1A0_1022, "MOV     R1,R2,LSR #32"),
        (0xE1D2_00B4, "LDRH    R0,[R2,#4]"),
        (0xE1C0_20D0, "LDRD    R2,R3,[R0]"),
        (0xEE00_0300, "CDP     CP3,0,C0,C0,C0,0"),
        (0xEE01_0F10, "MCR     CP15,0,R0,C1,C0,0"),
        (0xED90_1301, "LDC     CP3,C1,[R0,#4]"),
        (0xE103_0091, "SWP     R0,R1,[R3]"),
        (0xE191_0F9F, "LDREX   R0,[R1]"),
        (0xE182_0F91, "STREX   R0,R1,[R2]"),
        (0xE107_0052, "QADD    R0,R2,R7"),
        (0xE6A1_0F32, "SSAT16  R0,#2,R2"),
        (0xE30A_0BCD, "MOVW    R0,#&ABCD"),
        (0xE16F_0F11, "CLZ     R0,R1"),
        (0xF102_0013, "CPS     #&13"),
        (0xF10C_0080, "CPSID   I"),
        (0xE120_0173, "BKPT    #&13"),
    ];
    for &(word, expected) in cases {
        assert_eq!(text(word, Isa::ALL, 0x8000), expected, "{word:08X}");
        let instruction = decode(word, Isa::ALL).expect("decodes");
        assert_eq!(encode(&instruction), Ok(word), "{expected}");
    }
}

#[test]
fn the_fpa_instructions_disassemble_as_basic_spells_them() {
    let cases: &[(u32, &str)] = &[
        (0xEE01_0109, "ADFS    F0,F1,#1"),
        (0xEE42_31CE, "DVFDM   F3,F2,#0.5"),
        (0xEEC1_1180, "POLD    F1,F1,F0"),
        (0xEE00_C188, "MVFD    F4,#0"),
        (0xEE10_F168, "MNFSZ   F7,#0"),
        (0xEE90_8120, "COSSP   F0,F0"),
        (0xEE30_81E6, "RNDDZ   F0,F6"),
        (0xEE00_0110, "FLTS    F0,R0"),
        (0xEE20_0110, "WFS     R0"),
        (0xEE30_0110, "RFS     R0"),
        (0xEE90_F111, "CMF     F0,F1"),
        (0xEEF0_F111, "CNFE    F0,F1"),
        (0xEDB1_8102, "LDFD    F0,[R1,#8]!"),
        (0xEC21_0101, "STFS    F0,[R1],#-4"),
        (0xEDC1_D103, "STFP    F5,[R1,#&C]"),
        (0xED9F_2101, "LDFS    F2,[PC,#4]"),
        (0xED2D_420C, "SFM     F4,4,[R13,#-&30]!"),
        (0xECBD_4203, "LFM     F4,4,[R13],#&C"),
    ];
    for &(word, expected) in cases {
        assert_eq!(text(word, Isa::ALL, 0x8000), expected, "{word:08X}");
        let instruction = decode(word, Isa::ALL).expect("decodes");
        assert_eq!(encode(&instruction), Ok(word), "{expected}");
    }
}

#[test]
fn the_fpa_claims_its_coprocessors_words_and_leaves_the_rest_generic() {
    let cases: &[(u32, &str)] = &[
        // FPEmulator's: the flags to a register, and at the base unindexed.
        (0xEE90_3111, "MRC     CP1,4,R3,C0,C1,0"),
        (0xECD1_1103, "LDCL    CP1,C1,[R1],{3}"),
        (0xEC80_4200, "STC     CP2,C4,[R0],{0}"),
        // Not FPEmulator's: precision 11, dyadic opcode 13, WFC, and
        // another coprocessor.
        (0xEE08_0180, "CDP     CP1,0,C0,C8,C0,4"),
        (0xEED0_0100, "CDP     CP1,13,C0,C0,C0,0"),
        (0xEE40_8110, "MCR     CP1,2,R8,C0,C0,0"),
        (0xED91_7300, "LDC     CP3,C7,[R1,#0]"),
    ];
    for &(word, expected) in cases {
        assert_eq!(text(word, Isa::ALL, 0x8000), expected, "{word:08X}");
    }
    let claimed = |word| {
        matches!(
            decode(word, Isa::ALL).map(|instruction| instruction.op),
            Some(Op::FpaCompare { .. } | Op::FpaTransfer { .. } | Op::FpaMultiple { .. })
        )
    };
    assert!(claimed(0xEE90_3111), "the flags to R3");
    assert!(claimed(0xECD1_1103), "unindexed, which MCRR's space holds");
    assert!(!claimed(0xEE08_0180), "precision 11");
    let generic = decode(0xEDB1_8102, armv4()).map(|instruction| instruction.op);
    assert!(
        matches!(generic, Some(Op::CoprocessorTransfer { .. })),
        "a core without the FPA"
    );
    assert!(
        matches!(
            decode(0xFD91_0100, Isa::ALL).map(|instruction| instruction.op),
            Some(Op::CoprocessorTransfer { .. })
        ),
        "the unconditional space is never the FPA's"
    );
    let never = decode(0xFD91_0100, armv4().with(Feature::Fpa)).expect("decodes");
    assert_eq!(never.cond, Condition::Nv);
    assert!(matches!(never.op, Op::FpaTransfer { .. }), "NV is never");
}

#[test]
fn bits_fpemulator_ignores_decode_alike_and_encode_clear() {
    // FIX R3,F2 with its unused Fn field, and WFS R0 with every field it
    // ignores set; precision 10 in both.
    for (word, canonical) in [(0xEE1E_3112, 0xEE18_3112), (0xEE2F_017F, 0xEE28_0110)] {
        let decoded = decode_detail(word, Isa::ALL).expect("decodes");
        assert!(decoded.canonical, "{word:08X} is no UNPREDICTABLE encoding");
        assert_eq!(encode(&decoded.instruction), Ok(canonical), "{word:08X}");
    }
}

#[test]
fn a_mnemonic_as_long_as_the_column_still_leaves_a_space() {
    assert_eq!(text(0x0143_02E7, Isa::ALL, 0), "SMLALTTEQ R0,R3,R7,R2");
    assert_eq!(text(0x01D1_0E9F, Isa::ALL, 0), "LDAEXBEQ R0,[R1]");
}

#[test]
fn a_branch_shows_its_target() {
    assert_eq!(text(0xEA00_0000, Isa::ALL, 0x8000), "B       &00008008");
    assert_eq!(text(0xEBFF_FFFE, Isa::ALL, 0x8000), "BL      &00008000");
    assert_eq!(text(0xFA00_0000, Isa::ALL, 0x8000), "BLX     &00008008");
    assert_eq!(text(0xFB00_0000, Isa::ALL, 0x8000), "BLX     &0000800A");
}

#[test]
fn the_comparisons_p_forms_and_the_never_condition_read_on_armv4() {
    assert_eq!(text(0xE33F_F003, armv4(), 0), "TEQP    PC,#3");
    assert_eq!(text(0xF000_0000, armv4(), 0), "ANDNV   R0,R0,R0");
    assert_eq!(text(0xF1A0_0000, armv4(), 0), "MOVNV   R0,R0");
    let never = decode(0xF1A0_0000, armv4()).expect("decodes");
    assert_eq!(never.cond, Condition::Nv);
    assert_eq!(encode(&never), Ok(0xF1A0_0000));
    assert_eq!(decode(0xF1A0_0000, Isa::ALL), None, "unconditional space");
}

#[test]
fn an_instruction_a_profile_lacks_is_undefined_there() {
    assert_eq!(decode(0xE12F_FF1E, armv4()), None, "BX needs ARMv4T");
    assert_eq!(decode(0xE6EF_0071, armv4()), None, "UXTB needs ARMv6");
    assert_eq!(decode(0xE30A_0BCD, armv4()), None, "MOVW needs ARMv6T2");
    let modern = Isa::ALL.without(Feature::Swp);
    assert_eq!(decode(0xE103_0091, modern), None, "ARMv8 removed SWP");
    assert!(decode(0xE103_0091, armv4()).is_some());
}

#[test]
fn the_monitor_and_hypervisor_calls_decode_only_with_their_extensions() {
    let cases: &[(u32, &str, Feature)] = &[
        (0xE160_0071, "SMC     #1", Feature::Security),
        (0x0160_007F, "SMCEQ   #&F", Feature::Security),
        (0xE14F_FF7F, "HVC     #&FFFF", Feature::Virtualization),
        (0xE160_006E, "ERET", Feature::Virtualization),
    ];
    for &(word, expected, feature) in cases {
        assert_eq!(text(word, Isa::ALL, 0), expected, "{word:08X}");
        let instruction = decode(word, Isa::ALL).expect("decodes");
        assert_eq!(encode(&instruction), Ok(word), "{expected}");
        assert_eq!(decode(word, Isa::ALL.without(feature)), None, "{expected}");
    }
    assert_eq!(decode(0x0140_0071, Isa::ALL), None, "HVC is unconditional");
}

#[test]
fn the_unprivileged_halfword_forms_claim_p_clear_with_w_set_only_from_armv6t2() {
    let word = 0xE0F2_10B4; // LDRHT R1,[R2],#4
    assert_eq!(text(word, Isa::ALL, 0), "LDRHT   R1,[R2],#4");
    assert_eq!(decode(word, armv4()), None);
}

#[test]
fn should_be_bits_out_of_place_decode_but_are_not_canonical() {
    let word = 0xE12F_0F1E; // BX with SBO bits 19:16 clear
    let decoded = decode_detail(word, Isa::ALL).expect("decodes as BX");
    assert!(!decoded.canonical);
    assert_eq!(encode(&decoded.instruction), Ok(0xE12F_FF1E));
}

#[test]
fn operands_no_encoding_holds_are_refused() {
    let mul_with_accumulator = Instruction {
        cond: Condition::Al,
        op: Op::Multiply {
            op: MultiplyOp::Mul,
            s: false,
            x: false,
            y: false,
            rd: Reg::field(0),
            rn: Reg::field(1),
            rm: Reg::field(2),
            ra: Some(Reg::field(3)),
        },
    };
    assert_eq!(encode(&mul_with_accumulator), Err(EncodeError::Unencodable));
    let bad_shift = Instruction {
        cond: Condition::Al,
        op: Op::Move {
            op: MoveOp::Mov,
            s: false,
            rd: Reg::field(0),
            operand: Operand::Register {
                rm: Reg::field(1),
                shift: Shift::Lsl(32),
            },
        },
    };
    assert_eq!(encode(&bad_shift), Err(EncodeError::Range));
    let branch = |op, offset| Instruction {
        cond: if op == BranchOp::Blx {
            Condition::Nv
        } else {
            Condition::Al
        },
        op: Op::Branch { op, offset },
    };
    assert_eq!(
        encode(&branch(BranchOp::B, 1 << 25)),
        Err(EncodeError::Range)
    );
    assert_eq!(
        encode(&branch(BranchOp::Bl, -(1 << 25) - 4)),
        Err(EncodeError::Range)
    );
    assert!(encode(&branch(BranchOp::B, (1 << 25) - 4)).is_ok());
    assert!(encode(&branch(BranchOp::B, -(1 << 25))).is_ok());
    for (op, offset) in [(BranchOp::B, 2), (BranchOp::Bl, 1), (BranchOp::Blx, 1)] {
        assert_eq!(
            encode(&branch(op, offset)),
            Err(EncodeError::Unencodable),
            "{op:?} misaligned by {offset}"
        );
    }
    assert!(
        encode(&branch(BranchOp::Blx, 2)).is_ok(),
        "BLX to a halfword"
    );
    let expanded = Instruction {
        cond: Condition::Al,
        op: Op::Move {
            op: MoveOp::Mov,
            s: false,
            rd: Reg::field(0),
            operand: Operand::Expanded(Expanded(0x100)),
        },
    };
    assert_eq!(
        encode(&expanded),
        Err(EncodeError::Unencodable),
        "a T32 immediate"
    );
    let conditional_pld = Instruction {
        cond: Condition::Eq,
        op: Op::Preload {
            op: PreloadOp::Pld,
            rn: Reg::field(0),
            up: true,
            offset: Offset::Immediate(0),
        },
    };
    assert_eq!(encode(&conditional_pld), Err(EncodeError::Unencodable));
    let wide_offset = Instruction {
        cond: Condition::Al,
        op: Op::Transfer {
            op: TransferOp::Ldrh,
            rt: Reg::field(0),
            rn: Reg::field(1),
            up: true,
            offset: Offset::Immediate(256),
            index: Index::Offset,
        },
    };
    assert_eq!(encode(&wide_offset), Err(EncodeError::Range));
}

#[test]
fn an_immediate_encodes_with_the_least_rotation() {
    assert_eq!(
        Immediate::encode(0xFF),
        Some(Immediate {
            value: 0xFF,
            rotation: 0
        })
    );
    assert_eq!(
        Immediate::encode(0xFF00_0000),
        Some(Immediate {
            value: 0xFF,
            rotation: 4
        })
    );
    assert_eq!(
        Immediate::encode(0x0000_0204),
        Some(Immediate {
            value: 0x81,
            rotation: 15
        })
    );
    assert_eq!(Immediate::encode(0x101), None);
    assert_eq!(Immediate::encode(0x102), None, "an odd rotation");
    for rotation in 0..16 {
        for value in [0u8, 1, 0x80, 0xFF] {
            let immediate = Immediate { value, rotation };
            let least = Immediate::encode(immediate.value()).expect("encodable");
            assert_eq!(least.value(), immediate.value());
            assert!(least.rotation <= rotation || value == 0);
        }
    }
}

#[test]
fn every_shift_round_trips_through_its_fields() {
    for kind in 0..4 {
        for amount in 0..32 {
            let shift = Shift::decode(kind, amount);
            assert_eq!(shift.encode(), Some((kind, amount)));
        }
    }
    for bad in [
        Shift::Lsl(32),
        Shift::Lsr(0),
        Shift::Asr(33),
        Shift::Ror(0),
        Shift::Ror(32),
    ] {
        assert_eq!(bad.encode(), None);
    }
}

#[test]
fn the_architecture_s_unpredictable_encodings_are_reported() {
    let check = |word: u32, isa: Isa| unpredictable(&decode(word, isa).expect("decodes"), isa);
    assert!(check(0xE000_0090, armv4()), "MUL R0,R0,R0 before ARMv6");
    assert!(!check(0xE000_0090, Isa::ALL), "fine from ARMv6");
    assert!(check(0xE5B1_1004, Isa::ALL), "LDR R1,[R1,#4]!");
    assert!(!check(0xE591_1004, Isa::ALL), "LDR R1,[R1,#4]");
    assert!(check(0xE8B1_0003, Isa::ALL), "LDMIA R1!,{{R0,R1}}");
    assert!(check(0xE8B0_0000, Isa::ALL), "an empty list");
    assert!(check(0xE081_F312, Isa::ALL), "ADD PC,R1,R2,LSL R3");
    assert!(check(0xE135_1001, Isa::ALL), "TEQ with Rd set");
    assert!(!check(0xE33F_F003, armv4()), "TEQP on a 26-bit core");
    assert!(check(0xE1C0_10D0, Isa::ALL), "LDRD of an odd pair");
    assert!(!check(0xE1C0_20D0, Isa::ALL), "LDRD R2,R3,[R0]");
    assert!(
        check(0xE1F1_10B0, Isa::ALL),
        "LDRH into its base with writeback"
    );
    assert!(check(0xE182_2F91, Isa::ALL), "STREX status is the base");
    assert!(check(0xE12F_FF2F, Isa::ALL), "BXJ PC");
    assert!(check(0xE12F_FF3F, Isa::ALL), "BLX PC");
    assert!(!check(0xE12F_FF1F, Isa::ALL), "BX PC from ARMv7");
}

#[test]
fn a_register_list_reads_in_ranges() {
    assert_eq!(format!("{}", RegList(0x400F)), "{R0-R3,R14}");
    assert_eq!(format!("{}", RegList(0x8003)), "{R0,R1,PC}");
    assert_eq!(format!("{}", RegList(0xFFFF)), "{R0-PC}");
    assert_eq!(format!("{}", RegList(0)), "{}");
}

#[test]
fn every_row_s_kind_builds_from_its_own_fields_and_the_encoder_finds_a_row_for_each() {
    let mut rng = Prng::new(0xA32_0003);
    let mut kinds_seen: alloc::vec::Vec<Kind> = alloc::vec::Vec::new();
    // The rarest forms, the quadword crypto operations, build from one
    // random word in sixteen: this many draws miss one with odds of 1e-14.
    for row in table::all() {
        for _ in 0..512 {
            let mut word = drawn(&mut rng, row);
            if !row.unconditional() && word >> 28 == 0xF {
                word &= 0xEFFF_FFFF;
            }
            let fields = row.layout.extract(word);
            if let Some(op) = row.kind.build(&row.layout, &fields) {
                assert_eq!(op.kind(), row.kind);
                if !kinds_seen.contains(&row.kind) {
                    kinds_seen.push(row.kind);
                }
            }
        }
    }
    let mut kinds: alloc::vec::Vec<Kind> = table::all().map(|row| row.kind).collect();
    kinds.dedup();
    let missing: alloc::vec::Vec<&Kind> = kinds
        .iter()
        .filter(|kind| !kinds_seen.contains(kind))
        .collect();
    assert!(missing.is_empty(), "{missing:?}");
}

#[test]
fn floating_point_and_simd_instructions_disassemble_in_ual() {
    let cases: &[(u32, &str)] = &[
        (0xEE30_0A81, "VADD.F32 S0,S1,S2"),
        (0xEE01_0B02, "VMLA.F64 D0,D1,D2"),
        (0xEEB7_0A00, "VMOV.F32 S0,#1.0"),
        (0xEEBD_0BC1, "VCVT.S32.F64 S0,D1"),
        (0xEEB3_0A60, "VCVTB.F16.F32 S0,S1"),
        (0xECB0_0B08, "VLDMIA  R0!,{D0-D3}"),
        (0xED2D_8B10, "VPUSH   {D8-D15}"),
        (0xECBD_8A10, "VPOP    {S16-S31}"),
        (0xED42_0A02, "VSTR    S1,[R2,#-8]"),
        (0xEC51_0B12, "VMOV    R0,R1,D2"),
        (0xEE20_1B10, "VMOV.32 D0[1],R1"),
        (0xEE73_2B30, "VMOV.S8 R2,D3[5]"),
        (0xEEC0_1B10, "VDUP.8  D0,R1"),
        (0xEEF1_FA10, "VMRS    APSR_nzcv,FPSCR"),
        (0xF420_022D, "VLD1.8  {D0-D3},[R0:128]!"),
        (0xF4A1_0542, "VLD2.16 {D0[1],D1[1]},[R1],R2"),
        (0xF222_0844, "VADD.I32 Q0,Q1,Q2"),
        (0xF202_0401, "VSHL.S8 D0,D1,D2"),
        (0xF221_0111, "VMOV    D0,D1"),
        (0xF291_0C4A, "VQDMULH.S16 D0,D1,D2[1]"),
        (0xF28D_0812, "VSHRN.I16 D0,Q1,#3"),
        (0xF2B2_0344, "VEXT.8  Q0,Q1,Q2,#3"),
        (0xF3B1_0A04, "VTBL.8  D0,{D1-D3},D4"),
        (0xF3BA_0C41, "VDUP.16 Q0,D1[2]"),
        (0xF382_0E3A, "VMOV.I64 D0,#&FF00FF00FF00FF00"),
        (0xF387_035F, "VORR.I32 Q0,#&FF00"),
        (0xF3B0_0302, "AESE.8  Q0,Q1"),
        (0xF302_0C44, "SHA256H.32 Q0,Q1,Q2"),
        (0xF2A1_0E02, "VMULL.P64 Q0,D1,D2"),
        (0xFE20_0A81, "VSELGE.F32 S0,S1,S2"),
        (0xFEBC_0AE0, "VCVTA.S32.F32 S0,S1"),
        (0xEEB6_0BC1, "VRINTZ.F64 D0,D1"),
    ];
    for &(word, expected) in cases {
        assert_eq!(text(word, Isa::ALL, 0x8000), expected, "{word:08X}");
        let instruction = decode(word, Isa::ALL).expect("decodes");
        assert_eq!(encode(&instruction), Ok(word), "{expected}");
    }
}

#[test]
fn a_coprocessor_form_on_vfp_s_numbers_is_undefined_where_vfp_claims_them() {
    // CDP and LDC on coprocessor 10: VFP's, so no generic form decodes them,
    // but a core without VFP reads them as the generic forms.
    for word in [0xEE00_0A00u32, 0xED90_0A00 | 1 << 22] {
        let without = Isa::ALL.without(Feature::Vfp);
        assert!(
            decode(word, without).is_some(),
            "{word:08X} on a core without VFP"
        );
    }
    assert_eq!(decode(0xEEB9_0A40, Isa::ALL), None, "no VFP encoding");
    assert!(decode(0xEEB9_0A40, Isa::ALL.without(Feature::Vfp)).is_some());
}

#[test]
fn an_advanced_simd_immediate_s_operation_is_its_cmode_s() {
    use crate::instruction::SimdImmediateOp as O;
    assert_eq!(O::of(0x01), Some(O::Vorr));
    assert_eq!(O::of(0x00), Some(O::Vmov));
    assert_eq!(O::of(0x11), Some(O::Vbic));
    assert_eq!(O::of(0x10), Some(O::Vmvn));
    assert_eq!(O::of(0x1E), Some(O::Vmov), "the 64-bit form");
    assert_eq!(O::of(0x1F), None, "reserved");
}
