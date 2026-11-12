use super::*;
use crate::bios::modes::CRTC_MONO as MONO_80X25;
use crate::time::NANOS_PER_MILLISECOND as MS;
use alloc::vec::Vec;

/// The Hercules BIOS's graphics programming.
const HERCULES_GRAPHICS: [u8; 12] = [
    0x35, 0x2D, 0x2E, 0x07, 0x5B, 0x02, 0x57, 0x57, 0x02, 0x03, 0x00, 0x00,
];

/// Where display dot 0 of line 0 falls in the frame, in text.
const TEXT_ORIGIN: (usize, usize) = (144 - 119, 20 - 16);

fn programmed(model: Model, registers: &[u8], mode: u8) -> Mda {
    let mut card = Mda::new(model, Phosphor::Green).unwrap();
    if model == Model::Hercules {
        card.output(0x3BF, 0x03, 0);
    }
    for (index, &value) in (0u8..).zip(registers) {
        card.output(0x3B4, index, 0);
        card.output(0x3B5, value, 0);
    }
    card.output(0x3B8, mode, 0);
    card
}

fn pixel(frame: &Frame<'_>, x: usize, y: usize) -> [u8; 3] {
    let at = (y * frame.width as usize + x) * 4;
    [frame.pixels[at], frame.pixels[at + 1], frame.pixels[at + 2]]
}

fn frame_at(card: &mut Mda, t: Nanos) -> Vec<u8> {
    card.catch_up(t);
    card.take_frame()
        .map(|f| f.pixels.to_vec())
        .expect("a frame finished")
}

fn dot(frame: &[u8], x: usize, y: usize) -> [u8; 3] {
    pixel(
        &Frame {
            width: WINDOW.width,
            height: WINDOW.height,
            pixels: frame,
            displayed: None,
        },
        x,
        y,
    )
}

#[test]
fn text_lands_where_the_timing_puts_it() {
    let mut card = programmed(Model::Mda, &MONO_80X25, 0x29);
    card.write(MEMORY_BASE, 0x41, 0);
    card.write(MEMORY_BASE + 1, 0x07, 0);
    let frame = frame_at(&mut card, 60 * MS);
    let (x, y) = TEXT_ORIGIN;
    let green = Phosphor::Green.normal();
    assert_eq!(
        dot(&frame, x + 2, y + 3),
        green,
        "'A' starts on the cap line"
    );
    assert_eq!(dot(&frame, x + 1, y + 3), [0, 0, 0]);
    assert_eq!(dot(&frame, x, y + 8), green);
    assert_eq!(
        dot(&frame, x + 9 + 3, y + 3),
        [0, 0, 0],
        "the next cell is empty"
    );
}

#[test]
fn attributes_follow_the_mda_rules() {
    let mut card = programmed(Model::Mda, &MONO_80X25, 0x09);
    for (cell, attr) in [(0u32, 0x70u8), (1, 0x01), (2, 0x0F), (3, 0x00), (4, 0xF0)] {
        card.write(MEMORY_BASE + cell * 2, 0x20, 0);
        card.write(MEMORY_BASE + cell * 2 + 1, attr, 0);
    }
    let frame = frame_at(&mut card, 60 * MS);
    let (x, y) = TEXT_ORIGIN;
    assert_eq!(
        dot(&frame, x + 4, y + 5),
        Phosphor::Green.normal(),
        "reverse lights the cell"
    );
    assert_eq!(
        dot(&frame, x + 9 + 4, y + 12),
        Phosphor::Green.normal(),
        "underline on row 12"
    );
    assert_eq!(dot(&frame, x + 9 + 4, y + 11), [0, 0, 0]);
    assert_eq!(
        dot(&frame, x + 36 + 4, y + 5),
        Phosphor::Green.bright(),
        "blink off: bit 7 brightens a reverse cell"
    );
}

#[test]
fn the_ninth_column_repeats_for_line_pieces_only() {
    let mut card = programmed(Model::Mda, &MONO_80X25, 0x29);
    for (cell, code) in [(0u32, 0xC4u8), (1, 0xB4)] {
        card.write(MEMORY_BASE + cell * 2, code, 0);
        card.write(MEMORY_BASE + cell * 2 + 1, 0x07, 0);
    }
    let frame = frame_at(&mut card, 60 * MS);
    let (x, y) = TEXT_ORIGIN;
    assert_eq!(
        dot(&frame, x + 8, y + 6),
        Phosphor::Green.normal(),
        "─ joins its neighbour"
    );
    assert_eq!(
        dot(&frame, x + 9 + 8, y + 6),
        [0, 0, 0],
        "┤ has no right arm to carry"
    );
}

