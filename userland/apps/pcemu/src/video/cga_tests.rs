use super::*;
use crate::video::monitor::CGA_COLOURS;
use alloc::vec::Vec;

use crate::bios::modes::{CRTC_40 as TEXT_40, CRTC_80 as TEXT_80, CRTC_GRAPHICS as GRAPHICS};
use crate::time::NANOS_PER_MILLISECOND as MS;

/// Where display dot 0 of line 0 falls in the frame.
const ORIGIN: (usize, usize) = (912 - 720 - 128, 262 - 224 - 22);

fn programmed(registers: &[u8; 16], mode: u8, colour: u8, monitor: Monitor) -> Cga {
    let mut card = Cga::new(monitor).unwrap();
    for (index, &value) in (0u8..).zip(registers) {
        card.output(0x3D4, index, 0);
        card.output(0x3D5, value, 0);
    }
    card.output(0x3D8, mode, 0);
    card.output(0x3D9, colour, 0);
    card
}

fn frame_at(card: &mut Cga, t: Nanos) -> Vec<u8> {
    card.catch_up(t);
    card.take_frame()
        .map(|f| f.pixels.to_vec())
        .expect("a frame finished")
}

fn dot(frame: &[u8], x: usize, y: usize) -> [u8; 3] {
    let at = (y * WINDOW.width as usize + x) * 4;
    [frame[at], frame[at + 1], frame[at + 2]]
}

#[test]
fn eighty_column_text_draws_its_attributes() {
    let mut card = programmed(&TEXT_80, 0x29, 0x00, Monitor::Colour);
    card.write(MEMORY_BASE, 0xDB, 0);
    card.write(MEMORY_BASE + 1, 0x1E, 0);
    let frame = frame_at(&mut card, 50 * MS);
    let (x, y) = ORIGIN;
    assert_eq!(
        dot(&frame, x + 3, y + 2),
        CGA_COLOURS[14],
        "yellow on blue, the block lit"
    );
    let mut card = programmed(&TEXT_80, 0x29, 0x00, Monitor::Colour);
    card.write(MEMORY_BASE, 0x20, 0);
    card.write(MEMORY_BASE + 1, 0x1E, 0);
    assert_eq!(
        dot(&frame_at(&mut card, 50 * MS), x + 3, y + 2),
        CGA_COLOURS[1],
        "a space shows the background"
    );
}

#[test]
fn forty_columns_double_every_dot_and_the_border_shows_its_colour() {
    let mut card = programmed(&TEXT_40, 0x28, 0x04, Monitor::Colour);
    card.write(MEMORY_BASE, 0x41, 0);
    card.write(MEMORY_BASE + 1, 0x0F, 0);
    let frame = frame_at(&mut card, 50 * MS);
    let (x, y) = ORIGIN;
    for dx in 4..10 {
        assert_eq!(
            dot(&frame, x + dx, y),
            CGA_COLOURS[15],
            "'A' row 0, cols 2-4 doubled: dot {dx}"
        );
    }
    assert_eq!(dot(&frame, x + 3, y), [0, 0, 0]);
    assert_eq!(dot(&frame, x - 4, y), CGA_COLOURS[4], "the border is red");
}

#[test]
fn medium_resolution_graphics_uses_its_palettes() {
    let mut card = programmed(&GRAPHICS, 0x0A, 0x01, Monitor::Colour);
    card.write(MEMORY_BASE, 0b0001_1011, 0);
    card.write(MEMORY_BASE + 0x2000, 0b1100_0000, 0);
    let frame = frame_at(&mut card, 50 * MS);
    let (x, y) = ORIGIN;
    let pixels: Vec<[u8; 3]> = (0..4).map(|p| dot(&frame, x + p * 2, y)).collect();
    assert_eq!(
        pixels,
        [
            CGA_COLOURS[1],
            CGA_COLOURS[2],
            CGA_COLOURS[4],
            CGA_COLOURS[6]
        ],
        "background, then palette 0"
    );
    assert_eq!(
        dot(&frame, x, y + 1),
        CGA_COLOURS[6],
        "odd lines come from the second bank"
    );
    card.output(0x3D9, 0x31, 50 * MS);
    let frame = frame_at(&mut card, 100 * MS);
    assert_eq!(
        dot(&frame, x + 6, y),
        CGA_COLOURS[15],
        "palette 1, intense: white"
    );
    card.output(0x3D8, 0x0E, 100 * MS);
    let frame = frame_at(&mut card, 150 * MS);
    assert_eq!(
        dot(&frame, x + 4, y),
        CGA_COLOURS[12],
        "the third palette: intense red"
    );
}

#[test]
fn high_resolution_graphics_draws_in_the_foreground_colour() {
    let mut card = programmed(&GRAPHICS, 0x1E, 0x0A, Monitor::Colour);
    card.write(MEMORY_BASE, 0b1010_0000, 0);
    let frame = frame_at(&mut card, 50 * MS);
    let (x, y) = ORIGIN;
    assert_eq!(
        [
            dot(&frame, x, y),
            dot(&frame, x + 1, y),
            dot(&frame, x + 2, y)
        ],
        [CGA_COLOURS[10], [0, 0, 0], CGA_COLOURS[10]]
    );
    assert_eq!(dot(&frame, x - 4, y), [0, 0, 0], "the border stays black");
}

