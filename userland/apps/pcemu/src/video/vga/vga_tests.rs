use super::*;
use crate::bios::modes::{self, Mode};
use crate::time::NANOS_PER_MILLISECOND as MS;
use crate::video::font::VGA as FONT;
use alloc::vec::Vec;

const MODE_3: Mode = modes::VGA_TEXT_80;
const MODE_13: Mode = modes::VGA_LINEAR;
const MODE_10: Mode = modes::EGA_PLANAR_350;

/// Programs `mode` as the BIOS does.
fn set_mode(card: &mut Vga, mode: &Mode) {
    card.output(0x3C2, mode.misc, 0);
    for (i, &v) in (0u8..).zip(&[0x03]).chain((1u8..).zip(&mode.seq)) {
        card.output(0x3C4, i, 0);
        card.output(0x3C5, v, 0);
    }
    let crtc = if mode.misc & 1 != 0 { 0x3D4 } else { 0x3B4 };
    card.output(crtc, 0x11, 0);
    card.output(crtc + 1, 0x00, 0);
    for (i, &v) in (0u8..).zip(&mode.crtc) {
        card.output(crtc, i, 0);
        card.output(crtc + 1, v, 0);
    }
    for (i, &v) in (0u8..).zip(&mode.gc) {
        card.output(0x3CE, i, 0);
        card.output(0x3CF, v, 0);
    }
    card.input(crtc + 6, 0);
    for (i, &v) in (0u8..).zip(mode.ac.iter().chain(&[0])) {
        card.output(0x3C0, i, 0);
        card.output(0x3C0, v, 0);
    }
    card.output(0x3C0, 0x20, 0);
}

fn set_dac(card: &mut Vga, index: u8, rgb: [u8; 3]) {
    card.output(0x3C8, index, 0);
    for c in rgb {
        card.output(0x3C9, c, 0);
    }
}

fn frame(card: &mut Vga, t: Nanos) -> (u32, u32, Vec<u8>) {
    card.catch_up(t);
    let f = card.take_frame().expect("a frame finished");
    (f.width, f.height, f.pixels.to_vec())
}

fn dot(frame: &(u32, u32, Vec<u8>), x: u32, y: u32) -> [u8; 3] {
    let at = ((y * frame.0 + x) * 4) as usize;
    [frame.2[at], frame.2[at + 1], frame.2[at + 2]]
}

#[test]
fn mode_3_text_shows_its_attributes_through_the_dac() {
    let mut card = Vga::new(Generation::Vga, Monitor::Colour).unwrap();
    set_mode(&mut card, &MODE_3);
    card.load_font(0, |code, row| FONT.row(code, row), 16);
    set_dac(&mut card, 0x01, [0, 0, 42]);
    set_dac(&mut card, 0x3E, [63, 63, 21]);
    card.write(0xB_8000, 0x41, 0);
    card.write(0xB_8001, 0x1E, 0);
    let f = frame(&mut card, 60 * MS);
    assert_eq!(
        (f.0, f.1),
        (82 * 9, 414),
        "blanking leaves a border of two characters and eight lines"
    );
    let (x, y) = (2 * 9, 8);
    assert_eq!(
        dot(&f, x + 3, y + 4),
        [255, 255, 85],
        "'A' row 3 of the 8×14 shape, a row down in 8×16"
    );
    assert_eq!(dot(&f, x, y + 4), [0, 0, 170], "on blue");
    assert_eq!(dot(&f, 0, y), [0, 0, 0], "the border is overscan colour 0");
}

#[test]
fn mode_13h_shows_chained_pixels_two_dots_wide() {
    let mut card = Vga::new(Generation::Vga, Monitor::Colour).unwrap();
    set_mode(&mut card, &MODE_13);
    set_dac(&mut card, 5, [63, 0, 0]);
    set_dac(&mut card, 6, [0, 63, 0]);
    card.write(0xA_0000, 5, 0);
    card.write(0xA_0001, 6, 0);
    card.write(0xA_0000 + 320, 6, 0);
    let f = frame(&mut card, 60 * MS);
    let (x, y) = (2 * 8, 8);
    assert_eq!(
        [dot(&f, x, y), dot(&f, x + 1, y), dot(&f, x + 2, y)],
        [[255, 0, 0], [255, 0, 0], [0, 255, 0]]
    );
    assert_eq!(dot(&f, x, y + 1), [255, 0, 0], "each row is scanned twice");
    assert_eq!(dot(&f, x, y + 2), [0, 255, 0]);
    assert_eq!(
        card.read(0xA_0001),
        6,
        "chain-4 reads back through the plane its address picks"
    );
}

