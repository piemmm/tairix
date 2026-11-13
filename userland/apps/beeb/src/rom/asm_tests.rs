use alloc::vec::Vec;

use super::{abs, hi, imm, lo, sym, zp, Acc, Addr, Asm, AsmError, Byte, Operand, ERASED, RTS};
use crate::cpu::disasm::Instruction;
use crate::cpu::isa::{Mnemonic, Mode, OPCODES};

const ORIGIN: u16 = 0xC000;

fn finished(build: impl FnOnce(&mut Asm)) -> Result<Vec<u8>, AsmError> {
    let mut asm = Asm::new(ORIGIN, 0x100);
    build(&mut asm);
    asm.finish().map(|image| image.bytes)
}

/// The operand of `mode` naming `&0012`, or `&1234` for the two-byte modes.
fn operand_for(mode: Mode) -> Option<Operand> {
    let zero_page = zp(0x12);
    let absolute = abs(0x1234);
    match mode {
        Mode::Implied | Mode::Relative => None,
        Mode::Accumulator => Some(Acc.into()),
        Mode::Immediate => Some(imm(0x12)),
        Mode::ZeroPage => Some(zero_page.into()),
        Mode::ZeroPageX => Some(zero_page.x()),
        Mode::ZeroPageY => Some(zero_page.y()),
        Mode::IndexedIndirect => Some(zero_page.ind_x()),
        Mode::IndirectIndexed => Some(zero_page.ind_y()),
        Mode::Absolute => Some(absolute.into()),
        Mode::AbsoluteX => Some(absolute.x()),
        Mode::AbsoluteY => Some(absolute.y()),
        Mode::Indirect => Some(absolute.ind()),
    }
}

#[test]
fn every_documented_opcode_encodes_to_its_own_byte_and_operand() {
    let mut encoded = 0;
    for (code, opcode) in OPCODES
        .iter()
        .enumerate()
        .filter(|(_, opcode)| opcode.documented)
    {
        let bytes = finished(|asm| {
            if opcode.mode == Mode::Relative {
                let target = asm.local();
                asm.branch(opcode.mnemonic, target.into());
                asm.bind(target);
            } else {
                asm.instruction(opcode.mnemonic, operand_for(opcode.mode));
            }
        })
        .unwrap();
        assert_eq!(usize::from(bytes[0]), code, "{opcode:?}");
        let decoded = Instruction::decode(ORIGIN, [bytes[0], bytes[1], bytes[2]]);
        assert_eq!(decoded.opcode, *opcode);
        let operand = match opcode.mode.operand_bytes() {
            0 => &[][..],
            1 if opcode.mode == Mode::Relative => &[0x00][..],
            1 => &[0x12][..],
            _ => &[0x34, 0x12][..],
        };
        assert_eq!(&bytes[1..=operand.len()], operand, "{opcode:?}");
        assert_eq!(
            bytes[1 + operand.len()],
            ERASED,
            "{opcode:?} is no longer than its size"
        );
        encoded += 1;
    }
    assert_eq!(encoded, 151, "the documented set");
}

#[test]
fn only_documented_modes_are_encodable() {
    assert_eq!(
        finished(|asm| asm.stx(abs(0x1234).x())),
        Err(AsmError::NoEncoding {
            at: ORIGIN,
            mnemonic: Mnemonic::Stx,
            mode: Mode::AbsoluteX
        })
    );
    assert!(matches!(
        finished(|asm| asm.lda(Acc)),
        Err(AsmError::NoEncoding { .. })
    ));
    assert!(matches!(
        finished(|asm| asm.jmp(zp(0x12))),
        Err(AsmError::NoEncoding { .. })
    ));
}

