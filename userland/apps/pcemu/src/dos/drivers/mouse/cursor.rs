//! The cursor on the screen: the position mapped into each mode the BIOS
//! sets, the cursor drawn over what it covers, and what it covered put back.
//! Planar modes are drawn a plane at a time through the graphics
//! controller, whose registers are read back and restored on a card that
//! reads them, as the VGA does, and left as the BIOS sets them on one that
//! does not, as the EGA's are write-only.

use super::{at, state, Mouse, WIDTH};
use crate::bios::bda;
use crate::bios::guest::Guest;
use crate::bios::modes::Layout;
use crate::bios::video::Screen;

/// What the drawn cursor is.
pub mod drawn {
    pub const NONE: u8 = 0;
    /// A text cell changed; its old word and the one shown are kept.
    pub const TEXT: u8 = 1;
    /// The CRTC's own cursor moved.
    pub const HARDWARE: u8 = 2;
    /// A graphics image; the bytes it covered are kept.
    pub const GRAPHICS: u8 = 3;
}

/// The default graphics cursor, an arrow: `#` black, `.` white, a space the
/// screen left as it is.
const SHAPE: [&[u8; 16]; 16] = [
    b"#               ",
    b"##              ",
    b"#.#             ",
    b"#..#            ",
    b"#...#           ",
    b"#....#          ",
    b"#.....#         ",
    b"#......#        ",
    b"#.......#       ",
    b"#........#      ",
    b"#.....#####     ",
    b"#..#..#         ",
    b"#.# #..#        ",
    b"##  #..#        ",
    b"     #..#       ",
    b"     ####       ",
];

/// [`SHAPE`]'s screen and cursor masks, a row at a time.
pub const ARROW: [(u16, u16); 16] = masks();

const fn masks() -> [(u16, u16); 16] {
    let mut out = [(0, 0); 16];
    let mut row = 0;
    while row < 16 {
        let (mut screen, mut cursor) = (0u16, 0u16);
        let mut column = 0;
        while column < 16 {
            let bit = 0x8000 >> column;
            match SHAPE[row][column] {
                b'#' => {}
                b'.' => cursor |= bit,
                _ => screen |= bit,
            }
            column += 1;
        }
        out[row] = (screen, cursor);
        row += 1;
    }
    out
}

/// The XOR colour of mode 13h's cursor: white in the default palette.
const LINEAR_WHITE: u8 = 0x0F;

/// The graphics controller's and sequencer's ports.
const GC: u16 = 0x3CE;
const SEQUENCER: u16 = 0x3C4;
/// The graphics controller registers a planar draw sets: set/reset, its
/// enable, the rotate and function, the read map, the mode, the bit mask.
const GC_USED: [u8; 6] = [0, 1, 3, 4, 5, 8];
/// Their values as the BIOS leaves them in a planar mode.
const GC_DEFAULTS: [u8; 6] = [0, 0, 0, 0, 0, 0xFF];
/// The sequencer's map mask, and its value as the BIOS leaves it.
const MAP_MASK: u8 = 2;
const ALL_PLANES: u8 = 0x0F;

/// Whether the display's registers read back: a graphics controller index
/// written reads as written on a VGA, and as open bus on anything else.
pub fn readable_registers(g: &mut Guest<'_>) -> bool {
    let index = g.input(GC);
    g.output(GC, 0x08);
    let readable = g.input(GC) & 0x0F == 0x08;
    g.output(GC, index);
    readable
}

/// Where the cursor is drawn: the mode, its layout and display page, and
/// the virtual units a column or pixel takes.
pub struct View {
    screen: Screen,
    mode: u8,
    page: u8,
    unit_x: u16,
    /// The virtual screen's height.
    pub height: u16,
}

/// A planar draw's register state, to be put back.
struct Registers {
    gc_index: u8,
    gc: [u8; 6],
    sequencer_index: u8,
    map_mask: u8,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pass {
    Draw,
    Erase,
}

impl Mouse<'_, '_> {
    /// The screen the cursor is on now.
    pub(super) fn view(&mut self) -> View {
        let screen = Screen::of(self.g);
        let mode = self.g.bda8(bda::VIDEO_MODE) & 0x7F;
        let page = u8::try_from(self.state(state::PAGE) & 7).unwrap_or(0);
        let (unit_x, height) = match screen.layout() {
            Layout::Text => (
                WIDTH / screen.text_columns().clamp(1, WIDTH),
                screen.text_rows().saturating_mul(8),
            ),
            _ => (WIDTH / screen.width().clamp(1, WIDTH), screen.lines()),
        };
        View {
            screen,
            mode,
            page,
            unit_x,
            height,
        }
    }

