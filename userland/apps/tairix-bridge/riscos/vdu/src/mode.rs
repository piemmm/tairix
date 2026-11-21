//! A screen mode as the VDU drivers hold it: its geometry and pixel format,
//! made from a numbered mode, a mode selector or a sprite mode word, with the
//! mode variables `OS_ReadModeVariable` reads from it.

use tairix_vdu::mode::{Flags, Mode};

use crate::teletext::{MOST_COLUMNS, MOST_ROWS};

/// The most pixels across or down a screen named by a selector.
const MOST_PIXELS: u32 = 0x4000;

pub(crate) use tairix_vdu::mode::{var, VARIABLES};

/// A mode's variables, as `OS_ReadModeVariable` reads them for a mode named
/// by number, selector or sprite mode word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Format {
    /// Each variable by number, 0 to 12.
    vars: [u32; 13],
}

impl Format {
    /// A numbered mode's.
    pub(crate) const fn numbered(mode: &Mode) -> Self {
        Self {
            vars: [
                mode.flags.bits(),
                mode.columns as u32 - 1,
                mode.rows as u32 - 1,
                mode.ncolour as u32,
                mode.x_eig as u32,
                mode.y_eig as u32,
                mode.line_length as u32,
                mode.screen_size,
                mode.y_shift as u32,
                mode.log2_bpp as u32,
                mode.log2_bpc as u32,
                mode.width as u32 - 1,
                mode.height as u32 - 1,
            ],
        }
    }

    /// A selector's: `width` by `height` at `log2_bpp`, each variable as a
    /// standard mode of the size takes it, its text as many cells as fit,
    /// then `pairs` setting any. `None` for a variable past those there are,
    /// or variables that make no screen the drivers can draw.
    pub(crate) fn selector(
        width: u32,
        height: u32,
        log2_bpp: u32,
        pairs: impl IntoIterator<Item = (u32, u32)>,
    ) -> Option<Self> {
        if log2_bpp > 5 || width == 0 || height == 0 || width > MOST_PIXELS || height > MOST_PIXELS
        {
            return None;
        }
        let ncolour = default_ncolour(log2_bpp);
        let flags = if log2_bpp == 3 {
            Flags::FULL_PALETTE.bits()
        } else {
            0
        };
        let y_eig = if height >= width / 2 { 1 } else { 2 };
        let line_length = (width << log2_bpp).div_ceil(8);
        let mut format = Self {
            vars: [
                flags,
                0,
                0,
                ncolour,
                1,
                y_eig,
                line_length,
                line_length * height,
                0,
                log2_bpp,
                log2_bpp,
                width - 1,
                height - 1,
            ],
        };
        let (mut columns_given, mut rows_given) = (false, false);
        for (index, value) in pairs {
            if index == var::LOG2_BPP {
                return None;
            }
            *format.vars.get_mut(index as usize)? = value;
            columns_given |= index == var::SCR_R_COL;
            rows_given |= index == var::SCR_B_ROW;
        }
        let cell = format.cell();
        let fitted = |pixels: u32, per_cell: u32| (pixels / per_cell).saturating_sub(1);
        if !columns_given {
            format.vars[var::SCR_R_COL as usize] = fitted(format.width_limit()?, cell.size_x);
        }
        if !rows_given {
            format.vars[var::SCR_B_ROW as usize] = fitted(format.height_limit()?, cell.space_y);
        }
        format.drawable().then_some(format)
    }

    /// The pixels across and down its window limits give, if within
    /// [`MOST_PIXELS`].
    fn width_limit(&self) -> Option<u32> {
        self.vars[var::X_WIND_LIMIT as usize]
            .checked_add(1)
            .filter(|&w| w <= MOST_PIXELS)
    }

    fn height_limit(&self) -> Option<u32> {
        self.vars[var::Y_WIND_LIMIT as usize]
            .checked_add(1)
            .filter(|&h| h <= MOST_PIXELS)
    }

