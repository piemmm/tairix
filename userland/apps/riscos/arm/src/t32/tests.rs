use alloc::format;
use alloc::string::String;

use super::form::{self, Shape};
use super::table::{self, Row};
use super::*;
use crate::encoding::layout::Field;
use crate::instruction::*;
use crate::reg::Reg;
use tairix_fuzzseed::Prng;

const OUTSIDE: Position = Position::Outside;

fn text_at(halves: &[u16], position: Position, address: u32) -> String {
    let second = halves.get(1).copied().unwrap_or(0);
    let decoded = decode(halves[0], second, Isa::ALL, position).expect("decodes");
    assert_eq!(decoded.wide, halves.len() == 2, "{halves:04X?}");
    format!("{}", decoded.instruction.text(Set::T32, address))
}

fn encoding(halves: &[u16]) -> Encoding {
    match *halves {
        [first] => Encoding::Narrow(first),
        [first, second] => Encoding::Wide(first, second),
        _ => unreachable!("one or two halfwords"),
    }
}

/// The halfwords' bits as a row spells them: a 16-bit instruction alone, a
/// 32-bit one's first halfword above its second.
fn bits(encoding: Encoding) -> u32 {
    match encoding {
        Encoding::Narrow(first) => u32::from(first),
        Encoding::Wide(first, second) => (u32::from(first) << 16) | u32::from(second),
    }
}

fn halves(word: u32, wide: bool) -> (u16, u16) {
    if wide {
        (halfword(word >> 16), halfword(word))
    } else {
        (halfword(word), 0)
    }
}

#[test]
fn every_row_spells_each_bit_once() {
    for row in table::all() {
        let layout = &row.layout;
        let width = if row.wide { u32::MAX } else { 0xFFFF };
        let mut seen = layout.mask | layout.should_mask;
        assert_eq!(layout.mask & layout.should_mask, 0, "{row:?}");
        for (n, &field) in layout.fields.iter().enumerate() {
            assert_eq!(seen & field, 0, "field {n} overlaps in {row:?}");
            seen |= field;
        }
        assert_eq!(seen, width, "{row:?}");
        assert_eq!(layout.bits & !layout.mask, 0);
        assert_eq!(layout.should_bits & !layout.should_mask, 0);
    }
}

#[test]
fn every_row_sits_in_the_group_its_fixed_bits_choose() {
    for (group, rows) in table::NARROW.iter().enumerate() {
        for row in *rows {
            assert_eq!(row.layout.mask & 0xE000, 0xE000, "{row:?}");
            assert_eq!(usize::try_from(row.layout.bits >> 13), Ok(group), "{row:?}");
            assert!(!is_wide(halfword(row.layout.bits)), "{row:?}");
        }
    }
    for (group, rows) in table::WIDE.iter().enumerate() {
        for row in *rows {
            let first = row.layout.bits >> 16;
            let deciding = if first >> 11 == 0b11110 {
                0xF800_8000
            } else {
                0xFE00_0000
            };
            assert_eq!(row.layout.mask & deciding, deciding, "{row:?}");
            assert!(is_wide(halfword(first)), "{row:?}");
            assert_eq!(table::wide_group(row.layout.bits), group, "{row:?}");
        }
    }
}

/// Whether two rows can match one word.
fn overlap(a: &Row, b: &Row) -> bool {
    let both = a.layout.mask & b.layout.mask;
    (a.layout.bits ^ b.layout.bits) & both == 0
}

#[test]
fn no_row_is_wholly_covered_by_an_earlier_one() {
    for rows in table::NARROW.iter().chain(&table::WIDE) {
        for (i, earlier) in rows.iter().enumerate() {
            for later in &rows[i + 1..] {
                let covers = overlap(earlier, later)
                    && earlier.layout.mask & !later.layout.mask == 0
                    && earlier.since == later.since;
                assert!(!covers, "{earlier:?} covers every word of {later:?}");
            }
        }
    }
}

/// A word in `row`'s encoding with its free bits drawn at random.
fn drawn(rng: &mut Prng, row: &Row) -> u32 {
    let width = if row.wide { u32::MAX } else { 0xFFFF };
    let free = width & !(row.layout.mask | row.layout.should_mask);
    row.layout.bits | row.layout.should_bits | (rng.next_u32() & free)
}

