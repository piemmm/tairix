//! The display modes as the BIOS keeps them: the 6845 cards' blocks of the
//! table INT 1Dh points at, and the EGA's and VGA's parameter table, a
//! 64-byte entry for each mode at each resolution in IBM's numbering. The
//! EGA and VGA set a mode from the entry the save pointer table at `40:A8h`
//! leads to, so a program may substitute a table of its own.

/// The 6845's registers as the MDA's and CGA's BIOS programs them: for
/// 40-column text, 80-column text, the graphics modes and the monochrome
/// mode.
pub const CRTC_40: [u8; 16] = [
    0x38, 0x28, 0x2D, 0x0A, 0x1F, 0x06, 0x19, 0x1C, 0x02, 0x07, 0x06, 0x07, 0x00, 0x00, 0x00, 0x00,
];
/// See [`CRTC_40`].
pub const CRTC_80: [u8; 16] = [
    0x71, 0x50, 0x5A, 0x0A, 0x1F, 0x06, 0x19, 0x1C, 0x02, 0x07, 0x06, 0x07, 0x00, 0x00, 0x00, 0x00,
];
/// See [`CRTC_40`].
pub const CRTC_GRAPHICS: [u8; 16] = [
    0x38, 0x28, 0x2D, 0x0A, 0x7F, 0x06, 0x64, 0x70, 0x02, 0x01, 0x06, 0x07, 0x00, 0x00, 0x00, 0x00,
];
/// See [`CRTC_40`].
pub const CRTC_MONO: [u8; 16] = [
    0x61, 0x50, 0x52, 0x0F, 0x19, 0x06, 0x19, 0x19, 0x02, 0x0D, 0x0B, 0x0C, 0x00, 0x00, 0x00, 0x00,
];

/// How a mode lays its memory out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Characters and attributes in interleaved bytes.
    Text,
    /// The CGA's 320×200 in four colours.
    Cga4,
    /// The CGA's 640×200 in two.
    Cga2,
    /// Four planes, a bit a pixel in each.
    Planar,
    /// A byte a pixel, chained across the planes.
    Linear,
}

/// How mode `mode` lays out its memory, if the card has the mode.
#[must_use]
pub const fn layout(mode: u8, vga: bool) -> Option<Layout> {
    Some(match mode {
        0..=3 | 7 => Layout::Text,
        4 | 5 => Layout::Cga4,
        6 => Layout::Cga2,
        0x0D..=0x10 => Layout::Planar,
        0x11 | 0x12 if vga => Layout::Planar,
        0x13 if vga => Layout::Linear,
        _ => return None,
    })
}

/// The scan lines the VGA shows a text mode in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lines {
    /// The CGA's, each line shown twice.
    L200,
    /// The EGA's.
    L350,
    /// The VGA's own.
    L400,
}

/// A parameter table entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    /// Text columns.
    pub columns: u8,
    /// Text rows.
    pub rows: u8,
    /// Scan lines a character takes.
    pub char_height: u8,
    /// Bytes a page takes, in the BIOS's reckoning.
    pub page_size: u16,
    /// Sequencer registers 1–4.
    pub seq: [u8; 4],
    /// The miscellaneous output register.
    pub misc: u8,
    /// The CRT controller's registers.
    pub crtc: [u8; 25],
    /// Attribute controller registers 00h–13h.
    pub ac: [u8; 20],
    /// The graphics controller's registers.
    pub gc: [u8; 9],
}

impl Mode {
    /// An entry's size.
    pub const BYTES: usize = 64;

    /// Whether it drives a monochrome monitor.
    #[must_use]
    pub const fn mono(&self) -> bool {
        self.misc & 0x01 == 0
    }

    /// The entry as the table holds it.
    #[must_use]
    pub fn bytes(&self) -> [u8; Self::BYTES] {
        let mut out = [0; Self::BYTES];
        let [low, high] = self.page_size.to_le_bytes();
        out[..5].copy_from_slice(&[self.columns, self.rows - 1, self.char_height, low, high]);
        out[5..9].copy_from_slice(&self.seq);
        out[9] = self.misc;
        out[10..35].copy_from_slice(&self.crtc);
        out[35..55].copy_from_slice(&self.ac);
        out[55..64].copy_from_slice(&self.gc);
        out
    }

