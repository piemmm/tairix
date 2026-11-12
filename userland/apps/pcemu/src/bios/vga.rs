//! The EGA's or VGA's ROM at `C0000h`: INT 10h for every mode the card
//! has, set from IBM's parameter table through the save pointers at
//! `40:A8h`; the palette, the DAC, the character generator and its fonts,
//! cursor emulation, the alternate-select, write-string, display-combination,
//! state and save-restore services, and the print screen of every row. The
//! system BIOS's INT 10h moves to INT 42h.

use super::guest::Guest;
use super::layout::Image;
use super::modes::{self, Layout, Lines, Mode};
use super::{bda, call, system, video, BuildError};
use crate::asm::{Asm, Reg16, Reg8};
use crate::cpu::Seg;
use crate::video::font;
use crate::video::monitor::Monitor;
use crate::video::Adapter;
use alloc::vec::Vec;

/// Where the ROM sits.
pub const SEGMENT: u16 = super::layout::VIDEO_ROM;
pub const BASE: u32 = (SEGMENT as u32) << 4;
const INIT: u16 = 0x0003;
/// The card the ROM is for: VGA or EGA, and a monochrome monitor or not.
const CARD: u16 = 0x0010;
const SERVICE: u16 = 0x0100;
const PRINT_SCREEN: u16 = 0x0180;
/// What AH 1Bh reports the VGA can do.
const FUNCTIONALITY: u16 = 0x0200;
const SAVE_POINTERS: u16 = 0x0210;
const SECONDARY_POINTERS: u16 = 0x0230;
const DCC_TABLE: u16 = 0x0250;
const PARAMETERS: u16 = 0x0300;
const FONT_8: u16 = 0x0C00;
const FONT_14: u16 = 0x1400;
const FONT_16: u16 = 0x2200;

/// The card's memory in 64 KiB, less one, as the BIOS reports it: both
/// cards have 256 KiB.
const MEMORY_CODE: u8 = 3;
/// The EGA's switches with an Enhanced Color Display or a monochrome one;
/// the VGA reports the first, the EGA it emulates at 350 lines.
const SWITCHES_COLOUR: u8 = 0x09;
const SWITCHES_MONO: u8 = 0x0B;
/// The switches that make the VGA's next text mode the CGA's 200 lines.
const SWITCHES_200: u8 = 0x08;
/// The shape a text mode starts with, in the CGA's eight lines.
const DEFAULT_CURSOR: u16 = 0x0607;

/// Display combination codes.
mod dcc {
    pub const NONE: u8 = 0x00;
    pub const MDA: u8 = 0x01;
    pub const CGA: u8 = 0x02;
    pub const EGA: u8 = 0x04;
    pub const EGA_MONO: u8 = 0x05;
    pub const VGA_MONO: u8 = 0x07;
    pub const VGA: u8 = 0x08;
}

/// The display combination table: the active display, then the other.
const COMBINATIONS: [[u8; 2]; 12] = [
    [dcc::NONE, dcc::NONE],
    [dcc::MDA, dcc::NONE],
    [dcc::CGA, dcc::NONE],
    [dcc::CGA, dcc::MDA],
    [dcc::EGA, dcc::NONE],
    [dcc::EGA, dcc::MDA],
    [dcc::EGA_MONO, dcc::NONE],
    [dcc::EGA_MONO, dcc::CGA],
    [dcc::VGA_MONO, dcc::NONE],
    [dcc::VGA_MONO, dcc::CGA],
    [dcc::VGA, dcc::NONE],
    [dcc::VGA, dcc::MDA],
];

/// AH 1Bh's static table: modes 0–7, 0Dh–0Fh and 10h–13h; 200, 350 and
/// 400 lines; eight font blocks, two at once; every palette, font and cursor
/// function; save-restore, blink and the display combination, but no light
/// pen; every save pointer.
const STATIC_FUNCTIONALITY: [u8; 16] = [
    0xFF, 0xE0, 0x0F, 0, 0, 0, 0, 0x07, 8, 2, 0xFF, 0x0E, 0, 0, 0x3F, 0,
];

/// The save pointer table's fields, and the VGA's secondary table's.
mod pointer {
    pub const PARAMETERS: u16 = 0x00;
    pub const DYNAMIC_SAVE: u16 = 0x04;
    pub const ALPHA_FONT: u16 = 0x08;
    pub const GRAPHICS_FONT: u16 = 0x0C;
    pub const SECONDARY: u16 = 0x10;
    pub const BYTES: usize = 0x1C;
    pub const SECONDARY_BYTES: u16 = 0x1A;
    pub const DCC: u16 = 0x02;
    pub const SECOND_ALPHA_FONT: u16 = 0x06;
    pub const PALETTE_PROFILE: u16 = 0x0A;
}

/// `offset` in the ROM as a far pointer's bytes.
fn far(offset: u16) -> [u8; 4] {
    let [o0, o1] = offset.to_le_bytes();
    let [s0, s1] = SEGMENT.to_le_bytes();
    [o0, o1, s0, s1]
}

/// The ROM for `adapter`, if it is an EGA or VGA.
pub fn rom(adapter: Adapter) -> Result<Option<Vec<u8>>, BuildError> {
    let (vga, monitor) = match adapter {
        Adapter::Ega(monitor) => (false, monitor),
        Adapter::Vga(monitor) => (true, monitor),
        _ => return Ok(None),
    };
    let blocks: u8 = if vga { 64 } else { 32 };
    let mut rom = Image::new(usize::from(blocks) * 512);
    rom.place(0, &[0x55, 0xAA, blocks])?;
    rom.code(INIT, |asm| {
        asm.host_call(call::VGA_INIT);
        asm.retf();
    })?;
    let mono = matches!(monitor, Monitor::Mono(_));
    rom.place(CARD, &[u8::from(vga), u8::from(mono)])?;
    rom.code(SERVICE, |asm: &mut Asm| {
        super::layout::waiting(asm, call::VGA_VIDEO, call::VIDEO_WAIT, video::STATE);
    })?;
    rom.code(PRINT_SCREEN, |asm| {
        super::layout::print_screen(asm, call::VGA_PRINT_SCREEN);
    })?;
    let mut pointers = [0u8; pointer::BYTES];
    pointers[..4].copy_from_slice(&far(PARAMETERS));
    if vga {
        pointers[0x10..0x14].copy_from_slice(&far(SECONDARY_POINTERS));
        let mut secondary = [0u8; pointer::SECONDARY_BYTES as usize];
        secondary[..2].copy_from_slice(&pointer::SECONDARY_BYTES.to_le_bytes());
        secondary[2..6].copy_from_slice(&far(DCC_TABLE));
        rom.place(SECONDARY_POINTERS, &secondary)?;
        let count = u8::try_from(COMBINATIONS.len()).unwrap_or(u8::MAX);
        let mut table = Vec::from([count, 1, dcc::VGA, 0]);
        table.extend(COMBINATIONS.iter().flatten());
        rom.place(DCC_TABLE, &table)?;
        rom.place(FUNCTIONALITY, &STATIC_FUNCTIONALITY)?;
    }
    rom.place(SAVE_POINTERS, &pointers)?;
    rom.place(PARAMETERS, &modes::table(vga))?;
    let glyphs = |set: &dyn Fn(u8) -> Vec<u8>| (0..=255u8).flat_map(set).collect::<Vec<u8>>();
    rom.place(FONT_8, &glyphs(&|code| font::CGA.glyph(code).to_vec()))?;
    rom.place(FONT_14, &glyphs(&|code| font::EGA.glyph(code).to_vec()))?;
    if vga {
        rom.place(FONT_16, &glyphs(&|code| font::VGA.glyph(code).to_vec()))?;
    }
    Ok(Some(rom.finish()))
}

