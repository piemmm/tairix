//! INT 10h's text and graphics services: modes on the MDA and CGA, and for
//! every card the cursor, pages, scrolling, characters in text and graphics,
//! pixels and the teletype, with the bell a half-second tone the stub waits
//! out. The EGA's and VGA's own services are their ROM's.

use super::bda;
use super::guest::Guest;
use super::layout::{table, BUFFER_LENGTHS, MODE_COLUMNS, MODE_REGISTERS, SEGMENT};
use super::modes::Layout;
use super::vga;
use crate::asm::{Reg16, Reg8};
use crate::chips::ppi::port_b;

/// The stub's state: the tick the bell stops at.
pub const STATE: u8 = 2;
/// The bell lasts nine ticks, half a second.
const BELL_TICKS: u16 = 9;

/// The current mode, as the data area describes it.
#[derive(Clone, Copy, Debug)]
pub struct Screen {
    pub(super) layout: Layout,
    segment: u16,
    pub(super) columns: u16,
    rows: u16,
    page_size: u16,
    pub(super) crtc: u16,
    /// Scan lines a character takes.
    pub(super) height: u16,
}

impl Screen {
    /// Its columns of text.
    pub(crate) const fn text_columns(&self) -> u16 {
        self.columns
    }

    /// Its rows of text.
    pub(crate) const fn text_rows(&self) -> u16 {
        self.rows
    }

    /// How the mode lays its memory out.
    pub(crate) const fn layout(&self) -> Layout {
        self.layout
    }

    /// The segment its memory starts at.
    pub(crate) const fn segment(&self) -> u16 {
        self.segment
    }

    /// Bytes a display page takes.
    pub(crate) const fn page_size(&self) -> u16 {
        self.page_size
    }

    /// The CRTC's index port.
    pub(crate) const fn crtc_port(&self) -> u16 {
        self.crtc
    }

    pub fn of(g: &mut Guest<'_>) -> Self {
        let mode = g.bda8(bda::VIDEO_MODE);
        let crtc = g.bda16(bda::CRTC_PORT);
        let columns = g.bda16(bda::COLUMNS).max(1);
        let page_size = g.bda16(bda::PAGE_SIZE);
        if vga::present(g) {
            if let Some(layout) = vga::layout(g, mode) {
                let segment = match layout {
                    Layout::Text if mode == 7 => 0xB000,
                    Layout::Text | Layout::Cga4 | Layout::Cga2 => 0xB800,
                    Layout::Planar | Layout::Linear => 0xA000,
                };
                let rows = u16::from(g.bda8(bda::ROWS)) + 1;
                let height = g.bda16(bda::CHAR_HEIGHT).clamp(1, 32);
                return Self {
                    layout,
                    segment,
                    columns,
                    rows,
                    page_size,
                    crtc,
                    height,
                };
            }
        }
        let layout = match mode {
            4 | 5 => Layout::Cga4,
            6 => Layout::Cga2,
            _ => Layout::Text,
        };
        Self {
            layout,
            segment: if mode == 7 { 0xB000 } else { 0xB800 },
            columns,
            rows: 25,
            page_size,
            crtc,
            height: 8,
        }
    }

    /// The offset of text cell `row`, `column` on `page`.
    pub(crate) fn cell(&self, page: u8, row: u16, column: u16) -> u16 {
        let index = row.wrapping_mul(self.columns).wrapping_add(column);
        u16::from(page)
            .wrapping_mul(self.page_size)
            .wrapping_add(index.wrapping_mul(2))
    }

    /// Bytes of a character cell's scan line in graphics.
    fn cell_bytes(&self) -> u16 {
        match self.layout {
            Layout::Cga4 => 2,
            Layout::Linear => 8,
            _ => 1,
        }
    }

    /// The offset of scan line `y`'s byte `x`: the CGA's odd lines in the
    /// buffer's second half, the planar and linear modes a line after
    /// another.
    pub(crate) fn line(&self, y: u16, x: u16) -> u16 {
        match self.layout {
            Layout::Cga4 | Layout::Cga2 => ((y & 1) * 0x2000)
                .wrapping_add((y >> 1).wrapping_mul(80))
                .wrapping_add(x),
            Layout::Linear => y.wrapping_mul(320).wrapping_add(x),
            _ => y.wrapping_mul(self.columns).wrapping_add(x),
        }
    }