    /// An entry read from a table.
    #[must_use]
    pub fn parse(bytes: &[u8; Self::BYTES]) -> Self {
        let field = |range: core::ops::Range<usize>| &bytes[range];
        let mut mode = Self {
            columns: bytes[0],
            rows: bytes[1].wrapping_add(1),
            char_height: bytes[2],
            page_size: u16::from_le_bytes([bytes[3], bytes[4]]),
            seq: [0; 4],
            misc: bytes[9],
            crtc: [0; 25],
            ac: [0; 20],
            gc: [0; 9],
        };
        mode.seq.copy_from_slice(field(5..9));
        mode.crtc.copy_from_slice(field(10..35));
        mode.ac.copy_from_slice(field(35..55));
        mode.gc.copy_from_slice(field(55..64));
        mode
    }
}

/// The EGA's sixteen colours of its 64, as the colour text and planar modes
/// name them.
const PALETTE: [u8; 16] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x14, 0x07, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F,
];

const fn attributes(palette: [u8; 16], tail: [u8; 4]) -> [u8; 20] {
    let mut out = [0; 20];
    let mut i = 0;
    while i < 16 {
        out[i] = palette[i];
        i += 1;
    }
    while i < 20 {
        out[i] = tail[i - 16];
        i += 1;
    }
    out
}

/// Colour text nine dots wide: line graphics and panning by eight.
const TEXT_AC: [u8; 20] = attributes(PALETTE, [0x0C, 0x00, 0x0F, 0x08]);
/// Colour text eight dots wide.
const TEXT_8_AC: [u8; 20] = attributes(PALETTE, [0x08, 0x00, 0x0F, 0x00]);
const TEXT_GC: [u8; 9] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x0E, 0x00, 0xFF];
const MONO_AC: [u8; 20] = attributes(
    [
        0x00, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x10, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18,
        0x18,
    ],
    [0x0E, 0x00, 0x0F, 0x08],
);
const MONO_GC: [u8; 9] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x0A, 0x00, 0xFF];
const CGA4_AC: [u8; 20] = attributes(
    [
        0x00, 0x13, 0x15, 0x17, 0x02, 0x04, 0x06, 0x07, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
        0x17,
    ],
    [0x01, 0x00, 0x03, 0x00],
);
const CGA4_GC: [u8; 9] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x30, 0x0F, 0x00, 0xFF];
const CGA2_AC: [u8; 20] = attributes([0x17; 16], [0x01, 0x00, 0x01, 0x00]);
const CGA2_GC: [u8; 9] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0D, 0x00, 0xFF];
/// Sixteen colours in the default palette, through every plane.
const PLANAR_AC: [u8; 20] = attributes(PALETTE, [0x01, 0x00, 0x0F, 0x00]);
/// The 200-line planar modes, whose colours are the CGA's sixteen.
const PLANAR_200_AC: [u8; 20] = attributes(
    [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
        0x17,
    ],
    [0x01, 0x00, 0x0F, 0x00],
);
const PLANAR_GC: [u8; 9] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x0F, 0xFF];
const MONO_GRAPHICS_AC: [u8; 20] = attributes(
    [
        0x00, 0x08, 0x00, 0x00, 0x18, 0x18, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x18, 0x00,
        0x00,
    ],
    [0x0B, 0x00, 0x05, 0x00],
);
const MONO_GRAPHICS_GC: [u8; 9] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x05, 0xFF];

/// A colour text mode of 25 rows; `alpha` is the EGA's text bit in the
/// sequencer's memory mode.
const fn text(
    misc: u8,
    clocking: u8,
    crtc: [u8; 25],
    ac: [u8; 20],
    (columns, char_height): (u8, u8),
    alpha: u8,
) -> Mode {
    Mode {
        columns,
        rows: 25,
        char_height,
        page_size: if columns == 40 { 0x0800 } else { 0x1000 },
        seq: [clocking, 0x03, 0x00, 0x02 | alpha],
        misc,
        crtc,
        ac,
        gc: TEXT_GC,
    }
}