/// The card, as its ROM records it.
#[derive(Clone, Copy)]
struct Card {
    vga: bool,
    mono: bool,
}

fn card(g: &mut Guest<'_>) -> Card {
    Card {
        vga: g.read8(SEGMENT, CARD) != 0,
        mono: g.read8(SEGMENT, CARD + 1) != 0,
    }
}

/// Whether an EGA's or VGA's ROM is installed: whether INT 10h is this
/// module's.
pub fn present(g: &mut Guest<'_>) -> bool {
    g.read16(SEGMENT, 0) == super::layout::OPTION_ROM_SIGNATURE
}

/// How mode `mode` lays out its memory on the installed card.
pub fn layout(g: &mut Guest<'_>, mode: u8) -> Option<Layout> {
    let vga = card(g).vga;
    modes::layout(mode, vga)
}

fn far_pointer(g: &mut Guest<'_>, segment: u16, offset: u16) -> (u16, u16) {
    let low = g.read16(segment, offset);
    (g.read16(segment, offset.wrapping_add(2)), low)
}

/// A field of the save pointer table `40:A8h` names.
fn save_pointer(g: &mut Guest<'_>, field: u16) -> (u16, u16) {
    let (segment, offset) = far_pointer(g, bda::SEGMENT, bda::SAVE_POINTERS);
    far_pointer(g, segment, offset.wrapping_add(field))
}

/// A field of the VGA's secondary save pointer table, if there is one.
fn secondary_pointer(g: &mut Guest<'_>, field: u16) -> (u16, u16) {
    match save_pointer(g, pointer::SECONDARY) {
        (0, 0) => (0, 0),
        (segment, offset) => far_pointer(g, segment, offset.wrapping_add(field)),
    }
}

/// Whether the mode list at `segment:offset`, ended by `FFh`, names `mode`.
/// The list is the guest's and need not end, so it is read no further than
/// one entry a mode number.
fn lists(g: &mut Guest<'_>, (segment, offset): (u16, u16), mode: u8) -> bool {
    (0..=0xFFu16)
        .map(|i| g.read8(segment, offset.wrapping_add(i)))
        .take_while(|&listed| listed != 0xFF)
        .any(|listed| listed == mode)
}

/// The parameter table entry `index`, from the table the save pointers
/// name.
fn read_entry(g: &mut Guest<'_>, index: usize) -> Mode {
    let (segment, offset) = save_pointer(g, pointer::PARAMETERS);
    let start = offset.wrapping_add(u16::try_from(index * Mode::BYTES).unwrap_or(0));
    let mut bytes = [0u8; Mode::BYTES];
    for (i, byte) in (0u16..).zip(&mut bytes) {
        *byte = g.read8(segment, start.wrapping_add(i));
    }
    Mode::parse(&bytes)
}

/// The ROM's initialisation, called from the POST: its vectors, the data
/// area's description of the card, and the first mode.
pub fn init(g: &mut Guest<'_>) {
    let (segment, offset) = far_pointer(g, 0, 0x10 * 4);
    g.write16(0, 0x42 * 4, offset);
    g.write16(0, 0x42 * 4 + 2, segment);
    for (n, at) in [(0x10, SERVICE), (0x1F, FONT_8 + 1024), (0x43, FONT_8)] {
        g.write16(0, n * 4, at);
        g.write16(0, n * 4 + 2, SEGMENT);
    }
    g.set_bda16(bda::SAVE_POINTERS, SAVE_POINTERS);
    g.set_bda16(bda::SAVE_POINTERS + 2, SEGMENT);
    let card = card(g);
    g.set_bda8(
        bda::EGA_INFO,
        (MEMORY_CODE << 5) | (u8::from(card.mono) << 1),
    );
    g.set_bda8(bda::EGA_SWITCHES, own_switches(card));
    if card.vga {
        // Active, 400 lines; a monochrome display sums colours to grey.
        g.set_bda8(bda::VGA_FLAGS, 0x11 | (u8::from(card.mono) * 0x06));
        let active = if card.mono { dcc::VGA_MONO } else { dcc::VGA };
        let index = COMBINATIONS.iter().position(|&c| c == [active, dcc::NONE]);
        let index = index.and_then(|i| u8::try_from(i).ok()).unwrap_or(0xFF);
        g.set_bda8(bda::DCC_INDEX, index);
    }
    set_mode(g, if card.mono { 7 } else { 3 });
}

const fn own_switches(card: Card) -> u8 {
    if card.mono {
        SWITCHES_MONO
    } else {
        SWITCHES_COLOUR
    }
}

/// Writes `value` to register `index` behind the index port `port`.
fn indexed(g: &mut Guest<'_>, port: u16, index: u8, value: u8) {
    g.output(port, index);
    g.output(port.wrapping_add(1), value);
}

fn read_indexed(g: &mut Guest<'_>, port: u16, index: u8) -> u8 {
    g.output(port, index);
    g.input(port.wrapping_add(1))
}

/// Resets the attribute controller's flip-flop to its index.
fn attribute_index(g: &mut Guest<'_>) {
    let status = g.bda16(bda::CRTC_PORT).wrapping_add(6);
    g.input(status);
}

fn attribute(g: &mut Guest<'_>, index: u8, value: u8) {
    attribute_index(g);
    g.output(0x3C0, index);
    g.output(0x3C0, value);
    attribute_index(g);
}

fn read_attribute(g: &mut Guest<'_>, index: u8) -> u8 {
    attribute_index(g);
    g.output(0x3C0, index | 0x20);
    let value = g.input(0x3C1);
    attribute_index(g);
    value
}

/// Sets palette register `index` (0–0Fh, or the border at 11h), keeping it
/// in the dynamic save area too if the save pointers name one: the EGA's
/// registers cannot be read back.
fn palette_register(g: &mut Guest<'_>, index: u8, value: u8) {
    attribute(g, index, value);
    let slot = match index {
        0x00..=0x0F => index,
        0x11 => 16,
        _ => return,
    };
    let (segment, offset) = save_pointer(g, pointer::DYNAMIC_SAVE);
    if (segment, offset) != (0, 0) {
        g.write8(segment, offset.wrapping_add(u16::from(slot)), value);
    }
}

/// A colour as a grey of the same brightness, as the VGA sums them.
fn grey([red, green, blue]: [u8; 3]) -> u8 {
    let sum = 30 * u16::from(red) + 59 * u16::from(green) + 11 * u16::from(blue);
    u8::try_from((sum + 50) / 100).unwrap_or(0x3F)
}

fn summing(g: &mut Guest<'_>) -> bool {
    card(g).vga && g.bda8(bda::VGA_FLAGS) & 0x02 != 0
}

/// Writes `colour` to the DAC entry its index port points at, as a grey if
/// `summed`.
fn dac_colour(g: &mut Guest<'_>, colour: [u8; 3], summed: bool) {
    let colour = if summed { [grey(colour); 3] } else { colour };
    for component in colour {
        g.output(0x3C9, component);
    }
}