#[test]
fn ega_mode_10h_paints_planar_pixels_in_64_colours() {
    let mut card = Vga::new(Generation::Ega, Monitor::Colour).unwrap();
    set_mode(&mut card, &MODE_10);
    // Write mode 2, colour 14 (palette 3Eh) into the leftmost pixel.
    card.output(0x3CE, 5, 0);
    card.output(0x3CF, 0x02, 0);
    card.output(0x3CE, 8, 0);
    card.output(0x3CF, 0x80, 0);
    card.read(0xA_0000);
    card.write(0xA_0000, 14, 0);
    let f = frame(&mut card, 60 * MS);
    let lit: Vec<(u32, u32)> = (0..f.1)
        .flat_map(|yy| (0..f.0).map(move |xx| (xx, yy)))
        .filter(|&(xx, yy)| dot(&f, xx, yy) != [0, 0, 0])
        .collect();
    assert_eq!(lit.len(), 1, "one pixel lit");
    assert_eq!(
        dot(&f, lit[0].0, lit[0].1),
        [0xFF, 0xFF, 0x55],
        "3Eh: bright yellow from rgbRGB"
    );
}

#[test]
fn the_dac_reads_back_and_increments() {
    let mut card = Vga::new(Generation::Vga, Monitor::Colour).unwrap();
    set_dac(&mut card, 10, [1, 2, 3]);
    set_dac(&mut card, 11, [4, 5, 63]);
    card.output(0x3C7, 10, 0);
    let values: Vec<u8> = (0..6).map(|_| card.input(0x3C9, 0)).collect();
    assert_eq!(values, [1, 2, 3, 4, 5, 63]);
    assert_eq!(card.input(0x3C7, 0), 3, "the DAC is reading");
    assert_eq!(card.input(0x3C8, 0), 12, "the write index moved on");
}

#[test]
fn only_the_vga_reads_its_registers_back() {
    let mut vga = Vga::new(Generation::Vga, Monitor::Colour).unwrap();
    let mut ega = Vga::new(Generation::Ega, Monitor::Colour).unwrap();
    for card in [&mut vga, &mut ega] {
        set_mode(card, &MODE_3);
        card.output(0x3CE, 8, 0);
    }
    assert_eq!((vga.input(0x3CF, 0), ega.input(0x3CF, 0)), (0xFF, 0xFF));
    vga.output(0x3CF, 0x5A, 0);
    ega.output(0x3CF, 0x5A, 0);
    assert_eq!(vga.input(0x3CF, 0), 0x5A);
    assert_eq!(
        ega.input(0x3CF, 0),
        0xFF,
        "the EGA's controller is write-only"
    );
    assert_eq!(vga.input(0x3CC, 0), 0x67);
}

#[test]
fn the_misc_output_moves_the_crt_controller_and_status_ports() {
    let mut card = Vga::new(Generation::Vga, Monitor::Colour).unwrap();
    assert_eq!(card.ports(), MONO_PORTS);
    assert!(card.output(0x3C2, 0x67, 0).ports);
    assert_eq!(card.ports(), COLOUR_PORTS);
    card.output(0x3D4, 0x0E, 0);
    card.output(0x3D5, 0x12, 0);
    assert_eq!(card.input(0x3D5, 0), 0x12);
    assert_eq!(
        card.input(0x3B5, 0),
        0xFF,
        "the mono address no longer answers"
    );
    card.output(0x3CE, 6, 0);
    assert!(
        card.output(0x3CF, 0x05, 0).window,
        "memory map 1: A000h for 64 KiB"
    );
    assert_eq!(card.window(), (0xA_0000, 0x1_0000));
}

#[test]
fn the_vertical_interrupt_reaches_irq_2() {
    let mut card = Vga::new(Generation::Ega, Monitor::Colour).unwrap();
    set_mode(&mut card, &MODE_10);
    card.output(0x3D4, 0x11, 0);
    card.output(0x3D5, 0x0B, 0);
    card.catch_up(40 * MS);
    assert!(card.irq());
    assert_ne!(
        card.input(0x3C2, 40 * MS) & 0x80,
        0,
        "input status 0 shows it pending"
    );
    card.output(0x3D5, 0x2B, 40 * MS);
    assert!(!card.irq());
}

#[test]
fn pixel_panning_shifts_the_display_left() {
    let mut plain = Vga::new(Generation::Vga, Monitor::Colour).unwrap();
    let mut panned = Vga::new(Generation::Vga, Monitor::Colour).unwrap();
    for card in [&mut plain, &mut panned] {
        set_mode(card, &MODE_13);
        set_dac(card, 7, [63, 63, 63]);
        card.write(0xA_0001, 7, 0);
    }
    // Reading input status 1 sends the attribute flip-flop back to the
    // index, as software must before addressing it.
    panned.input(0x3DA, 0);
    panned.output(0x3C0, 0x33, 0);
    panned.output(0x3C0, 0x02, 0);
    let a = frame(&mut plain, 60 * MS);
    let b = frame(&mut panned, 60 * MS);
    let (x, y) = (2 * 8, 8);
    assert_eq!(dot(&a, x + 2, y), [255, 255, 255]);
    assert_eq!(
        dot(&b, x, y),
        [255, 255, 255],
        "two dots of panning bring pixel 1 to the edge"
    );
}

