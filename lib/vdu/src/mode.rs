//! RISC OS 5's numbered screen modes, 0 to 53: each one's mode variables, as
//! `OS_ReadModeVariable` reads them for it. Mode 7 is Teletext as RISC OS 5
//! keeps it on every machine it runs on now, in 256 colours at 640 by 500.

/// A mode's `ModeFlags`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Flags(u16);

impl Flags {
    /// Text only: no graphics, and no sprite may be in it.
    pub const NON_GRAPHIC: u16 = 1;
    /// Teletext.
    pub const TELETEXT: u16 = 1 << 1;
    /// A gap between text rows.
    pub const GAP: u16 = 1 << 2;
    /// The BBC's gaps, which its text modes 3 and 6 have.
    pub const BBC_GAP: u16 = 1 << 3;
    /// A high-resolution monochrome mode.
    pub const HI_RES_MONO: u16 = 1 << 4;
    /// Characters twice their height.
    pub const DOUBLE_VERTICAL: u16 = 1 << 5;
    /// A full palette, each of its colours its own.
    pub const FULL_PALETTE: u16 = 1 << 7;

    /// The flags `bits` hold.
    #[must_use]
    pub const fn new(bits: u16) -> Self {
        Self(bits)
    }

    /// The bits.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Whether `flag` is among them.
    #[must_use]
    pub const fn has(self, flag: u16) -> bool {
        self.0 & flag != 0
    }
}

/// One numbered mode's variables.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Mode {
    /// Pixels across: `XWindLimit` is one less.
    pub width: u16,
    /// Pixels down: `YWindLimit` is one less.
    pub height: u16,
    /// Text columns: `ScrRCol` is one less.
    pub columns: u8,
    /// Text rows: `ScrBRow` is one less.
    pub rows: u8,
    /// `XEigFactor`: a pixel is `1 << x_eig` OS units across.
    pub x_eig: u8,
    /// `YEigFactor`: and `1 << y_eig` down.
    pub y_eig: u8,
    /// `Log2BPP`: the bits a pixel takes in memory, as a power of two.
    pub log2_bpp: u8,
    /// `Log2BPC`: the bits a pixel as a character sees it takes, which is
    /// more where each is doubled across.
    pub log2_bpc: u8,
    /// `NColour`: the highest colour number.
    pub ncolour: u8,
    /// `LineLength`: the bytes a pixel row takes.
    pub line_length: u16,
    /// `ScreenSize`: the bytes the screen takes.
    pub screen_size: u32,
    /// `YShftFactor`: the BBC-compatible modes' row address shift, else 0.
    pub y_shift: u8,
    /// `ModeFlags`.
    pub flags: Flags,
}

impl Mode {
    /// Whether it has pixels to draw, and so a sprite may be in it.
    #[must_use]
    pub const fn graphic(&self) -> bool {
        !self.flags.has(Flags::NON_GRAPHIC)
    }
}