/// The sixteen EGA colours mode 13h starts with, as indexes into the 64.
const EGA16: [usize; 16] = [0, 1, 2, 3, 4, 5, 20, 7, 56, 57, 58, 59, 60, 61, 62, 63];
/// Mode 13h's grey ramp.
const GREYS: [u8; 16] = [
    0x00, 0x05, 0x08, 0x0B, 0x0E, 0x11, 0x14, 0x18, 0x1C, 0x20, 0x24, 0x28, 0x2D, 0x32, 0x38, 0x3F,
];
/// The levels mode 13h's hues are made of: in each of three intensities,
/// three saturations of five steps each.
const LEVELS: [[[u8; 5]; 3]; 3] = [
    [
        [0x00, 0x10, 0x1F, 0x2F, 0x3F],
        [0x1F, 0x27, 0x2F, 0x37, 0x3F],
        [0x2D, 0x31, 0x36, 0x3A, 0x3F],
    ],
    [
        [0x00, 0x07, 0x0E, 0x15, 0x1C],
        [0x0E, 0x11, 0x15, 0x18, 0x1C],
        [0x14, 0x16, 0x18, 0x1A, 0x1C],
    ],
    [
        [0x00, 0x04, 0x08, 0x0C, 0x10],
        [0x08, 0x0A, 0x0C, 0x0E, 0x10],
        [0x0B, 0x0C, 0x0D, 0x0F, 0x10],
    ],
];
/// The wheel of hues, as steps of red, green and blue: blue, toward
/// magenta, red, yellow, green, cyan, and back.
const HUES: [[usize; 3]; 24] = [
    [0, 0, 4],
    [1, 0, 4],
    [2, 0, 4],
    [3, 0, 4],
    [4, 0, 4],
    [4, 0, 3],
    [4, 0, 2],
    [4, 0, 1],
    [4, 0, 0],
    [4, 1, 0],
    [4, 2, 0],
    [4, 3, 0],
    [4, 4, 0],
    [3, 4, 0],
    [2, 4, 0],
    [1, 4, 0],
    [0, 4, 0],
    [0, 4, 1],
    [0, 4, 2],
    [0, 4, 3],
    [0, 4, 4],
    [0, 3, 4],
    [0, 2, 4],
    [0, 1, 4],
];

/// The DAC's default colours: the monochrome modes' greys of their
/// intensity bits, the other modes' EGA 64 as the attribute controller
/// names them, and for 13h the VGA's 256: the sixteen EGA colours, a grey
/// ramp, and the wheel of hues in three intensities and three saturations.
fn default_dac(linear: bool, mono: bool) -> [[u8; 3]; 256] {
    let mut dac = [[0u8; 3]; 256];
    let ega = |i: usize| {
        let level = |primary: usize, secondary: usize| {
            u8::from(i & primary != 0) * 0x2A + u8::from(i & secondary != 0) * 0x15
        };
        [level(4, 32), level(2, 16), level(1, 8)]
    };
    if !linear {
        for (i, entry) in dac.iter_mut().enumerate().take(64) {
            *entry = if mono {
                [u8::from(i & 0x08 != 0) * 0x2A + u8::from(i & 0x10 != 0) * 0x15; 3]
            } else {
                ega(i)
            };
        }
        return dac;
    }
    for (entry, &i) in dac.iter_mut().zip(&EGA16) {
        *entry = ega(i);
    }
    for (entry, &level) in dac[16..32].iter_mut().zip(&GREYS) {
        *entry = [level; 3];
    }
    let wheel = LEVELS
        .iter()
        .flatten()
        .flat_map(|levels| HUES.iter().map(move |hue| hue.map(|i| levels[i])));
    for (entry, colour) in dac[32..248].iter_mut().zip(wheel) {
        *entry = colour;
    }
    dac
}

/// The offset in plane 2 of font block `block`: the EGA's four 16 KiB
/// blocks, or the VGA's eight interleaved by 8 KiB.
fn block_base(block: u8, vga: bool) -> u16 {
    let base = u16::from(block & 3) * 0x4000;
    if vga {
        base + u16::from((block >> 2) & 1) * 0x2000
    } else {
        base
    }
}

/// Loads `count` glyphs from `first`, `height` lines each, from the font at
/// `segment:offset` into block `block` of plane 2, as a text mode reads
/// them.
fn load_glyphs(
    g: &mut Guest<'_>,
    (segment, offset): (u16, u16),
    height: u8,
    block: u8,
    (first, count): (u16, u16),
) {
    let vga = card(g).vga;
    font_access(g, true);
    let base = block_base(block, vga);
    for code in first..first.saturating_add(count).min(256) {
        for row in 0..32u16 {
            let byte = if row < u16::from(height) {
                let at = (code - first) * u16::from(height) + row;
                g.read8(segment, offset.wrapping_add(at))
            } else {
                0
            };
            g.write8(0xA000, base + code * 32 + row, byte);
        }
    }
    font_access(g, false);
}

/// Opens plane 2 to the CPU at `A0000h`, as fonts are written, or restores
/// a text mode's interleaved access.
fn font_access(g: &mut Guest<'_>, open: bool) {
    let mono = g.bda16(bda::CRTC_PORT) == 0x3B4;
    let (map_mask, memory_mode, read_map, gc_mode, gc_misc) = if open {
        (0x04, 0x07, 0x02, 0x00, 0x04)
    } else {
        (0x03, 0x02, 0x00, 0x10, if mono { 0x0A } else { 0x0E })
    };
    indexed(g, 0x3C4, 2, map_mask);
    indexed(g, 0x3C4, 4, memory_mode);
    indexed(g, 0x3CE, 4, read_map);
    indexed(g, 0x3CE, 5, gc_mode);
    indexed(g, 0x3CE, 6, gc_misc);
}

/// The ROM font of `height` lines.
fn rom_font(height: u8) -> u16 {
    match height {
        14 => FONT_14,
        16 => FONT_16,
        _ => FONT_8,
    }
}

/// The lines the VGA's next text mode is to have, as AH 12h BL 30h asked.
fn next_lines(g: &mut Guest<'_>) -> Lines {
    let flags = g.bda8(bda::VGA_FLAGS);
    match (flags & 0x80 != 0, flags & 0x10 != 0) {
        (_, true) => Lines::L400,
        (true, false) => Lines::L200,
        (false, false) => Lines::L350,
    }
}

/// Sets mode `requested`; bit 7 keeps the memory as it is. A mode the
/// card or its monitor cannot show leaves the screen as it was.
pub fn set_mode(g: &mut Guest<'_>, requested: u8) {
    let number = requested & 0x7F;
    let card = card(g);
    let Some(layout) = modes::layout(number, card.vga) else {
        return;
    };
    let lines = if card.vga { next_lines(g) } else { Lines::L350 };
    let Some(index) = modes::index(number, lines, card.vga) else {
        return;
    };
    let mode = read_entry(g, index);
    if !card.vga && mode.mono() != card.mono {
        return;
    }
    program(g, &mode, card.vga);
    g.set_bda8(bda::VIDEO_MODE, number);
    g.set_bda16(bda::COLUMNS, u16::from(mode.columns));
    g.set_bda16(bda::PAGE_SIZE, mode.page_size);
    g.set_bda16(bda::PAGE_START, 0);
    for page in 0..8 {
        g.set_bda16(bda::CURSOR + page * 2, 0);
    }
    g.set_bda8(bda::ACTIVE_PAGE, 0);
    g.set_bda8(bda::ROWS, mode.rows.wrapping_sub(1));
    g.set_bda16(bda::CHAR_HEIGHT, u16::from(mode.char_height));
    let info = g.bda8(bda::EGA_INFO);
    g.set_bda8(bda::EGA_INFO, (info & 0x7F) | (requested & 0x80));
    if g.bda8(bda::VGA_FLAGS) & 0x08 == 0 {
        load_palette(g, &mode, layout == Layout::Linear, card.vga);
    }
    if layout == Layout::Text {
        let height = mode.char_height;
        load_glyphs(g, (SEGMENT, rom_font(height)), height, 0, (0, 256));
        alpha_overrides(g, number);
    } else {
        g.write16(0, 0x43 * 4, rom_font(mode.char_height));
        g.write16(0, 0x43 * 4 + 2, SEGMENT);
        graphics_override(g, number);
    }
    if card.vga {
        palette_profile(g, number, layout);
    }
    if requested & 0x80 == 0 {
        clear(g, layout, mode.mono());
    }
    let screen = video::Screen::of(g);
    if layout == Layout::Text {
        default_cursor(g);
    } else {
        video::set_shape(g, &screen, 0x2000);
    }
    video::place_cursor(g, &screen, 0);
    if card.vga {
        let flags = g.bda8(bda::VGA_FLAGS);
        g.set_bda8(bda::VGA_FLAGS, (flags & !0x80) | 0x10);
        let switches = g.bda8(bda::EGA_SWITCHES);
        g.set_bda8(bda::EGA_SWITCHES, (switches & 0xF0) | own_switches(card));
    }
    attribute_index(g);
    g.output(0x3C0, 0x20);
}