    pub(crate) fn width(&self) -> u16 {
        match self.layout {
            Layout::Cga4 | Layout::Linear => 320,
            _ => self.columns.saturating_mul(8),
        }
    }

    pub(crate) fn lines(&self) -> u16 {
        self.rows * self.height
    }
}

pub(super) fn crtc(g: &mut Guest<'_>, screen: &Screen, register: u8, value: u8) {
    g.output(screen.crtc, register);
    g.output(screen.crtc.wrapping_add(1), value);
}

fn gc(g: &mut Guest<'_>, register: u8, value: u8) {
    g.output(0x3CE, register);
    g.output(0x3CF, value);
}

/// Back to the graphics controller's plain writes after a planar access.
fn gc_restore(g: &mut Guest<'_>) {
    for (register, value) in [(3, 0x00), (4, 0x00), (5, 0x00), (8, 0xFF)] {
        gc(g, register, value);
    }
}

/// The parameter table INT 1Dh points at, which a program may replace.
fn parameters(g: &mut Guest<'_>) -> (u16, u16) {
    (g.read16(0, 0x1D * 4 + 2), g.read16(0, 0x1D * 4))
}

/// Sets mode `mode` on the MDA or CGA: the mono adapter only ever shows
/// mode 7, and a colour one modes 0–6.
pub fn set_mode(g: &mut Guest<'_>, mode: u8) {
    let mono = (g.bda16(bda::EQUIPMENT) >> 4) & 3 == 3;
    let mode = match (mono, mode & 0x7F) {
        (true, _) => 7,
        (false, m @ 0..=6) => m,
        (false, _) => return,
    };
    let crtc_port = if mono { 0x3B4 } else { 0x3D4 };
    let (seg, base) = parameters(g);
    let block = match mode {
        0 | 1 => 0,
        2 | 3 => 1,
        4..=6 => 2,
        _ => 3,
    };
    let control = MODE_REGISTERS[usize::from(mode)];
    // The picture off while the timing changes.
    g.output(crtc_port + 4, control & !0x08);
    for register in 0..16 {
        let value = g.read8(seg, base.wrapping_add(block * 16 + u16::from(register)));
        g.output(crtc_port, register);
        g.output(crtc_port + 1, value);
    }
    let graphics = (4..=6).contains(&mode);
    let segment = if mono { 0xB000 } else { 0xB800 };
    let (words, fill) = if mono {
        (0x0800, 0x0720)
    } else if graphics {
        (0x2000, 0)
    } else {
        (0x2000, 0x0720)
    };
    for word in 0..words {
        g.write16(segment, word * 2, fill);
    }
    g.output(crtc_port + 4, control);
    let palette = if mode == 6 { 0x3F } else { 0x30 };
    if !mono {
        g.output(0x3D9, palette);
    }
    let length = BUFFER_LENGTHS[usize::from(mode / 2)];
    let columns = MODE_COLUMNS[usize::from(mode)];
    g.set_bda8(bda::VIDEO_MODE, mode);
    g.set_bda16(bda::COLUMNS, u16::from(columns));
    g.set_bda16(bda::PAGE_SIZE, length);
    g.set_bda16(bda::PAGE_START, 0);
    for page in 0..8 {
        g.set_bda16(bda::CURSOR + page * 2, 0);
    }
    g.set_bda8(bda::ACTIVE_PAGE, 0);
    g.set_bda16(bda::CRTC_PORT, crtc_port);
    g.set_bda8(bda::MODE_REGISTER, control);
    g.set_bda8(bda::PALETTE, palette);
    let screen = Screen::of(g);
    set_shape(g, &screen, if mono { 0x0B0C } else { 0x0607 });
    place_cursor(g, &screen, 0);
}

pub fn set_shape(g: &mut Guest<'_>, screen: &Screen, shape: u16) {
    g.set_bda16(bda::CURSOR_SHAPE, shape);
    let [end, start] = shape.to_le_bytes();
    crtc(g, screen, 10, start);
    crtc(g, screen, 11, end);
}

/// Page `page`'s cursor: its row and column.
pub fn cursor_of(g: &mut Guest<'_>, page: u8) -> (u16, u16) {
    let [column, row] = g.bda16(bda::CURSOR + u16::from(page & 7) * 2).to_le_bytes();
    (u16::from(row), u16::from(column))
}

