use super::*;
use alloc::vec::Vec;

const VGA_TEXT: [u8; 25] = crate::bios::modes::VGA_TEXT_80.crtc;
const VGA_256: [u8; 25] = crate::bios::modes::VGA_LINEAR.crtc;

pub(super) fn programmed(generation: Generation, registers: &[u8; 25]) -> Crtc {
    let mut crtc = Crtc::new(generation);
    crtc.select(0x11);
    crtc.write(0x00);
    for (index, &value) in (0u8..).zip(registers) {
        crtc.select(index);
        crtc.write(value);
    }
    crtc
}

fn to_frame(crtc: &mut Crtc) {
    let fields = crtc.fields();
    while crtc.fields() == fields {
        crtc.step();
    }
}

#[test]
fn vga_text_is_449_lines_of_100_clocks() {
    let mut crtc = programmed(Generation::Vga, &VGA_TEXT);
    assert_eq!((crtc.line_clocks(), crtc.frame_lines()), (100, 449));
    to_frame(&mut crtc);
    let ticks: Vec<Tick> = (0..100 * 449).map(|_| crtc.step()).collect();
    assert_eq!(ticks.iter().filter(|t| t.display).count(), 80 * 400);
    assert_eq!(ticks.iter().filter(|t| t.vsync).count(), 1);
    assert_eq!(
        ticks.iter().position(|t| t.vsync),
        Some(412 * 100),
        "retrace at line 412"
    );
    assert_eq!(
        crtc.visible(),
        (13, 82, 29, 414),
        "blanking leaves a two-character left border"
    );
}

#[test]
fn text_addresses_advance_a_row_of_words_per_sixteen_lines() {
    let mut crtc = programmed(Generation::Vga, &VGA_TEXT);
    to_frame(&mut crtc);
    let ticks: Vec<Tick> = (0..100 * 32).map(|_| crtc.step()).collect();
    assert_eq!((ticks[0].addr, ticks[0].row), (0, 0));
    assert_eq!(ticks[1].addr, 2, "word mode: two plane bytes a character");
    assert_eq!((ticks[15 * 100].addr, ticks[15 * 100].row), (0, 15));
    assert_eq!((ticks[16 * 100].addr, ticks[16 * 100].row), (160, 0));
}

#[test]
fn mode_13h_doubles_each_row_and_addresses_doublewords() {
    let mut crtc = programmed(Generation::Vga, &VGA_256);
    to_frame(&mut crtc);
    let ticks: Vec<Tick> = (0..100 * 4).map(|_| crtc.step()).collect();
    assert_eq!(ticks[1].addr, 4);
    assert_eq!(
        ticks[100].addr, 0,
        "the second scan line repeats the first row"
    );
    assert_eq!(ticks[200].addr, 320);
}

#[test]
fn the_line_compare_splits_the_screen() {
    let mut registers = VGA_TEXT;
    registers[0x0C] = 0x10;
    // Line compare 99: its ninth and tenth bits, in the overflow and max
    // scan registers, cleared.
    registers[0x07] &= !0x10;
    registers[0x09] &= !0x40;
    registers[0x18] = 99;
    let mut crtc = programmed(Generation::Vga, &registers);
    to_frame(&mut crtc);
    let ticks: Vec<Tick> = (0..100 * 120).map(|_| crtc.step()).collect();
    assert_eq!(ticks[0].addr, 0x2000, "the top starts at the start address");
    assert_eq!(
        ticks[99 * 100].addr & 0xF000,
        0x2000,
        "line 99 is still the top"
    );
    assert!(ticks[100 * 100].split);
    assert_eq!(
        (ticks[100 * 100].addr, ticks[100 * 100].row),
        (0, 0),
        "line 100 shows memory from zero"
    );
}

#[test]
fn the_cga_interleave_substitutes_the_row_scan() {
    let mut registers = VGA_TEXT;
    registers[0x09] = 0x01;
    registers[0x17] = 0xA2;
    let mut crtc = programmed(Generation::Vga, &registers);
    to_frame(&mut crtc);
    let ticks: Vec<Tick> = (0..100 * 2).map(|_| crtc.step()).collect();
    assert_eq!(ticks[100].addr, 0x2000, "odd lines read the second bank");
}

#[test]
fn the_vertical_interrupt_latches_until_cleared() {
    let mut crtc = programmed(Generation::Ega, &VGA_TEXT);
    crtc.select(0x11);
    crtc.write(0x1E);
    to_frame(&mut crtc);
    to_frame(&mut crtc);
    assert!(crtc.interrupt());
    crtc.write(0x0E);
    assert!(!crtc.interrupt(), "writing a clear bit 4 drops it");
    crtc.write(0x3E);
    to_frame(&mut crtc);
    assert!(!crtc.interrupt(), "bit 5 disables it");
}