/// Programs the card's timing and data path from `mode`, the screen off
/// while the timing changes. The palette registers are the palette load's.
fn program(g: &mut Guest<'_>, mode: &Mode, vga: bool) {
    let crtc = if mode.mono() { 0x3B4 } else { 0x3D4 };
    g.set_bda16(bda::CRTC_PORT, crtc);
    attribute_index(g);
    g.output(0x3C0, 0x00);
    indexed(g, 0x3C4, 0, 0x01);
    g.output(0x3C2, mode.misc);
    for (i, &value) in (1u8..).zip(&mode.seq) {
        indexed(g, 0x3C4, i, value);
    }
    indexed(g, 0x3C4, 0, 0x03);
    // Register 11h's bit 7 locks registers 0–7 on the VGA.
    indexed(g, crtc, 0x11, mode.crtc[0x11] & 0x7F);
    for (i, &value) in (0u8..).zip(&mode.crtc) {
        indexed(g, crtc, i, value);
    }
    for (i, &value) in (0u8..).zip(&mode.gc) {
        indexed(g, 0x3CE, i, value);
    }
    attribute_index(g);
    for i in [0x10u8, 0x12, 0x13] {
        g.output(0x3C0, i);
        g.output(0x3C0, mode.ac[usize::from(i)]);
    }
    if vga {
        g.output(0x3C0, 0x14);
        g.output(0x3C0, 0x00);
    }
}

/// The mode's palette registers and border, and the VGA's DAC: greys for
/// a monochrome mode, summed if summing is on.
fn load_palette(g: &mut Guest<'_>, mode: &Mode, linear: bool, vga: bool) {
    for (i, &value) in (0u8..).zip(&mode.ac[..0x10]) {
        palette_register(g, i, value);
    }
    palette_register(g, 0x11, mode.ac[0x11]);
    if vga {
        let summed = summing(g);
        g.output(0x3C6, 0xFF);
        g.output(0x3C8, 0);
        for colour in default_dac(linear, mode.mono()) {
            dac_colour(g, colour, summed);
        }
    }
}

/// The alpha font overrides the save pointers name for `mode`: a font the
/// program supplies, with the rows it is to give the screen, and the VGA's
/// second set.
fn alpha_overrides(g: &mut Guest<'_>, mode: u8) {
    let (segment, offset) = save_pointer(g, pointer::ALPHA_FONT);
    if (segment, offset) != (0, 0) && lists(g, (segment, offset.wrapping_add(0x0B)), mode) {
        let at = |i: u16| offset.wrapping_add(i);
        let (height, block) = (g.read8(segment, offset), g.read8(segment, at(1)));
        let range = (g.read16(segment, at(4)), g.read16(segment, at(2)));
        let font = far_pointer(g, segment, at(6));
        let rows = g.read8(segment, at(0x0A));
        load_glyphs(g, font, height, block, range);
        if height != 0 && u16::from(height) != g.bda16(bda::CHAR_HEIGHT) {
            set_text_height(g, height);
        }
        if rows != 0xFF && rows != 0 {
            g.set_bda8(bda::ROWS, rows - 1);
        }
    }
    let (segment, offset) = secondary_pointer(g, pointer::SECOND_ALPHA_FONT);
    if (segment, offset) != (0, 0) && lists(g, (segment, offset.wrapping_add(7)), mode) {
        let height = g.read8(segment, offset);
        let block = g.read8(segment, offset.wrapping_add(1));
        let font = far_pointer(g, segment, offset.wrapping_add(3));
        load_glyphs(g, font, height, block, (0, 256));
    }
}

/// The graphics font override the save pointers name for `mode`.
fn graphics_override(g: &mut Guest<'_>, mode: u8) {
    let (segment, offset) = save_pointer(g, pointer::GRAPHICS_FONT);
    if (segment, offset) == (0, 0) || !lists(g, (segment, offset.wrapping_add(7)), mode) {
        return;
    }
    let rows = g.read8(segment, offset);
    let height = g.read16(segment, offset.wrapping_add(1));
    let (font_segment, font_offset) = far_pointer(g, segment, offset.wrapping_add(3));
    g.write16(0, 0x43 * 4, font_offset);
    g.write16(0, 0x43 * 4 + 2, font_segment);
    g.set_bda16(bda::CHAR_HEIGHT, height);
    g.set_bda8(bda::ROWS, rows.wrapping_sub(1));
}

/// The VGA's user palette profile, if it names `mode`: its underlining,
/// attribute registers and DAC entries.
fn palette_profile(g: &mut Guest<'_>, mode: u8, layout: Layout) {
    let (segment, offset) = secondary_pointer(g, pointer::PALETTE_PROFILE);
    if (segment, offset) == (0, 0) || !lists(g, (segment, offset.wrapping_add(0x14)), mode) {
        return;
    }
    let at = |i: u16| offset.wrapping_add(i);
    let underline = g.read8(segment, offset);
    if layout == Layout::Text && matches!(underline, 0x01 | 0xFF) {
        let height = g.bda16(bda::CHAR_HEIGHT).clamp(1, 32);
        let line = if underline == 0x01 {
            u8::try_from(height - 1).unwrap_or(0x1F)
        } else {
            0x1F
        };
        let crtc = g.bda16(bda::CRTC_PORT);
        indexed(g, crtc, 0x14, line);
    }
    let (count, first) = (g.read16(segment, at(4)), g.read16(segment, at(6)));
    let (table_segment, table_offset) = far_pointer(g, segment, at(8));
    for (i, register) in (0u16..).zip(first..first.saturating_add(count).min(0x14)) {
        let value = g.read8(table_segment, table_offset.wrapping_add(i));
        let register = u8::try_from(register).unwrap_or(0);
        palette_register(g, register, value);
    }
    let (count, first) = (g.read16(segment, at(0x0C)), g.read16(segment, at(0x0E)));
    let (table_segment, table_offset) = far_pointer(g, segment, at(0x10));
    let summed = summing(g);
    for (i, entry) in (0u16..).zip(first..first.saturating_add(count).min(256)) {
        let base = table_offset.wrapping_add(i.wrapping_mul(3));
        let colour = [0, 1, 2].map(|c| g.read8(table_segment, base.wrapping_add(c)));
        g.output(0x3C8, u8::try_from(entry).unwrap_or(0));
        dac_colour(g, colour, summed);
    }
}

/// Clears the mode's memory: blanks in text, black in graphics.
fn clear(g: &mut Guest<'_>, layout: Layout, mono: bool) {
    match layout {
        Layout::Text => {
            let segment = if mono { 0xB000 } else { 0xB800 };
            for word in 0..0x4000u16 {
                g.write16(segment, word * 2, 0x0720);
            }
        }
        Layout::Cga4 | Layout::Cga2 => {
            for word in 0..0x2000u16 {
                g.write16(0xB800, word * 2, 0);
            }
        }
        Layout::Planar | Layout::Linear => {
            indexed(g, 0x3C4, 2, 0x0F);
            let last: u16 = if layout == Layout::Linear {
                0xF9FF
            } else {
                0xFFFF
            };
            for offset in 0..=last {
                g.write8(0xA000, offset, 0);
            }
        }
    }
}

fn emulating(g: &mut Guest<'_>) -> bool {
    g.bda8(bda::EGA_INFO) & 0x01 == 0
}