    /// Draws the cursor where it is, if it shows: what was drawn before is
    /// put back first, and a region set by function `10h` hides it.
    pub(super) fn update(&mut self) {
        self.erase();
        if self.excluded() || self.signed(state::VISIBLE) != 0 || self.get8(at::ENABLED) == 0 {
            return;
        }
        let view = self.view();
        self.set8(at::DRAWN_MODE, view.mode);
        match view.screen.layout() {
            Layout::Text if self.state(state::TEXT_KIND) == 1 => self.hardware(&view),
            Layout::Text => self.draw_text(&view),
            _ => self.graphics(&view, Pass::Draw),
        }
    }

    /// Puts back what the drawn cursor covers. A mode set since has
    /// cleared the screen, so nothing is put back over the new mode.
    pub(super) fn erase(&mut self) {
        let kind = self.get8(at::DRAWN);
        if kind == drawn::NONE {
            return;
        }
        self.set8(at::DRAWN, drawn::NONE);
        let view = self.view();
        if self.get8(at::DRAWN_MODE) != view.mode {
            return;
        }
        match kind {
            drawn::TEXT => self.erase_text(&view),
            drawn::HARDWARE => self.bios_cursor(&view),
            _ => self.graphics(&view, Pass::Erase),
        }
    }

    /// Whether the cursor has entered the region function `10h` set, which
    /// hides it as function `02h` does.
    fn excluded(&mut self) -> bool {
        if self.state(state::EXCLUDING) == 0 {
            return false;
        }
        let [left, top, right, bottom] =
            [0u16, 1, 2, 3].map(|n| self.signed(state::EXCLUSION + n * 2));
        let (x, y) = (self.signed(state::X), self.signed(state::Y));
        let inside = (left.min(right)..=left.max(right)).contains(&x)
            && (top.min(bottom)..=top.max(bottom)).contains(&y);
        if inside {
            self.set_state(state::EXCLUDING, 0);
            let visible = self.signed(state::VISIBLE).saturating_sub(1);
            self.set_signed(state::VISIBLE, visible);
        }
        inside
    }

    /// The text cell the cursor is on, and its offset on the page.
    fn cell(&mut self, view: &View) -> (u16, u16, u16) {
        let columns = view.screen.text_columns().max(1);
        let rows = view.screen.text_rows().max(1);
        let x = self.signed(state::X).max(0).unsigned_abs();
        let y = self.signed(state::Y).max(0).unsigned_abs();
        let (row, column) = (
            (y / 8).min(rows - 1),
            (x / view.unit_x.max(1)).min(columns - 1),
        );
        (row, column, view.screen.cell(view.page, row, column))
    }

    fn draw_text(&mut self, view: &View) {
        let (_, _, at) = self.cell(view);
        let segment = view.screen.segment();
        let under = self.g.read16(segment, at);
        let shown = (under & self.state(state::TEXT_FIRST)) ^ self.state(state::TEXT_SECOND);
        self.g.write16(segment, at, shown);
        self.set16(at::BACKGROUND, under);
        self.set16(at::BACKGROUND + 2, shown);
        self.set16(at::DRAWN_X, at);
        self.set8(at::DRAWN, drawn::TEXT);
    }

    /// A cell the program has written since keeps what it wrote.
    fn erase_text(&mut self, view: &View) {
        let at = self.get16(at::DRAWN_X);
        let segment = view.screen.segment();
        if self.g.read16(segment, at) == self.get16(at::BACKGROUND + 2) {
            let under = self.get16(at::BACKGROUND);
            self.g.write16(segment, at, under);
        }
    }

    /// The CRTC's own cursor moved to the cell, its scan lines function
    /// `0Ah`'s; a card that reads its registers back keeps the shape it had.
    fn hardware(&mut self, view: &View) {
        let (_, _, at) = self.cell(view);
        if self.get8(at::READABLE) != 0 {
            let shape = [0x0A, 0x0B].map(|register| crtc_read(self.g, view, register));
            self.g.write_bytes(self.own, at::BACKGROUND, &shape);
        }
        let [first, _] = self.state(state::TEXT_FIRST).to_le_bytes();
        let [second, _] = self.state(state::TEXT_SECOND).to_le_bytes();
        let [low, high] = (at / 2).to_le_bytes();
        for (register, value) in [(0x0A, first), (0x0B, second), (0x0E, high), (0x0F, low)] {
            crtc(self.g, view, register, value);
        }
        self.set8(at::DRAWN, drawn::HARDWARE);
    }

