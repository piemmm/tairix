use alloc::format;
use alloc::string::String;

use super::table::{self, Row};
use super::*;
use crate::encoding::form::Kind;
use crate::encoding::layout::Field;
use crate::instruction::*;
use crate::isa::{Feature, Isa};
use crate::reg::{Reg, RegList};
use tairix_fuzzseed::Prng;

/// The 26-bit profile's instruction set: ARMv4 as the SA-110 has it.
const ARMV4: Isa = Isa::of(&[
    Feature::V2,
    Feature::Swp,
    Feature::V3,
    Feature::LongMultiply,
    Feature::V4,
    Feature::ConditionNever,
    Feature::Modes26,
]);

fn text(word: u32, isa: Isa, address: u32) -> String {
    let instruction = decode(word, isa).expect("decodes");
    format!("{}", instruction.text(Set::A32, address))
}

#[test]
fn every_row_spells_each_bit_once() {
    for row in table::all() {
        let layout = &row.layout;
        let mut seen = layout.mask | layout.should_mask;
        assert_eq!(layout.mask & layout.should_mask, 0, "{row:?}");
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

#[test]
fn no_row_is_shadowed_by_an_earlier_less_specific_one() {
    for group in 0..8 {
        let rows = table::rows(group);
        for (i, earlier) in rows.iter().enumerate() {
            for later in &rows[i + 1..] {
                if overlap(earlier, later) {
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
fn each_decoded_word_encodes_to_itself_with_its_should_be_bits_made_canonical() {
    let mut rng = Prng::new(0xA32_0002);
    for _ in 0..200_000 {
        let word = rng.next_u32();
        for isa in [Isa::ALL, ARMV4] {
            let Some(decoded) = decode_detail(word, isa) else {
                continue;
            };
            let instruction = decoded.instruction;
            let Ok(encoded) = encode(&instruction) else {
                // ARMv4 reads a few encodings later architectures gave to
                // other instructions; those have no encoding of their own.
                assert_eq!(isa, ARMV4, "{word:08X}: {instruction:?}");
                assert_ne!(
                    decode(word, Isa::ALL.with(Feature::ConditionNever)),
                    Some(instruction)
                );
                continue;
            };
            assert_eq!(
                decoded.canonical,
                encoded == word,
                "{word:08X} {encoded:08X}"
            );
            let row = table::all()
                .find(|row| row.kind == instruction.op.kind() && row.layout.matches(encoded))
                .expect("a row");
            assert_eq!(
                encoded & !row.layout.should_mask,
                word & !row.layout.should_mask
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
        (0xEE00_0100, "CDP     CP1,0,C0,C0,C0,0"),
        (0xEE01_0F10, "MCR     CP15,0,R0,C1,C0,0"),
        (0xED90_1101, "LDC     CP1,C1,[R0,#4]"),
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
    assert_eq!(text(0xE33F_F003, ARMV4, 0), "TEQP    PC,#3");
    assert_eq!(text(0xF000_0000, ARMV4, 0), "ANDNV   R0,R0,R0");
    assert_eq!(text(0xF1A0_0000, ARMV4, 0), "MOVNV   R0,R0");
    let never = decode(0xF1A0_0000, ARMV4).expect("decodes");
    assert_eq!(never.cond, Condition::Nv);
    assert_eq!(encode(&never), Ok(0xF1A0_0000));
    assert_eq!(decode(0xF1A0_0000, Isa::ALL), None, "unconditional space");
}

#[test]
fn an_instruction_a_profile_lacks_is_undefined_there() {
    assert_eq!(decode(0xE12F_FF1E, ARMV4), None, "BX needs ARMv4T");
    assert_eq!(decode(0xE6EF_0071, ARMV4), None, "UXTB needs ARMv6");
    assert_eq!(decode(0xE30A_0BCD, ARMV4), None, "MOVW needs ARMv6T2");
    let modern = Isa::ALL.without(Feature::Swp);
    assert_eq!(decode(0xE103_0091, modern), None, "ARMv8 removed SWP");
    assert!(decode(0xE103_0091, ARMV4).is_some());
}

#[test]
fn the_unprivileged_halfword_forms_claim_p_clear_with_w_set_only_from_armv6t2() {
    let word = 0xE0F2_10B4; // LDRHT R1,[R2],#4
    assert_eq!(text(word, Isa::ALL, 0), "LDRHT   R1,[R2],#4");
    assert_eq!(decode(word, ARMV4), None);
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
    assert!(check(0xE000_0090, ARMV4), "MUL R0,R0,R0 before ARMv6");
    assert!(!check(0xE000_0090, Isa::ALL), "fine from ARMv6");
    assert!(check(0xE5B1_1004, Isa::ALL), "LDR R1,[R1,#4]!");
    assert!(!check(0xE591_1004, Isa::ALL), "LDR R1,[R1,#4]");
    assert!(check(0xE8B1_0003, Isa::ALL), "LDMIA R1!,{{R0,R1}}");
    assert!(check(0xE8B0_0000, Isa::ALL), "an empty list");
    assert!(check(0xE081_F312, Isa::ALL), "ADD PC,R1,R2,LSL R3");
    assert!(check(0xE135_1001, Isa::ALL), "TEQ with Rd set");
    assert!(!check(0xE33F_F003, ARMV4), "TEQP on a 26-bit core");
    assert!(check(0xE1C0_10D0, Isa::ALL), "LDRD of an odd pair");
    assert!(!check(0xE1C0_20D0, Isa::ALL), "LDRD R2,R3,[R0]");
    assert!(
        check(0xE1F1_10B0, Isa::ALL),
        "LDRH into its base with writeback"
    );
    assert!(check(0xE182_2F91, Isa::ALL), "STREX status is the base");
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
    for row in table::all() {
        for _ in 0..32 {
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
    assert!(kinds.iter().all(|kind| kinds_seen.contains(kind)));
}