/// AH 01h: the cursor's shape. The data area keeps the shape asked for;
/// with emulation on in a text mode, the card is given it moved from the
/// CGA's eight lines into the cell.
fn cursor_type(g: &mut Guest<'_>, shape: u16) {
    g.set_bda16(bda::CURSOR_SHAPE, shape);
    let screen = video::Screen::of(g);
    let [end, start] = shape.to_le_bytes();
    let (start, end) = if screen.layout == Layout::Text && emulating(g) {
        let height = u8::try_from(screen.height).unwrap_or(32);
        emulated(start, end, height, card(g).vga)
    } else {
        (start, end)
    };
    video::crtc(g, &screen, 10, start);
    video::crtc(g, &screen, 11, end);
}

/// A text mode's first cursor: the CGA's underline moved into the cell.
/// Without emulation the data area keeps the registers' own values.
fn default_cursor(g: &mut Guest<'_>) {
    if emulating(g) {
        cursor_type(g, DEFAULT_CURSOR);
        return;
    }
    let height = u8::try_from(g.bda16(bda::CHAR_HEIGHT).clamp(1, 32)).unwrap_or(32);
    let [end, start] = DEFAULT_CURSOR.to_le_bytes();
    let (start, end) = emulated(start, end, height, card(g).vga);
    cursor_type(g, u16::from_le_bytes([end, start]));
}

/// Where line `line` of the CGA's 8-line cell falls in a cell `height`
/// lines tall: the upper half keeps its place, the lower half its distance
/// from the foot, which a cell taller than the CGA's keeps clear as IBM's
/// tables do.
fn emulated_line(line: u8, height: u8) -> u8 {
    let last = height.saturating_sub(1);
    if line < 4 {
        return line.min(last);
    }
    let foot = if height > 8 { height - 2 } else { last };
    (foot + line).saturating_sub(7).min(last)
}

/// The cursor registers that show the CGA-style lines `start` to `end` in
/// a cell `height` lines tall. The VGA shows its start to its end; the EGA
/// stops before its end, an end past the cell lighting all of it and an end
/// above the start splitting it round the lines between. Values outside the
/// CGA's cell are the program's own and pass unchanged.
fn emulated(start: u8, end: u8, height: u8, vga: bool) -> (u8, u8) {
    let (first, last) = (start & 0x1F, end & 0x1F);
    if first > 7 || last > 7 {
        return (start, end);
    }
    let options = start & 0x60;
    let (first, last) = (emulated_line(first, height), emulated_line(last, height));
    if vga {
        return (options | first, last);
    }
    let bottom = height.saturating_sub(1);
    let whole = (first == 0 && last == bottom) || first == last + 1;
    if whole {
        (options, 0x1F)
    } else if last < bottom {
        (options | first, last + 1)
    } else {
        (options | first, 0)
    }
}

/// INT 10h on the EGA or VGA.
pub fn service(g: &mut Guest<'_>) {
    let vga = card(g).vga;
    match g.r8(Reg8::Ah) {
        0x00 => {
            let mode = g.r8(Reg8::Al);
            set_mode(g, mode);
        }
        0x01 => {
            let shape = g.r16(Reg16::Cx);
            cursor_type(g, shape);
        }
        0x10 => palette(g),
        0x11 => character_generator(g),
        0x12 => alternate(g),
        0x13 => write_string(g),
        0x1A if vga => display_combination(g),
        0x1B if vga => functionality(g),
        0x1C if vga => save_restore(g),
        _ => return video::common(g),
    }
    g.done(true);
}

fn palette(g: &mut Guest<'_>) {
    let (al, bh, bl) = (g.r8(Reg8::Al), g.r8(Reg8::Bh), g.r8(Reg8::Bl));
    let vga = card(g).vga;
    match al {
        0x00 if bl <= 0x0F || bl == 0x11 => palette_register(g, bl, bh),
        0x01 => palette_register(g, 0x11, bh),
        0x02 => {
            let (segment, offset) = (g.seg(Seg::Es), g.r16(Reg16::Dx));
            for i in 0..17u8 {
                let value = g.read8(segment, offset.wrapping_add(u16::from(i)));
                palette_register(g, if i == 16 { 0x11 } else { i }, value);
            }
        }
        0x03 => {
            // The EGA's mode control cannot be read: its text and
            // graphics bits follow the mode.
            let mode = if vga {
                read_attribute(g, 0x10)
            } else {
                let mode = g.bda8(bda::VIDEO_MODE);
                let index = modes::index(mode, Lines::L350, false).unwrap_or(0x15);
                read_entry(g, index).ac[0x10]
            };
            attribute(g, 0x10, if bl == 0 { mode & !0x08 } else { mode | 0x08 });
        }
        0x07 if vga => {
            let value = read_attribute(g, bl & 0x1F);
            g.set_r8(Reg8::Bh, value);
        }
        0x08 if vga => {
            let value = read_attribute(g, 0x11);
            g.set_r8(Reg8::Bh, value);
        }
        0x09 if vga => {
            let (segment, offset) = (g.seg(Seg::Es), g.r16(Reg16::Dx));
            for i in 0..17u8 {
                let value = read_attribute(g, if i == 16 { 0x11 } else { i });
                g.write8(segment, offset.wrapping_add(u16::from(i)), value);
            }
        }
        0x10 if vga => {
            g.output(0x3C8, bl);
            let colour = [g.r8(Reg8::Dh), g.r8(Reg8::Ch), g.r8(Reg8::Cl)];
            let summed = summing(g);
            dac_colour(g, colour, summed);
        }
        0x12 if vga => {
            let (segment, offset) = (g.seg(Seg::Es), g.r16(Reg16::Dx));
            let summed = summing(g);
            g.output(0x3C8, bl);
            for i in 0..g.r16(Reg16::Cx).min(256) {
                let at = offset.wrapping_add(i * 3);
                let colour = [0, 1, 2].map(|c| g.read8(segment, at.wrapping_add(c)));
                dac_colour(g, colour, summed);
            }
        }
        0x13 if vga => {
            let mode = read_attribute(g, 0x10);
            if bl == 0 {
                attribute(g, 0x10, (mode & 0x7F) | ((bh & 1) << 7));
            } else {
                attribute(g, 0x14, bh & 0x0F);
            }
        }
        0x15 if vga => {
            g.output(0x3C7, bl);
            for component in [Reg8::Dh, Reg8::Ch, Reg8::Cl] {
                let value = g.input(0x3C9);
                g.set_r8(component, value);
            }
        }
        0x17 if vga => {
            let (segment, offset) = (g.seg(Seg::Es), g.r16(Reg16::Dx));
            g.output(0x3C7, bl);
            for i in 0..g.r16(Reg16::Cx).min(256) * 3 {
                let value = g.input(0x3C9);
                g.write8(segment, offset.wrapping_add(i), value);
            }
        }
        0x18 if vga => g.output(0x3C6, bl),
        0x19 if vga => {
            let mask = g.input(0x3C6);
            g.set_r8(Reg8::Bl, mask);
        }
        0x1A if vga => {
            let mode = read_attribute(g, 0x10);
            let select = read_attribute(g, 0x14);
            g.set_r16(Reg16::Bx, u16::from_le_bytes([mode >> 7, select & 0x0F]));
        }
        0x1B if vga => {
            let first = g.r16(Reg16::Bx);
            for index in first..first.saturating_add(g.r16(Reg16::Cx)).min(256) {
                let index = u8::try_from(index).unwrap_or(0);
                g.output(0x3C7, index);
                let colour = [g.input(0x3C9), g.input(0x3C9), g.input(0x3C9)];
                g.output(0x3C8, index);
                dac_colour(g, colour, true);
            }
        }
        _ => {}
    }
}