    /// The BIOS's cursor back where the data area has it, in its shape.
    fn bios_cursor(&mut self, view: &View) {
        let [end, start] = if self.get8(at::READABLE) != 0 {
            let [start, end] = [self.get8(at::BACKGROUND), self.get8(at::BACKGROUND + 1)];
            [end, start]
        } else {
            self.g.bda16(bda::CURSOR_SHAPE).to_le_bytes()
        };
        let page = self.g.bda8(bda::ACTIVE_PAGE) & 7;
        let [column, row] = self
            .g
            .bda16(bda::CURSOR + u16::from(page) * 2)
            .to_le_bytes();
        let columns = view.screen.text_columns();
        let place = (self.g.bda16(bda::PAGE_START) / 2)
            .wrapping_add(u16::from(row).wrapping_mul(columns))
            .wrapping_add(u16::from(column));
        let [low, high] = place.to_le_bytes();
        for (register, value) in [(0x0A, start), (0x0B, end), (0x0E, high), (0x0F, low)] {
            crtc(self.g, view, register, value);
        }
    }

    /// Draws the graphics cursor, keeping each byte it covers, or puts the
    /// kept bytes back where it was drawn.
    fn graphics(&mut self, view: &View, pass: Pass) {
        let layout = view.screen.layout();
        let (left, top) = match pass {
            Pass::Draw => {
                let x = i32::from(self.signed(state::X)) / i32::from(view.unit_x.max(1))
                    - i32::from(self.signed(state::HOT_X));
                let y = i32::from(self.signed(state::Y)) - i32::from(self.signed(state::HOT_Y));
                let (x, y) = (clamp16(x), clamp16(y));
                self.set_signed_field(at::DRAWN_X, x);
                self.set_signed_field(at::DRAWN_Y, y);
                let page = if layout == Layout::Planar {
                    u16::from(view.page).wrapping_mul(view.screen.page_size())
                } else {
                    0
                };
                self.set16(at::DRAWN_PAGE, page);
                self.set8(at::DRAWN, drawn::GRAPHICS);
                (i32::from(x), i32::from(y))
            }
            Pass::Erase => (
                i32::from(self.signed_field(at::DRAWN_X)),
                i32::from(self.signed_field(at::DRAWN_Y)),
            ),
        };
        let page = self.get16(at::DRAWN_PAGE);
        let segment = view.screen.segment();
        let registers = (layout == Layout::Planar).then(|| self.planar_begin());
        let lines = i32::from(view.screen.lines());
        let mut kept = at::BACKGROUND;
        for row in 0..16u16 {
            let Ok(y) = u16::try_from(top + i32::from(row)) else {
                continue;
            };
            if i32::from(y) >= lines {
                continue;
            }
            let masks = (
                self.state(state::SCREEN_MASK + row * 2),
                self.state(state::CURSOR_MASK + row * 2),
            );
            if layout == Layout::Linear {
                for (bit, column) in (0..16).zip(left..) {
                    let Some(x) = u16::try_from(column).ok().filter(|&x| x < 320) else {
                        continue;
                    };
                    let and = if masks.0 & (0x8000 >> bit) != 0 {
                        0xFF
                    } else {
                        0x00
                    };
                    let xor = if masks.1 & (0x8000 >> bit) != 0 {
                        LINEAR_WHITE
                    } else {
                        0x00
                    };
                    kept = self.cover(
                        pass,
                        kept,
                        (segment, view.screen.line(y, x)),
                        None,
                        (and, xor),
                    );
                }
                continue;
            }
            let (first, shift) = match layout {
                Layout::Cga4 => (left.div_euclid(4), left.rem_euclid(4) * 2),
                _ => (left.div_euclid(8), left.rem_euclid(8)),
            };
            let shift = u32::try_from(shift).unwrap_or(0);
            let and = (!((u32::from(!masks.0) << 8) >> shift)).to_be_bytes();
            let xor = ((u32::from(masks.1) << 8) >> shift).to_be_bytes();
            let per_line =
                i32::from(view.screen.width() / if layout == Layout::Cga4 { 4 } else { 8 });
            for (i, byte) in (first..first + 3).enumerate() {
                let Some(column) = u16::try_from(byte).ok().filter(|_| byte < per_line) else {
                    continue;
                };
                let address = page.wrapping_add(view.screen.line(y, column));
                let planes = (layout == Layout::Planar).then_some(4);
                kept = self.cover(
                    pass,
                    kept,
                    (segment, address),
                    planes,
                    (and[1 + i], xor[1 + i]),
                );
            }
        }
        if let Some(registers) = registers {
            self.planar_end(&registers);
        }
    }