#[test]
fn the_ega_reads_only_the_cursor_and_the_vga_reads_all() {
    let mut ega = programmed(Generation::Ega, &VGA_TEXT);
    let mut vga = programmed(Generation::Vga, &VGA_TEXT);
    for crtc in [&mut ega, &mut vga] {
        crtc.select(0x0F);
        crtc.write(0x42);
        crtc.select(0x13);
    }
    assert_eq!((ega.read(), vga.read()), (0, 0x28));
    ega.select(0x0F);
    assert_eq!(ega.read(), 0x42);
}

#[test]
fn the_protect_bit_locks_the_horizontal_timing() {
    let mut crtc = programmed(Generation::Vga, &VGA_TEXT);
    crtc.select(0x11);
    crtc.write(0x8E);
    crtc.select(0x00);
    crtc.write(0x10);
    assert_eq!(crtc.register(0), 0x5F);
    crtc.select(0x07);
    crtc.write(0x00);
    assert_eq!(
        crtc.register(7),
        0x0F,
        "only the line compare bit of the overflow is writable"
    );
}

#[test]
fn the_ega_cursor_stops_before_its_end_and_splits_round_the_rows_between() {
    let lit = |generation, start: u8, end: u8| {
        let mut registers = VGA_TEXT;
        registers[0x09] = 13;
        registers[0x0A] = start;
        registers[0x0B] = end;
        let crtc = programmed(generation, &registers);
        (0..14)
            .filter(|&row| crtc.cursor_lit(row))
            .collect::<Vec<u8>>()
    };
    let all_but = |gap: u8| (0..14).filter(|&row| row != gap).collect::<Vec<u8>>();
    assert_eq!(lit(Generation::Ega, 11, 13), [11, 12]);
    assert_eq!(lit(Generation::Ega, 5, 4), all_but(4));
    assert_eq!(lit(Generation::Ega, 4, 4), [4]);
    assert_eq!(
        lit(Generation::Ega, 4, 20),
        [4],
        "the end's low bits name the start"
    );
    assert_eq!(
        lit(Generation::Ega, 4, 19),
        (0..14).collect::<Vec<u8>>(),
        "past the cell"
    );
    assert!(
        lit(Generation::Ega, 14, 15).is_empty(),
        "a start past the cell"
    );
    assert_eq!(lit(Generation::Vga, 11, 13), [11, 12, 13]);
    assert!(lit(Generation::Vga, 5, 4).is_empty());
    assert!(
        lit(Generation::Vga, 0x2B, 13).is_empty(),
        "bit 5 turns it off"
    );
}

const VGA_TEXT_200: [u8; 25] = crate::bios::modes::VGA_TEXT_80_200.crtc;

/// The plane address of every character clock displayed on the first line
/// of each character row, top first, as a whole frame scans them.
fn scanned_rows(crtc: &mut Crtc) -> Vec<Vec<u16>> {
    to_frame(crtc);
    let clocks = usize::from(crtc.line_clocks());
    let mut rows = Vec::new();
    let mut last_scan = None;
    for line in 0..crtc.frame_lines() {
        let ticks: Vec<Tick> = (0..clocks).map(|_| crtc.step()).collect();
        let scan = ticks[0].row;
        let begins = line == 0 || (scan == 0 && last_scan != Some(0));
        last_scan = Some(scan);
        let shown: Vec<u16> = ticks.iter().filter(|t| t.display).map(|t| t.addr).collect();
        if begins && !shown.is_empty() {
            rows.push(shown);
        }
    }
    rows
}

fn read_rows(crtc: &Crtc) -> Vec<Vec<u16>> {
    crtc.text_rows()
        .map(|(counter, scan)| {
            (0..crtc.text_columns())
                .map(|column| crtc.text_address(counter, scan, column))
                .collect()
        })
        .collect()
}

#[test]
fn each_rows_addresses_are_those_the_scan_reads_split_preset_or_doubled() {
    let set = |registers: &[u8; 25], changes: &[(u8, u8)]| {
        let mut crtc = programmed(Generation::Vga, registers);
        for &(index, value) in changes {
            crtc.select(index);
            crtc.write(value);
        }
        crtc
    };
    let cases = [
        set(&VGA_TEXT, &[]),
        set(&VGA_TEXT, &[(0x0C, 0x01), (0x0D, 0x90)]),
        set(&VGA_TEXT, &[(0x07, 0x0F), (0x09, 0x0F), (0x18, 191)]),
        set(&VGA_TEXT, &[(0x08, 0x25)]),
        set(&VGA_TEXT_200, &[]),
        set(&VGA_TEXT, &[(0x0C, 0xFF), (0x0D, 0xE0)]),
    ];
    for (n, mut crtc) in cases.into_iter().enumerate() {
        let read = read_rows(&crtc);
        assert_eq!(read, scanned_rows(&mut crtc), "case {n}");
        assert_eq!(read.len(), 25 + usize::from(n == 3), "case {n}");
    }
}