/// AH 11h: loads character sets into the card, or points INT 1Fh and 43h at
/// graphics fonts, or reports where the fonts are.
fn character_generator(g: &mut Guest<'_>) {
    let (al, bl, bh) = (g.r8(Reg8::Al), g.r8(Reg8::Bl), g.r8(Reg8::Bh));
    let vga = card(g).vga;
    match al {
        0x00 | 0x10 => {
            let font = (g.seg(Seg::Es), g.r16(Reg16::Bp));
            let range = (g.r16(Reg16::Dx), g.r16(Reg16::Cx));
            load_glyphs(g, font, bh, bl, range);
            if al == 0x10 {
                set_text_height(g, bh);
            }
        }
        0x01 | 0x11 | 0x02 | 0x12 | 0x04 | 0x14 => {
            let height = match al & 0x0F {
                1 => 14,
                2 => 8,
                _ if vga => 16,
                _ => return,
            };
            load_glyphs(g, (SEGMENT, rom_font(height)), height, bl, (0, 256));
            if al & 0x10 != 0 {
                set_text_height(g, height);
            }
        }
        0x03 => indexed(g, 0x3C4, 3, bl),
        0x20 => {
            let (segment, offset) = (g.seg(Seg::Es), g.r16(Reg16::Bp));
            g.write16(0, 0x1F * 4, offset);
            g.write16(0, 0x1F * 4 + 2, segment);
        }
        0x21..=0x24 => {
            let (segment, offset, height) = match al {
                0x21 => (g.seg(Seg::Es), g.r16(Reg16::Bp), g.r8(Reg8::Cl)),
                0x22 => (SEGMENT, FONT_14, 14),
                0x23 => (SEGMENT, FONT_8, 8),
                _ if vga => (SEGMENT, FONT_16, 16),
                _ => (SEGMENT, FONT_14, 14),
            };
            g.write16(0, 0x43 * 4, offset);
            g.write16(0, 0x43 * 4 + 2, segment);
            g.set_bda16(bda::CHAR_HEIGHT, u16::from(height));
            let rows = match bl {
                0 => g.r8(Reg8::Dl),
                1 => 14,
                2 => 25,
                _ => 43,
            };
            g.set_bda8(bda::ROWS, rows.saturating_sub(1));
        }
        0x30 => {
            let (segment, offset) = match bh {
                0 => far_pointer(g, 0, 0x1F * 4),
                1 => far_pointer(g, 0, 0x43 * 4),
                2 => (SEGMENT, FONT_14),
                3 => (SEGMENT, FONT_8),
                4 => (SEGMENT, FONT_8 + 1024),
                6 if vga => (SEGMENT, FONT_16),
                _ => (0, 0),
            };
            g.regs_mut().set_sreg(Seg::Es, segment);
            g.set_r16(Reg16::Bp, offset);
            let height = g.bda16(bda::CHAR_HEIGHT);
            let rows = g.bda8(bda::ROWS);
            g.set_r16(Reg16::Cx, height);
            g.set_r8(Reg8::Dl, rows);
        }
        _ => {}
    }
}

/// The scan lines a text mode shows: the EGA's 350, or what the VGA's
/// vertical display end and line doubling make.
fn displayed_lines(g: &mut Guest<'_>, crtc: u16) -> u16 {
    if !card(g).vga {
        return 350;
    }
    let low = read_indexed(g, crtc, 0x12);
    let overflow = read_indexed(g, crtc, 0x07);
    let doubled = read_indexed(g, crtc, 0x09) & 0x80 != 0;
    let end =
        u16::from(low) | (u16::from(overflow & 0x02) << 7) | (u16::from(overflow & 0x40) << 3);
    (end + 1) >> u8::from(doubled)
}

/// A text mode's characters become `height` lines tall: the rows on the
/// screen, the display's end at the last whole row, the cursor and the data
/// area follow.
fn set_text_height(g: &mut Guest<'_>, height: u8) {
    if height == 0 || height > 32 {
        return;
    }
    let vga = card(g).vga;
    let crtc = g.bda16(bda::CRTC_PORT);
    let lines = displayed_lines(g, crtc);
    // The EGA's register reads as zero; it has no bits above the height.
    let max_scan = read_indexed(g, crtc, 0x09);
    let doubled = vga && max_scan & 0x80 != 0;
    indexed(g, crtc, 0x09, (max_scan & 0xE0) | (height - 1));
    let rows = u8::try_from(lines / u16::from(height))
        .unwrap_or(u8::MAX)
        .max(1);
    let end = ((u16::from(rows) * u16::from(height)) << u8::from(doubled)) - 1;
    let [end_low, end_high] = end.to_le_bytes();
    indexed(g, crtc, 0x12, end_low);
    if vga {
        let overflow = read_indexed(g, crtc, 0x07);
        let bits = ((end_high & 1) << 1) | ((end_high & 2) << 5);
        indexed(g, crtc, 0x07, (overflow & !0x42) | bits);
    }
    g.set_bda8(bda::ROWS, rows - 1);
    g.set_bda16(bda::CHAR_HEIGHT, u16::from(height));
    let columns = g.bda16(bda::COLUMNS);
    let page = (u32::from(rows) * u32::from(columns) * 2).next_multiple_of(256);
    g.set_bda16(bda::PAGE_SIZE, u16::try_from(page).unwrap_or(u16::MAX));
    default_cursor(g);
}

/// AH 12h, alternate select: the card's description, the print screen of
/// every row, and the VGA's options, each answering AL = 12h. Display
/// switching has no second display to switch to.
fn alternate(g: &mut Guest<'_>) {
    let (bl, al) = (g.r8(Reg8::Bl), g.r8(Reg8::Al));
    match bl {
        0x10 => {
            let mono = g.bda16(bda::CRTC_PORT) == 0x3B4;
            g.set_r16(Reg16::Bx, u16::from_le_bytes([MEMORY_CODE, u8::from(mono)]));
            let switches = g.bda8(bda::EGA_SWITCHES);
            g.set_r16(
                Reg16::Cx,
                u16::from_le_bytes([switches & 0x0F, switches >> 4]),
            );
        }
        0x20 => {
            g.write16(0, 0x05 * 4, PRINT_SCREEN);
            g.write16(0, 0x05 * 4 + 2, SEGMENT);
        }
        0x30..=0x34 | 0x36 if card(g).vga && (al <= 1 || (bl == 0x30 && al == 2)) => {
            option(g, bl, al);
            g.set_r8(Reg8::Al, 0x12);
        }
        _ => {}
    }
}

/// One of the VGA's options: the next text mode's lines, palette loading,
/// video, summing, cursor emulation, or refresh.
fn option(g: &mut Guest<'_>, bl: u8, al: u8) {
    let on = al == 0;
    let set = |g: &mut Guest<'_>, offset: u16, mask: u8, value: bool| {
        let byte = g.bda8(offset);
        g.set_bda8(offset, if value { byte | mask } else { byte & !mask });
    };
    match bl {
        0x30 => {
            let flags = g.bda8(bda::VGA_FLAGS) & !0x90;
            let (flags, switches) = match al {
                0 => (flags | 0x80, SWITCHES_200),
                1 => (flags, SWITCHES_COLOUR),
                _ => (flags | 0x10, SWITCHES_COLOUR),
            };
            g.set_bda8(bda::VGA_FLAGS, flags);
            let old = g.bda8(bda::EGA_SWITCHES);
            g.set_bda8(bda::EGA_SWITCHES, (old & 0xF0) | switches);
        }
        0x31 => set(g, bda::VGA_FLAGS, 0x08, !on),
        // A disabled card answers only its enable register, so the misc
        // output is read with the card connected.
        0x32 if on => {
            g.output(0x3C3, 0x01);
            let misc = g.input(0x3CC);
            g.output(0x3C2, misc | 0x02);
        }
        0x32 => {
            let misc = g.input(0x3CC);
            g.output(0x3C2, misc & !0x02);
            g.output(0x3C3, 0x00);
        }
        0x33 => set(g, bda::VGA_FLAGS, 0x02, on),
        0x34 => set(g, bda::EGA_INFO, 0x01, !on),
        _ => {
            let clocking = read_indexed(g, 0x3C4, 0x01);
            let clocking = if on {
                clocking & !0x20
            } else {
                clocking | 0x20
            };
            indexed(g, 0x3C4, 0x01, clocking);
        }
    }
}