    /// Draws over the byte at `segment:address` — each plane of it when
    /// `planes` names them — keeping what it held, or puts back what was
    /// kept; the next place to keep a byte.
    fn cover(
        &mut self,
        pass: Pass,
        kept: u16,
        (segment, address): (u16, u16),
        planes: Option<u8>,
        (and, xor): (u8, u8),
    ) -> u16 {
        let mut kept = kept;
        for plane in 0..planes.unwrap_or(1) {
            if kept >= at::BACKGROUND + at::BACKGROUND_LEN {
                return kept;
            }
            match pass {
                Pass::Draw => {
                    let under = if planes.is_some() {
                        self.plane_read(plane, address)
                    } else {
                        self.g.read8(segment, address)
                    };
                    self.set8(kept, under);
                    let shown = (under & and) ^ xor;
                    if planes.is_some() {
                        self.plane_write(plane, address, shown);
                    } else {
                        self.g.write8(segment, address, shown);
                    }
                }
                Pass::Erase => {
                    let under = self.get8(kept);
                    if planes.is_some() {
                        self.plane_write(plane, address, under);
                    } else {
                        self.g.write8(segment, address, under);
                    }
                }
            }
            kept += 1;
        }
        kept
    }

    fn plane_read(&mut self, plane: u8, address: u16) -> u8 {
        self.g.output(GC, 4);
        self.g.output(GC + 1, plane);
        self.g.read8(0xA000, address)
    }

    fn plane_write(&mut self, plane: u8, address: u16, value: u8) {
        self.g.output(SEQUENCER, MAP_MASK);
        self.g.output(SEQUENCER + 1, 1 << plane);
        self.g.write8(0xA000, address, value);
    }

    /// Sets the graphics controller to plain writes of whole bytes, keeping
    /// what it was set to where the card reads it back.
    fn planar_begin(&mut self) -> Registers {
        let registers = if self.get8(at::READABLE) != 0 {
            let gc_index = self.g.input(GC);
            let gc = GC_USED.map(|register| {
                self.g.output(GC, register);
                self.g.input(GC + 1)
            });
            let sequencer_index = self.g.input(SEQUENCER);
            self.g.output(SEQUENCER, MAP_MASK);
            let map_mask = self.g.input(SEQUENCER + 1);
            Registers {
                gc_index,
                gc,
                sequencer_index,
                map_mask,
            }
        } else {
            Registers {
                gc_index: 0,
                gc: GC_DEFAULTS,
                sequencer_index: 0,
                map_mask: ALL_PLANES,
            }
        };
        for (register, value) in GC_USED.into_iter().zip(GC_DEFAULTS) {
            self.g.output(GC, register);
            self.g.output(GC + 1, value);
        }
        registers
    }

    fn planar_end(&mut self, registers: &Registers) {
        for (register, value) in GC_USED.into_iter().zip(registers.gc) {
            self.g.output(GC, register);
            self.g.output(GC + 1, value);
        }
        self.g.output(GC, registers.gc_index);
        self.g.output(SEQUENCER, MAP_MASK);
        self.g.output(SEQUENCER + 1, registers.map_mask);
        self.g.output(SEQUENCER, registers.sequencer_index);
    }

    fn signed_field(&mut self, field: u16) -> i16 {
        i16::from_le_bytes(self.get16(field).to_le_bytes())
    }

    fn set_signed_field(&mut self, field: u16, value: i16) {
        self.set16(field, u16::from_le_bytes(value.to_le_bytes()));
    }
}

fn clamp16(value: i32) -> i16 {
    i16::try_from(value.clamp(i32::from(i16::MIN), i32::from(i16::MAX))).unwrap_or(0)
}

fn crtc(g: &mut Guest<'_>, view: &View, register: u8, value: u8) {
    let port = view.screen.crtc_port();
    g.output(port, register);
    g.output(port.wrapping_add(1), value);
}

fn crtc_read(g: &mut Guest<'_>, view: &View, register: u8) -> u8 {
    let port = view.screen.crtc_port();
    g.output(port, register);
    g.input(port.wrapping_add(1))
}
