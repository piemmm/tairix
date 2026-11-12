//! The eight screen modes: geometry, colour depth, and the 6845 and video
//! ULA settings that make each one.

use super::latch;
use crate::machine::latch as machine_latch;
use crate::rom::asm::Asm;

/// What one mode is.
struct Mode {
    bytes_per_character: u8,
    columns: u8,
    rows: u8,
    /// The first page of screen memory.
    screen: u8,
    /// Screen memory's size in pages.
    size: u8,
    map_type: u8,
    ula: u8,
    /// Which of the 6845 register sets.
    crtc: u8,
    /// Pixels a byte holds.
    pixels_per_byte: u8,
    /// The graphics window's last pixel across; 79 in the text modes, as
    /// MOS 1.20 sets it.
    last_pixel: u16,
    /// The right shift taking an external x to a pixel.
    x_shift: u8,
}

const MODES: [Mode; 8] = [
    Mode {
        bytes_per_character: 8,
        columns: 80,
        rows: 32,
        screen: 0x30,
        size: 0x50,
        map_type: 0,
        ula: 0x9C,
        crtc: 0,
        pixels_per_byte: 8,
        last_pixel: 639,
        x_shift: 1,
    },
    Mode {
        bytes_per_character: 16,
        columns: 40,
        rows: 32,
        screen: 0x30,
        size: 0x50,
        map_type: 0,
        ula: 0xD8,
        crtc: 0,
        pixels_per_byte: 4,
        last_pixel: 319,
        x_shift: 2,
    },
    Mode {
        bytes_per_character: 32,
        columns: 20,
        rows: 32,
        screen: 0x30,
        size: 0x50,
        map_type: 0,
        ula: 0xF4,
        crtc: 0,
        pixels_per_byte: 2,
        last_pixel: 159,
        x_shift: 3,
    },
    Mode {
        bytes_per_character: 8,
        columns: 80,
        rows: 25,
        screen: 0x40,
        size: 0x40,
        map_type: 1,
        ula: 0x9C,
        crtc: 1,
        pixels_per_byte: 8,
        last_pixel: 79,
        x_shift: 1,
    },
    Mode {
        bytes_per_character: 8,
        columns: 40,
        rows: 32,
        screen: 0x58,
        size: 0x28,
        map_type: 2,
        ula: 0x88,
        crtc: 2,
        pixels_per_byte: 8,
        last_pixel: 319,
        x_shift: 2,
    },
    Mode {
        bytes_per_character: 16,
        columns: 20,
        rows: 32,
        screen: 0x58,
        size: 0x28,
        map_type: 2,
        ula: 0xC4,
        crtc: 2,
        pixels_per_byte: 4,
        last_pixel: 159,
        x_shift: 3,
    },
    Mode {
        bytes_per_character: 8,
        columns: 40,
        rows: 25,
        screen: 0x60,
        size: 0x20,
        map_type: 3,
        ula: 0x88,
        crtc: 3,
        pixels_per_byte: 8,
        last_pixel: 79,
        x_shift: 1,
    },
    Mode {
        bytes_per_character: 1,
        columns: 40,
        rows: 25,
        screen: 0x7C,
        size: 0x04,
        map_type: 4,
        ula: 0x4B,
        crtc: 4,
        pixels_per_byte: 1,
        last_pixel: 79,
        x_shift: 1,
    },
];

/// The 6845's registers 0–11 for each set.
const CRTC: [[u8; 12]; 5] = [
    [
        0x7F, 0x50, 0x62, 0x28, 0x26, 0x00, 0x20, 0x23, 0x01, 0x07, 0x67, 0x08,
    ],
    [
        0x7F, 0x50, 0x62, 0x28, 0x1E, 0x02, 0x19, 0x1C, 0x01, 0x09, 0x67, 0x09,
    ],
    [
        0x3F, 0x28, 0x31, 0x24, 0x26, 0x00, 0x20, 0x23, 0x01, 0x07, 0x67, 0x08,
    ],
    [
        0x3F, 0x28, 0x31, 0x24, 0x1E, 0x02, 0x19, 0x1C, 0x01, 0x09, 0x67, 0x09,
    ],
    [
        0x3F, 0x28, 0x33, 0x24, 0x1E, 0x02, 0x19, 0x1C, 0x93, 0x12, 0x72, 0x13,
    ],
];

/// The latch's screen-size bits, C0 then C1, for each memory map type.
const LATCH: [[u8; 2]; 5] = {
    const C0: u8 = machine_latch::SCREEN_SIZE & machine_latch::SCREEN_SIZE.wrapping_neg();
    const C1: u8 = machine_latch::SCREEN_SIZE & !C0;
    [
        [latch(C0, true), latch(C1, true)],
        [latch(C0, false), latch(C1, false)],
        [latch(C0, false), latch(C1, true)],
        [latch(C0, true), latch(C1, false)],
        [latch(C0, true), latch(C1, true)],
    ]
};

/// Emit the per-mode tables the VDU drivers index by MODE.
pub fn emit(a: &mut Asm) {
    let table = |a: &mut Asm, name: &'static str, field: fn(&Mode) -> u8| {
        a.here(name);
        for mode in &MODES {
            a.byte(field(mode));
        }
    };
    table(a, "mode_bytes_per_character", |m| m.bytes_per_character);
    table(a, "mode_right", |m| m.columns - 1);
    table(a, "mode_bottom", |m| m.rows - 1);
    table(a, "mode_screen", |m| m.screen);
    table(a, "mode_size", |m| m.size);
    table(a, "mode_map_type", |m| m.map_type);
    table(a, "mode_ula", |m| m.ula);
    table(a, "mode_crtc", |m| m.crtc * 12);
    table(a, "mode_pixels", |m| m.pixels_per_byte - 1);
    table(a, "mode_width_low", |m| m.last_pixel.to_le_bytes()[0]);
    table(a, "mode_width_high", |m| m.last_pixel.to_le_bytes()[1]);
    table(a, "mode_x_shift", |m| m.x_shift);
    table(a, "mode_colours", |m| match m.pixels_per_byte {
        8 => 1,
        4 => 3,
        2 => 15,
        _ => 0,
    });
    // Bytes per row, low and high.
    table(a, "mode_row_low", |m| {
        (u16::from(m.columns) * u16::from(m.bytes_per_character)).to_le_bytes()[0]
    });
    table(a, "mode_row_high", |m| {
        (u16::from(m.columns) * u16::from(m.bytes_per_character)).to_le_bytes()[1]
    });
    // The row multiplication table each mode uses.
    a.here("mode_rows_table");
    for mode in &MODES {
        let name = match u16::from(mode.columns) * u16::from(mode.bytes_per_character) {
            640 => "rows_640",
            320 => "rows_320",
            _ => "rows_40",
        };
        a.byte(crate::rom::asm::lo(name));
    }
    a.here("mode_rows_table_high");
    for mode in &MODES {
        let name = match u16::from(mode.columns) * u16::from(mode.bytes_per_character) {
            640 => "rows_640",
            320 => "rows_320",
            _ => "rows_40",
        };
        a.byte(crate::rom::asm::hi(name));
    }
    a.here("crtc_registers");
    for set in &CRTC {
        a.bytes(set);
    }
    a.here("latch_size");
    for bits in &LATCH {
        a.bytes(bits);
    }
    for (name, width, rows) in [
        ("rows_640", 640u16, 32u16),
        ("rows_320", 320, 32),
        ("rows_40", 40, 25),
    ] {
        a.here(name);
        for row in 0..rows {
            a.word(row * width);
        }
    }
}