/// AH 13h: a string at a position, with one attribute or a byte of its own
/// for each character, moving the cursor if asked.
fn write_string(g: &mut Guest<'_>) {
    let (mode, page, attribute) = (g.r8(Reg8::Al), g.r8(Reg8::Bh), g.r8(Reg8::Bl));
    let (segment, offset, count) = (g.seg(Seg::Es), g.r16(Reg16::Bp), g.r16(Reg16::Cx));
    let (row, column) = (g.r8(Reg8::Dh), g.r8(Reg8::Dl));
    let saved = video::cursor_of(g, page);
    video::move_cursor(g, page, u16::from(row), u16::from(column));
    let step = if mode & 2 != 0 { 2 } else { 1 };
    for i in 0..count {
        let at = offset.wrapping_add(i.wrapping_mul(step));
        let code = g.read8(segment, at);
        let colour = if mode & 2 != 0 {
            g.read8(segment, at.wrapping_add(1))
        } else {
            attribute
        };
        video::put(g, page, code, colour);
    }
    if mode & 1 == 0 {
        video::move_cursor(g, page, saved.0, saved.1);
    }
}

/// The display combination table the save pointers name, and its length.
fn combinations(g: &mut Guest<'_>) -> Option<((u16, u16), u8)> {
    match secondary_pointer(g, pointer::DCC) {
        (0, 0) => None,
        (segment, offset) => Some(((segment, offset), g.read8(segment, offset))),
    }
}

fn combination_at(g: &mut Guest<'_>, (segment, offset): (u16, u16), index: u8) -> [u8; 2] {
    let at = offset.wrapping_add(4 + u16::from(index) * 2);
    [g.read8(segment, at), g.read8(segment, at.wrapping_add(1))]
}

/// The display combination the data area's index names; `FFh`s for an index
/// past the table's end.
fn combination(g: &mut Guest<'_>) -> [u8; 2] {
    let index = g.bda8(bda::DCC_INDEX);
    match combinations(g) {
        Some((table, count)) if index < count => combination_at(g, table, index),
        _ => [0xFF; 2],
    }
}

/// AH 1Ah: the active display and the other one as display combination
/// codes, or recording them as the table's entry naming them.
fn display_combination(g: &mut Guest<'_>) {
    match g.r8(Reg8::Al) {
        0x00 => {
            let pair = combination(g);
            g.set_r16(Reg16::Bx, u16::from_le_bytes(pair));
        }
        0x01 => {
            let wanted = [g.r8(Reg8::Bl), g.r8(Reg8::Bh)];
            let index = combinations(g).and_then(|(table, count)| {
                (0..count).find(|&i| combination_at(g, table, i) == wanted)
            });
            g.set_bda8(bda::DCC_INDEX, index.unwrap_or(0xFF));
        }
        _ => return,
    }
    g.set_r8(Reg8::Al, 0x1A);
}

/// The scan lines the current mode shows, as AH 1Bh numbers them: 200,
/// 350, 400 or 480. The 400-line timing also shows the 200-line modes,
/// each line twice.
fn scan_lines(g: &mut Guest<'_>, mode: u8) -> u8 {
    match g.input(0x3CC) >> 6 {
        2 => 1,
        3 => 3,
        _ => {
            let crtc = g.bda16(bda::CRTC_PORT);
            let doubled = read_indexed(g, crtc, 0x09) & 0x80 != 0;
            if doubled || matches!(mode, 4..=6 | 0x0D | 0x0E | 0x13) {
                0
            } else {
                2
            }
        }
    }
}

/// AH 1Bh: the VGA's state, and where its static table is.
fn functionality(g: &mut Guest<'_>) {
    if g.r16(Reg16::Bx) != 0 {
        return;
    }
    let (segment, offset) = (g.seg(Seg::Es), g.r16(Reg16::Di));
    let mut state = [0u8; 64];
    state[..4].copy_from_slice(&far(FUNCTIONALITY));
    for (slot, at) in state[4..0x22].iter_mut().zip(bda::VIDEO_MODE..) {
        *slot = g.bda8(at);
    }
    let mode = g.bda8(bda::VIDEO_MODE);
    let layout = modes::layout(mode, true).unwrap_or(Layout::Text);
    state[0x22] = g.bda8(bda::ROWS).wrapping_add(1);
    state[0x23..0x25].copy_from_slice(&g.bda16(bda::CHAR_HEIGHT).to_le_bytes());
    state[0x25..0x27].copy_from_slice(&combination(g));
    let colours: u16 = match (mode, layout) {
        (7 | 0x0F, _) => 0,
        (_, Layout::Cga4) => 4,
        (_, Layout::Cga2) | (0x11, _) => 2,
        (_, Layout::Linear) => 256,
        _ => 16,
    };
    state[0x27..0x29].copy_from_slice(&colours.to_le_bytes());
    state[0x29] = match mode {
        0..=3 | 7 | 0x0D => 8,
        0x0E => 4,
        0x0F | 0x10 => 2,
        _ => 1,
    };
    state[0x2A] = scan_lines(g, mode);
    let maps = read_indexed(g, 0x3C4, 0x03);
    let normal = ((maps >> 2) & 0x04) | (maps & 0x03);
    let high = ((maps >> 3) & 0x04) | ((maps >> 2) & 0x03);
    state[0x2B] = normal;
    state[0x2C] = high;
    let flags = g.bda8(bda::VGA_FLAGS);
    let blink = read_attribute(g, 0x10) & 0x08 != 0;
    state[0x2D] = 0x01 | (flags & 0x0E) | (u8::from(emulating(g)) << 4) | (u8::from(blink) << 5);
    state[0x31] = MEMORY_CODE;
    let named = [
        save_pointer(g, pointer::DYNAMIC_SAVE),
        save_pointer(g, pointer::ALPHA_FONT),
        save_pointer(g, pointer::GRAPHICS_FONT),
        secondary_pointer(g, pointer::PALETTE_PROFILE),
    ]
    .map(|pointer| pointer != (0, 0));
    let dcc_moved = secondary_pointer(g, pointer::DCC) != (SEGMENT, DCC_TABLE);
    let bits = [
        normal != high,
        named[0],
        named[1],
        named[2],
        named[3],
        dcc_moved,
    ];
    state[0x32] = (0u8..)
        .zip(bits)
        .fold(0, |all, (bit, on)| all | (u8::from(on) << bit));
    for (i, &byte) in (0u16..).zip(&state) {
        g.write8(segment, offset.wrapping_add(i), byte);
    }
    g.set_r8(Reg8::Al, 0x1B);
}

/// The state AH 1Ch keeps: a header of each group's offset, then the groups
/// CX selects. Only this ROM reads it back.
mod saved {
    pub const HEADER: u16 = 6;
    /// The sequencer's, CRT controller's and graphics controller's indexes,
    /// the attribute index, the feature control, the misc output and the
    /// enable, then the registers of each.
    pub const HARDWARE: u16 = 7 + 5 + 25 + 21 + 9;
    /// The data area's video bytes, its EGA and VGA bytes and save pointer,
    /// and the vectors of INT 05h, 1Dh, 1Fh and 43h.
    pub const DATA: u16 = 0x1E + 7 + 4 + 16;
    /// The DAC's mask, its state and index, and its 256 colours.
    pub const DAC: u16 = 3 + 768;
    pub const VECTORS: [u16; 4] = [0x05, 0x1D, 0x1F, 0x43];
}