/// Moves page `page`'s cursor.
pub fn move_cursor(g: &mut Guest<'_>, page: u8, row: u16, column: u16) {
    let position = u16::from_le_bytes([column.to_le_bytes()[0], row.to_le_bytes()[0]]);
    g.set_bda16(bda::CURSOR + u16::from(page & 7) * 2, position);
    let screen = Screen::of(g);
    place_cursor(g, &screen, page);
}

/// Shows `page`'s cursor, if it is the page on the screen.
pub fn place_cursor(g: &mut Guest<'_>, screen: &Screen, page: u8) {
    if page != g.bda8(bda::ACTIVE_PAGE) {
        return;
    }
    let (row, column) = cursor_of(g, page);
    let at = (g.bda16(bda::PAGE_START) / 2)
        .wrapping_add(row.wrapping_mul(screen.columns).wrapping_add(column));
    let [low, high] = at.to_le_bytes();
    crtc(g, screen, 14, high);
    crtc(g, screen, 15, low);
}

/// Shows page `page`: text pages on every card, and the EGA's and VGA's
/// planar ones. The CRT controller counts a text mode's words and a planar
/// mode's bytes.
fn select_page(g: &mut Guest<'_>, page: u8) {
    let screen = Screen::of(g);
    let (pages, divisor) = match screen.layout {
        // The EGA and VGA give text 32 KiB, the CGA 16.
        Layout::Text if vga::present(g) => {
            ((0x8000 / u32::from(screen.page_size.max(1))).clamp(1, 8), 2)
        }
        Layout::Text => (if screen.columns == 40 { 8 } else { 4 }, 2),
        Layout::Planar => (
            (0x1_0000 / u32::from(screen.page_size.max(1))).clamp(1, 8),
            1,
        ),
        _ => return,
    };
    let page = u8::try_from(u32::from(page) % pages).unwrap_or(0);
    let start = u16::from(page).wrapping_mul(screen.page_size);
    g.set_bda8(bda::ACTIVE_PAGE, page);
    g.set_bda16(bda::PAGE_START, start);
    let [low, high] = (start / divisor).to_le_bytes();
    crtc(g, &screen, 12, high);
    crtc(g, &screen, 13, low);
    place_cursor(g, &screen, page);
}

/// Scrolls the window from `top`, `left` to `bottom`, `right` by `lines`
/// (all of it when 0), up or down, filling with `fill`.
fn scroll(
    g: &mut Guest<'_>,
    up: bool,
    lines: u8,
    fill: u8,
    (top, left): (u16, u16),
    (bottom, right): (u16, u16),
) {
    let screen = Screen::of(g);
    let bottom = bottom.min(screen.rows - 1);
    let right = right.min(screen.columns - 1);
    if top > bottom || left > right {
        return;
    }
    let height = bottom - top + 1;
    let lines = match u16::from(lines) {
        0 => height,
        n => n.min(height),
    };
    if screen.layout == Layout::Planar {
        // Write mode 1 copies all four planes through the latches.
        gc(g, 5, 0x01);
    }
    for i in 0..height {
        let row = if up { top + i } else { bottom - i };
        let source = (i + lines < height).then(|| if up { row + lines } else { row - lines });
        for column in left..=right {
            copy_cell(g, &screen, source, row, column, fill);
        }
    }
    if screen.layout == Layout::Planar {
        gc_restore(g);
    }
}

