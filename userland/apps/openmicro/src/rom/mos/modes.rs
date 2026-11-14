//! The screen modes: geometry, colour depth, and the settings that make
//! each one — the Model B's eight through its 6845 and video ULA, the
//! Electron's seven through its ULA's mode field.

use super::latch;
use crate::machine::bbc::latch as machine_latch;
use crate::machine::electron::control;
use crate::machine::Model;
use crate::rom::asm::{hi, lo, Asm, Byte};

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
        pixels_per_byte: 1,
        last_pixel: 79,
        x_shift: 1,
    },
];

/// The last of the 6845's registers MODE sets, from 0.
pub(super) const LAST_CRTC_REGISTER: u8 = 11;

/// The 6845's registers 0–11 for each memory map type, R7 before
/// `vdu::crtc_write` moves it by *TV's shift and one more.
const CRTC: [[u8; LAST_CRTC_REGISTER as usize + 1]; 5] = [
    [
        0x7F, 0x50, 0x62, 0x28, 0x26, 0x00, 0x20, 0x22, 0x01, 0x07, 0x67, 0x08,
    ],
    [
        0x7F, 0x50, 0x62, 0x28, 0x1E, 0x02, 0x19, 0x1B, 0x01, 0x09, 0x67, 0x09,
    ],
    [
        0x3F, 0x28, 0x31, 0x24, 0x26, 0x00, 0x20, 0x22, 0x01, 0x07, 0x67, 0x08,
    ],
    [
        0x3F, 0x28, 0x31, 0x24, 0x1E, 0x02, 0x19, 0x1B, 0x01, 0x09, 0x67, 0x09,
    ],
    [
        0x3F, 0x28, 0x33, 0x24, 0x1E, 0x02, 0x19, 0x1B, 0x93, 0x12, 0x72, 0x13,
    ],
];

/// The two latch writes setting the screen size for each memory map type.
/// Mode 7's wrap is the teletext addressing's own, so, as MOS 1.20 is seen
/// to, it writes C0 low twice and leaves C1 as the last mode set it.
const LATCH: [[u8; 2]; 5] = {
    const C0: u8 = machine_latch::SCREEN_SIZE & machine_latch::SCREEN_SIZE.wrapping_neg();
    const C1: u8 = machine_latch::SCREEN_SIZE & !C0;
    [
        [latch(C0, false), latch(C1, true)],
        [latch(C0, false), latch(C1, false)],
        [latch(C0, true), latch(C1, true)],
        [latch(C0, true), latch(C1, false)],
        [latch(C0, false), latch(C0, false)],
    ]
};

/// Where MOS 1.20's table of 640-byte row offsets lies; software reads it
/// there.
pub const ROWS_640: u16 = 0xC375;

/// The Model B's 640-byte row table, at the address software reads it from.
pub fn pin_rows_640(a: &mut Asm) {
    a.pin(ROWS_640, |a| rows(a, "rows_640", 640, 32));
}

/// A row multiplication table: each row's offset, high byte first, as the
/// pointer at &E0 names it.
fn rows(a: &mut Asm, name: &'static str, width: u16, count: u16) {
    a.here(name);
    for row in 0..count {
        let [low, high] = (row * width).to_le_bytes();
        a.byte(high);
        a.byte(low);
    }
}

/// The modes the Electron has: all but the teletext mode.
const ELECTRON_MODES: usize = 7;

/// The bytes in one of `mode`'s character rows.
const fn row_bytes(mode: &Mode) -> u16 {
    mode.columns as u16 * mode.bytes_per_character as u16
}

/// The row multiplication table of `mode`'s row length.
const fn rows_table(mode: &Mode) -> &'static str {
    match row_bytes(mode) {
        640 => "rows_640",
        320 => "rows_320",
        _ => "rows_40",
    }
}

/// Emit the per-mode tables the VDU drivers index by MODE.
pub fn emit(a: &mut Asm, model: Model) {
    let present = match model {
        Model::ModelB => &MODES[..],
        Model::Electron => &MODES[..ELECTRON_MODES],
    };
    let table = |a: &mut Asm, name: &'static str, field: fn(&Mode) -> u8| {
        a.later(move |a| {
            a.here(name);
            for mode in present {
                a.byte(field(mode));
            }
        });
    };
    // What follows from the memory map type, an entry a type, from the
    // first mode of each.
    let map_table = |a: &mut Asm, name: &'static str, field: fn(&Mode) -> Byte| {
        a.later(move |a| {
            a.here(name);
            for map_type in 0u8.. {
                match present.iter().find(|mode| mode.map_type == map_type) {
                    Some(mode) => a.byte(field(mode)),
                    None => break,
                }
            }
        });
    };
    table(a, "mode_bytes_per_character", |m| m.bytes_per_character);
    table(a, "mode_right", |m| m.columns - 1);
    table(a, "mode_bottom", |m| m.rows - 1);
    table(a, "mode_screen", |m| m.screen);
    map_table(a, "map_size", |m| m.size.into());
    table(a, "mode_map_type", |m| m.map_type);
    match model {
        Model::ModelB => {
            table(a, "mode_ula", |m| m.ula);
            // The type's last register in `crtc_registers`: MODE writes them
            // downwards.
            map_table(a, "map_crtc", |m| {
                (m.map_type * (LAST_CRTC_REGISTER + 1) + LAST_CRTC_REGISTER).into()
            });
        }
        Model::Electron => a.later(move |a| {
            // `&FE07`'s mode field.
            a.here("mode_ula");
            for (number, _) in (0u8..).zip(present) {
                a.byte(number << control::MODE.trailing_zeros());
            }
        }),
    }
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
    map_table(a, "map_row_low", |m| row_bytes(m).to_le_bytes()[0].into());
    map_table(a, "map_row_high", |m| row_bytes(m).to_le_bytes()[1].into());
    // The row multiplication table each type uses.
    map_table(a, "map_rows_table", |m| lo(rows_table(m)));
    map_table(a, "map_rows_table_high", |m| hi(rows_table(m)));
    if model == Model::ModelB {
        a.later(|a| {
            a.here("crtc_registers");
            for set in &CRTC {
                a.bytes(set);
            }
        });
        a.later(|a| {
            a.here("latch_size");
            for bits in &LATCH {
                a.bytes(bits);
            }
        });
    }
    // The Model B's 640-byte table is pinned by `pin_rows_640`.
    let tables: &[(&str, u16, u16)] = match model {
        Model::ModelB => &[("rows_320", 320, 32), ("rows_40", 40, 25)],
        Model::Electron => &[("rows_640", 640, 32), ("rows_320", 320, 32)],
    };
    for &(name, width, count) in tables {
        a.later(move |a| rows(a, name, width, count));
    }
}