/// Each group, whether CX selects it, and its size.
fn groups(cx: u16) -> [(bool, u16); 3] {
    [
        (cx & 1 != 0, saved::HARDWARE),
        (cx & 2 != 0, saved::DATA),
        (cx & 4 != 0, saved::DAC),
    ]
}

/// AH 1Ch: how much the state takes in 64-byte blocks, or saving or
/// restoring it.
fn save_restore(g: &mut Guest<'_>) {
    let cx = g.r16(Reg16::Cx);
    let (segment, offset) = (g.seg(Seg::Es), g.r16(Reg16::Bx));
    match g.r8(Reg8::Al) {
        0x00 => {
            let chosen = groups(cx).into_iter().filter(|&(on, _)| on);
            let bytes = chosen.fold(saved::HEADER, |total, (_, size)| total + size);
            g.set_r16(Reg16::Bx, bytes.div_ceil(64));
        }
        0x01 => {
            let mut at = saved::HEADER;
            for (i, (on, size)) in (0u16..).zip(groups(cx)) {
                g.write16(segment, offset.wrapping_add(i * 2), if on { at } else { 0 });
                if !on {
                    continue;
                }
                let mut out = Run::new(segment, offset.wrapping_add(at));
                match i {
                    0 => save_hardware(g, &mut out),
                    1 => copy_data(g, &mut out, true),
                    _ => save_dac(g, &mut out),
                }
                at += size;
            }
        }
        0x02 => {
            for (i, (on, _)) in (0u16..).zip(groups(cx)) {
                let place = g.read16(segment, offset.wrapping_add(i * 2));
                if !on || place == 0 {
                    continue;
                }
                let mut input = Run::new(segment, offset.wrapping_add(place));
                match i {
                    0 => restore_hardware(g, &mut input),
                    1 => copy_data(g, &mut input, false),
                    _ => restore_dac(g, &mut input),
                }
            }
        }
        _ => return,
    }
    g.set_r8(Reg8::Al, 0x1C);
}

/// Successive bytes of guest memory, wrapping within their segment.
struct Run {
    segment: u16,
    offset: u16,
}

impl Run {
    const fn new(segment: u16, offset: u16) -> Self {
        Self { segment, offset }
    }

    fn put(&mut self, g: &mut Guest<'_>, value: u8) {
        g.write8(self.segment, self.offset, value);
        self.offset = self.offset.wrapping_add(1);
    }

    fn take(&mut self, g: &mut Guest<'_>) -> u8 {
        let value = g.read8(self.segment, self.offset);
        self.offset = self.offset.wrapping_add(1);
        value
    }
}

fn save_hardware(g: &mut Guest<'_>, out: &mut Run) {
    let crtc = g.bda16(bda::CRTC_PORT);
    let indexes = [g.input(0x3C4), g.input(crtc), g.input(0x3CE)];
    attribute_index(g);
    let ac_index = g.input(0x3C0);
    let rest = [ac_index, g.input(0x3CA), g.input(0x3CC), g.input(0x3C3)];
    for value in indexes.into_iter().chain(rest) {
        out.put(g, value);
    }
    for i in 0..5 {
        let value = read_indexed(g, 0x3C4, i);
        out.put(g, value);
    }
    for i in 0..25 {
        let value = read_indexed(g, crtc, i);
        out.put(g, value);
    }
    for i in 0..21 {
        let value = read_attribute(g, i);
        out.put(g, value);
    }
    for i in 0..9 {
        let value = read_indexed(g, 0x3CE, i);
        out.put(g, value);
    }
    restore_indexes(g, crtc, indexes, ac_index);
}

fn restore_indexes(g: &mut Guest<'_>, crtc: u16, [seq, crt, gc]: [u8; 3], ac: u8) {
    g.output(0x3C4, seq);
    g.output(crtc, crt);
    g.output(0x3CE, gc);
    attribute_index(g);
    g.output(0x3C0, ac);
    attribute_index(g);
}

fn restore_hardware(g: &mut Guest<'_>, input: &mut Run) {
    let indexes: [u8; 3] = core::array::from_fn(|_| input.take(g));
    let [ac_index, feature, misc, enabled]: [u8; 4] = core::array::from_fn(|_| input.take(g));
    g.output(0x3C3, enabled);
    let crtc = if misc & 0x01 != 0 { 0x3D4 } else { 0x3B4 };
    g.set_bda16(bda::CRTC_PORT, crtc);
    let seq: [u8; 5] = core::array::from_fn(|_| input.take(g));
    indexed(g, 0x3C4, 0, 0x01);
    g.output(0x3C2, misc);
    for (i, &value) in (1u8..).zip(&seq[1..]) {
        indexed(g, 0x3C4, i, value);
    }
    indexed(g, 0x3C4, 0, seq[0]);
    let registers: [u8; 25] = core::array::from_fn(|_| input.take(g));
    indexed(g, crtc, 0x11, registers[0x11] & 0x7F);
    for (i, &value) in (0u8..).zip(&registers) {
        indexed(g, crtc, i, value);
    }
    let ac: [u8; 21] = core::array::from_fn(|_| input.take(g));
    for (i, &value) in (0u8..).zip(&ac) {
        attribute(g, i, value);
    }
    for i in 0..9 {
        let value = input.take(g);
        indexed(g, 0x3CE, i, value);
    }
    g.output(crtc.wrapping_add(6), feature);
    restore_indexes(g, crtc, indexes, ac_index);
}

/// The data area's bytes and the vectors AH 1Ch keeps.
fn data_bytes() -> impl Iterator<Item = (u16, u16)> {
    let area = (bda::VIDEO_MODE..=0x66)
        .chain(bda::ROWS..=bda::DCC_INDEX)
        .chain(bda::SAVE_POINTERS..bda::SAVE_POINTERS + 4)
        .map(|at| (bda::SEGMENT, at));
    let vectors = saved::VECTORS
        .into_iter()
        .flat_map(|n| (0..4).map(move |i| (0, n * 4 + i)));
    area.chain(vectors)
}

/// Saves the data area's bytes to `run`, or restores them from it.
fn copy_data(g: &mut Guest<'_>, run: &mut Run, saving: bool) {
    for (segment, at) in data_bytes() {
        if saving {
            let value = g.read8(segment, at);
            run.put(g, value);
        } else {
            let value = run.take(g);
            g.write8(segment, at, value);
        }
    }
}

fn save_dac(g: &mut Guest<'_>, out: &mut Run) {
    let (mask, reading, index) = (g.input(0x3C6), g.input(0x3C7), g.input(0x3C8));
    for value in [mask, reading, index] {
        out.put(g, value);
    }
    g.output(0x3C7, 0);
    for _ in 0..768 {
        let value = g.input(0x3C9);
        out.put(g, value);
    }
    g.output(if reading == 3 { 0x3C7 } else { 0x3C8 }, index);
}

fn restore_dac(g: &mut Guest<'_>, input: &mut Run) {
    let [mask, reading, index]: [u8; 3] = core::array::from_fn(|_| input.take(g));
    g.output(0x3C6, mask);
    g.output(0x3C8, 0);
    for _ in 0..768 {
        let value = input.take(g);
        g.output(0x3C9, value);
    }
    g.output(if reading == 3 { 0x3C7 } else { 0x3C8 }, index);
}

/// INT 05h after AH 12h BL 20h: the print screen of every row on the
/// screen.
pub fn print_screen(g: &mut Guest<'_>) {
    let rows = g.bda8(bda::ROWS).wrapping_add(1);
    system::print_screen(g, rows);
}

#[cfg(test)]
#[path = "vga_tests.rs"]
mod tests;
