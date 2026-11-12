use super::super::run_until;
use super::super::tests::{booted, call, marked, poke, row};
use super::*;
use crate::board::BoardConfig;
use crate::cpu::Bus;
use crate::machine::{Machine, Template};
use crate::time::NANOS_PER_SECOND;
use crate::video::monitor::Phosphor;
use alloc::vec::Vec;

fn vga() -> Machine {
    booted(Template::TurboXt, Some(marked(1440)))
}

fn ega() -> Machine {
    booted(Template::Workstation186, Some(marked(1440)))
}

fn port_in(m: &mut Machine, port: u16) -> u8 {
    let at = m.board().cycles_at(m.now());
    m.board_mut().input(port, at)
}

fn port_out(m: &mut Machine, port: u16, value: u8) {
    let at = m.board().cycles_at(m.now());
    m.board_mut().output(port, value, at);
}

fn register(m: &mut Machine, port: u16, index: u8) -> u8 {
    port_out(m, port, index);
    port_in(m, port + 1)
}

fn peek16(m: &Machine, addr: u32) -> u16 {
    u16::from_le_bytes([m.board().peek(addr), m.board().peek(addr + 1)])
}

/// The save pointer table copied to `0:5000h`, which `40:A8h` then names.
fn own_save_pointers(m: &mut Machine) {
    let table = (u32::from(peek16(m, 0x4AA)) << 4) + u32::from(peek16(m, 0x4A8));
    let bytes: Vec<u8> = (0..28).map(|i| m.board().peek(table + i)).collect();
    poke(m, 0x5000, &bytes);
    poke(m, 0x4A8, &[0x00, 0x50, 0x00, 0x00]);
}

/// Points save pointer `field` at `0:offset`.
fn set_pointer(m: &mut Machine, field: u32, offset: u16) {
    let [low, high] = offset.to_le_bytes();
    poke(m, 0x5000 + field, &[low, high, 0, 0]);
}

/// The VGA's state, from AH 1Bh into `0:5000h`.
fn state(m: &mut Machine) -> Vec<u8> {
    let answer = call(m, 0x10, |r| {
        r.set16(0, 0x1B00);
        r.set16(3, 0);
        r.set_sreg(Seg::Es, 0);
        r.set16(7, 0x5000);
    });
    assert_eq!(answer.get8(0), 0x1B);
    (0..64).map(|i| m.board().peek(0x5000 + i)).collect()
}

fn option(m: &mut Machine, bl: u8, al: u8) -> u8 {
    call(m, 0x10, |r| {
        r.set16(0, 0x1200 | u16::from(al));
        r.set16(3, u16::from(bl));
    })
    .get8(0)
}

fn dac(m: &mut Machine, entry: u8) -> [u8; 3] {
    let colour = call(m, 0x10, |r| {
        r.set16(0, 0x1015);
        r.set16(3, u16::from(entry));
    });
    [colour.get8(6), colour.get8(5), colour.get8(1)]
}

fn set_mode(m: &mut Machine, mode: u8) {
    call(m, 0x10, |r| r.set16(0, u16::from(mode)));
}

#[test]
fn the_vga_rom_sets_its_text_mode_and_shows_the_banner() {
    let mut m = vga();
    assert!(
        row(&m, 0).starts_with("TAIRiX PC BIOS    640 KB"),
        "{:?}",
        row(&m, 0)
    );
    assert_eq!(m.board().peek(0x449), 3, "mode 3");
    assert_eq!(m.board().peek(0x484), 24, "25 rows");
    let frame = m.board_mut().frame(0).expect("a frame");
    assert_eq!(
        (frame.width, frame.height),
        (738, 414),
        "720 × 400 inside the overscan border"
    );
    assert!(
        frame.pixels.chunks(4).any(|p| p[0] > 0x80),
        "the banner's letters are lit"
    );
    let combination = call(&mut m, 0x10, |r| r.set16(0, 0x1A00));
    assert_eq!(
        (combination.get8(0), combination.get8(3)),
        (0x1A, 0x08),
        "an analog colour VGA"
    );
    let info = call(&mut m, 0x10, |r| {
        r.set16(0, 0x1200);
        r.set16(3, 0x0010);
    });
    assert_eq!((info.get8(7), info.get8(3)), (0, 3), "colour, 256 KiB");
    assert_eq!(
        info.get8(1),
        0x09,
        "the switches of an Enhanced Color Display"
    );
}