#[test]
fn labels_resolve_forwards_and_backwards_in_every_use() {
    let mut asm = Asm::new(ORIGIN, 0x100);
    let start = asm.here("start");
    let ahead = asm.label("ahead");
    asm.jmp(ahead);
    asm.lda(imm(lo(ahead)));
    asm.ldx(imm(hi(ahead + 1)));
    asm.lda(abs(ahead + 2).x());
    asm.bne(ahead);
    asm.beq(start);
    asm.word(start);
    asm.byte(lo(ahead));
    asm.org(0xC080);
    asm.bind(ahead);
    asm.rts();
    let image = asm.finish().unwrap();
    assert_eq!(&image.bytes[..3], &[0x4C, 0x80, 0xC0], "JMP ahead");
    assert_eq!(
        &image.bytes[3..7],
        &[0xA9, 0x80, 0xA2, 0xC0],
        "its low and high bytes"
    );
    assert_eq!(
        &image.bytes[7..10],
        &[0xBD, 0x82, 0xC0],
        "plus a displacement"
    );
    // A branch counts from the instruction after it.
    assert_eq!(&image.bytes[10..12], &[0xD0, 0x80 - 0x0C]);
    assert_eq!(&image.bytes[12..14], &[0xF0, (-14i8).cast_unsigned()]);
    assert_eq!(&image.bytes[14..17], &[0x00, 0xC0, 0x80]);
    assert_eq!(image.bytes[0x80], 0x60);
    assert_eq!(image.symbol("ahead"), Some(0xC080));
    assert_eq!(image.symbol("start"), Some(ORIGIN));
    assert_eq!(image.symbol("nowhere"), None);
}

#[test]
fn a_branch_reaches_a_signed_byte_and_no_further() {
    let reach = |gap: usize| {
        finished(|asm| {
            let target = asm.local();
            asm.bcc(target);
            asm.fill(gap, 0xEA);
            asm.bind(target);
        })
    };
    assert_eq!(reach(127).unwrap()[1], 127);
    assert_eq!(
        reach(128),
        Err(AsmError::BranchRange {
            at: ORIGIN,
            target: ORIGIN + 130,
            name: "",
            after: ""
        })
    );
    let back = |gap: usize| {
        finished(|asm| {
            let target = asm.here_local();
            asm.fill(gap, 0xEA);
            asm.bvs(target);
        })
    };
    assert_eq!(back(126).unwrap()[127], 0x80);
    assert!(matches!(back(127), Err(AsmError::BranchRange { .. })));
}

#[test]
fn a_zero_page_operand_must_lie_in_page_zero() {
    assert_eq!(
        finished(|asm| asm.lda(zp(0x100))),
        Err(AsmError::NotZeroPage { at: ORIGIN })
    );
    let mut asm = Asm::new(0x0000, 0x200);
    let low = asm.label("low");
    let high = asm.label("high");
    asm.lda(zp(low));
    asm.lda(zp(high).ind_y());
    asm.org(0x00F0);
    asm.bind(low);
    asm.org(0x0100);
    asm.bind(high);
    assert_eq!(asm.finish(), Err(AsmError::NotZeroPage { at: 0x0002 }));
}

#[test]
fn placing_over_placed_bytes_or_outside_the_image_is_refused() {
    assert_eq!(
        finished(|asm| {
            asm.fill(4, 0xEA);
            asm.org(ORIGIN + 2);
            asm.rts();
        }),
        Err(AsmError::Overlap { at: ORIGIN + 2 })
    );
    assert_eq!(
        finished(|asm| asm.fill(0x101, 0)),
        Err(AsmError::OutsideImage { offset: 0x100 })
    );
    assert!(matches!(
        finished(|asm| asm.org(0x8000)),
        Err(AsmError::OutsideImage { .. })
    ));
    assert!(matches!(
        Asm::new(0xFF00, 0x200).finish(),
        Err(AsmError::OutsideImage { .. })
    ));
    let erased = finished(|asm| asm.org(ORIGIN + 0x10)).unwrap();
    assert!(erased.iter().all(|&byte| byte == ERASED));
}