#[test]
fn every_row_decodes_some_of_its_own_words_and_loses_the_rest_only_to_an_overlap() {
    let mut rng = Prng::new(0x732_0001);
    for row in table::all() {
        let mut own = 0;
        for _ in 0..64 {
            let word = drawn(&mut rng, row);
            let (first, second) = halves(word, row.wide);
            let Some(detail) = decode_detail(first, second, Isa::ALL, OUTSIDE) else {
                continue;
            };
            if core::ptr::eq(detail.row, row) {
                own += 1;
            } else {
                assert!(
                    overlap(detail.row, row),
                    "{word:08X}: {row:?} lost to an unrelated {:?}",
                    detail.row
                );
            }
        }
        assert!(own > 0, "no word of {row:?} decodes by it");
    }
}

/// A position at random: mostly outside a block, else inside or last under
/// any condition.
fn position(rng: &mut Prng) -> Position {
    let draw = rng.next_u32();
    let cond = Condition::field(draw >> 8);
    match draw % 4 {
        0 => Position::Inside(cond),
        1 => Position::Last(cond),
        _ => Position::Outside,
    }
}

#[test]
fn each_floating_point_and_simd_instruction_encodes_back_as_a32_s_encoding() {
    let mut rng = Prng::new(0x732_0005);
    let prefixes = [
        0xEF00_0000u32,
        0xFF00_0000,
        0xF900_0000,
        0xEC00_0A00,
        0xED00_0A00,
        0xEE00_0A00,
        0xFE00_0A00,
    ];
    let mut decoded = 0;
    for _ in 0..200_000 {
        let draw = rng.next_u32();
        let prefix = prefixes[draw as usize % prefixes.len()];
        let low = rng.next_u32();
        let word = match prefix {
            // Element transfers clear bit 20, which loads of a core register set.
            0xF900_0000 => prefix | (low & 0x00EF_FFFF),
            _ if prefix & 0xF00 == 0 => prefix | (low & 0x00FF_FFFF),
            _ => prefix | (low & 0x00FF_F1FF),
        };
        let (first, second) = (halfword(word >> 16), halfword(word));
        let position = position(&mut rng);
        let Some(found) = decode(first, second, Isa::ALL, position) else {
            continue;
        };
        decoded += 1;
        assert!(matches!(found.instruction.op, Op::SimdFp(_)), "{word:08X}");
        let encoded = encode(&found.instruction, position, Size::Wide)
            .unwrap_or_else(|e| panic!("{word:08X} {:?}: {e:?}", found.instruction));
        let Encoding::Wide(a, b) = encoded else {
            panic!("{word:08X} narrowed");
        };
        let back = decode(a, b, Isa::ALL, position).expect("decodes");
        assert_eq!(back.instruction, found.instruction, "{word:08X}");
    }
    assert!(decoded > 50_000, "{decoded}");
}

#[test]
fn each_decoded_instruction_encodes_back_and_each_row_inverts_its_own_fields() {
    let mut rng = Prng::new(0x732_0002);
    for _ in 0..200_000 {
        let draw = rng.next_u32();
        let first = halfword(draw);
        let second = halfword(draw >> 16);
        let position = position(&mut rng);
        let Some(detail) = decode_detail(first, second, Isa::ALL, position) else {
            continue;
        };
        let instruction = detail.instruction;
        let row = detail.row;
        for size in [
            Size::Narrowest,
            if row.wide { Size::Wide } else { Size::Narrow },
        ] {
            let encoded = encode(&instruction, position, size)
                .unwrap_or_else(|e| panic!("{first:04X} {second:04X} {instruction:?}: {e:?}"));
            let (a, b) = match encoded {
                Encoding::Narrow(a) => (a, 0),
                Encoding::Wide(a, b) => (a, b),
            };
            let back = decode(a, b, Isa::ALL, position).expect("decodes");
            assert_eq!(back.instruction, instruction, "{encoded:04X?}");
        }
        if position != OUTSIDE || row.shape == Shape::Nop {
            continue;
        }
        let word = if row.wide {
            (u32::from(first) << 16) | u32::from(second)
        } else {
            u32::from(first)
        };
        let mut fields = form::fields(row.shape, &row.layout, &instruction.op);
        fields.set(Field::Cond, instruction.cond.bits());
        let again = row.layout.deposit(&fields).expect("its own fields fit");
        if detail.canonical {
            assert_eq!(again, word, "{row:?}");
        } else {
            let free = !row.layout.should_mask;
            assert_eq!(
                again & free & !twice(row),
                word & free & !twice(row),
                "{row:?}"
            );
        }
    }
}