#[test]
fn mode_13h_takes_a_pixel_in_the_default_palette() {
    let mut m = vga();
    set_mode(&mut m, 0x13);
    assert_eq!(m.board().peek(0x449), 0x13);
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x0C2C);
        r.set16(1, 160);
        r.set16(2, 100);
    });
    assert_eq!(m.board().peek(0xA_0000 + 100 * 320 + 160), 0x2C);
    let back = call(&mut m, 0x10, |r| {
        r.set16(0, 0x0D00);
        r.set16(1, 160);
        r.set16(2, 100);
    });
    assert_eq!(back.get8(0), 0x2C);
    assert_eq!(dac(&mut m, 1), [0, 0, 0x2A], "entry 1 is the EGA's blue");
}

#[test]
fn mode_12h_reads_back_the_colour_it_was_given() {
    let mut m = vga();
    set_mode(&mut m, 0x12);
    for (x, colour) in [(10u16, 5u8), (11, 12), (639, 15)] {
        call(&mut m, 0x10, |r| {
            r.set16(0, 0x0C00 | u16::from(colour));
            r.set16(1, x);
            r.set16(2, 479);
        });
    }
    for (x, colour) in [(10u16, 5u8), (11, 12), (639, 15), (12, 0)] {
        let back = call(&mut m, 0x10, |r| {
            r.set16(0, 0x0D00);
            r.set16(1, x);
            r.set16(2, 479);
        });
        assert_eq!(back.get8(0), colour, "pixel {x}");
    }
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x0E41);
        r.set16(3, 0x000E);
    });
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x0200);
        r.set16(2, 0x0000);
    });
    let back = call(&mut m, 0x10, |r| r.set16(0, 0x0800));
    assert_eq!(
        back.get8(0),
        b'A',
        "the teletype drew in graphics, and it reads back"
    );
}

#[test]
fn the_8x8_font_gives_the_vga_fifty_rows_and_ends_the_display_there() {
    let mut m = vga();
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x1112);
        r.set16(3, 0x0000);
    });
    assert_eq!(m.board().peek(0x484), 49, "50 rows");
    assert_eq!(m.board().peek(0x485), 8);
    let info = call(&mut m, 0x10, |r| r.set16(0, 0x1130));
    assert_eq!((info.get16(1), info.get8(2)), (8, 49));
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x1111);
        r.set16(3, 0x0000);
    });
    assert_eq!(m.board().peek(0x484), 27, "28 rows of 14 lines");
    let end = u16::from(register(&mut m, 0x3D4, 0x12))
        | (u16::from(register(&mut m, 0x3D4, 0x07) & 0x02) << 7);
    assert_eq!(end, 28 * 14 - 1, "no part of a 29th row shows");
}