#[allow(clippy::too_many_arguments, reason = "every field a mode differs in")]
const fn graphics(
    misc: u8,
    seq: [u8; 4],
    crtc: [u8; 25],
    ac: [u8; 20],
    gc: [u8; 9],
    (columns, rows, char_height): (u8, u8, u8),
    page_size: u16,
) -> Mode {
    Mode {
        columns,
        rows,
        char_height,
        page_size,
        seq,
        misc,
        crtc,
        ac,
        gc,
    }
}

/// The VGA's 40-column text: 360×400 at 70 Hz.
pub const VGA_TEXT_40: Mode = text(
    0x67,
    0x08,
    [
        0x2D, 0x27, 0x28, 0x90, 0x2B, 0xA0, 0xBF, 0x1F, 0x00, 0x4F, 0x0D, 0x0E, 0x00, 0x00, 0x00,
        0x00, 0x9C, 0x8E, 0x8F, 0x14, 0x1F, 0x96, 0xB9, 0xA3, 0xFF,
    ],
    TEXT_AC,
    (40, 16),
    0,
);
/// The VGA's 80-column text: 720×400 at 70 Hz.
pub const VGA_TEXT_80: Mode = text(
    0x67,
    0x00,
    [
        0x5F, 0x4F, 0x50, 0x82, 0x55, 0x81, 0xBF, 0x1F, 0x00, 0x4F, 0x0D, 0x0E, 0x00, 0x00, 0x00,
        0x00, 0x9C, 0x8E, 0x8F, 0x28, 0x1F, 0x96, 0xB9, 0xA3, 0xFF,
    ],
    TEXT_AC,
    (80, 16),
    0,
);
/// The VGA's 40-column text at the CGA's 200 lines: 320×200, each line
/// shown twice.
pub const VGA_TEXT_40_200: Mode = text(
    0x63,
    0x09,
    [
        0x2D, 0x27, 0x28, 0x90, 0x2B, 0xA0, 0xBF, 0x1F, 0x00, 0xC7, 0x06, 0x07, 0x00, 0x00, 0x00,
        0x00, 0x9C, 0x8E, 0x8F, 0x14, 0x1F, 0x96, 0xB9, 0xA3, 0xFF,
    ],
    TEXT_8_AC,
    (40, 8),
    0,
);
/// The VGA's 80-column text at 200 lines: 640×200.
pub const VGA_TEXT_80_200: Mode = text(
    0x63,
    0x01,
    [
        0x5F, 0x4F, 0x50, 0x82, 0x55, 0x81, 0xBF, 0x1F, 0x00, 0xC7, 0x06, 0x07, 0x00, 0x00, 0x00,
        0x00, 0x9C, 0x8E, 0x8F, 0x28, 0x1F, 0x96, 0xB9, 0xA3, 0xFF,
    ],
    TEXT_8_AC,
    (80, 8),
    0,
);
/// The VGA's 40-column text at the EGA's 350 lines: 320×350.
pub const VGA_TEXT_40_350: Mode = text(
    0xA3,
    0x09,
    [
        0x2D, 0x27, 0x28, 0x90, 0x2B, 0xA0, 0xBF, 0x1F, 0x00, 0x4D, 0x0B, 0x0C, 0x00, 0x00, 0x00,
        0x00, 0x83, 0x85, 0x5D, 0x14, 0x1F, 0x63, 0xBA, 0xA3, 0xFF,
    ],
    TEXT_8_AC,
    (40, 14),
    0,
);
/// The VGA's 80-column text at 350 lines: 640×350.
pub const VGA_TEXT_80_350: Mode = text(
    0xA3,
    0x01,
    [
        0x5F, 0x4F, 0x50, 0x82, 0x55, 0x81, 0xBF, 0x1F, 0x00, 0x4D, 0x0B, 0x0C, 0x00, 0x00, 0x00,
        0x00, 0x83, 0x85, 0x5D, 0x28, 0x1F, 0x63, 0xBA, 0xA3, 0xFF,
    ],
    TEXT_8_AC,
    (80, 14),
    0,
);
/// The VGA's monochrome text: 720×400.
pub const VGA_MONO_TEXT: Mode = Mode {
    misc: 0x66,
    crtc: [
        0x5F, 0x4F, 0x50, 0x82, 0x55, 0x81, 0xBF, 0x1F, 0x00, 0x4F, 0x0D, 0x0E, 0x00, 0x00, 0x00,
        0x00, 0x9C, 0x8E, 0x8F, 0x28, 0x0F, 0x96, 0xB9, 0xA3, 0xFF,
    ],
    ac: MONO_AC,
    gc: MONO_GC,
    ..VGA_TEXT_80
};
/// The VGA's monochrome text at 350 lines, as the MDA shows it: 720×350.
pub const VGA_MONO_TEXT_350: Mode = Mode {
    misc: 0xA6,
    crtc: [
        0x5F, 0x4F, 0x50, 0x82, 0x55, 0x81, 0xBF, 0x1F, 0x00, 0x4D, 0x0B, 0x0C, 0x00, 0x00, 0x00,
        0x00, 0x83, 0x85, 0x5D, 0x28, 0x0D, 0x63, 0xBA, 0xA3, 0xFF,
    ],
    char_height: 14,
    ..VGA_MONO_TEXT
};
/// Modes 4 and 5: the CGA's 320×200 in four colours.
pub const VGA_CGA4: Mode = graphics(
    0x63,
    [0x09, 0x03, 0x00, 0x02],
    [
        0x2D, 0x27, 0x28, 0x90, 0x2B, 0x80, 0xBF, 0x1F, 0x00, 0xC1, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x9C, 0x8E, 0x8F, 0x14, 0x00, 0x96, 0xB9, 0xA2, 0xFF,
    ],
    CGA4_AC,
    CGA4_GC,
    (40, 25, 8),
    0x4000,
);
/// Mode 6: the CGA's 640×200 in two.
pub const VGA_CGA2: Mode = graphics(
    0x63,
    [0x01, 0x01, 0x00, 0x06],
    [
        0x5F, 0x4F, 0x50, 0x82, 0x54, 0x80, 0xBF, 0x1F, 0x00, 0xC1, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x9C, 0x8E, 0x8F, 0x28, 0x00, 0x96, 0xB9, 0xC2, 0xFF,
    ],
    CGA2_AC,
    CGA2_GC,
    (80, 25, 8),
    0x4000,
);
/// Mode 0Dh: 320×200 in sixteen colours.
pub const VGA_PLANAR_320: Mode = graphics(
    0x63,
    [0x09, 0x0F, 0x00, 0x06],
    [
        0x2D, 0x27, 0x28, 0x90, 0x2B, 0x80, 0xBF, 0x1F, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x9C, 0x8E, 0x8F, 0x14, 0x00, 0x96, 0xB9, 0xE3, 0xFF,
    ],
    PLANAR_200_AC,
    PLANAR_GC,
    (40, 25, 8),
    0x2000,
);
/// Mode 0Eh: 640×200 in sixteen colours.
pub const VGA_PLANAR_640_200: Mode = graphics(
    0x63,
    [0x01, 0x0F, 0x00, 0x06],
    [
        0x5F, 0x4F, 0x50, 0x82, 0x54, 0x80, 0xBF, 0x1F, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x9C, 0x8E, 0x8F, 0x28, 0x00, 0x96, 0xB9, 0xE3, 0xFF,
    ],
    PLANAR_200_AC,
    PLANAR_GC,
    (80, 25, 8),
    0x4000,
);
/// Mode 0Fh: 640×350 monochrome graphics.
pub const VGA_MONO_GRAPHICS: Mode = graphics(
    0xA2,
    [0x01, 0x0F, 0x00, 0x06],
    [
        0x5F, 0x4F, 0x50, 0x82, 0x54, 0x80, 0xBF, 0x1F, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x83, 0x85, 0x5D, 0x28, 0x0F, 0x63, 0xBA, 0xE3, 0xFF,
    ],
    MONO_GRAPHICS_AC,
    MONO_GRAPHICS_GC,
    (80, 25, 14),
    0x8000,
);
/// Mode 10h: 640×350 in sixteen of 64 colours.
pub const VGA_PLANAR_350: Mode = graphics(
    0xA3,
    [0x01, 0x0F, 0x00, 0x06],
    [
        0x5F, 0x4F, 0x50, 0x82, 0x54, 0x80, 0xBF, 0x1F, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x83, 0x85, 0x5D, 0x28, 0x0F, 0x63, 0xBA, 0xE3, 0xFF,
    ],
    PLANAR_AC,
    PLANAR_GC,
    (80, 25, 14),
    0x8000,
);
/// Mode 11h: 640×480 in two colours.
pub const VGA_TWO_480: Mode = graphics(
    0xE3,
    [0x01, 0x0F, 0x00, 0x06],
    [
        0x5F, 0x4F, 0x50, 0x82, 0x54, 0x80, 0x0B, 0x3E, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0xEA, 0x8C, 0xDF, 0x28, 0x00, 0xE7, 0x04, 0xC3, 0xFF,
    ],
    attributes([0x3F; 16], [0x01, 0x00, 0x0F, 0x00]),
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x0F, 0xFF],
    (80, 30, 16),
    0xA000,
);
/// Mode 12h: 640×480 in sixteen colours.
pub const VGA_PLANAR_480: Mode = graphics(
    0xE3,
    [0x01, 0x0F, 0x00, 0x06],
    [
        0x5F, 0x4F, 0x50, 0x82, 0x54, 0x80, 0x0B, 0x3E, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0xEA, 0x8C, 0xDF, 0x28, 0x00, 0xE7, 0x04, 0xE3, 0xFF,
    ],
    PLANAR_AC,
    PLANAR_GC,
    (80, 30, 16),
    0xA000,
);
/// Mode 13h: 320×200 in 256 colours.
pub const VGA_LINEAR: Mode = graphics(
    0x63,
    [0x01, 0x0F, 0x00, 0x0E],
    [
        0x5F, 0x4F, 0x50, 0x82, 0x54, 0x80, 0xBF, 0x1F, 0x00, 0x41, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x9C, 0x8E, 0x8F, 0x28, 0x40, 0x96, 0xB9, 0xA3, 0xFF,
    ],
    attributes(
        [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D,
            0x0E, 0x0F,
        ],
        [0x41, 0x00, 0x0F, 0x00],
    ),
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x05, 0x0F, 0xFF],
    (40, 25, 8),
    0xFA00,
);