    /// Whether its variables make a screen the drivers can draw: eigen
    /// factors to 3; a pixel of a depth there is, no wider in memory than a
    /// word nor narrower than its colours; lines long enough for their
    /// pixels; colours the depth has; Teletext in sixteen colours; and text
    /// within the screen, or a Teletext page within the most it holds.
    fn drawable(&self) -> bool {
        let (Some(width), Some(height)) = (self.width_limit(), self.height_limit()) else {
            return false;
        };
        let (bpp, bpc) = (self.log2_bpp(), self.log2_bpc());
        let pixels_fit = bpp <= 5 && bpc >= bpp && bpc <= 5;
        let lines_fit = pixels_fit
            && self.line_length() >= (width << bpc).div_ceil(8)
            && self.line_length() <= (MOST_PIXELS << 5) / 8;
        let colours = crate::modestring::is_colour_format(bpp, self.ncolour());
        let cell = self.cell();
        let (columns, rows) = (
            self.vars[var::SCR_R_COL as usize],
            self.vars[var::SCR_B_ROW as usize],
        );
        let text_fits = if self.has(Flags::TELETEXT) {
            bpp == 2 && bpc == 2 && (columns as usize) < MOST_COLUMNS && (rows as usize) < MOST_ROWS
        } else {
            u64::from(columns) < u64::from(width / cell.size_x)
                && u64::from(rows) < u64::from(height / cell.space_y)
        };
        self.x_eig() <= 3 && self.y_eig() <= 3 && lines_fit && colours && text_fits
    }

    /// A sprite mode word's: only the variables a sprite's type and
    /// resolutions decide; `None` for a word naming no type there is.
    pub(crate) fn sprite_word(word: u32) -> Option<SpriteFormat> {
        let kind = word >> 27;
        let log2_bpp = match kind {
            1..=6 => kind - 1,
            _ => return None,
        };
        let eig = |dpi: u32| match dpi {
            180 => Some(0),
            90 => Some(1),
            45 => Some(2),
            22 | 23 => Some(3),
            _ => None,
        };
        Some(SpriteFormat {
            log2_bpp,
            ncolour: default_ncolour(log2_bpp),
            x_eig: eig((word >> 1) & 0x1FFF)?,
            y_eig: eig((word >> 14) & 0x1FFF)?,
        })
    }

    /// Variable `number`, as a mode named rather than the current one reads
    /// it.
    pub(crate) fn var(&self, number: u32) -> Option<u32> {
        match number {
            var::MIN_SCREEN_BANKS => Some(0),
            _ => self.vars.get(number as usize).copied(),
        }
    }

    pub(crate) const fn flags(&self) -> u32 {
        self.vars[var::MODE_FLAGS as usize]
    }

    pub(crate) const fn columns(&self) -> u32 {
        self.vars[var::SCR_R_COL as usize] + 1
    }

    pub(crate) const fn rows(&self) -> u32 {
        self.vars[var::SCR_B_ROW as usize] + 1
    }

    pub(crate) const fn ncolour(&self) -> u32 {
        self.vars[var::N_COLOUR as usize]
    }

    pub(crate) const fn x_eig(&self) -> u32 {
        self.vars[var::X_EIG as usize]
    }

    pub(crate) const fn y_eig(&self) -> u32 {
        self.vars[var::Y_EIG as usize]
    }

    pub(crate) const fn line_length(&self) -> u32 {
        self.vars[var::LINE_LENGTH as usize]
    }

    pub(crate) const fn log2_bpp(&self) -> u32 {
        self.vars[var::LOG2_BPP as usize]
    }

    pub(crate) const fn log2_bpc(&self) -> u32 {
        self.vars[var::LOG2_BPC as usize]
    }

    /// Pixels across.
    pub(crate) const fn width(&self) -> u32 {
        self.vars[var::X_WIND_LIMIT as usize] + 1
    }

    /// How many pixels the screen shows for each one stored: its character
    /// pixel's bits over its pixel's.
    pub(crate) const fn doubling(&self) -> u32 {
        1 << (self.log2_bpc() - self.log2_bpp())
    }

    /// Pixels across as the screen shows them.
    pub(crate) const fn shown_width(&self) -> u32 {
        self.width() * self.doubling()
    }

    /// The bits of a pixel value the mode keeps.
    pub(crate) const fn pixel_mask(&self) -> u32 {
        crate::canvas::mask_between(0, 1 << self.log2_bpp())
    }