/// Each numbered mode, by its number.
#[rustfmt::skip]
pub static MODES: [Mode; 54] = [
    /*  0 */ Mode { width: 640, height: 256, columns: 80, rows: 32, x_eig: 1, y_eig: 2, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 80, screen_size: 20_480, y_shift: 4, flags: Flags::new(0) },
    /*  1 */ Mode { width: 320, height: 256, columns: 40, rows: 32, x_eig: 2, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 80, screen_size: 20_480, y_shift: 4, flags: Flags::new(0) },
    /*  2 */ Mode { width: 160, height: 256, columns: 20, rows: 32, x_eig: 3, y_eig: 2, log2_bpp: 2, log2_bpc: 3, ncolour: 15, line_length: 160, screen_size: 40_960, y_shift: 5, flags: Flags::new(0) },
    /*  3 */ Mode { width: 640, height: 250, columns: 80, rows: 25, x_eig: 1, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 1, line_length: 160, screen_size: 40_960, y_shift: 5, flags: Flags::new(Flags::NON_GRAPHIC | Flags::GAP | Flags::BBC_GAP) },
    /*  4 */ Mode { width: 320, height: 256, columns: 40, rows: 32, x_eig: 2, y_eig: 2, log2_bpp: 0, log2_bpc: 1, ncolour: 1, line_length: 80, screen_size: 20_480, y_shift: 4, flags: Flags::new(0) },
    /*  5 */ Mode { width: 160, height: 256, columns: 20, rows: 32, x_eig: 3, y_eig: 2, log2_bpp: 1, log2_bpc: 2, ncolour: 3, line_length: 80, screen_size: 20_480, y_shift: 4, flags: Flags::new(0) },
    /*  6 */ Mode { width: 320, height: 250, columns: 40, rows: 25, x_eig: 2, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 1, line_length: 80, screen_size: 20_480, y_shift: 4, flags: Flags::new(Flags::NON_GRAPHIC | Flags::GAP | Flags::BBC_GAP) },
    /*  7 */ Mode { width: 640, height: 500, columns: 40, rows: 25, x_eig: 1, y_eig: 1, log2_bpp: 3, log2_bpc: 3, ncolour: 255, line_length: 640, screen_size: 655_360, y_shift: 5, flags: Flags::new(Flags::NON_GRAPHIC | Flags::TELETEXT | Flags::GAP | Flags::DOUBLE_VERTICAL | Flags::FULL_PALETTE) },
    /*  8 */ Mode { width: 640, height: 256, columns: 80, rows: 32, x_eig: 1, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 160, screen_size: 40_960, y_shift: 5, flags: Flags::new(0) },
    /*  9 */ Mode { width: 320, height: 256, columns: 40, rows: 32, x_eig: 2, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 160, screen_size: 40_960, y_shift: 5, flags: Flags::new(0) },
    /* 10 */ Mode { width: 160, height: 256, columns: 20, rows: 32, x_eig: 3, y_eig: 2, log2_bpp: 3, log2_bpc: 4, ncolour: 63, line_length: 320, screen_size: 81_920, y_shift: 6, flags: Flags::new(0) },
    /* 11 */ Mode { width: 640, height: 250, columns: 80, rows: 25, x_eig: 1, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 160, screen_size: 40_960, y_shift: 5, flags: Flags::new(Flags::GAP) },
    /* 12 */ Mode { width: 640, height: 256, columns: 80, rows: 32, x_eig: 1, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 320, screen_size: 81_920, y_shift: 6, flags: Flags::new(0) },
    /* 13 */ Mode { width: 320, height: 256, columns: 40, rows: 32, x_eig: 2, y_eig: 2, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 320, screen_size: 81_920, y_shift: 6, flags: Flags::new(0) },
    /* 14 */ Mode { width: 640, height: 250, columns: 80, rows: 25, x_eig: 1, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 320, screen_size: 81_920, y_shift: 6, flags: Flags::new(Flags::GAP) },
    /* 15 */ Mode { width: 640, height: 256, columns: 80, rows: 32, x_eig: 1, y_eig: 2, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 640, screen_size: 163_840, y_shift: 7, flags: Flags::new(0) },
    /* 16 */ Mode { width: 1056, height: 256, columns: 132, rows: 32, x_eig: 1, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 528, screen_size: 135_168, y_shift: 0, flags: Flags::new(0) },
    /* 17 */ Mode { width: 1056, height: 250, columns: 132, rows: 25, x_eig: 1, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 528, screen_size: 135_168, y_shift: 0, flags: Flags::new(Flags::GAP) },
    /* 18 */ Mode { width: 640, height: 512, columns: 80, rows: 64, x_eig: 1, y_eig: 1, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 80, screen_size: 40_960, y_shift: 4, flags: Flags::new(0) },
    /* 19 */ Mode { width: 640, height: 512, columns: 80, rows: 64, x_eig: 1, y_eig: 1, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 160, screen_size: 81_920, y_shift: 5, flags: Flags::new(0) },
    /* 20 */ Mode { width: 640, height: 512, columns: 80, rows: 64, x_eig: 1, y_eig: 1, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 320, screen_size: 163_840, y_shift: 6, flags: Flags::new(0) },
    /* 21 */ Mode { width: 640, height: 512, columns: 80, rows: 64, x_eig: 1, y_eig: 1, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 640, screen_size: 327_680, y_shift: 7, flags: Flags::new(0) },
    /* 22 */ Mode { width: 768, height: 288, columns: 96, rows: 36, x_eig: 0, y_eig: 1, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 384, screen_size: 110_592, y_shift: 0, flags: Flags::new(0) },
    /* 23 */ Mode { width: 1152, height: 896, columns: 144, rows: 56, x_eig: 1, y_eig: 1, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 144, screen_size: 129_024, y_shift: 0, flags: Flags::new(Flags::HI_RES_MONO | Flags::DOUBLE_VERTICAL) },
    /* 24 */ Mode { width: 1056, height: 256, columns: 132, rows: 32, x_eig: 1, y_eig: 2, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 1056, screen_size: 270_336, y_shift: 0, flags: Flags::new(0) },
    /* 25 */ Mode { width: 640, height: 480, columns: 80, rows: 60, x_eig: 1, y_eig: 1, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 80, screen_size: 38_400, y_shift: 4, flags: Flags::new(0) },
    /* 26 */ Mode { width: 640, height: 480, columns: 80, rows: 60, x_eig: 1, y_eig: 1, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 160, screen_size: 76_800, y_shift: 5, flags: Flags::new(0) },
    /* 27 */ Mode { width: 640, height: 480, columns: 80, rows: 60, x_eig: 1, y_eig: 1, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 320, screen_size: 153_600, y_shift: 6, flags: Flags::new(0) },
    /* 28 */ Mode { width: 640, height: 480, columns: 80, rows: 60, x_eig: 1, y_eig: 1, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 640, screen_size: 307_200, y_shift: 7, flags: Flags::new(0) },
    /* 29 */ Mode { width: 800, height: 600, columns: 100, rows: 75, x_eig: 1, y_eig: 1, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 100, screen_size: 60_000, y_shift: 0, flags: Flags::new(0) },
    /* 30 */ Mode { width: 800, height: 600, columns: 100, rows: 75, x_eig: 1, y_eig: 1, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 200, screen_size: 120_000, y_shift: 0, flags: Flags::new(0) },
    /* 31 */ Mode { width: 800, height: 600, columns: 100, rows: 75, x_eig: 1, y_eig: 1, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 400, screen_size: 240_000, y_shift: 0, flags: Flags::new(0) },
    /* 32 */ Mode { width: 800, height: 600, columns: 100, rows: 75, x_eig: 1, y_eig: 1, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 800, screen_size: 480_000, y_shift: 0, flags: Flags::new(0) },
    /* 33 */ Mode { width: 768, height: 288, columns: 96, rows: 36, x_eig: 1, y_eig: 2, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 96, screen_size: 27_648, y_shift: 0, flags: Flags::new(0) },
    /* 34 */ Mode { width: 768, height: 288, columns: 96, rows: 36, x_eig: 1, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 192, screen_size: 55_296, y_shift: 0, flags: Flags::new(0) },
    /* 35 */ Mode { width: 768, height: 288, columns: 96, rows: 36, x_eig: 1, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 384, screen_size: 110_592, y_shift: 0, flags: Flags::new(0) },
    /* 36 */ Mode { width: 768, height: 288, columns: 96, rows: 36, x_eig: 1, y_eig: 2, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 768, screen_size: 221_184, y_shift: 0, flags: Flags::new(0) },
    /* 37 */ Mode { width: 896, height: 352, columns: 112, rows: 44, x_eig: 1, y_eig: 2, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 112, screen_size: 39_424, y_shift: 0, flags: Flags::new(0) },
    /* 38 */ Mode { width: 896, height: 352, columns: 112, rows: 44, x_eig: 1, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 224, screen_size: 78_848, y_shift: 0, flags: Flags::new(0) },
    /* 39 */ Mode { width: 896, height: 352, columns: 112, rows: 44, x_eig: 1, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 448, screen_size: 157_696, y_shift: 0, flags: Flags::new(0) },
    /* 40 */ Mode { width: 896, height: 352, columns: 112, rows: 44, x_eig: 1, y_eig: 2, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 896, screen_size: 315_392, y_shift: 0, flags: Flags::new(0) },
    /* 41 */ Mode { width: 640, height: 352, columns: 80, rows: 44, x_eig: 1, y_eig: 2, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 80, screen_size: 28_160, y_shift: 0, flags: Flags::new(0) },
    /* 42 */ Mode { width: 640, height: 352, columns: 80, rows: 44, x_eig: 1, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 160, screen_size: 56_320, y_shift: 0, flags: Flags::new(0) },
    /* 43 */ Mode { width: 640, height: 352, columns: 80, rows: 44, x_eig: 1, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 320, screen_size: 112_640, y_shift: 0, flags: Flags::new(0) },
    /* 44 */ Mode { width: 640, height: 200, columns: 80, rows: 25, x_eig: 1, y_eig: 2, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 80, screen_size: 16_000, y_shift: 0, flags: Flags::new(0) },
    /* 45 */ Mode { width: 640, height: 200, columns: 80, rows: 25, x_eig: 1, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 160, screen_size: 32_000, y_shift: 0, flags: Flags::new(0) },
    /* 46 */ Mode { width: 640, height: 200, columns: 80, rows: 25, x_eig: 1, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 320, screen_size: 64_000, y_shift: 0, flags: Flags::new(0) },
    /* 47 */ Mode { width: 360, height: 480, columns: 45, rows: 60, x_eig: 2, y_eig: 2, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 360, screen_size: 172_800, y_shift: 0, flags: Flags::new(0) },
    /* 48 */ Mode { width: 320, height: 480, columns: 40, rows: 60, x_eig: 2, y_eig: 1, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 160, screen_size: 76_800, y_shift: 0, flags: Flags::new(0) },
    /* 49 */ Mode { width: 320, height: 480, columns: 40, rows: 60, x_eig: 2, y_eig: 1, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 320, screen_size: 153_600, y_shift: 0, flags: Flags::new(0) },
    /* 50 */ Mode { width: 320, height: 240, columns: 40, rows: 30, x_eig: 2, y_eig: 2, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 40, screen_size: 9_600, y_shift: 0, flags: Flags::new(0) },
    /* 51 */ Mode { width: 320, height: 240, columns: 40, rows: 30, x_eig: 2, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 80, screen_size: 19_200, y_shift: 0, flags: Flags::new(0) },
    /* 52 */ Mode { width: 320, height: 240, columns: 40, rows: 30, x_eig: 2, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 160, screen_size: 38_400, y_shift: 0, flags: Flags::new(0) },
    /* 53 */ Mode { width: 320, height: 240, columns: 40, rows: 30, x_eig: 2, y_eig: 2, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 320, screen_size: 76_800, y_shift: 0, flags: Flags::new(0) },
];

/// The mode numbered `number`; `None` past RISC OS 5's own, which only the
/// module that defined it knows.
#[must_use]
pub fn numbered(number: u32) -> Option<&'static Mode> {
    MODES.get(usize::try_from(number).ok()?)
}

#[cfg(test)]
#[path = "mode_tests.rs"]
mod tests;