/// The EGA's 40-column text on an Enhanced Color Display: 320×350. The
/// EGA's cursor stops before its end line.
pub const EGA_TEXT_40: Mode = text(
    0xA7,
    0x0B,
    [
        0x2D, 0x27, 0x2B, 0x2D, 0x28, 0x6D, 0x6C, 0x1F, 0x00, 0x0D, 0x0B, 0x0D, 0x00, 0x00, 0x00,
        0x00, 0x5E, 0x2B, 0x5D, 0x14, 0x0F, 0x5E, 0x0A, 0xA3, 0xFF,
    ],
    TEXT_8_AC,
    (40, 14),
    1,
);
/// The EGA's 80-column text: 640×350.
pub const EGA_TEXT_80: Mode = text(
    0xA7,
    0x01,
    [
        0x5B, 0x4F, 0x53, 0x37, 0x51, 0x5B, 0x6C, 0x1F, 0x00, 0x0D, 0x0B, 0x0D, 0x00, 0x00, 0x00,
        0x00, 0x5E, 0x2B, 0x5D, 0x28, 0x0F, 0x5E, 0x0A, 0xA3, 0xFF,
    ],
    TEXT_8_AC,
    (80, 14),
    1,
);
/// The EGA's monochrome text: 720×350, as the MDA shows it.
pub const EGA_MONO_TEXT: Mode = Mode {
    misc: 0xA6,
    seq: [0x00, 0x03, 0x00, 0x03],
    crtc: [
        0x60, 0x4F, 0x56, 0x3A, 0x51, 0x60, 0x70, 0x1F, 0x00, 0x0D, 0x0B, 0x0D, 0x00, 0x00, 0x00,
        0x00, 0x5E, 0x2E, 0x5D, 0x28, 0x0D, 0x5E, 0x6E, 0xA3, 0xFF,
    ],
    ac: MONO_AC,
    gc: MONO_GC,
    ..EGA_TEXT_80
};
const EGA_200_40: [u8; 25] = [
    0x37, 0x27, 0x2D, 0x37, 0x31, 0x15, 0x04, 0x11, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xE1, 0x24, 0xC7, 0x14, 0x00, 0xE0, 0xF0, 0xE3, 0xFF,
];
const EGA_200_80: [u8; 25] = [
    0x70, 0x4F, 0x59, 0x2D, 0x5E, 0x06, 0x04, 0x11, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xE0, 0x23, 0xC7, 0x28, 0x00, 0xDF, 0xEF, 0xE3, 0xFF,
];