#[test]
fn write_string_places_attributes_and_can_leave_the_cursor() {
    let mut m = vga();
    poke(&mut m, 0x5000, &[b'H', 0x1E, b'i', 0x4F]);
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x1303);
        r.set16(3, 0x0000);
        r.set16(1, 2);
        r.set16(2, 0x0A05);
        r.set_sreg(Seg::Es, 0);
        r.set16(5, 0x5000);
    });
    let cell = |m: &Machine, column: u32| {
        let at = 0xB_8000 + (10 * 80 + column) * 2;
        (m.board().peek(at), m.board().peek(at + 1))
    };
    assert_eq!((cell(&m, 5), cell(&m, 6)), ((b'H', 0x1E), (b'i', 0x4F)));
    let cursor = call(&mut m, 0x10, |r| r.set16(0, 0x0300));
    assert_eq!(
        cursor.get16(2),
        0x0A07,
        "mode 3 moves the cursor past the string"
    );
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x1302);
        r.set16(3, 0x0000);
        r.set16(1, 2);
        r.set16(2, 0x0C00);
        r.set_sreg(Seg::Es, 0);
        r.set16(5, 0x5000);
    });
    let cursor = call(&mut m, 0x10, |r| r.set16(0, 0x0300));
    assert_eq!(
        cursor.get16(2),
        0x0A07,
        "mode 2 leaves the cursor where it was"
    );
}

#[test]
fn the_ega_rom_draws_planar_pixels() {
    let mut m = ega();
    assert!(row(&m, 0).starts_with("TAIRiX PC BIOS"), "{:?}", row(&m, 0));
    set_mode(&mut m, 0x10);
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x0C09);
        r.set16(1, 320);
        r.set16(2, 200);
    });
    let back = call(&mut m, 0x10, |r| {
        r.set16(0, 0x0D00);
        r.set16(1, 320);
        r.set16(2, 200);
    });
    assert_eq!(back.get8(0), 9);
}

#[test]
fn the_cga_cursor_is_moved_into_the_cell_unless_emulation_is_off() {
    let mut m = vga();
    let shape = call(&mut m, 0x10, |r| r.set16(0, 0x0300));
    assert_eq!(
        shape.get16(1),
        0x0607,
        "the data area keeps the CGA's underline"
    );
    let cursor = |m: &mut Machine| (register(m, 0x3D4, 0x0A), register(m, 0x3D4, 0x0B));
    assert_eq!(cursor(&mut m), (0x0D, 0x0E));
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x0100);
        r.set16(1, 0x0007);
    });
    assert_eq!(
        cursor(&mut m),
        (0x00, 0x0E),
        "the block, above the cell's foot"
    );
    assert_eq!(option(&mut m, 0x34, 1), 0x12);
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x0100);
        r.set16(1, 0x0607);
    });
    assert_eq!(cursor(&mut m), (0x06, 0x07), "the lines as given");
}

#[test]
fn the_next_text_mode_takes_the_lines_asked_for_then_the_vgas_own() {
    let mut m = vga();
    for (al, height, lines) in [(0u8, 8u8, 0u8), (1, 14, 1), (2, 16, 2)] {
        assert_eq!(option(&mut m, 0x30, al), 0x12);
        set_mode(&mut m, 3);
        assert_eq!(m.board().peek(0x485), height, "lines {al}");
        assert_eq!(state(&mut m)[0x2A], lines);
        assert_eq!(
            m.board().peek(0x489) & 0x90,
            0x10,
            "400 lines for the mode after"
        );
    }
    assert_eq!(option(&mut m, 0x30, 1), 0x12);
    set_mode(&mut m, 7);
    assert_eq!(m.board().peek(0x485), 14, "monochrome at the MDA's 350");
    set_mode(&mut m, 3);
    assert_eq!(m.board().peek(0x485), 16);
    assert_ne!(option(&mut m, 0x30, 3), 0x12, "no fourth resolution");
}

#[test]
fn palette_loading_and_grey_summing_follow_their_options() {
    let mut m = vga();
    let register_1 = |m: &mut Machine| {
        call(m, 0x10, |r| {
            r.set16(0, 0x1007);
            r.set16(3, 0x0001);
        })
        .get8(7)
    };
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x1000);
        r.set16(3, 0x3F01);
    });
    assert_eq!(register_1(&mut m), 0x3F);
    assert_eq!(option(&mut m, 0x31, 1), 0x12);
    set_mode(&mut m, 3);
    assert_eq!(
        register_1(&mut m),
        0x3F,
        "the mode set left the palette alone"
    );
    option(&mut m, 0x31, 0);
    set_mode(&mut m, 3);
    assert_eq!(register_1(&mut m), 0x01);
    assert_eq!(option(&mut m, 0x33, 0), 0x12);
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x1010);
        r.set16(3, 0x0005);
        r.set16(2, 0x3F00);
        r.set16(1, 0x0000);
    });
    assert_eq!(dac(&mut m, 5), [19; 3], "red, summed to its grey");
}

