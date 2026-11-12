//! The services answering a program that has written over the data area,
//! the tables it points at, or the registers, with the sizes and addresses
//! a 16-bit BIOS computes: each case once overflowed a sum and stopped the
//! emulator.

use super::tests::{booted, call, carry, poke};
use crate::cpu::Bus as _;
use crate::machine::Template;

/// The data area's column count, page 0's cursor, the printers' and the
/// CRTC's ports.
const COLUMNS: u32 = 0x44A;
const CURSOR: u32 = 0x450;
const LPT1: u32 = 0x408;
const CRTC_PORT: u32 = 0x463;

#[test]
fn a_column_count_written_over_still_places_and_writes_characters() {
    for template in [Template::IbmXt, Template::TurboXt, Template::Workstation186] {
        let mut m = booted(template, None);
        poke(&mut m, COLUMNS, &[0xFF, 0xFF]);
        poke(&mut m, CURSOR, &[3, 2]);
        call(&mut m, 0x10, |r| {
            r.set16(0, 0x0941);
            r.set16(1, 0x0010);
            r.set16(3, 0x0007);
        });
        call(&mut m, 0x10, |r| {
            r.set16(0, 0x0200);
            r.set16(2, 0x0203);
        });
        call(&mut m, 0x10, |r| r.set16(0, 0x0800));
        call(&mut m, 0x10, |r| r.set16(0, 0x0E41));
    }
}

#[test]
fn a_text_height_set_over_a_hostile_data_area_is_answered() {
    for template in [Template::TurboXt, Template::Workstation186] {
        let mut m = booted(template, None);
        poke(&mut m, COLUMNS, &[0xFF, 0xFF]);
        poke(&mut m, CRTC_PORT, &[0xFF, 0xFF]);
        call(&mut m, 0x10, |r| r.set16(0, 0x1111));
        call(&mut m, 0x10, |r| {
            r.set16(0, 0x0100);
            r.set16(1, 0x0607);
        });
    }
}

#[test]
fn a_string_of_every_length_goes_through_the_teletype() {
    let mut m = booted(Template::TurboXt, None);
    let written = call(&mut m, 0x10, |r| {
        r.set16(0, 0x1303);
        r.set16(1, 0xFFFF);
        r.set16(3, 0x0007);
        r.set16(5, 0x8000);
    });
    assert!(!carry(&written));
}

#[test]
fn a_diskette_transfer_past_64_kib_is_a_dma_boundary_error() {
    let mut m = booted(Template::IbmXt, None);
    let mut table = crate::bios::layout::DISKETTE_PARAMETERS;
    table[3] = 3;
    poke(&mut m, 0x0800, &table);
    poke(&mut m, 0x1E * 4, &[0x00, 0x08, 0x00, 0x00]);
    let regs = call(&mut m, 0x13, |r| {
        r.set16(0, 0x02FF);
        r.set16(1, 0x0001);
        r.set16(3, 0x1000);
    });
    assert!(carry(&regs));
    assert_eq!(regs.get8(4), 0x09, "AH: DMA boundary");
}

#[test]
fn a_printer_at_the_top_of_the_port_space_is_driven() {
    let mut m = booted(Template::IbmXt, None);
    poke(&mut m, LPT1, &[0xFF, 0xFF]);
    for ah in [0x00, 0x01, 0x02] {
        call(&mut m, 0x17, |r| r.set16(0, u16::from_le_bytes([b'A', ah])));
    }
}

#[test]
fn a_print_screen_over_a_column_count_written_over_prints_each_row_by_its_low_byte() {
    // Once a sum of the word overflowed. INT 17h counts each character at
    // 0:05F0h and answers a selected, ready printer.
    let mut m = booted(Template::IbmXt, None);
    poke(&mut m, 0x0700, &[0xFF, 0x06, 0xF0, 0x05, 0xB4, 0x90, 0xCF]);
    poke(&mut m, 0x17 * 4, &[0x00, 0x07, 0x00, 0x00]);
    for (columns, printed) in [([0xFF, 0xFF], 2 + 25 * 257), ([0x00, 0x10], 2 + 25 * 2)] {
        poke(&mut m, COLUMNS, &columns);
        poke(&mut m, 0x05F0, &[0, 0]);
        poke(&mut m, 0x500, &[0]);
        call(&mut m, 0x05, |_| {});
        let count = u16::from_le_bytes([m.board().peek(0x05F0), m.board().peek(0x05F1)]);
        assert_eq!(count, printed, "{columns:02X?}");
    }
}