/// A row's second copy of a source named twice, which a non-canonical word
/// may name differently.
fn twice(row: &Row) -> u32 {
    if row.shape == Shape::Twice {
        row.layout.field(Field::Rm) & 0xFFFF_0000
    } else {
        0
    }
}

/// Instructions, as the Debugger spells them at `&8000`, each the only
/// encoding of its width.
const DISASSEMBLY: &[(&[u16], &str)] = &[
    (&[0x2001], "MOVS    R0,#1"),
    (&[0x1888], "ADDS    R0,R1,R2"),
    (&[0x1C48], "ADDS    R0,R1,#1"),
    (&[0x0088], "MOVS    R0,R1,LSL #2"),
    (&[0x0008], "MOVS    R0,R1"),
    (&[0x4088], "MOVS    R0,R0,LSL R1"),
    (&[0x4240], "RSBS    R0,R0,#0"),
    (&[0x4348], "MULS    R0,R1,R0"),
    (&[0x4408], "ADD     R0,R0,R1"),
    (&[0x4686], "MOV     R14,R0"),
    (&[0x4285], "CMP     R5,R0"),
    (&[0x4770], "BX      R14"),
    (&[0x4788], "BLX     R1"),
    (&[0x4A01], "LDR     R2,[PC,#4]"),
    (&[0x6848], "LDR     R0,[R1,#4]"),
    (&[0x5888], "LDR     R0,[R1,R2]"),
    (&[0x8848], "LDRH    R0,[R1,#2]"),
    (&[0x9001], "STR     R0,[R13,#4]"),
    (&[0xA001], "ADD     R0,PC,#4"),
    (&[0xA901], "ADD     R1,R13,#4"),
    (&[0xB082], "SUB     R13,R13,#8"),
    (&[0xB500], "STMFD   R13!,{R14}"),
    (&[0xBD00], "LDMFD   R13!,{PC}"),
    (&[0xC806], "LDMIA   R0!,{R1,R2}"),
    (&[0xC803], "LDMIA   R0,{R0,R1}"),
    (&[0xB110], "CBZ     R0,&00008008"),
    (&[0xB2C8], "UXTB    R0,R1"),
    (&[0xBA08], "REV     R0,R1"),
    (&[0xB662], "CPSIE   I"),
    (&[0xB658], "SETEND  BE"),
    (&[0xBE01], "BKPT    #1"),
    (&[0xBF00], "NOP"),
    (&[0xBF30], "WFI"),
    (&[0xBF08], "IT      EQ"),
    (&[0xBF0C], "ITE     EQ"),
    (&[0xBF1E], "ITTT    NE"),
    (&[0xD0FE], "BEQ     &00008000"),
    (&[0xDF01], "SWI     &000001"),
    (&[0xDE00], "UDF     #0"),
    (&[0xE7FE], "B       &00008000"),
    (&[0xF04F, 0x0001], "MOV     R0,#1"),
    (&[0xF44F, 0x7080], "MOV     R0,#&100"),
    (&[0xF240, 0x0000], "MOVW    R0,#0"),
    (&[0xF2C1, 0x2034], "MOVT    R0,#&1234"),
    (&[0xF201, 0x0004], "ADD     R0,R1,#4"),
    (&[0xF2AF, 0x0004], "SUB     R0,PC,#4"),
    (&[0xEA4F, 0x0081], "MOV     R0,R1,LSL #2"),
    (&[0xEB01, 0x0042], "ADD     R0,R1,R2,LSL #1"),
    (&[0xEBB0, 0x0F01], "CMP     R0,R1"),
    (&[0xFA01, 0xF002], "MOV     R0,R1,LSL R2"),
    (&[0xE92D, 0x4FF0], "STMFD   R13!,{R4-R11,R14}"),
    (&[0xE8BD, 0x8FF0], "LDMFD   R13!,{R4-R11,PC}"),
    (&[0xF8D1, 0x0004], "LDR     R0,[R1,#4]"),
    (&[0xF851, 0x0D04], "LDR     R0,[R1,#-4]!"),
    (&[0xF85F, 0x0004], "LDR     R0,[PC,#-4]"),
    (&[0xF851, 0x0022], "LDR     R0,[R1,R2,LSL #2]"),
    (&[0xF851, 0x0E04], "LDRT    R0,[R1,#4]"),
    (&[0xE9D1, 0x0102], "LDRD    R0,R1,[R1,#8]"),
    (&[0xE852, 0x1F00], "LDREX   R1,[R2]"),
    (&[0xE842, 0x0100], "STREX   R1,R0,[R2]"),
    (&[0xE8DF, 0xF000], "TBB     [PC,R0]"),
    (&[0xE8D0, 0xF011], "TBH     [R0,R1,LSL #1]"),
    (&[0xFAB0, 0xF080], "CLZ     R0,R0"),
    (&[0xFB01, 0xF002], "MUL     R0,R1,R2"),
    (&[0xFB91, 0xF0F2], "SDIV    R0,R1,R2"),
    (&[0xFBA2, 0x0103], "UMULL   R0,R1,R2,R3"),
    (&[0xFA91, 0xF012], "QADD16  R0,R1,R2"),
    (&[0xF3BF, 0x8F5F], "DMB     SY"),
    (&[0xF3BF, 0x8F40], "SSBB"),
    (&[0xF3AF, 0x8000], "NOP"),
    (&[0xF3EF, 0x8000], "MRS     R0,CPSR"),
    (&[0xF380, 0x8800], "MSR     CPSR_f,R0"),
    (&[0xF3DE, 0x8F04], "SUBS    PC,R14,#4"),
    (&[0xF000, 0xF800], "BL      &00008004"),
    (&[0xF000, 0xE800], "BLX     &00008004"),
    (&[0xF000, 0x8000], "BEQ     &00008004"),
    (&[0xF000, 0xB800], "B       &00008004"),
    (&[0xF3C1, 0x1003], "UBFX    R0,R1,#4,#4"),
    (&[0xF7F0, 0xA000], "UDF     #0"),
];