/// Moves the character cell at `source`'s row to `row`, or fills it.
fn copy_cell(
    g: &mut Guest<'_>,
    screen: &Screen,
    source: Option<u16>,
    row: u16,
    column: u16,
    fill: u8,
) {
    let page = g.bda8(bda::ACTIVE_PAGE);
    if screen.layout == Layout::Text {
        let to = screen.cell(page, row, column);
        let value = match source {
            Some(from) => g.read16(screen.segment, screen.cell(page, from, column)),
            None => u16::from_le_bytes([b' ', fill]),
        };
        g.write16(screen.segment, to, value);
        return;
    }
    let bytes = screen.cell_bytes();
    let start = if screen.layout == Layout::Planar {
        g.bda16(bda::PAGE_START)
    } else {
        0
    };
    let pattern = match screen.layout {
        Layout::Cga4 => (fill & 3) * 0x55,
        Layout::Cga2 => u8::from(fill & 1 != 0) * 0xFF,
        _ => fill,
    };
    for line in 0..screen.height {
        for byte in 0..bytes {
            let x = column.wrapping_mul(bytes).wrapping_add(byte);
            let to = start.wrapping_add(screen.line(row * screen.height + line, x));
            match (source, screen.layout) {
                (Some(from), _) => {
                    let value = g.read8(
                        screen.segment,
                        start.wrapping_add(screen.line(from * screen.height + line, x)),
                    );
                    g.write8(screen.segment, to, value);
                }
                (None, Layout::Planar) => {
                    gc(g, 5, 0x02);
                    g.read8(screen.segment, to);
                    g.write8(screen.segment, to, fill & 0x0F);
                    gc(g, 5, 0x01);
                }
                (None, _) => g.write8(screen.segment, to, pattern),
            }
        }
    }
}

/// The glyph of `code` for graphics, as tall as the mode's characters: on
/// the CGA the first 128 from the ROM and the rest from INT 1Fh's table;
/// on the EGA and VGA all of them from INT 43h's.
fn glyph(g: &mut Guest<'_>, screen: &Screen, code: u8) -> [u8; 32] {
    let height = screen.height.min(32);
    let (segment, base) = if vga::present(g) {
        (
            g.read16(0, 0x43 * 4 + 2),
            g.read16(0, 0x43 * 4).wrapping_add(u16::from(code) * height),
        )
    } else if code < 128 {
        (SEGMENT, table::FONT + u16::from(code) * 8)
    } else {
        (
            g.read16(0, 0x1F * 4 + 2),
            g.read16(0, 0x1F * 4)
                .wrapping_add(u16::from(code - 128) * 8),
        )
    };
    let mut rows = [0u8; 32];
    for (row, slot) in (0..height).zip(rows.iter_mut()) {
        *slot = g.read8(segment, base.wrapping_add(row));
    }
    rows
}

/// Draws `code` in graphics at `row`, `column` in `colour`, which with bit 7
/// set is combined by exclusive or with what is there.
fn draw(g: &mut Guest<'_>, screen: &Screen, row: u16, column: u16, code: u8, colour: u8) {
    let bits = glyph(g, screen, code);
    let xor = colour & 0x80 != 0;
    let start = if screen.layout == Layout::Planar {
        g.bda16(bda::PAGE_START)
    } else {
        0
    };
    for (line, &pattern) in (0..screen.height).zip(&bits) {
        let y = row * screen.height + line;
        match screen.layout {
            Layout::Cga4 => {
                let wide = (0..8).fold(0u16, |wide, bit| {
                    if pattern & (0x80 >> bit) != 0 {
                        wide | (u16::from(colour & 3) << (14 - 2 * bit))
                    } else {
                        wide
                    }
                });
                let [low, high] = wide.to_le_bytes();
                let x = column.wrapping_mul(2);
                for (x, value) in [(x, high), (x.wrapping_add(1), low)] {
                    let at = screen.line(y, x);
                    let value = if xor {
                        g.read8(screen.segment, at) ^ value
                    } else {
                        value
                    };
                    g.write8(screen.segment, at, value);
                }
            }
            Layout::Cga2 => {
                let at = screen.line(y, column);
                let value = if colour & 1 != 0 { pattern } else { 0 };
                let value = if xor {
                    g.read8(screen.segment, at) ^ value
                } else {
                    value
                };
                g.write8(screen.segment, at, value);
            }
            Layout::Planar => {
                let at = start.wrapping_add(screen.line(y, column));
                gc(g, 5, 0x02);
                gc(g, 3, if xor { 0x18 } else { 0x00 });
                if !xor {
                    gc(g, 8, 0xFF);
                    g.read8(screen.segment, at);
                    g.write8(screen.segment, at, 0);
                }
                gc(g, 8, pattern);
                g.read8(screen.segment, at);
                g.write8(screen.segment, at, colour & 0x0F);
                gc_restore(g);
            }
            Layout::Linear => {
                for bit in 0..8u16 {
                    let at = screen.line(y, column.wrapping_mul(8).wrapping_add(bit));
                    let lit = pattern & (0x80 >> bit) != 0;
                    let value = match (lit, xor) {
                        (true, true) => g.read8(screen.segment, at) ^ (colour & 0x7F),
                        (true, false) => colour,
                        (false, true) => continue,
                        (false, false) => 0,
                    };
                    g.write8(screen.segment, at, value);
                }
            }
            Layout::Text => {}
        }
    }
}