/// `crtc` with the CGA's two-bank scan line interleave.
const fn interleaved(crtc: [u8; 25], mode_control: u8) -> [u8; 25] {
    let mut out = crtc;
    out[9] = 0x01;
    out[23] = mode_control;
    out
}

/// Modes 4 and 5 on the EGA.
pub const EGA_CGA4: Mode = graphics(
    0x23,
    [0x0B, 0x03, 0x00, 0x02],
    interleaved(EGA_200_40, 0xA2),
    CGA4_AC,
    CGA4_GC,
    (40, 25, 8),
    0x4000,
);
/// Mode 6 on the EGA.
pub const EGA_CGA2: Mode = graphics(
    0x23,
    [0x01, 0x01, 0x00, 0x06],
    interleaved(EGA_200_80, 0xC2),
    CGA2_AC,
    CGA2_GC,
    (80, 25, 8),
    0x4000,
);
/// Mode 0Dh on the EGA.
pub const EGA_PLANAR_320: Mode = graphics(
    0x23,
    [0x0B, 0x0F, 0x00, 0x06],
    EGA_200_40,
    PLANAR_200_AC,
    PLANAR_GC,
    (40, 25, 8),
    0x2000,
);
/// Mode 0Eh on the EGA.
pub const EGA_PLANAR_640_200: Mode = graphics(
    0x23,
    [0x01, 0x0F, 0x00, 0x06],
    EGA_200_80,
    PLANAR_200_AC,
    PLANAR_GC,
    (80, 25, 8),
    0x4000,
);
/// Mode 0Fh on the EGA and a monochrome display: the MDA's timing, each
/// line's 80 bytes counted a byte at a time as the planes hold them.
pub const EGA_MONO_GRAPHICS: Mode = graphics(
    0xA2,
    [0x01, 0x0F, 0x00, 0x06],
    [
        0x60, 0x4F, 0x56, 0x1A, 0x50, 0xE0, 0x70, 0x1F, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x5E, 0x2E, 0x5D, 0x28, 0x0D, 0x5E, 0x6E, 0xE3, 0xFF,
    ],
    MONO_GRAPHICS_AC,
    MONO_GRAPHICS_GC,
    (80, 25, 14),
    0x8000,
);
/// Mode 10h on the EGA: 640×350 in sixteen of 64 colours.
pub const EGA_PLANAR_350: Mode = graphics(
    0xA7,
    [0x01, 0x0F, 0x00, 0x06],
    [
        0x5B, 0x4F, 0x53, 0x37, 0x52, 0x00, 0x6C, 0x1F, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x5E, 0x2B, 0x5D, 0x28, 0x0F, 0x5F, 0x0A, 0xE3, 0xFF,
    ],
    PLANAR_AC,
    PLANAR_GC,
    (80, 25, 14),
    0x8000,
);