#[test]
fn a_label_is_bound_once_named_once_and_always_bound_when_used() {
    assert_eq!(
        finished(|asm| {
            let label = asm.label("once");
            asm.jsr(label);
        }),
        Err(AsmError::Unbound { name: "once" })
    );
    assert_eq!(
        finished(|asm| {
            let local = asm.local();
            asm.word(local);
        }),
        Err(AsmError::Unbound { name: "" })
    );
    assert_eq!(
        finished(|asm| {
            let label = asm.here("twice");
            asm.nop();
            asm.bind(label);
        }),
        Err(AsmError::Rebound {
            name: "twice",
            at: ORIGIN + 1
        })
    );
    assert_eq!(
        finished(|asm| {
            asm.here("same");
            asm.here("same");
        }),
        Err(AsmError::Duplicate { name: "same" })
    );
    assert_eq!(
        finished(|asm| {
            asm.here("named");
        }),
        Ok(alloc::vec![ERASED; 0x100]),
        "an unused named label is fine"
    );
    let mut asm = Asm::new(ORIGIN, 0x10);
    asm.here_local();
    asm.label("never");
    assert_eq!(
        asm.finish(),
        Err(AsmError::Unbound { name: "never" }),
        "a named label must be bound"
    );
}

#[test]
fn code_falls_into_a_label_only_where_the_label_is_next() {
    let image = finished(|asm| {
        asm.lda(imm(1));
        asm.fall_into("next");
        asm.here("next");
        asm.rts();
    })
    .unwrap();
    assert_eq!(&image[..3], &[0xA9, 0x01, 0x60], "nothing is emitted");
    assert_eq!(
        finished(|asm| {
            asm.lda(imm(1));
            asm.fall_into("later");
            asm.nop();
            asm.here("later");
        }),
        Err(AsmError::NotNext {
            at: ORIGIN + 2,
            name: "later"
        })
    );
    assert_eq!(
        finished(|asm| {
            let local = asm.local();
            asm.fall_into(local);
        }),
        Err(AsmError::Unbound { name: "" }),
        "the label must be bound"
    );
}

#[test]
fn data_directives_place_bytes_words_and_ascii_text() {
    let bytes = finished(|asm| {
        asm.byte(0x01);
        asm.bytes(&[0x02, 0x03]);
        asm.word(0xBEEB);
        asm.text("OK\r");
        asm.byte(Byte::High(Addr::At(0x1234)));
    })
    .unwrap();
    assert_eq!(
        &bytes[..9],
        &[0x01, 0x02, 0x03, 0xEB, 0xBE, b'O', b'K', b'\r', 0x12]
    );
    assert_eq!(
        finished(|asm| asm.text("caf\u{e9}")),
        Err(AsmError::NotAscii { at: ORIGIN })
    );
    assert_eq!(
        finished(|asm| {
            let label = asm.here_local();
            asm.word(label + -0xC001);
        }),
        Err(AsmError::AddressRange { at: ORIGIN })
    );
}

#[test]
fn a_named_label_is_reached_by_its_name_from_anywhere() {
    let mut asm = Asm::new(ORIGIN, 0x100);
    asm.jsr("routine");
    asm.lda(abs(sym("table") + 1).y());
    asm.ldx(imm(hi("table")));
    asm.bcs("routine");
    asm.word("table");
    asm.here("routine");
    asm.rts();
    asm.here("table");
    asm.bytes(&[1, 2]);
    let image = asm.finish().unwrap();
    let routine = image.symbol("routine").unwrap();
    let table = image.symbol("table").unwrap();
    assert_eq!(&image.bytes[..3], &[0x20, routine.to_le_bytes()[0], 0xC0]);
    assert_eq!(
        &image.bytes[3..6],
        &[0xB9, (table + 1).to_le_bytes()[0], 0xC0]
    );
    assert_eq!(&image.bytes[6..8], &[0xA2, 0xC0]);
    assert_eq!(
        image.bytes[9],
        u8::try_from(routine - (ORIGIN + 10)).unwrap()
    );
    assert_eq!(&image.bytes[10..12], &table.to_le_bytes());
    assert_eq!(
        finished(|asm| asm.jmp("nowhere")),
        Err(AsmError::Unbound { name: "nowhere" })
    );
}

