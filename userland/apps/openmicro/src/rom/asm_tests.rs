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

#[test]
fn code_running_on_hops_over_a_pin_with_a_jump() {
    let image = finished(|asm| {
        asm.pin(ORIGIN + 0x10, |asm| asm.jmp(abs(0x1234)));
        for _ in 0..16 {
            asm.nop();
        }
        asm.rts();
    })
    .unwrap();
    // Thirteen NOPs leave room for the hop past the pin.
    assert!(image[..0x0D].iter().all(|&byte| byte == 0xEA));
    assert_eq!(image[0x0D..0x10], [0x4C, 0x13, 0xC0], "the hop");
    assert_eq!(image[0x10..0x13], [0x4C, 0x34, 0x12], "the pin");
    assert!(image[0x13..0x16].iter().all(|&byte| byte == 0xEA));
    assert_eq!(image[0x16], 0x60);
}

#[test]
fn a_pinned_entry_lies_at_its_address_the_code_before_it_ahead() {
    let mut asm = Asm::new(ORIGIN, 0x100);
    asm.pin_entry(ORIGIN + 0x10, "inner", |asm| {
        asm.here("outer");
        asm.lda(imm(1));
        asm.here("inner");
        asm.rts();
    });
    for _ in 0..0x10 {
        asm.nop();
    }
    let image = asm.finish().unwrap();
    assert_eq!(image.symbol("outer"), Some(ORIGIN + 0x0E));
    assert_eq!(image.symbol("inner"), Some(ORIGIN + 0x10));
    assert_eq!(image.bytes[0x0E..0x11], [0xA9, 0x01, 0x60]);
    assert_eq!(image.bytes[0x0B..0x0E], [0x4C, 0x11, 0xC0], "the hop");
}

#[test]
fn a_pinned_entry_unbound_or_placed_before_the_image_is_refused() {
    let lead = |asm: &mut Asm| {
        asm.lda(imm(1));
        asm.here("inner");
        asm.rts();
    };
    assert_eq!(
        finished(|asm| asm.pin_entry(ORIGIN + 4, "absent", lead)),
        Err(AsmError::Unbound { name: "absent" })
    );
    assert_eq!(
        finished(|asm| asm.pin_entry(ORIGIN + 1, "inner", lead)),
        Err(AsmError::OutsideImage { offset: 0xFFFF })
    );
    let mut low = Asm::new(0, 0x100);
    low.pin_entry(1, "inner", lead);
    assert_eq!(low.finish(), Err(AsmError::AddressRange { at: 1 }));
}

#[test]
fn after_a_jump_or_data_the_flow_resumes_past_a_pin_with_its_labels() {
    let mut asm = Asm::new(ORIGIN, 0x100);
    asm.pin(ORIGIN + 6, Asm::rts);
    asm.jmp(abs(0x1234));
    asm.here("after_jump");
    asm.lda(imm(1));
    asm.pin(ORIGIN + 0x0C, Asm::rts);
    asm.byte(0x55);
    asm.here("after_data");
    asm.lda(imm(2));
    let image = asm.finish().unwrap();
    assert_eq!(image.symbol("after_jump"), Some(ORIGIN + 7));
    assert_eq!(image.symbol("after_data"), Some(ORIGIN + 0x0D));
    assert_eq!(
        image.bytes[..0x0F],
        [
            0x4C, 0x34, 0x12, ERASED, ERASED, ERASED, 0x60, 0xA9, 0x01, 0x55, ERASED, ERASED, 0x60,
            0xA9, 0x02
        ]
    );
}

#[test]
fn a_run_of_pins_is_hopped_at_once() {
    let image = finished(|asm| {
        asm.pin(ORIGIN + 0x0C, |asm| asm.jmp(abs(0x2222)));
        asm.pin(ORIGIN + 8, |asm| asm.jmp(abs(0x1111)));
        for _ in 0..6 {
            asm.nop();
        }
    })
    .unwrap();
    assert_eq!(image[5..8], [0x4C, 0x0F, 0xC0], "past both");
    assert_eq!(image[0x0F], 0xEA);
}

#[test]
fn data_an_origin_fixes_meeting_a_pin_or_a_pin_placed_under_running_code_is_refused() {
    assert_eq!(
        finished(|asm| {
            asm.pin(ORIGIN + 2, Asm::rts);
            asm.fill(4, 0);
        }),
        Err(AsmError::Overlap { at: ORIGIN + 2 })
    );
    assert_eq!(
        finished(|asm| {
            asm.pin(ORIGIN + 0x12, Asm::rts);
            asm.rts();
            asm.org(ORIGIN + 0x10);
            asm.fill(4, 0);
        }),
        Err(AsmError::Overlap { at: ORIGIN + 0x12 })
    );
    assert_eq!(
        finished(|asm| {
            asm.nop();
            asm.pin(ORIGIN + 2, Asm::rts);
            asm.nop();
        }),
        Err(AsmError::PinBlocked {
            at: ORIGIN + 1,
            pin: ORIGIN + 2
        })
    );
}