#[test]
fn video_and_refresh_switch_off_and_on() {
    let mut m = vga();
    assert_eq!(option(&mut m, 0x32, 1), 0x12);
    m.board_mut().write(0xB_8000, b'Z', 0);
    assert_ne!(m.board().peek(0xB_8000), b'Z', "the memory is disconnected");
    assert_eq!(
        port_in(&mut m, 0x3CC),
        0xFF,
        "and every register but the enable"
    );
    option(&mut m, 0x32, 0);
    m.board_mut().write(0xB_8000, b'Z', 0);
    assert_eq!(m.board().peek(0xB_8000), b'Z');
    assert_eq!(
        port_in(&mut m, 0x3CC),
        0x67,
        "the misc output as the mode left it"
    );
    assert_eq!(option(&mut m, 0x36, 1), 0x12);
    assert_ne!(register(&mut m, 0x3C4, 1) & 0x20, 0, "the screen is off");
    option(&mut m, 0x36, 0);
    assert_eq!(register(&mut m, 0x3C4, 1) & 0x20, 0);
}

#[test]
fn the_state_reports_the_mode_and_where_the_static_table_is() {
    let mut m = vga();
    let state = state(&mut m);
    let word = |i: usize| u16::from_le_bytes([state[i], state[i + 1]]);
    assert_eq!((state[4], word(5)), (3, 80));
    assert_eq!((word(0x1B), word(0x1E)), (0x0607, 0x3D4));
    assert_eq!((state[0x22], word(0x23)), (25, 16));
    assert_eq!((state[0x25], state[0x26]), (0x08, 0x00));
    assert_eq!((word(0x27), state[0x29], state[0x2A]), (16, 8, 2));
    assert_eq!(state[0x31], 3, "256 KiB");
    assert_eq!(state[0x2D] & 0x30, 0x30, "cursor emulation on, blinking");
    let table = (u32::from(word(2)) << 4) + u32::from(word(0));
    assert_eq!(
        (m.board().peek(table), m.board().peek(table + 7)),
        (0xFF, 0x07),
        "modes 0–7; 200, 350 and 400 lines"
    );
}

#[test]
fn the_state_saved_comes_back() {
    let mut m = vga();
    let size = call(&mut m, 0x10, |r| {
        r.set16(0, 0x1C00);
        r.set16(1, 7);
    });
    assert_eq!((size.get8(0), size.get16(3)), (0x1C, 15));
    let saved = call(&mut m, 0x10, |r| {
        r.set16(0, 0x1C01);
        r.set16(1, 7);
        r.set_sreg(Seg::Es, 0);
        r.set16(3, 0x6000);
    });
    assert_eq!(saved.get8(0), 0x1C);
    set_mode(&mut m, 0x13);
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x1010);
        r.set16(3, 0x0001);
        r.set16(2, 0x0100);
        r.set16(1, 0x0203);
    });
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x1C02);
        r.set16(1, 7);
        r.set_sreg(Seg::Es, 0);
        r.set16(3, 0x6000);
    });
    assert_eq!(m.board().peek(0x449), 3);
    assert_eq!(port_in(&mut m, 0x3CC), 0x67);
    assert_eq!(register(&mut m, 0x3D4, 0x09), 0x4F, "16-line text");
    assert_eq!(dac(&mut m, 1), [0, 0, 0x2A]);
}