#[test]
fn blinking_characters_and_the_cursor_flash() {
    let mut card = programmed(Model::Mda, &MONO_80X25, 0x29);
    card.write(MEMORY_BASE, 0xDB, 0);
    card.write(MEMORY_BASE + 1, 0x87, 0);
    let (x, y) = TEXT_ORIGIN;
    let mut seen = [false; 2];
    for n in 1..80u64 {
        card.catch_up(n * 21 * MS);
        if let Some(frame) = card.take_frame() {
            seen[usize::from(pixel(&frame, x + 3, y + 5) == Phosphor::Green.normal())] = true;
        }
    }
    assert_eq!(seen, [true, true], "both phases of the blink appear");
}

#[test]
fn the_status_port_shows_retrace() {
    let mut mda = programmed(Model::Mda, &MONO_80X25, 0x29);
    let mut herc = programmed(Model::Hercules, &MONO_80X25, 0x29);
    let mut hsync = [false; 2];
    let mut vsync_bit = [false; 2];
    let mut mda_bit7 = [false; 2];
    for step in 0..40_000u64 {
        let t = step * 997;
        hsync[usize::from(mda.input(0x3BA, t) & 1)] = true;
        mda_bit7[usize::from(mda.input(0x3BA, t) >> 7)] = true;
        vsync_bit[usize::from(herc.input(0x3BA, t) >> 7)] = true;
    }
    assert_eq!(hsync, [true, true]);
    assert_eq!(
        vsync_bit,
        [true, true],
        "the Hercules card shows vertical retrace in bit 7"
    );
    assert_eq!(
        mda_bit7,
        [false, true],
        "the MDA's bit 7 never moves: how software tells them apart"
    );
}

#[test]
fn hercules_graphics_shows_its_bitmap_and_pages() {
    let mut card = programmed(Model::Hercules, &HERCULES_GRAPHICS, 0x0A);
    card.write(MEMORY_BASE, 0x80, 0);
    card.write(MEMORY_BASE + 0x2000, 0x01, 0);
    let frame = frame_at(&mut card, 70 * MS);
    let (x, y) = (128 - 119, 22 - 16);
    assert_eq!(
        dot(&frame, x, y),
        Phosphor::Green.normal(),
        "the first pixel of line 0"
    );
    assert_eq!(
        dot(&frame, x + 7, y + 1),
        Phosphor::Green.normal(),
        "line 1 comes from the second bank"
    );
    assert_eq!(dot(&frame, x + 1, y), [0, 0, 0]);
    card.output(0x3B8, 0x8A, 70 * MS);
    let frame = frame_at(&mut card, 140 * MS);
    assert_eq!(dot(&frame, x, y), [0, 0, 0], "page 1 is blank");
}

#[test]
fn the_memory_window_follows_the_card_and_its_switch() {
    let mut mda = Mda::new(Model::Mda, Phosphor::Amber).unwrap();
    assert_eq!(mda.window(), (MEMORY_BASE, 0x8000));
    mda.write(MEMORY_BASE + 0x10, 0x5A, 0);
    assert_eq!(
        mda.read(MEMORY_BASE + 0x1010),
        0x5A,
        "4 KiB repeats through the window"
    );
    let mut herc = Mda::new(Model::Hercules, Phosphor::White).unwrap();
    assert_eq!(herc.window(), (MEMORY_BASE, 0x8000));
    assert!(
        herc.output(0x3BF, 0x02, 0),
        "allowing page 1 opens its memory"
    );
    assert_eq!(herc.window(), (MEMORY_BASE, 0x1_0000));
    herc.write(MEMORY_BASE + 0x8010, 0xA5, 0);
    assert_eq!(herc.read(MEMORY_BASE + 0x8010), 0xA5);
    assert_eq!(herc.read(MEMORY_BASE + 0x0010), 0);
}

#[test]
fn text_is_read_back_from_the_page_shown_and_hercules_graphics_is_not_text() {
    let mut card = programmed(Model::Mda, &MONO_80X25, 0x29);
    card.write(MEMORY_BASE + 2 * 81, b'M', 0);
    let shown = card.shown_text().unwrap();
    assert_eq!((shown.columns, shown.rows), (80, 25));
    assert_eq!(shown.codes[81], b'M');
    let mut hercules = programmed(Model::Hercules, &MONO_80X25, 0x29 | 0x80);
    hercules.write(MEMORY_BASE + 0x8000, b'P', 0);
    hercules.write(MEMORY_BASE, b'0', 0);
    assert_eq!(
        hercules.shown_text().unwrap().codes[0],
        b'P',
        "the second page"
    );
    let graphics = programmed(Model::Hercules, &HERCULES_GRAPHICS, 0x0A);
    assert_eq!(graphics.shown_text(), Err(Unread::NoText));
}