#[test]
fn instructions_disassemble_as_the_debugger_spells_them() {
    for &(halves, expected) in DISASSEMBLY {
        assert_eq!(text_at(halves, OUTSIDE, 0x8000), expected, "{halves:04X?}");
        let decoded = decode(
            halves[0],
            halves.get(1).copied().unwrap_or(0),
            Isa::ALL,
            OUTSIDE,
        )
        .expect("decodes");
        assert!(!decoded.unpredictable, "{expected}");
        let size = if halves.len() == 2 {
            Size::Wide
        } else {
            Size::Narrow
        };
        assert_eq!(
            encode(&decoded.instruction, OUTSIDE, size),
            Ok(encoding(halves)),
            "{expected}"
        );
    }
}

#[test]
fn the_narrowest_encoding_is_chosen_where_one_holds_the_instruction() {
    let wide = decode(0xF8D1, 0x0004, Isa::ALL, OUTSIDE).expect("LDR.W");
    assert_eq!(
        encode(&wide.instruction, OUTSIDE, Size::Narrowest),
        Ok(Encoding::Narrow(0x6848))
    );
    let flags_kept = decode(0xF04F, 0x0001, Isa::ALL, OUTSIDE).expect("MOV.W");
    assert_eq!(
        encode(&flags_kept.instruction, OUTSIDE, Size::Narrowest),
        Ok(Encoding::Wide(0xF04F, 0x0001)),
        "outside a block the 16-bit MOV sets the flags"
    );
    assert_eq!(
        encode(&flags_kept.instruction, OUTSIDE, Size::Narrow),
        Err(EncodeError::Unencodable)
    );
}

#[test]
fn a_block_gives_its_instructions_their_condition_and_keeps_their_flags() {
    let inside = Position::Inside(Condition::Eq);
    assert_eq!(text_at(&[0x1888], inside, 0), "ADDEQ   R0,R1,R2");
    assert_eq!(text_at(&[0xF04F, 0x0001], inside, 0), "MOVEQ   R0,#1");
    let moved = decode(0x2001, 0, Isa::ALL, inside).expect("MOV");
    assert_eq!(
        moved.instruction.op,
        Op::Move {
            op: MoveOp::Mov,
            s: false,
            rd: Reg::field(0),
            operand: Operand::Literal(1),
        },
        "a 16-bit operation sets no flags in a block"
    );
    let breakpoint = decode(0xBE01, 0, Isa::ALL, inside).expect("BKPT");
    assert_eq!(
        breakpoint.instruction.cond,
        Condition::Al,
        "always unconditional"
    );
}