#[test]
fn the_display_combination_is_recorded_as_the_tables_entry() {
    let mut m = vga();
    let set = call(&mut m, 0x10, |r| {
        r.set16(0, 0x1A01);
        r.set16(3, 0x0108);
    });
    assert_eq!(set.get8(0), 0x1A);
    let read = |m: &mut Machine| call(m, 0x10, |r| r.set16(0, 0x1A00)).get16(3);
    assert_eq!(read(&mut m), 0x0108);
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x1A01);
        r.set16(3, 0x0906);
    });
    assert_eq!(read(&mut m), 0xFFFF, "no such combination");
}

#[test]
fn a_parameter_table_of_the_programs_own_sets_the_mode() {
    let mut m = vga();
    own_save_pointers(&mut m);
    let entries = u32::try_from(modes::VGA_ENTRIES * Mode::BYTES).unwrap();
    let table: Vec<u8> = (0..entries)
        .map(|i| m.board().peek(BASE + u32::from(PARAMETERS) + i))
        .collect();
    poke(&mut m, 0x5100, &table);
    // Mode 3's entry at 400 lines, its misc output on the 25 MHz clock.
    poke(&mut m, 0x5100 + 0x18 * 64 + 9, &[0x63]);
    set_pointer(&mut m, 0, 0x5100);
    set_mode(&mut m, 3);
    assert_eq!(port_in(&mut m, 0x3CC), 0x63);
}

/// A byte of plane 2, opened as fonts are written.
fn plane_2(m: &mut Machine, offset: u32) -> u8 {
    for (port, index, value) in [
        (0x3C4, 2, 4),
        (0x3C4, 4, 7),
        (0x3CE, 4, 2),
        (0x3CE, 5, 0),
        (0x3CE, 6, 4),
    ] {
        port_out(m, port, index);
        port_out(m, port + 1, value);
    }
    m.board().peek(0xA_0000 + offset)
}

#[test]
fn an_alpha_font_override_is_loaded_with_its_mode() {
    let mut m = vga();
    own_save_pointers(&mut m);
    // Sixteen lines into block 0, one glyph from 'A', the font at 0:5500h,
    // the mode's own rows, for mode 3.
    poke(
        &mut m,
        0x5400,
        &[16, 0, 1, 0, 0x41, 0, 0x00, 0x55, 0x00, 0x00, 0xFF, 3, 0xFF],
    );
    poke(&mut m, 0x5500, &[0xAA; 16]);
    set_pointer(&mut m, 8, 0x5400);
    set_mode(&mut m, 3);
    assert_eq!(plane_2(&mut m, 0x41 * 32), 0xAA);
    assert_eq!(
        plane_2(&mut m, 0x42 * 32 + 15),
        font::VGA.glyph(b'B')[15],
        "the rest the ROM's"
    );
}

#[test]
fn the_dynamic_save_area_keeps_the_palette_as_it_is_set() {
    let mut m = vga();
    own_save_pointers(&mut m);
    set_pointer(&mut m, 4, 0x5800);
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x1000);
        r.set16(3, 0x3F02);
    });
    assert_eq!(m.board().peek(0x5802), 0x3F);
    set_mode(&mut m, 3);
    let kept: Vec<u8> = (0..17).map(|i| m.board().peek(0x5800 + i)).collect();
    assert_eq!(kept[..16], modes::VGA_TEXT_80.ac[..16]);
    assert_eq!(kept[16], 0, "the border");
}

