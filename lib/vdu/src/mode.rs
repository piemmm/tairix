//! RISC OS 5's numbered screen modes, 0 to 53: each one's mode variables, as
//! `OS_ReadModeVariable` reads them for it on RISC OS 5.30. Mode 7 is
//! Teletext in sixteen colours at 640 by 500.

/// A mode's `ModeFlags`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Flags(u32);

impl Flags {
    /// None.
    pub const NONE: Self = Self(0);
    /// Text only: no graphics, and no sprite may be in it.
    pub const NON_GRAPHIC: Self = Self(1);
    /// Teletext.
    pub const TELETEXT: Self = Self(1 << 1);
    /// A gap between text rows.
    pub const GAP: Self = Self(1 << 2);
    /// The BBC's gaps, which its text modes 3 and 6 have.
    pub const BBC_GAP: Self = Self(1 << 3);
    /// A high-resolution monochrome mode.
    pub const HI_RES_MONO: Self = Self(1 << 4);
    /// Characters twice their height.
    pub const DOUBLE_VERTICAL: Self = Self(1 << 5);
    /// No hardware scrolling: the screen drawn is a sprite output has been
    /// switched to.
    pub const HARD_SCROLL_DISABLED: Self = Self(1 << 6);
    /// A full palette, each of its colours its own.
    pub const FULL_PALETTE: Self = Self(1 << 7);
    /// A default palette of greys.
    pub const GREYSCALE: Self = Self(1 << 9);
    /// Red and blue swapped in a direct-colour pixel.
    pub const RGB: Self = Self(1 << 14);
    /// Alpha rather than unused in a direct-colour pixel's top bits.
    pub const ALPHA: Self = Self(1 << 15);

    /// The flags `bits` hold.
    #[must_use]
    pub const fn new(bits: u32) -> Self {
        Self(bits)
    }

    /// The bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// These and `other`.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every flag of `flags` is among them.
    #[must_use]
    pub const fn has(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }
}

/// RISC OS 5's default mode: 640 by 480 in 256 colours.
pub const DEFAULT_MODE: u32 = 28;

/// The mode variables there are: 0 to 13.
pub const VARIABLES: u32 = 14;

/// The mode variables' numbers, as `OS_ReadModeVariable` takes them.
pub mod var {
    /// `ModeFlags`.
    pub const MODE_FLAGS: u32 = 0;
    /// `ScrRCol`.
    pub const SCR_R_COL: u32 = 1;
    /// `ScrBRow`.
    pub const SCR_B_ROW: u32 = 2;
    /// `NColour`.
    pub const N_COLOUR: u32 = 3;
    /// `XEigFactor`.
    pub const X_EIG: u32 = 4;
    /// `YEigFactor`.
    pub const Y_EIG: u32 = 5;
    /// `LineLength`.
    pub const LINE_LENGTH: u32 = 6;
    /// `ScreenSize`.
    pub const SCREEN_SIZE: u32 = 7;
    /// `YShftFactor`.
    pub const Y_SHIFT: u32 = 8;
    /// `Log2BPP`.
    pub const LOG2_BPP: u32 = 9;
    /// `Log2BPC`.
    pub const LOG2_BPC: u32 = 10;
    /// `XWindLimit`.
    pub const X_WIND_LIMIT: u32 = 11;
    /// `YWindLimit`.
    pub const Y_WIND_LIMIT: u32 = 12;
    /// `MinScreenBanks`.
    pub const MIN_SCREEN_BANKS: u32 = 13;
}