/// Entries in the VGA's table.
pub const VGA_ENTRIES: usize = 0x1D;
/// Entries in the EGA's.
pub const EGA_ENTRIES: usize = 0x17;

/// The entry numbered `index` in the card's table. The numbers are IBM's;
/// the `PCjr`'s modes, and the modes of an EGA with 64 KiB or a 200-line
/// colour display, which the emulated cards are not, hold no entry.
#[must_use]
pub const fn entry(index: usize, vga: bool) -> Option<&'static Mode> {
    Some(match (index, vga) {
        (0 | 1, true) => &VGA_TEXT_40_200,
        (2 | 3, true) => &VGA_TEXT_80_200,
        (4 | 5, true) => &VGA_CGA4,
        (4 | 5, false) => &EGA_CGA4,
        (6, true) => &VGA_CGA2,
        (6, false) => &EGA_CGA2,
        (7, true) => &VGA_MONO_TEXT_350,
        (7, false) => &EGA_MONO_TEXT,
        (0x0D, true) => &VGA_PLANAR_320,
        (0x0D, false) => &EGA_PLANAR_320,
        (0x0E, true) => &VGA_PLANAR_640_200,
        (0x0E, false) => &EGA_PLANAR_640_200,
        (0x11, true) => &VGA_MONO_GRAPHICS,
        (0x11, false) => &EGA_MONO_GRAPHICS,
        (0x12, true) => &VGA_PLANAR_350,
        (0x12, false) => &EGA_PLANAR_350,
        (0x13 | 0x14, true) => &VGA_TEXT_40_350,
        (0x13 | 0x14, false) => &EGA_TEXT_40,
        (0x15 | 0x16, true) => &VGA_TEXT_80_350,
        (0x15 | 0x16, false) => &EGA_TEXT_80,
        (0x17, true) => &VGA_TEXT_40,
        (0x18, true) => &VGA_TEXT_80,
        (0x19, true) => &VGA_MONO_TEXT,
        (0x1A, true) => &VGA_TWO_480,
        (0x1B, true) => &VGA_PLANAR_480,
        (0x1C, true) => &VGA_LINEAR,
        _ => return None,
    })
}