#[test]
fn data_meeting_a_pin_moves_past_it_from_its_last_named_label() {
    let mut asm = Asm::new(ORIGIN, 0x100);
    asm.pin(ORIGIN + 6, Asm::rts);
    asm.rts();
    asm.here("first");
    asm.bytes(&[1; 4]);
    asm.here("second");
    asm.word(sym("first"));
    asm.bytes(&[2; 2]);
    let image = asm.finish().unwrap();
    assert_eq!(image.symbol("first"), Some(ORIGIN + 1), "the first stays");
    assert_eq!(image.symbol("second"), Some(ORIGIN + 7), "the second moves");
    assert_eq!(image.bytes[1..5], [1; 4]);
    assert_eq!(image.bytes[5], ERASED);
    assert_eq!(
        image.bytes[7..11],
        [0x01, 0xC0, 2, 2],
        "its use moved with it"
    );
}

#[test]
fn a_hop_after_a_certain_flag_is_a_branch() {
    let image = finished(|asm| {
        asm.pin(ORIGIN + 5, Asm::rts);
        asm.clc();
        asm.lda(imm(1));
    })
    .unwrap();
    assert_eq!(
        image[..3],
        [0x18, 0x90, 0x03],
        "C clear, so BCC past the pin"
    );
    assert_eq!(image[6..8], [0xA9, 0x01]);
}

#[test]
fn a_call_with_its_text_and_an_error_block_are_placed_whole() {
    let image = finished(|asm| {
        asm.pin(ORIGIN + 8, Asm::rts);
        asm.nop();
        asm.call_with_text(0x1234u16, &["A", "B"]);
        asm.error(0x11, "E");
    })
    .unwrap();
    assert_eq!(image[..4], [0xEA, 0x4C, 0x09, 0xC0], "a hop over the pin");
    assert_eq!(
        image[9..15],
        [0x20, 0x34, 0x12, b'A', b'B', 0],
        "the call and its text"
    );
    assert_eq!(image[15..19], [0x00, 0x11, b'E', 0], "the block");
}

#[test]
fn held_tables_fill_the_room_a_pin_leaves_and_the_rest_follow_where_placed() {
    let mut asm = Asm::new(ORIGIN, 0x100);
    asm.pin(ORIGIN + 6, Asm::rts);
    asm.later(|asm| {
        asm.here("big");
        asm.bytes(&[9; 20]);
    });
    asm.later(|asm| {
        asm.here("small");
        asm.bytes(&[1, 2]);
    });
    asm.jmp(abs(0x1234));
    asm.here("next");
    asm.lda(imm(1));
    asm.place_later();
    let image = asm.finish().unwrap();
    assert_eq!(
        image.symbol("small"),
        Some(ORIGIN + 3),
        "in the room before the pin"
    );
    assert_eq!(image.symbol("next"), Some(ORIGIN + 7));
    assert_eq!(
        image.symbol("big"),
        Some(ORIGIN + 9),
        "where held items are placed"
    );
    assert_eq!(image.bytes[3..5], [1, 2]);
}

#[test]
fn room_up_to_an_address_takes_held_tables_and_leaves_the_flow() {
    let mut asm = Asm::new(ORIGIN, 0x100);
    asm.later(|asm| {
        asm.here("held");
        asm.bytes(&[7; 3]);
    });
    asm.rts();
    asm.place_later_before(ORIGIN + 4);
    asm.here("after");
    asm.rts();
    let image = asm.finish().unwrap();
    assert_eq!(image.symbol("held"), Some(ORIGIN + 1));
    assert_eq!(
        image.symbol("after"),
        Some(ORIGIN + 4),
        "the flow carries on from there"
    );
}

#[test]
fn a_pin_s_room_to_its_own_address_takes_tables_held_after_it() {
    let mut asm = Asm::new(ORIGIN, 0x100);
    asm.pin(ORIGIN + 0x10, |asm| {
        asm.rts();
        asm.room_to(ORIGIN + 0x14);
        asm.here("fixed");
        asm.rts();
    });
    asm.lda(imm(1));
    asm.later(|asm| {
        asm.here("held");
        asm.bytes(&[7; 3]);
    });
    asm.place_later();
    let image = asm.finish().unwrap();
    assert_eq!(image.symbol("fixed"), Some(ORIGIN + 0x14));
    assert_eq!(
        image.symbol("held"),
        Some(ORIGIN + 0x11),
        "in the pin's room"
    );
    assert_eq!(image.bytes[0x11..0x14], [7; 3]);
}

#[test]
fn room_to_an_address_already_passed_is_an_overlap() {
    assert_eq!(
        finished(|asm| {
            asm.bytes(&[1; 4]);
            asm.room_to(ORIGIN + 2);
            asm.rts();
        }),
        Err(AsmError::Overlap { at: ORIGIN + 2 })
    );
}

#[test]
fn a_hop_where_a_label_lies_is_a_jump_whatever_the_flags() {
    let image = finished(|asm| {
        asm.pin(ORIGIN + 5, Asm::rts);
        asm.clc();
        asm.here("reached_with_any_carry");
        asm.lda(imm(1));
    })
    .unwrap();
    assert_eq!(image[..4], [0x18, 0x4C, 0x06, 0xC0], "JMP past the pin");
}