/// The VDU variables the text window's size is read from, as
/// `OS_ReadVduVariables` takes them.
pub mod vdu_var {
    /// `WindowWidth`.
    pub const WINDOW_WIDTH: u32 = 256;
    /// `WindowHeight`.
    pub const WINDOW_HEIGHT: u32 = 257;
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
    /*  0 */ Mode { width: 640, height: 256, columns: 80, rows: 32, x_eig: 1, y_eig: 2, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 80, screen_size: 20_480, y_shift: 4, flags: Flags::NONE },
    /*  1 */ Mode { width: 320, height: 256, columns: 40, rows: 32, x_eig: 2, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 80, screen_size: 20_480, y_shift: 4, flags: Flags::NONE },
    /*  2 */ Mode { width: 160, height: 256, columns: 20, rows: 32, x_eig: 3, y_eig: 2, log2_bpp: 2, log2_bpc: 3, ncolour: 15, line_length: 160, screen_size: 40_960, y_shift: 5, flags: Flags::NONE },
    /*  3 */ Mode { width: 640, height: 250, columns: 80, rows: 25, x_eig: 1, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 1, line_length: 160, screen_size: 40_960, y_shift: 5, flags: Flags::NON_GRAPHIC.union(Flags::GAP).union(Flags::BBC_GAP) },
    /*  4 */ Mode { width: 320, height: 256, columns: 40, rows: 32, x_eig: 2, y_eig: 2, log2_bpp: 0, log2_bpc: 1, ncolour: 1, line_length: 80, screen_size: 20_480, y_shift: 4, flags: Flags::NONE },
    /*  5 */ Mode { width: 160, height: 256, columns: 20, rows: 32, x_eig: 3, y_eig: 2, log2_bpp: 1, log2_bpc: 2, ncolour: 3, line_length: 80, screen_size: 20_480, y_shift: 4, flags: Flags::NONE },
    /*  6 */ Mode { width: 320, height: 250, columns: 40, rows: 25, x_eig: 2, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 1, line_length: 80, screen_size: 20_480, y_shift: 4, flags: Flags::NON_GRAPHIC.union(Flags::GAP).union(Flags::BBC_GAP) },
    /*  7 */ Mode { width: 640, height: 500, columns: 40, rows: 25, x_eig: 1, y_eig: 1, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 320, screen_size: 327_680, y_shift: 5, flags: Flags::NON_GRAPHIC.union(Flags::TELETEXT).union(Flags::GAP).union(Flags::DOUBLE_VERTICAL) },
    /*  8 */ Mode { width: 640, height: 256, columns: 80, rows: 32, x_eig: 1, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 160, screen_size: 40_960, y_shift: 5, flags: Flags::NONE },
    /*  9 */ Mode { width: 320, height: 256, columns: 40, rows: 32, x_eig: 2, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 160, screen_size: 40_960, y_shift: 5, flags: Flags::NONE },
    /* 10 */ Mode { width: 160, height: 256, columns: 20, rows: 32, x_eig: 3, y_eig: 2, log2_bpp: 3, log2_bpc: 4, ncolour: 63, line_length: 320, screen_size: 81_920, y_shift: 6, flags: Flags::NONE },
    /* 11 */ Mode { width: 640, height: 250, columns: 80, rows: 25, x_eig: 1, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 160, screen_size: 40_960, y_shift: 5, flags: Flags::GAP },
    /* 12 */ Mode { width: 640, height: 256, columns: 80, rows: 32, x_eig: 1, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 320, screen_size: 81_920, y_shift: 6, flags: Flags::NONE },
    /* 13 */ Mode { width: 320, height: 256, columns: 40, rows: 32, x_eig: 2, y_eig: 2, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 320, screen_size: 81_920, y_shift: 6, flags: Flags::NONE },
    /* 14 */ Mode { width: 640, height: 250, columns: 80, rows: 25, x_eig: 1, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 320, screen_size: 81_920, y_shift: 6, flags: Flags::GAP },
    /* 15 */ Mode { width: 640, height: 256, columns: 80, rows: 32, x_eig: 1, y_eig: 2, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 640, screen_size: 163_840, y_shift: 7, flags: Flags::NONE },
    /* 16 */ Mode { width: 1056, height: 256, columns: 132, rows: 32, x_eig: 1, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 528, screen_size: 135_168, y_shift: 0, flags: Flags::NONE },
    /* 17 */ Mode { width: 1056, height: 250, columns: 132, rows: 25, x_eig: 1, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 528, screen_size: 135_168, y_shift: 0, flags: Flags::GAP },
    /* 18 */ Mode { width: 640, height: 512, columns: 80, rows: 64, x_eig: 1, y_eig: 1, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 80, screen_size: 40_960, y_shift: 4, flags: Flags::NONE },
    /* 19 */ Mode { width: 640, height: 512, columns: 80, rows: 64, x_eig: 1, y_eig: 1, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 160, screen_size: 81_920, y_shift: 5, flags: Flags::NONE },
    /* 20 */ Mode { width: 640, height: 512, columns: 80, rows: 64, x_eig: 1, y_eig: 1, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 320, screen_size: 163_840, y_shift: 6, flags: Flags::NONE },
    /* 21 */ Mode { width: 640, height: 512, columns: 80, rows: 64, x_eig: 1, y_eig: 1, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 640, screen_size: 327_680, y_shift: 7, flags: Flags::NONE },
    /* 22 */ Mode { width: 768, height: 288, columns: 96, rows: 36, x_eig: 0, y_eig: 1, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 384, screen_size: 110_592, y_shift: 0, flags: Flags::NONE },
    /* 23 */ Mode { width: 1152, height: 896, columns: 144, rows: 56, x_eig: 1, y_eig: 1, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 144, screen_size: 129_024, y_shift: 0, flags: Flags::HI_RES_MONO.union(Flags::DOUBLE_VERTICAL) },
    /* 24 */ Mode { width: 1056, height: 256, columns: 132, rows: 32, x_eig: 1, y_eig: 2, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 1056, screen_size: 270_336, y_shift: 0, flags: Flags::NONE },
    /* 25 */ Mode { width: 640, height: 480, columns: 80, rows: 60, x_eig: 1, y_eig: 1, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 80, screen_size: 38_400, y_shift: 4, flags: Flags::NONE },
    /* 26 */ Mode { width: 640, height: 480, columns: 80, rows: 60, x_eig: 1, y_eig: 1, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 160, screen_size: 76_800, y_shift: 5, flags: Flags::NONE },
    /* 27 */ Mode { width: 640, height: 480, columns: 80, rows: 60, x_eig: 1, y_eig: 1, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 320, screen_size: 153_600, y_shift: 6, flags: Flags::NONE },
    /* 28 */ Mode { width: 640, height: 480, columns: 80, rows: 60, x_eig: 1, y_eig: 1, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 640, screen_size: 307_200, y_shift: 7, flags: Flags::NONE },
    /* 29 */ Mode { width: 800, height: 600, columns: 100, rows: 75, x_eig: 1, y_eig: 1, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 100, screen_size: 60_000, y_shift: 0, flags: Flags::NONE },
    /* 30 */ Mode { width: 800, height: 600, columns: 100, rows: 75, x_eig: 1, y_eig: 1, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 200, screen_size: 120_000, y_shift: 0, flags: Flags::NONE },
    /* 31 */ Mode { width: 800, height: 600, columns: 100, rows: 75, x_eig: 1, y_eig: 1, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 400, screen_size: 240_000, y_shift: 0, flags: Flags::NONE },
    /* 32 */ Mode { width: 800, height: 600, columns: 100, rows: 75, x_eig: 1, y_eig: 1, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 800, screen_size: 480_000, y_shift: 0, flags: Flags::NONE },
    /* 33 */ Mode { width: 768, height: 288, columns: 96, rows: 36, x_eig: 1, y_eig: 2, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 96, screen_size: 27_648, y_shift: 0, flags: Flags::NONE },
    /* 34 */ Mode { width: 768, height: 288, columns: 96, rows: 36, x_eig: 1, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 192, screen_size: 55_296, y_shift: 0, flags: Flags::NONE },
    /* 35 */ Mode { width: 768, height: 288, columns: 96, rows: 36, x_eig: 1, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 384, screen_size: 110_592, y_shift: 0, flags: Flags::NONE },
    /* 36 */ Mode { width: 768, height: 288, columns: 96, rows: 36, x_eig: 1, y_eig: 2, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 768, screen_size: 221_184, y_shift: 0, flags: Flags::NONE },
    /* 37 */ Mode { width: 896, height: 352, columns: 112, rows: 44, x_eig: 1, y_eig: 2, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 112, screen_size: 39_424, y_shift: 0, flags: Flags::NONE },
    /* 38 */ Mode { width: 896, height: 352, columns: 112, rows: 44, x_eig: 1, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 224, screen_size: 78_848, y_shift: 0, flags: Flags::NONE },
    /* 39 */ Mode { width: 896, height: 352, columns: 112, rows: 44, x_eig: 1, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 448, screen_size: 157_696, y_shift: 0, flags: Flags::NONE },
    /* 40 */ Mode { width: 896, height: 352, columns: 112, rows: 44, x_eig: 1, y_eig: 2, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 896, screen_size: 315_392, y_shift: 0, flags: Flags::NONE },
    /* 41 */ Mode { width: 640, height: 352, columns: 80, rows: 44, x_eig: 1, y_eig: 2, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 80, screen_size: 28_160, y_shift: 0, flags: Flags::NONE },
    /* 42 */ Mode { width: 640, height: 352, columns: 80, rows: 44, x_eig: 1, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 160, screen_size: 56_320, y_shift: 0, flags: Flags::NONE },
    /* 43 */ Mode { width: 640, height: 352, columns: 80, rows: 44, x_eig: 1, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 320, screen_size: 112_640, y_shift: 0, flags: Flags::NONE },
    /* 44 */ Mode { width: 640, height: 200, columns: 80, rows: 25, x_eig: 1, y_eig: 2, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 80, screen_size: 16_000, y_shift: 0, flags: Flags::NONE },
    /* 45 */ Mode { width: 640, height: 200, columns: 80, rows: 25, x_eig: 1, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 160, screen_size: 32_000, y_shift: 0, flags: Flags::NONE },
    /* 46 */ Mode { width: 640, height: 200, columns: 80, rows: 25, x_eig: 1, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 320, screen_size: 64_000, y_shift: 0, flags: Flags::NONE },
    /* 47 */ Mode { width: 360, height: 480, columns: 45, rows: 60, x_eig: 2, y_eig: 2, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 360, screen_size: 172_800, y_shift: 0, flags: Flags::NONE },
    /* 48 */ Mode { width: 320, height: 480, columns: 40, rows: 60, x_eig: 2, y_eig: 1, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 160, screen_size: 76_800, y_shift: 0, flags: Flags::NONE },
    /* 49 */ Mode { width: 320, height: 480, columns: 40, rows: 60, x_eig: 2, y_eig: 1, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 320, screen_size: 153_600, y_shift: 0, flags: Flags::NONE },
    /* 50 */ Mode { width: 320, height: 240, columns: 40, rows: 30, x_eig: 2, y_eig: 2, log2_bpp: 0, log2_bpc: 0, ncolour: 1, line_length: 40, screen_size: 9_600, y_shift: 0, flags: Flags::NONE },
    /* 51 */ Mode { width: 320, height: 240, columns: 40, rows: 30, x_eig: 2, y_eig: 2, log2_bpp: 1, log2_bpc: 1, ncolour: 3, line_length: 80, screen_size: 19_200, y_shift: 0, flags: Flags::NONE },
    /* 52 */ Mode { width: 320, height: 240, columns: 40, rows: 30, x_eig: 2, y_eig: 2, log2_bpp: 2, log2_bpc: 2, ncolour: 15, line_length: 160, screen_size: 38_400, y_shift: 0, flags: Flags::NONE },
    /* 53 */ Mode { width: 320, height: 240, columns: 40, rows: 30, x_eig: 2, y_eig: 2, log2_bpp: 3, log2_bpc: 3, ncolour: 63, line_length: 320, screen_size: 76_800, y_shift: 0, flags: Flags::NONE },
];

/// The mode numbered `number`; `None` past RISC OS 5's own, which only the
/// module that defined it knows.
#[must_use]
pub fn numbered(number: u32) -> Option<&'static Mode> {
    MODES.get(usize::try_from(number).ok()?)
}

/// The low `width` bits of `value` repeated until `total` bits are filled:
/// how screen memory holds a pixel a mode doubles across its wider field,
/// and how a colour fills a word of pixels.
#[must_use]
pub const fn repeated(value: u32, width: u32, total: u32) -> u32 {
    let mut out = value;
    let mut filled = width;
    while filled < total {
        out |= out << filled;
        filled *= 2;
    }
    out
}

#[cfg(test)]
#[path = "mode_tests.rs"]
mod tests;
