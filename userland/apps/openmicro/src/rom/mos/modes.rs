//! The screen modes: geometry, colour depth, and the settings that make
//! each one — the Model B's eight through its 6845 and video ULA, the
//! Electron's seven through its ULA's mode field.

use super::latch;
use super::Model;
use crate::machine::bbc::latch as machine_latch;
use crate::machine::electron::control;
use crate::rom::asm::{byte, lo, Asm};

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
    /// Whether it draws graphics: MOS 1.20's text modes set &361 to 0, and
    /// its PLOT, CLG and VDU 5 then do nothing.
    graphics: bool,
}

/// External x's width.
const EXTERNAL_WIDTH: u16 = 1280;
/// The points across a text mode, external x over 16, as MOS 1.20 scales it.
const TEXT_POINTS: u16 = 80;

/// Points across: a character is eight pixels wide in each graphics mode.
const fn points_across(mode: &Mode) -> u16 {
    if mode.graphics {
        mode.columns as u16 * 8
    } else {
        TEXT_POINTS
    }
}

/// The right shift taking an external x to a point.
const fn x_shift(mode: &Mode) -> u8 {
    let mut shift = 0;
    while EXTERNAL_WIDTH >> shift > points_across(mode) {
        shift += 1;
    }
    shift
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
        graphics: true,
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
        graphics: true,
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
        graphics: true,
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
        graphics: false,
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
        graphics: true,
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
        graphics: true,
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
        graphics: false,
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
        graphics: false,
    },
];

/// Each pixel's mask in its byte, at its byte's pixels less one, as &361 holds
/// it, plus its place across: a text mode's one place, whose bit 0 is what
/// MOS 1.20's POINT reads, then two, four and eight pixels a byte.
const PIXEL_MASKS: [u8; 15] = [
    0x01, 0xAA, 0x55, 0x88, 0x44, 0x22, 0x11, 0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01,
];
/// &361 for eight pixels a byte: where mode 0's masks start, which MOS 1.20
/// gives the text modes too.
pub(super) const EIGHT_PIXELS: u8 = 7;
const _: () = assert!(PIXEL_MASKS[EIGHT_PIXELS as usize] == 0x80);

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
/// there, and a mode set copied from MOS 1.20 points &E0 into its page.
pub const ROWS_640: u16 = 0xC375;
/// Where a mode set copied from MOS 1.20 reads the mode tables: the Model
/// B's lie there in MOS 1.20's order, each sharing the bytes its neighbour
/// ends with.
const MODE_TABLES: u16 = 0xC3F7;
const PIXEL_TABLES: u16 = 0xC43A;
const MAP_TABLES: u16 = 0xC44B;

/// A memory map type's kind of row, as such a mode set finds it: 2 for 640
/// bytes, 1 for 320, 0 for 40, each the high byte of its rows' length.
pub(super) const fn row_kind(map_type: u8) -> u8 {
    ((map_type + 2) ^ 7) >> 1
}
const ROW_LENGTHS: [u16; 3] = [40, 320, 640];
const _: () = {
    let mut i = 0;
    while i < MODES.len() {
        let kind = row_kind(MODES[i].map_type) as usize;
        assert!(row_bytes(&MODES[i]) == ROW_LENGTHS[kind]);
        assert!(ROW_LENGTHS[kind] >> 8 == byte(kind) as u16);
        i += 1;
    }
};

/// A field of each mode, or of each memory map type's first mode.
#[derive(Clone, Copy)]
enum Field {
    Ula,
    BytesPerCharacter,
    Colours,
    Pixels,
    MapType,
    Size,
    Screen,
}

const fn field(mode: &Mode, field: Field) -> u8 {
    match field {
        Field::Ula => mode.ula,
        Field::BytesPerCharacter => mode.bytes_per_character,
        // Logical colours less one, as &360 holds them.
        Field::Colours => match mode.pixels_per_byte {
            8 => 1,
            4 => 3,
            2 => 15,
            _ => 0,
        },
        Field::Pixels => {
            if mode.graphics {
                mode.pixels_per_byte - 1
            } else {
                0
            }
        }
        Field::MapType => mode.map_type,
        Field::Size => mode.size,
        Field::Screen => mode.screen,
    }
}

const fn column(of: Field) -> [u8; MODES.len()] {
    let mut table = [0; MODES.len()];
    let mut i = 0;
    while i < MODES.len() {
        table[i] = field(&MODES[i], of);
        i += 1;
    }
    table
}

const MAP_TYPES: usize = CRTC.len();

const fn map_column(of: Field) -> [u8; MAP_TYPES] {
    let mut table = [0; MAP_TYPES];
    let mut map_type = 0;
    while map_type < MAP_TYPES {
        let mut i = 0;
        while MODES[i].map_type as usize != map_type {
            i += 1;
        }
        table[map_type] = field(&MODES[i], of);
        map_type += 1;
    }
    table
}

const ULA: [u8; MODES.len()] = column(Field::Ula);
const BYTES_PER_CHARACTER: [u8; MODES.len()] = column(Field::BytesPerCharacter);
const COLOURS: [u8; MODES.len()] = column(Field::Colours);
const PIXELS: [u8; MODES.len()] = column(Field::Pixels);
const MAP_TYPE: [u8; MODES.len()] = column(Field::MapType);
const MAP_SIZE: [u8; MAP_TYPES] = map_column(Field::Size);
const MAP_SCREEN: [u8; MAP_TYPES] = map_column(Field::Screen);
const _: () = assert!(
    BYTES_PER_CHARACTER[7] == PIXEL_MASKS[0]
        && PIXEL_MASKS[14] == COLOURS[0]
        && PIXELS[6] == MAP_TYPE[0]
        && PIXELS[7] == MAP_TYPE[1]
        && LATCH[4][1] == LATCH[0][0]
);