#[test]
fn the_split_screen_shows_memory_from_zero_below_the_compare() {
    let mut card = Vga::new(Generation::Vga, Monitor::Colour).unwrap();
    set_mode(&mut card, &MODE_13);
    set_dac(&mut card, 9, [0, 0, 63]);
    card.write(0xA_0000, 9, 0);
    card.output(0x3D4, 0x0C, 0);
    card.output(0x3D5, 0x08, 0);
    card.output(0x3D4, 0x07, 0);
    card.output(0x3D5, 0x0F, 0);
    card.output(0x3D4, 0x09, 0);
    card.output(0x3D5, 0x01, 0);
    card.output(0x3D4, 0x18, 0);
    card.output(0x3D5, 199, 0);
    let f = frame(&mut card, 60 * MS);
    let (x, y) = (2 * 8, 8);
    assert_eq!(dot(&f, x, y), [0, 0, 0], "the top starts at 2000h");
    assert_eq!(
        dot(&f, x, y + 200),
        [0, 0, 255],
        "the line after the compare shows address 0"
    );
}

#[test]
fn the_predicted_interrupt_latches_at_its_time() {
    let mut card = Vga::new(Generation::Ega, Monitor::Colour).unwrap();
    set_mode(&mut card, &MODE_10);
    card.output(0x3D4, 0x11, 0);
    card.output(0x3D5, 0x1B, 0);
    let mut times = Vec::new();
    for _ in 0..6 {
        let at = card.next_interrupt().expect("enabled");
        card.catch_up(at - 1);
        assert!(!card.irq(), "not before its time");
        card.catch_up(at);
        assert!(card.irq(), "latched by its time");
        times.push(at);
        card.output(0x3D5, 0x0B, at);
        card.output(0x3D5, 0x1B, at);
    }
    let periods: Vec<Nanos> = times.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(
        periods.iter().all(|&p| p.abs_diff(periods[0]) <= 1),
        "one a frame: {periods:?}"
    );
}

#[test]
fn the_memory_answers_only_while_the_misc_output_and_enable_connect_it() {
    let mut card = Vga::new(Generation::Vga, Monitor::Colour).unwrap();
    assert_eq!(
        card.window().1,
        0,
        "power-up leaves the memory disconnected"
    );
    assert!(card.output(0x3C2, 0x63, 0).window);
    assert_eq!(card.window(), (0xA_0000, 0x2_0000));
    let moved = card.output(0x3C3, 0x00, 0);
    assert!(moved.ports && moved.window);
    assert_eq!(card.window().1, 0);
    assert_eq!(card.ports(), &[(0x3C3, 0x3C3)]);
    assert_eq!((card.input(0x3CC, 0), card.input(0x3C3, 0)), (0xFF, 0));
    card.output(0x3C2, 0x00, 0);
    card.output(0x3C3, 0x01, 0);
    assert_eq!(
        card.input(0x3CC, 0),
        0x63,
        "the disabled card took no write"
    );
    assert!(card.output(0x3C2, 0x61, 0).window);
    assert_eq!(card.window().1, 0, "the misc output's RAM enable");
}

#[test]
fn text_is_read_back_from_where_the_display_shows_it() {
    let mut card = Vga::new(Generation::Vga, Monitor::Colour).unwrap();
    set_mode(&mut card, &MODE_3);
    for row in 0..30u32 {
        card.write(0xB_8000 + row * 160, b'A' + u8::try_from(row).unwrap(), 0);
    }
    card.output(0x3D4, 0x0C, 0);
    card.output(0x3D5, 0x01, 0);
    card.output(0x3D4, 0x0D, 0);
    card.output(0x3D5, 0x90, 0);
    for (index, value) in [(0x07, 0x0F), (0x09, 0x0F), (0x18, 191)] {
        card.output(0x3D4, index, 0);
        card.output(0x3D5, value, 0);
    }
    let shown = card.shown_text().unwrap();
    assert_eq!((shown.columns, shown.rows), (80, 25));
    let firsts: Vec<u8> = shown.codes.chunks(80).map(|row| row[0]).collect();
    let mut expected: Vec<u8> = (b'F'..b'F' + 12).collect();
    expected.extend(b'A'..b'A' + 13);
    assert_eq!(
        firsts, expected,
        "five rows down, then from the top below the split"
    );
    set_mode(&mut card, &MODE_13);
    assert_eq!(card.shown_text(), Err(Unread::NoText));
}