#[test]
fn the_position_follows_the_it_state() {
    assert_eq!(Position::of(0), Position::Outside);
    assert_eq!(Position::of(0x08), Position::Last(Condition::Eq));
    assert_eq!(Position::of(0x04), Position::Inside(Condition::Eq));
    assert_eq!(Position::of(0x1C), Position::Inside(Condition::Ne));
    assert_eq!(Position::Inside(Condition::Gt).condition(), Condition::Gt);
    assert_eq!(Position::Outside.condition(), Condition::Al);
}

#[test]
fn the_block_rules_are_reported() {
    let unpredictable = |halves: &[u16], position| {
        let second = halves.get(1).copied().unwrap_or(0);
        decode(halves[0], second, Isa::ALL, position)
            .expect("decodes")
            .unpredictable
    };
    let inside = Position::Inside(Condition::Eq);
    let last = Position::Last(Condition::Eq);
    assert!(unpredictable(&[0xE7FE], inside), "a branch before the last");
    assert!(!unpredictable(&[0xE7FE], last), "a branch as the last");
    assert!(
        unpredictable(&[0xD0FE], last),
        "a conditional branch in a block"
    );
    assert!(unpredictable(&[0xBF08], last), "IT in a block");
    assert!(unpredictable(&[0xB110], last), "CBZ in a block");
    assert!(unpredictable(&[0x0008], last), "MOVS R0,R1 in a block");
    assert!(!unpredictable(&[0x4608], last), "MOV R0,R1 in a block");
    assert!(
        unpredictable(&[0xBD00], inside),
        "POP {{PC}} before the last"
    );
    assert!(!unpredictable(&[0xBD00], last), "POP {{PC}} as the last");
    assert!(
        unpredictable(&[0x2001], Position::Inside(Condition::Nv)),
        "a condition no IT gives"
    );
    assert!(unpredictable(&[0xBF00 | 0xE0 | 0x0C], OUTSIDE), "ITE AL");
    assert!(!unpredictable(&[0xBF00 | 0xE0 | 0x08], OUTSIDE), "IT AL");
    assert!(unpredictable(&[0xF3BF, 0x8F40], last), "SSBB in a block");
}

#[test]
fn the_register_rules_are_reported() {
    let unpredictable = |halves: &[u16]| {
        let second = halves.get(1).copied().unwrap_or(0);
        decode(halves[0], second, Isa::ALL, OUTSIDE)
            .expect("decodes")
            .unpredictable
    };
    assert!(
        unpredictable(&[0x4505]),
        "CMP of two low registers in the high form"
    );
    assert!(!unpredictable(&[0x4285]), "CMP R5,R0");
    assert!(unpredictable(&[0x47F8]), "BLX PC");
    assert!(!unpredictable(&[0x4778]), "BX PC");
    assert!(unpredictable(&[0x44FF]), "ADD PC,PC");
    assert!(unpredictable(&[0xF04F, 0x0F01]), "MOV.W PC,#1");
    assert!(
        !unpredictable(&[0xF04F, 0x0D01]),
        "MOV.W SP,#1: ARMv8 permits R13"
    );
    assert!(unpredictable(&[0xE8BD, 0xC000]), "LDM of the PC and LR");
    assert!(
        unpredictable(&[0xE92D, 0x0001]),
        "a 32-bit STM of one register"
    );
    assert!(unpredictable(&[0xE8B0, 0x0003]), "LDM R0! loading R0");
    assert!(unpredictable(&[0xFAB1, 0xF080]), "CLZ naming two sources");
    assert!(
        unpredictable(&[0xE9D1, 0x0000]),
        "LDRD of one register twice"
    );
    assert!(unpredictable(&[0xF851, 0x1D04]), "LDR R1,[R1,#-4]!");
    assert!(!unpredictable(&[0xF8D1, 0xF004]), "LDR PC,[R1,#4]");
    assert!(unpredictable(&[0xF8C1, 0xF004]), "STR PC,[R1,#4]");
    assert!(unpredictable(&[0xE842, 0x0200]), "STREX status the base");
    assert!(
        unpredictable(&[0xFBA2, 0x1103]),
        "UMULL writing one register twice"
    );
    assert!(unpredictable(&[0xF380, 0x8000]), "MSR of no field");
    assert!(
        unpredictable(&[0xF3DD, 0x8F04]),
        "SUBS PC from other than LR"
    );
}