#[test]
fn the_status_port_reports_retrace_and_the_light_pen() {
    let mut card = programmed(&TEXT_80, 0x29, 0, Monitor::Colour);
    let mut seen = [[false; 2]; 2];
    for step in 0..20_000u64 {
        let status = card.input(0x3DA, step * 1013);
        seen[0][usize::from(status & 1)] = true;
        seen[1][usize::from((status >> 3) & 1)] = true;
    }
    assert_eq!(seen, [[true, true], [true, true]]);
    assert_eq!(card.input(0x3DA, 30 * MS) & 0x02, 0);
    card.output(0x3DC, 0, 30 * MS);
    assert_ne!(
        card.input(0x3DA, 30 * MS) & 0x02,
        0,
        "presetting the latch sets its flag"
    );
    card.output(0x3DB, 0, 30 * MS);
    assert_eq!(card.input(0x3DA, 30 * MS) & 0x02, 0);
}

#[test]
fn memory_repeats_through_its_window_and_waits_for_a_slot() {
    let mut card = Cga::new(Monitor::Colour).unwrap();
    card.write(MEMORY_BASE + 5, 0x77, 0);
    assert_eq!(card.read(MEMORY_BASE + 0x4005), 0x77);
    let waits: Vec<Nanos> = (0..200).map(|t| Cga::access_wait(t * 7)).collect();
    let slot = Hertz::CRYSTAL.time_of(16);
    assert!(
        waits.iter().all(|&w| w > 0 && w <= slot + 1),
        "never more than one sixteen-dot cycle"
    );
}

#[test]
fn a_composite_monitor_decodes_artifact_colour() {
    // 1Ah keeps the colour burst that BIOS mode 6's 1Eh turns off.
    let mut card = programmed(&GRAPHICS, 0x1A, 0x0F, Monitor::Composite);
    for i in 0..80 {
        card.write(MEMORY_BASE + i, 0b1100_1100, 0);
    }
    let frame = frame_at(&mut card, 50 * MS);
    let (x, y) = ORIGIN;
    let [red, green, blue] = dot(&frame, x + 40, y);
    assert!(
        red.abs_diff(green).max(green.abs_diff(blue)) > 40,
        "a 640-pixel pattern becomes colour: {red} {green} {blue}"
    );
    card.output(0x3D8, 0x1E, 50 * MS);
    let frame = frame_at(&mut card, 100 * MS);
    let [red, green, blue] = dot(&frame, x + 40, y + 2);
    assert!(
        red == green && green == blue,
        "the mono bit drops the colour burst"
    );
}

#[test]
fn a_frame_names_the_picture_its_crtc_displays() {
    let (x, y) = (
        u32::try_from(ORIGIN.0).unwrap(),
        u32::try_from(ORIGIN.1).unwrap(),
    );
    for (registers, mode) in [(&TEXT_80, 0x29), (&TEXT_40, 0x28), (&GRAPHICS, 0x2A)] {
        let mut card = programmed(registers, mode, 0x04, Monitor::Colour);
        card.catch_up(50 * MS);
        let frame = card.take_frame().expect("a frame finished");
        assert_eq!(
            frame.displayed,
            Some(crate::video::raster::Area {
                left: x,
                top: y,
                right: x + 640,
                bottom: y + 200,
            }),
            "mode {mode:#x}: 640 dots of 200 lines, the border round them"
        );
    }
}

#[test]
fn text_is_read_back_from_the_start_address_and_graphics_is_not_text() {
    let mut card = programmed(&TEXT_80, 0x29, 0x00, Monitor::Colour);
    card.output(0x3D4, 12, 0);
    card.output(0x3D5, 0x1F, 0);
    card.output(0x3D4, 13, 0);
    card.output(0x3D5, 0xF0, 0);
    let vram = u32::try_from(VRAM).unwrap();
    let at = |cell: u32| MEMORY_BASE + (cell * 2) % vram;
    card.write(at(0x1FF0 + 80 + 2), b'H', 0);
    card.write(at(0x1FF0 + 24 * 80 + 79), b'!', 0);
    let shown = card.shown_text().unwrap();
    assert_eq!((shown.columns, shown.rows), (80, 25));
    assert_eq!(shown.codes[80 + 2], b'H');
    assert_eq!(
        shown.codes[24 * 80 + 79],
        b'!',
        "past the end of memory, round to its start"
    );
    let forty = programmed(&TEXT_40, 0x28, 0x00, Monitor::Colour);
    assert_eq!(forty.shown_text().unwrap().columns, 40);
    let graphics = programmed(&GRAPHICS, 0x2A, 0x00, Monitor::Colour);
    assert_eq!(graphics.shown_text(), Err(Unread::NoText));
}