/// The 6845 register set each memory map type starts at in
/// `crtc_registers`, at its last register: MODE writes them downwards.
const MAP_CRTC: [u8; MAP_TYPES] = {
    let mut table = [0; MAP_TYPES];
    let mut map_type = 0;
    while map_type < MAP_TYPES {
        table[map_type] = byte(map_type) * (LAST_CRTC_REGISTER + 1) + LAST_CRTC_REGISTER;
        map_type += 1;
    }
    table
};

/// Each kind of row's length, low byte.
const ROW_LOW: [u8; 3] = [
    ROW_LENGTHS[0].to_le_bytes()[0],
    ROW_LENGTHS[1].to_le_bytes()[0],
    ROW_LENGTHS[2].to_le_bytes()[0],
];

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

/// The row multiplication table each kind of row reads, `forty` the
/// 40-byte rows': the 320-byte rows read the 640-byte table halved.
fn row_tables(a: &mut Asm, forty: &'static str) {
    a.here("row_table_low");
    for name in [forty, "rows_640", "rows_640"] {
        a.byte(lo(name));
    }
}

/// The modes the Electron has: all but the teletext mode.
const ELECTRON_MODES: usize = 7;

/// The bytes in one of `mode`'s character rows.
const fn row_bytes(mode: &Mode) -> u16 {
    mode.columns as u16 * mode.bytes_per_character as u16
}

/// The Model B's mode tables at MOS 1.20's addresses: its two row tables in
/// one page, then the tables by mode and by memory map type.
pub fn pin_tables(a: &mut Asm) {
    a.pin(ROWS_640, |a| {
        rows(a, "rows_640", 640, 32);
        rows(a, "rows_40", 40, 25);
    });
    // Each table but the last ends at the next's first byte.
    a.pin(MODE_TABLES, |a| {
        a.here("mode_ula");
        a.bytes(&ULA);
        a.here("mode_bytes_per_character");
        a.bytes(&BYTES_PER_CHARACTER[..7]);
        a.here("pixel_mask_table");
        a.bytes(&PIXEL_MASKS[..14]);
        a.here("mode_colours");
        a.bytes(&COLOURS);
    });
    a.pin(PIXEL_TABLES, |a| {
        a.here("mode_pixels");
        a.bytes(&PIXELS[..6]);
        a.here("mode_map_type");
        a.bytes(&MAP_TYPE);
    });
    a.pin(MAP_TABLES, |a| {
        a.here("latch_second");
        for bits in &LATCH[..4] {
            a.byte(bits[1]);
        }
        a.here("latch_first");
        for bits in &LATCH {
            a.byte(bits[0]);
        }
        // Where MOS 1.20 keeps each type's screen clear, which such a mode
        // set copies into &35D: this MOS sets &35D for each VDU code itself.
        a.fill(MAP_TYPES, 0);
        a.here("map_size");
        a.bytes(&MAP_SIZE);
        a.here("map_screen");
        a.bytes(&MAP_SCREEN);
        a.here("row_low");
        a.bytes(&ROW_LOW);
        row_tables(a, "rows_40");
        a.here("map_crtc");
        a.bytes(&MAP_CRTC);
        a.here("crtc_registers");
        for set in &CRTC {
            a.bytes(set);
        }
    });
}

/// Emit the mode tables the Model B does not pin; the Electron holds them
/// all.
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
    table(a, "mode_right", |m| m.columns - 1);
    table(a, "mode_bottom", |m| m.rows - 1);
    table(a, "mode_width_low", |m| {
        (points_across(m) - 1).to_le_bytes()[0]
    });
    table(a, "mode_width_high", |m| {
        (points_across(m) - 1).to_le_bytes()[1]
    });
    table(a, "mode_x_shift", x_shift);
    if model == Model::ModelB {
        return;
    }
    let held = |a: &mut Asm, name: &'static str, bytes: &'static [u8]| {
        a.later(move |a| {
            a.here(name);
            a.bytes(bytes);
        });
    };
    held(
        a,
        "mode_bytes_per_character",
        &BYTES_PER_CHARACTER[..ELECTRON_MODES],
    );
    held(a, "mode_colours", &COLOURS[..ELECTRON_MODES]);
    held(a, "mode_pixels", &PIXELS[..ELECTRON_MODES]);
    held(a, "mode_map_type", &MAP_TYPE[..ELECTRON_MODES]);
    held(a, "pixel_mask_table", &PIXEL_MASKS);
    let electron_maps = usize::from(MAP_TYPE[ELECTRON_MODES - 1]) + 1;
    held(a, "map_size", &MAP_SIZE[..electron_maps]);
    held(a, "map_screen", &MAP_SCREEN[..electron_maps]);
    held(a, "row_low", &ROW_LOW);
    // The Electron has no 40-byte rows to read.
    a.later(|a| row_tables(a, "rows_640"));
    a.later(|a| {
        // `&FE07`'s mode field.
        a.here("mode_ula");
        for number in 0..byte(ELECTRON_MODES) {
            a.byte(number << control::MODE.trailing_zeros());
        }
    });
    a.later(|a| rows(a, "rows_640", 640, 32));
}