#[test]
fn a_store_addressing_from_the_pc_is_undefined() {
    assert_eq!(
        decode(0xF8CF, 0x0004, Isa::ALL, OUTSIDE),
        None,
        "STR.W R0,[PC,#4]"
    );
    assert!(
        decode(0xF8DF, 0x0004, Isa::ALL, OUTSIDE).is_some(),
        "LDR.W literal"
    );
    assert_eq!(
        decode(0xE9CF, 0x0102, Isa::ALL, OUTSIDE),
        None,
        "STRD R0,R1,[PC,#8]"
    );
    let literal = decode(0xE9DF, 0x0102, Isa::ALL, OUTSIDE).expect("LDRD literal");
    assert!(!literal.unpredictable);
}

#[test]
fn an_instruction_a_profile_lacks_is_undefined_there() {
    let thumb1 = Isa::of(&[
        crate::isa::Feature::V2,
        crate::isa::Feature::V3,
        crate::isa::Feature::V4,
        crate::isa::Feature::V4T,
    ]);
    assert!(
        decode(0x1888, 0, thumb1, OUTSIDE).is_some(),
        "ADDS from ARMv4T"
    );
    assert_eq!(decode(0xB2C8, 0, thumb1, OUTSIDE), None, "UXTB needs ARMv6");
    assert_eq!(decode(0xBF08, 0, thumb1, OUTSIDE), None, "IT needs ARMv6T2");
    assert_eq!(
        decode(0xF04F, 0x0001, thumb1, OUTSIDE),
        None,
        "Thumb-2 needs ARMv6T2"
    );
}

#[test]
fn operands_no_encoding_holds_are_refused() {
    let at = |op| Instruction {
        cond: Condition::Al,
        op,
    };
    let far = at(Op::Branch {
        op: BranchOp::B,
        offset: 1 << 25,
    });
    assert_eq!(
        encode(&far, OUTSIDE, Size::Narrowest),
        Err(EncodeError::Range)
    );
    let odd = at(Op::Branch {
        op: BranchOp::B,
        offset: 3,
    });
    assert_eq!(
        encode(&odd, OUTSIDE, Size::Narrowest),
        Err(EncodeError::Range)
    );
    let wide_literal = at(Op::Data {
        op: DataOp::Add,
        s: false,
        rd: Reg::field(0),
        rn: Reg::field(1),
        operand: Operand::Literal(4096),
    });
    assert_eq!(
        encode(&wide_literal, OUTSIDE, Size::Narrowest),
        Err(EncodeError::Range)
    );
    let conditional = Instruction {
        cond: Condition::Eq,
        op: Op::Hint {
            op: HintOp::Nop,
            value: 0,
        },
    };
    assert_eq!(
        encode(&conditional, OUTSIDE, Size::Narrowest),
        Err(EncodeError::Unencodable),
        "a condition outside a block"
    );
    assert_eq!(
        encode(
            &conditional,
            Position::Inside(Condition::Eq),
            Size::Narrowest
        ),
        Ok(Encoding::Narrow(0xBF00))
    );
}

#[test]
fn a_reserved_hint_behaves_as_nop() {
    let hint = decode(0xF9B1, 0xF004, Isa::ALL, OUTSIDE).expect("decodes");
    assert_eq!(
        hint.instruction.op,
        Op::Hint {
            op: HintOp::Nop,
            value: 0
        }
    );
    assert!(!hint.unpredictable);
}

#[test]
fn every_wide_prefix_and_no_narrow_one_reads_a_second_halfword() {
    for first in 0..=u16::MAX {
        assert_eq!(is_wide(first), first >> 11 >= 0b11101, "{first:04X}");
    }
    assert_eq!(Encoding::Narrow(0).bytes(), 2);
    assert_eq!(Encoding::Wide(0, 0).bytes(), 4);
    assert_eq!(bits(Encoding::Wide(0xF000, 0xF800)), 0xF000_F800);
}