/// The entry the BIOS sets mode `mode` from; a text mode on the VGA at
/// `lines`, the monochrome mode at 350 lines unless 400 are asked for.
#[must_use]
pub const fn index(mode: u8, lines: Lines, vga: bool) -> Option<usize> {
    let mode = mode as usize;
    Some(match mode {
        0..=3 if !vga => 0x13 + mode,
        0..=3 => match lines {
            Lines::L200 => mode,
            Lines::L350 => 0x13 + mode,
            Lines::L400 => 0x17 + mode / 2,
        },
        7 if vga && matches!(lines, Lines::L400) => 0x19,
        4..=7 | 0x0D | 0x0E => mode,
        0x0F | 0x10 => mode + 2,
        0x11..=0x13 if vga => mode + 9,
        _ => return None,
    })
}

/// The card's whole table.
#[must_use]
pub fn table(vga: bool) -> alloc::vec::Vec<u8> {
    let entries = if vga { VGA_ENTRIES } else { EGA_ENTRIES };
    (0..entries)
        .flat_map(|i| entry(i, vga).map_or([0; Mode::BYTES], Mode::bytes))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_of_each_card_has_an_entry_that_reads_back() {
        for vga in [false, true] {
            for mode in 0..=0x13u8 {
                let Some(layout) = layout(mode, vga) else {
                    continue;
                };
                for lines in [Lines::L200, Lines::L350, Lines::L400] {
                    let index = index(mode, lines, vga).expect("an entry number");
                    let entry = entry(index, vga).expect("an entry");
                    assert_eq!(Mode::parse(&entry.bytes()), *entry);
                    let text = layout == Layout::Text;
                    assert_eq!(text, entry.gc[6] & 0x01 == 0, "mode {mode:#x}");
                }
            }
        }
        assert_eq!(table(true).len(), VGA_ENTRIES * Mode::BYTES);
        assert_eq!(table(false).len(), EGA_ENTRIES * Mode::BYTES);
    }

    #[test]
    fn the_vgas_text_modes_follow_the_lines_asked_for() {
        let at = |mode, lines| entry(index(mode, lines, true).unwrap(), true).unwrap();
        assert_eq!(at(3, Lines::L200).char_height, 8);
        assert_eq!(at(3, Lines::L350).char_height, 14);
        assert_eq!(at(3, Lines::L400).char_height, 16);
        assert_eq!(
            at(7, Lines::L200),
            at(7, Lines::L350),
            "no 200-line monochrome"
        );
        assert_eq!(at(7, Lines::L400).char_height, 16);
        let ega = entry(index(3, Lines::L400, false).unwrap(), false).unwrap();
        assert_eq!(ega.char_height, 14, "the EGA has only its own");
    }
}