/// A scan line of the character cell at `row`, `column` in graphics, as a
/// bit a pixel lit.
fn cell_line(g: &mut Guest<'_>, screen: &Screen, y: u16, column: u16) -> u8 {
    match screen.layout {
        Layout::Cga4 => {
            let x = column.wrapping_mul(2);
            let high = g.read8(screen.segment, screen.line(y, x));
            let low = g.read8(screen.segment, screen.line(y, x.wrapping_add(1)));
            let wide = u16::from_le_bytes([low, high]);
            (0..8).fold(0, |bits, bit| {
                if wide >> (14 - 2 * bit) & 3 != 0 {
                    bits | (0x80 >> bit)
                } else {
                    bits
                }
            })
        }
        Layout::Planar => {
            let at = g
                .bda16(bda::PAGE_START)
                .wrapping_add(screen.line(y, column));
            let lit = (0..4).fold(0, |bits, plane| {
                gc(g, 4, plane);
                bits | g.read8(screen.segment, at)
            });
            gc(g, 4, 0);
            lit
        }
        Layout::Linear => (0..8u16).fold(0, |bits, bit| {
            if g.read8(
                screen.segment,
                screen.line(y, column.wrapping_mul(8).wrapping_add(bit)),
            ) != 0
            {
                bits | (0x80 >> bit)
            } else {
                bits
            }
        }),
        _ => g.read8(screen.segment, screen.line(y, column)),
    }
}

/// The character at `row`, `column` of `page`: read back from graphics by
/// matching its pattern against the font, 0 where nothing matches.
fn read_character(g: &mut Guest<'_>, screen: &Screen, page: u8, row: u16, column: u16) -> (u8, u8) {
    if screen.layout == Layout::Text {
        let [code, attribute] = g
            .read16(screen.segment, screen.cell(page, row, column))
            .to_le_bytes();
        return (code, attribute);
    }
    let mut pattern = [0u8; 32];
    for (line, slot) in (0..screen.height).zip(pattern.iter_mut()) {
        *slot = cell_line(g, screen, row * screen.height + line, column);
    }
    let code = (0..=255u8)
        .find(|&code| glyph(g, screen, code) == pattern)
        .unwrap_or(0);
    (code, 0)
}

/// The character on the active page at `row`, `column`.
pub fn character_at(g: &mut Guest<'_>, row: u8, column: u8) -> u8 {
    let screen = Screen::of(g);
    let page = g.bda8(bda::ACTIVE_PAGE);
    read_character(g, &screen, page, u16::from(row), u16::from(column)).0
}

/// Where pixel `x`, `y` is: its byte, the shift to its bits and their mask.
fn pixel_at(screen: &Screen, x: u16, y: u16) -> (u16, u8, u8) {
    let [quarter, eighth] = [x % 4, x % 8].map(|n| n.to_le_bytes()[0]);
    match screen.layout {
        Layout::Cga4 => (screen.line(y, x / 4), 6 - 2 * quarter, 3),
        Layout::Linear => (screen.line(y, x), 0, 0xFF),
        _ => (screen.line(y, x / 8), 7 - eighth, 1),
    }
}

fn write_pixel(g: &mut Guest<'_>, x: u16, y: u16, colour: u8) {
    let screen = Screen::of(g);
    if screen.layout == Layout::Text || y >= screen.lines().max(200) || x >= screen.width() {
        return;
    }
    let (at, shift, mask) = pixel_at(&screen, x, y);
    if screen.layout == Layout::Planar {
        let at = g.bda16(bda::PAGE_START).wrapping_add(at);
        gc(g, 5, 0x02);
        gc(g, 3, if colour & 0x80 != 0 { 0x18 } else { 0x00 });
        gc(g, 8, 1 << shift);
        g.read8(screen.segment, at);
        g.write8(screen.segment, at, colour & 0x0F);
        gc_restore(g);
        return;
    }
    let old = g.read8(screen.segment, at);
    let value = (colour & mask) << shift;
    let new = if colour & 0x80 != 0 && screen.layout != Layout::Linear {
        old ^ value
    } else {
        (old & !(mask << shift)) | value
    };
    g.write8(screen.segment, at, new);
}