#[test]
fn a_skip_over_reads_past_its_instruction_which_a_label_enters() {
    let image = finished(|asm| {
        asm.ldx(imm(1));
        asm.skip_over(|asm| {
            asm.here("second");
            asm.ldx(imm(2));
        });
        asm.rts();
    })
    .unwrap();
    assert_eq!(image[..6], [0xA2, 0x01, 0x2C, 0xA2, 0x02, 0x60]);
}

#[test]
fn a_skip_over_of_one_byte_reads_zero_page() {
    let image = finished(|asm| {
        asm.sec();
        asm.skip_over(|asm| {
            asm.here("clear");
            asm.clc();
        });
        asm.rts();
    })
    .unwrap();
    assert_eq!(image[..4], [0x38, 0x24, 0x18, 0x60]);
}

#[test]
fn a_skip_over_takes_one_known_instruction_that_cannot_read_a_device() {
    for body in [
        (|asm: &mut Asm| asm.ldx(imm(0xD4))) as fn(&mut Asm),
        |asm| asm.lda(imm(0x80)),
        |asm| asm.lda(abs(0x1234)),
        |asm| asm.ldx(imm(lo("later"))),
    ] {
        let result = finished(|asm| {
            asm.skip_over(body);
            asm.here("later");
            asm.rts();
        });
        assert!(
            matches!(result, Err(AsmError::SkipOperand { .. })),
            "{result:?}"
        );
    }
}

#[test]
fn a_skip_over_keeps_to_the_pages_its_machine_reads_quietly() {
    let skipping = |quiet: fn(u8) -> bool| {
        let mut asm = Asm::new(ORIGIN, 0x100).with_quiet_pages(quiet);
        asm.skip_over(|asm| asm.stx(zp(0x38)));
        asm.rts();
        asm.finish().map(|_| ())
    };
    assert_eq!(skipping(|page| page < 0x80), Ok(()), "RAM at $3886");
    assert!(
        matches!(
            skipping(crate::machine::c64::drive::quiet_read_page),
            Err(AsmError::SkipOperand { .. })
        ),
        "a VIA's mirror at $3886 on the 1541"
    );
}

#[test]
fn the_1541_reads_quietly_from_all_but_its_vias_pages() {
    use crate::machine::c64::drive::quiet_read_page;
    let quiet: Vec<u8> = [0x00, 0x07, 0x17, 0x18, 0x1C, 0x20, 0x38, 0x58, 0x7F, 0x80]
        .into_iter()
        .filter(|&page| quiet_read_page(page))
        .collect();
    assert_eq!(quiet, [0x00, 0x07, 0x17, 0x20, 0x80]);
}

#[test]
fn code_flowing_after_a_claim_hops_its_runs_and_fills_the_gap_between() {
    let image = finished(|asm| {
        asm.org(ORIGIN + 0x08);
        asm.nop();
        asm.nop();
        asm.pin(ORIGIN + 0x10, |asm| asm.jmp(abs(ORIGIN)));
        asm.claim(ORIGIN, ORIGIN + 0x11);
        asm.org(ORIGIN + 0x04);
        for _ in 0..5 {
            asm.nop();
        }
        asm.rts();
    })
    .unwrap();
    let [low, high] = (ORIGIN + 0x0A).to_le_bytes();
    assert_eq!(image[4..8], [0xEA, 0x4C, low, high], "past the first run");
    assert_eq!(image[0x0A..0x0D], [0xEA; 3], "into the gap");
    let [low, high] = (ORIGIN + 0x13).to_le_bytes();
    assert_eq!(
        image[0x0D..0x10],
        [0x4C, low, high],
        "past the pin the claim met"
    );
    assert_eq!(image[0x13..0x15], [0xEA, 0x60]);
}

#[test]
fn a_room_code_has_since_taken_part_of_takes_tables_only_in_what_is_left() {
    let image = finished(|asm| {
        asm.pin(ORIGIN + 0x0C, Asm::rts);
        asm.rts();
        asm.here("moved");
        asm.bytes(&[1; 16]);
        asm.org(ORIGIN + 0x01);
        for _ in 0..4 {
            asm.nop();
        }
        asm.later(|asm| {
            asm.here("long");
            asm.bytes(&[2; 9]);
        });
        asm.later(|asm| {
            asm.here("short");
            asm.bytes(&[3; 5]);
        });
        asm.org(ORIGIN + 0x30);
        asm.place_later();
    })
    .unwrap();
    assert_eq!(
        image[0x0D..0x1D],
        [1; 16],
        "moved past the pin, leaving a room"
    );
    assert_eq!(
        image[0x01..0x05],
        [0xEA; 4],
        "code since placed in the room"
    );
    assert_eq!(
        image[0x05..0x0A],
        [3; 5],
        "the table that fits what is left"
    );
    assert_eq!(image[0x30..0x39], [2; 9], "the other placed after");
}