#[test]
fn the_alternate_print_screen_prints_every_row() {
    let mut m = vga();
    // INT 17h counts each character at 0:05F0h, clear of the print status
    // at 0050:0000, and answers a selected, ready printer.
    poke(&mut m, 0x0700, &[0xFF, 0x06, 0xF0, 0x05, 0xB4, 0x90, 0xCF]);
    poke(&mut m, 0x17 * 4, &[0x00, 0x07, 0x00, 0x00]);
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x1112);
        r.set16(3, 0x0000);
    });
    let printed = |m: &mut Machine| {
        poke(m, 0x05F0, &[0, 0]);
        call(m, 0x05, |_| {});
        peek16(m, 0x05F0)
    };
    assert_eq!(
        printed(&mut m),
        2 + 25 * 82,
        "the system BIOS's prints 25 rows"
    );
    option(&mut m, 0x20, 0);
    assert_eq!(printed(&mut m), 2 + 50 * 82);
}

#[test]
fn the_vgas_text_has_eight_pages() {
    let mut m = vga();
    call(&mut m, 0x10, |r| r.set16(0, 0x0505));
    assert_eq!(m.board().peek(0x462), 5);
    assert_eq!(peek16(&m, 0x44E), 0x5000);
    let start = (register(&mut m, 0x3D4, 0x0C), register(&mut m, 0x3D4, 0x0D));
    assert_eq!(start, (0x28, 0x00), "the start address in words");
}

#[test]
fn the_egas_monochrome_graphics_shows_each_byte_where_it_is() {
    let mut config: BoardConfig = Template::Workstation186.config();
    config.video = [Some(Adapter::Ega(Monitor::Mono(Phosphor::Green))), None];
    let mut m = crate::bios::machine(&config, [None, None]).unwrap();
    m.board_mut().insert_floppy(0, marked(1440)).unwrap();
    run_until(&mut m, 3 * NANOS_PER_SECOND);
    assert_eq!(
        m.board().peek(0x449),
        7,
        "a monochrome display starts in mode 7"
    );
    let banner = m.board_mut().frame(0).expect("a frame");
    assert!(
        banner
            .pixels
            .chunks(4)
            .any(|p| p[..3] == Phosphor::Green.normal()),
        "the banner in the display's normal intensity"
    );
    set_mode(&mut m, 0x0F);
    for x in [0u16, 8] {
        call(&mut m, 0x10, |r| {
            r.set16(0, 0x0C01);
            r.set16(1, x);
            r.set16(2, 0);
        });
    }
    let now = m.now();
    run_until(&mut m, now + NANOS_PER_SECOND / 10);
    let frame = m.board_mut().frame(0).expect("a frame");
    let width = usize::try_from(frame.width).unwrap();
    let lit: Vec<(usize, usize)> = (0..)
        .zip(frame.pixels.chunks(4))
        .filter(|(_, p)| p[..3] == Phosphor::Green.normal())
        .map(|(i, _)| (i % width, i / width))
        .collect();
    assert_eq!(lit.len(), 2, "{lit:?}");
    assert_eq!(
        (lit[1].0 - lit[0].0, lit[1].1),
        (8, lit[0].1),
        "the second byte shown"
    );
}

#[test]
fn the_cga_cursor_moves_into_each_cell_as_each_card_shows_it() {
    assert_eq!(emulated(6, 7, 16, true), (13, 14));
    assert_eq!(emulated(6, 7, 14, true), (11, 12));
    assert_eq!(emulated(6, 7, 8, true), (6, 7));
    assert_eq!(emulated(0, 7, 16, true), (0, 14));
    assert_eq!(
        emulated(0x20, 7, 16, true),
        (0x20, 14),
        "hidden stays hidden"
    );
    assert_eq!(
        emulated(0x0D, 0x0E, 16, true),
        (0x0D, 0x0E),
        "the program's own lines"
    );
    assert_eq!(
        emulated(6, 7, 14, false),
        (11, 13),
        "the EGA stops before its end"
    );
    assert_eq!(
        emulated(6, 7, 8, false),
        (6, 0),
        "the last line, through the split"
    );
    assert_eq!(emulated(0, 7, 8, false), (0, 0x1F), "the whole cell");
    assert_eq!(
        emulated(5, 4, 8, false),
        (0, 0x1F),
        "a split that leaves no gap"
    );
}