fn read_pixel(g: &mut Guest<'_>, x: u16, y: u16) -> u8 {
    let screen = Screen::of(g);
    if screen.layout == Layout::Text {
        return 0;
    }
    let (at, shift, mask) = pixel_at(&screen, x, y);
    if screen.layout == Layout::Planar {
        let at = g.bda16(bda::PAGE_START).wrapping_add(at);
        let colour = (0..4).fold(0, |colour, plane| {
            gc(g, 4, plane);
            colour | (((g.read8(screen.segment, at) >> shift) & 1) << plane)
        });
        gc(g, 4, 0);
        return colour;
    }
    (g.read8(screen.segment, at) >> shift) & mask
}

/// Writes `code` `count` times from the cursor of `page`, the cursor not
/// moving, with `attribute` where there is one.
fn write_characters(g: &mut Guest<'_>, page: u8, code: u8, attribute: Option<u8>, count: u16) {
    let screen = Screen::of(g);
    let (row, column) = cursor_of(g, page);
    let columns = u32::from(screen.columns);
    let first = u32::from(row) * columns + u32::from(column);
    for at in (first..).take(usize::from(count)) {
        let (Ok(row), Ok(column)) = (u16::try_from(at / columns), u16::try_from(at % columns))
        else {
            break;
        };
        match screen.layout {
            Layout::Text => {
                let at = screen.cell(page, row, column);
                g.write8(screen.segment, at, code);
                if let Some(attribute) = attribute {
                    g.write8(screen.segment, at.wrapping_add(1), attribute);
                }
            }
            _ if row < screen.rows => draw(g, &screen, row, column, code, attribute.unwrap_or(1)),
            _ => {}
        }
    }
}

/// The teletype on `page`: the character at its cursor, which moves on,
/// wrapping and scrolling; with `attribute`, a text cell takes it too. BEL
/// starts the bell and returns true.
fn teletype(g: &mut Guest<'_>, page: u8, code: u8, colour: u8, attribute: bool) -> bool {
    let screen = Screen::of(g);
    let (mut row, mut column) = cursor_of(g, page);
    match code {
        0x07 => {
            let on = g.input(0x61) | port_b::TIMER2_GATE | port_b::SPEAKER_DATA;
            g.output(0x61, on);
            return true;
        }
        0x08 => column = column.saturating_sub(1),
        0x0A => row += 1,
        0x0D => column = 0,
        _ => {
            match screen.layout {
                Layout::Text => {
                    let at = screen.cell(page, row, column);
                    g.write8(screen.segment, at, code);
                    if attribute {
                        g.write8(screen.segment, at.wrapping_add(1), colour);
                    }
                }
                _ => draw(g, &screen, row, column, code, colour),
            }
            column += 1;
            if column >= screen.columns {
                column = 0;
                row += 1;
            }
        }
    }
    if row >= screen.rows {
        let fill = if screen.layout == Layout::Text {
            read_character(g, &screen, page, screen.rows - 1, column).1
        } else {
            0
        };
        scroll(
            g,
            true,
            1,
            fill,
            (0, 0),
            (screen.rows - 1, screen.columns - 1),
        );
        row = screen.rows - 1;
    }
    move_cursor(g, page, row, column);
    false
}

/// Writes a character and its attribute through the teletype on `page`, as
/// AH 13h's strings go.
pub fn put(g: &mut Guest<'_>, page: u8, code: u8, colour: u8) {
    teletype(g, page, code, colour, true);
}

/// Writes `text` through the teletype, as the firmware's own messages do.
pub fn write_text(g: &mut Guest<'_>, text: &[u8]) {
    let page = g.bda8(bda::ACTIVE_PAGE);
    for &code in text {
        teletype(g, page, code, 7, false);
    }
}