#[test]
fn a_branch_to_rts_takes_the_nearest_return_in_reach() {
    // An RTS before the branch, one after it nearer, and one out of reach.
    let bytes = finished(|asm| {
        asm.rts();
        asm.fill(20, 0xEA);
        asm.beq(RTS);
        asm.fill(5, 0xEA);
        asm.rts();
        asm.fill(200, 0xEA);
        asm.rts();
    })
    .unwrap();
    assert_eq!(
        bytes[22], 5,
        "the RTS five bytes on is nearer than the one 23 back"
    );
    let back = finished(|asm| {
        asm.rts();
        asm.fill(20, 0xEA);
        asm.bne(RTS);
    })
    .unwrap();
    assert_eq!(back[22], 0xE9, "back 23 bytes to the only RTS in reach");
    assert_eq!(
        finished(|asm| {
            asm.bcc(RTS);
            asm.fill(200, 0xEA);
            asm.rts();
        }),
        Err(AsmError::NoReturnInReach {
            at: ORIGIN,
            after: ""
        })
    );
    assert_eq!(
        finished(|asm| {
            asm.rts();
            asm.word(RTS);
        }),
        Err(AsmError::ReturnOutsideBranch { at: ORIGIN + 1 })
    );
}

/// The regression: a fixed address displaced past the address space was
/// silently left undisplaced or wrapped, and the nearest `RTS` lost its
/// displacement.
#[test]
fn an_address_displaced_past_the_address_space_or_a_displaced_rts_is_refused() {
    assert_eq!(
        finished(|a| a.lda(abs(Addr::At(0xFFFF) + 1))),
        Err(AsmError::AddressRange { at: ORIGIN })
    );
    assert_eq!(
        finished(|a| a.word(Addr::At(0x1000) + 70_000)),
        Err(AsmError::AddressRange { at: ORIGIN })
    );
    assert_eq!(
        finished(|a| a.lda(abs(Addr::At(0x1000) + 0x10))),
        Ok(vec_of(&[0xAD, 0x10, 0x10]))
    );
    assert_eq!(
        finished(|a| {
            a.rts();
            a.bne(RTS + 2);
        }),
        Err(AsmError::AddressRange { at: ORIGIN + 1 })
    );
    assert!(finished(|a| {
        a.rts();
        a.bne(RTS + 0);
    })
    .is_ok());
}

/// The regression: a label is an index into the image that made it, so one
/// from another image panicked or bound one of this image's.
#[test]
fn a_label_from_another_image_is_refused() {
    let mut other = Asm::new(ORIGIN, 0x100);
    for _ in 0..4 {
        other.local();
    }
    let foreign = other.local();
    assert_eq!(
        finished(|a| a.bind(foreign)),
        Err(AsmError::ForeignLabel { at: ORIGIN })
    );
    assert_eq!(
        finished(|a| a.jmp(foreign)),
        Err(AsmError::ForeignLabel { at: ORIGIN })
    );
}

/// The regression: the image was allocated at its asked length before that
/// length was checked against the address space.
#[test]
fn an_image_past_the_address_space_is_refused_unallocated() {
    let asm = Asm::new(ORIGIN, usize::MAX);
    assert_eq!(
        asm.finish().err(),
        Some(AsmError::OutsideImage { offset: usize::MAX })
    );
}

fn vec_of(bytes: &[u8]) -> Vec<u8> {
    let mut image = bytes.to_vec();
    image.resize(0x100, ERASED);
    image
}