    /// Pixels down.
    pub(crate) const fn height(&self) -> u32 {
        self.vars[var::Y_WIND_LIMIT as usize] + 1
    }

    pub(crate) const fn has(&self, flags: Flags) -> bool {
        Flags::new(self.flags()).has(flags)
    }

    /// Whether it has pixels to draw.
    pub(crate) const fn graphic(&self) -> bool {
        !self.has(Flags::NON_GRAPHIC)
    }

    /// The bytes the screen takes: the lines it shows, twice over for
    /// Teletext, whose flashing alternates two frames.
    pub(crate) const fn screen_bytes(&self) -> u32 {
        let frame = self.line_length() * self.height();
        if self.has(Flags::TELETEXT) {
            2 * frame
        } else {
            frame
        }
    }

    /// `ModeFlags` as the current screen reads it: the greyscale bit where
    /// the default palette is black and white.
    pub(crate) const fn current_flags(&self) -> u32 {
        if self.ncolour() == 1 {
            self.flags() | Flags::GREYSCALE.bits()
        } else {
            self.flags()
        }
    }

    /// Variable `number` as the current screen reads it, where the screen's
    /// own size and flags differ from what the mode's name gives.
    pub(crate) fn current_var(&self, number: u32) -> Option<u32> {
        match number {
            var::MODE_FLAGS => Some(self.current_flags()),
            var::SCREEN_SIZE => Some(self.screen_bytes()),
            _ => self.var(number),
        }
    }

    /// The text cell: its glyph's pixels across and down, and the pixels a
    /// row of text takes, its gap below the glyph included. A Teletext cell
    /// is always sixteen by twenty, its glyph's twenty half-dot lines.
    pub(crate) const fn cell(&self) -> Cell {
        if self.has(Flags::TELETEXT) {
            return Cell {
                size_x: crate::teletext::CELL_WIDTH,
                size_y: 16,
                space_y: crate::teletext::CELL_HEIGHT,
            };
        }
        let double = self.has(Flags::DOUBLE_VERTICAL);
        let size_y = if double { 16 } else { 8 };
        let gap = match (self.has(Flags::GAP), double) {
            (false, _) => 0,
            (true, false) => 2,
            (true, true) => 4,
        };
        Cell {
            size_x: 8,
            size_y,
            space_y: size_y + gap,
        }
    }

    /// Whether the mode is a direct-colour one, without a palette.
    pub(crate) const fn direct(&self) -> bool {
        self.log2_bpp() >= 4
    }

    /// Whether an 8-bit mode takes its palette in VIDC1's groups of sixteen.
    pub(crate) const fn grouped(&self) -> bool {
        self.log2_bpp() == 3 && self.ncolour() == 63
    }
}

/// The text cell of a mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cell {
    /// The glyph's pixels across, the cell's too.
    pub(crate) size_x: u32,
    /// The glyph's pixels down.
    pub(crate) size_y: u32,
    /// The cell's pixels down.
    pub(crate) space_y: u32,
}

/// What a sprite mode word decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SpriteFormat {
    pub(crate) log2_bpp: u32,
    pub(crate) ncolour: u32,
    pub(crate) x_eig: u32,
    pub(crate) y_eig: u32,
}

impl SpriteFormat {
    /// Variable `number`, of those a sprite mode word decides.
    pub(crate) const fn var(&self, number: u32) -> Option<u32> {
        match number {
            var::MODE_FLAGS | var::Y_SHIFT => Some(0),
            var::N_COLOUR => Some(self.ncolour),
            var::X_EIG => Some(self.x_eig),
            var::Y_EIG => Some(self.y_eig),
            var::LOG2_BPP | var::LOG2_BPC => Some(self.log2_bpp),
            _ => None,
        }
    }
}

/// The highest colour number a depth gives a mode by default.
const fn default_ncolour(log2_bpp: u32) -> u32 {
    match log2_bpp {
        0 => 1,
        1 => 3,
        2 => 15,
        3 => 255,
        4 => 0xFFFF,
        _ => 0xFFFF_FFFF,
    }
}

#[cfg(test)]
mod tests;