/// Writes `n` in decimal through the teletype.
pub fn write_number(g: &mut Guest<'_>, n: u16) {
    let mut digits = [0u8; 5];
    let mut rest = n;
    let mut len = 0;
    loop {
        digits[len] = b'0' + (rest % 10).to_le_bytes()[0];
        len += 1;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    digits[..len].reverse();
    write_text(g, &digits[..len]);
}

/// INT 10h on the MDA and CGA.
pub fn service(g: &mut Guest<'_>) {
    if g.r8(Reg8::Ah) == 0x00 {
        let mode = g.r8(Reg8::Al);
        set_mode(g, mode);
        g.done(true);
        return;
    }
    common(g);
}

/// INT 10h's services every card shares.
pub fn common(g: &mut Guest<'_>) {
    let (ah, al, bh, bl) = (
        g.r8(Reg8::Ah),
        g.r8(Reg8::Al),
        g.r8(Reg8::Bh),
        g.r8(Reg8::Bl),
    );
    let (row, column) = (u16::from(g.r8(Reg8::Dh)), u16::from(g.r8(Reg8::Dl)));
    let mut bell = false;
    match ah {
        0x01 => {
            let screen = Screen::of(g);
            set_shape(g, &screen, g.r16(Reg16::Cx));
        }
        0x02 => move_cursor(g, bh, row, column),
        0x03 => {
            let (r, c) = cursor_of(g, bh);
            let shape = g.bda16(bda::CURSOR_SHAPE);
            g.set_r16(Reg16::Dx, (r << 8) | c);
            g.set_r16(Reg16::Cx, shape);
        }
        // No light pen is attached, so it has never triggered.
        0x04 => g.set_r8(Reg8::Ah, 0),
        0x05 => select_page(g, al),
        0x06 | 0x07 => {
            let corner = (u16::from(g.r8(Reg8::Ch)), u16::from(g.r8(Reg8::Cl)));
            scroll(g, ah == 0x06, al, bh, corner, (row, column));
        }
        0x08 => {
            let screen = Screen::of(g);
            let (r, c) = cursor_of(g, bh);
            let (code, attribute) = read_character(g, &screen, bh, r, c);
            g.set_r16(Reg16::Ax, u16::from_le_bytes([code, attribute]));
        }
        0x09 => write_characters(g, bh, al, Some(bl), g.r16(Reg16::Cx)),
        0x0A => write_characters(g, bh, al, None, g.r16(Reg16::Cx)),
        0x0B => {
            let old = g.bda8(bda::PALETTE);
            let palette = if bh == 0 {
                (old & 0xE0) | (bl & 0x1F)
            } else {
                (old & 0xDF) | ((bl & 1) << 5)
            };
            g.set_bda8(bda::PALETTE, palette);
            if g.bda16(bda::CRTC_PORT) == 0x3D4 && !vga::present(g) {
                g.output(0x3D9, palette);
            }
        }
        0x0C => write_pixel(g, g.r16(Reg16::Cx), g.r16(Reg16::Dx), al),
        0x0D => {
            let colour = read_pixel(g, g.r16(Reg16::Cx), g.r16(Reg16::Dx));
            g.set_r8(Reg8::Al, colour);
        }
        0x0E => {
            let page = g.bda8(bda::ACTIVE_PAGE);
            bell = teletype(g, page, al, bl, false);
        }
        0x0F => {
            let (mode, columns, page) = (
                g.bda8(bda::VIDEO_MODE),
                g.bda16(bda::COLUMNS),
                g.bda8(bda::ACTIVE_PAGE),
            );
            let keep = if vga::present(g) {
                g.bda8(bda::EGA_INFO) & 0x80
            } else {
                0
            };
            g.set_r16(
                Reg16::Ax,
                u16::from_le_bytes([mode | keep, columns.to_le_bytes()[0]]),
            );
            g.set_r8(Reg8::Bh, page);
        }
        _ => {}
    }
    if bell {
        let end = g.bda16(bda::TICKS).wrapping_add(BELL_TICKS);
        let [low, high] = end.to_le_bytes();
        g.set_local(0, low);
        g.set_local(1, high);
    }
    g.done(!bell);
}

/// The bell's tick: once its time is up the tone stops and INT 10h returns.
pub fn wait(g: &mut Guest<'_>) {
    let end = u16::from_le_bytes([g.local(0), g.local(1)]);
    if g.bda16(bda::TICKS).wrapping_sub(end) >= 0x8000 {
        g.done(false);
        return;
    }
    let off = g.input(0x61) & !(port_b::TIMER2_GATE | port_b::SPEAKER_DATA);
    g.output(0x61, off);
    g.done(true);
}
