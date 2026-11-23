//! A screen mode as the VDU drivers hold it: its geometry and pixel format,
//! made from a numbered mode, a mode selector or a sprite mode word, with the
//! mode variables `OS_ReadModeVariable` reads from it.

use tairix_vdu::mode::{Flags, Mode};

use crate::teletext::{MOST_COLUMNS, MOST_ROWS};

/// The most pixels across or down a screen named by a selector.
const MOST_PIXELS: u32 = 0x4000;

pub(crate) use tairix_vdu::mode::{var, VARIABLES};

/// What a sprite output is switched to is drawn as: its mode's flags,
/// colours, eigen factors and Y shift, its pixels and its rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Drawn {
    pub(crate) flags: u32,
    pub(crate) ncolour: u32,
    pub(crate) eig: (u32, u32),
    pub(crate) y_shift: u32,
    pub(crate) log2_bpp: u32,
    pub(crate) log2_bpc: u32,
    pub(crate) line_length: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// The largest eigen factor a screen the drivers draw, or a sprite's mode,
/// takes: a pixel eight OS units across.
pub(crate) const MOST_EIG: u32 = 3;

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
        let y_eig = if height >= width / 2 { 1 } else { 2 };
        let line_length = (width << log2_bpp).div_ceil(8);
        let mut format = Self {
            vars: [
                0,
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

    /// A sprite's, as output switched to it draws on it: `width` by `height`
    /// pixels in rows of `line_length` bytes, its text as many whole cells as
    /// fit, none where not one does; its screen size is the screen's, read
    /// from the screen.
    pub(crate) fn sprite(drawn: &Drawn) -> Self {
        let mut format = Self {
            vars: [
                drawn.flags,
                0,
                0,
                drawn.ncolour,
                drawn.eig.0,
                drawn.eig.1,
                drawn.line_length,
                drawn.line_length.saturating_mul(drawn.height),
                drawn.y_shift,
                drawn.log2_bpp,
                drawn.log2_bpc,
                drawn.width.wrapping_sub(1),
                drawn.height.wrapping_sub(1),
            ],
        };
        let cell = format.cell();
        format.vars[var::SCR_R_COL as usize] = (drawn.width / cell.size_x).wrapping_sub(1);
        format.vars[var::SCR_B_ROW as usize] = (drawn.height / cell.space_y).wrapping_sub(1);
        format
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
        self.x_eig() <= MOST_EIG && self.y_eig() <= MOST_EIG && lines_fit && colours && text_fits
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

    /// Text columns and rows: none for a sprite output is switched to that
    /// is narrower or shorter than a cell.
    pub(crate) const fn columns(&self) -> u32 {
        self.vars[var::SCR_R_COL as usize].wrapping_add(1)
    }

    pub(crate) const fn rows(&self) -> u32 {
        self.vars[var::SCR_B_ROW as usize].wrapping_add(1)
    }

    /// The columns and rows the text window may span: one of each where it
    /// holds no whole cell, a cell drawn nowhere.
    pub(crate) const fn cells(&self) -> (u32, u32) {
        let (columns, rows) = (self.columns(), self.rows());
        (
            if columns == 0 { 1 } else { columns },
            if rows == 0 { 1 } else { rows },
        )
    }

    /// Whether it holds a whole text cell, so text is drawn.
    pub(crate) const fn has_cells(&self) -> bool {
        self.columns() != 0 && self.rows() != 0
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

    /// Whether its pixels are `other`'s: the same depth, field and colours,
    /// held as the same colour flags hold them, whatever either's size or
    /// its other flags.
    pub(crate) const fn same_pixels(&self, other: &Self) -> bool {
        const COLOUR_FLAGS: u32 = Flags::FULL_PALETTE
            .union(Flags::GREYSCALE)
            .union(Flags::RGB)
            .union(Flags::ALPHA)
            .bits();
        (self.flags() ^ other.flags()) & COLOUR_FLAGS == 0
            && self.ncolour() == other.ncolour()
            && self.log2_bpp() == other.log2_bpp()
            && self.log2_bpc() == other.log2_bpc()
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
    /// Teletext, whose flashing alternates two frames; a word's most where
    /// that passes a word, which no screen memory holds.
    pub(crate) const fn screen_bytes(&self) -> u32 {
        let Some(frame) = self.line_length().checked_mul(self.height()) else {
            return u32::MAX;
        };
        if self.has(Flags::TELETEXT) {
            frame.saturating_mul(2)
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
        self.log2_bpp() == 3 && !self.full_palette()
    }

    /// Whether an 8-bit mode holds 256 palette entries of its own.
    pub(crate) const fn full_palette(&self) -> bool {
        full_palette(self.log2_bpp(), self.flags())
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

/// Whether a mode of `log2_bpp` bits a pixel and mode flags `flags` holds 256
/// palette entries of its own: an 8-bit one its flags say does, as RISC OS 5
/// tells them apart, whatever its colour count.
pub(crate) const fn full_palette(log2_bpp: u32, flags: u32) -> bool {
    log2_bpp == 3 && Flags::new(flags).has(Flags::FULL_PALETTE)
}

/// The highest colour number a depth gives a mode by default.
pub(crate) const fn default_ncolour(log2_bpp: u32) -> u32 {
    match log2_bpp {
        0 => 1,
        1 => 3,
        2 => 15,
        3 => 63,
        4 => 0xFFFF,
        _ => 0xFFFF_FFFF,
    }
}

#[cfg(test)]
mod tests;
