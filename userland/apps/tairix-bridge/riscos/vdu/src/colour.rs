//! Colours: `COLOUR`, `GCOL`, `TINT`, `OS_SetColour` and the ECF patterns,
//! and the colour blocks they make, which zero page holds at `&1480`,
//! `&14C0` and `&1500` with the ECF origin's offsets at `&11F8` and
//! `&11FC`, as RISC OS 5 keeps them there for software that reads and
//! writes them.
//!
//! A colour number becomes a pixel as RISC OS 5's modes have it: masked to
//! the mode's colours below eight bits; in eight bits the VIDC1 form, two
//! bits of each of red, green and blue from the number and the low two of
//! all three from the tint; in sixteen and thirty-two bits that eight-bit
//! form's default colour, packed.

use tairix_riscos_kernel::memory::layout::HIGH_ZERO_PAGE;
use tairix_riscos_kernel::os::{Fail, Os};
use tairix_riscos_kernel::output::workspace;
use tairix_vdu::mode::Flags;

use crate::mode::Format;

/// A `GCOL`: its action, its colour number as the VDU variable reads it, and
/// its tint; or a pixel value `OS_SetColour` gave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Gcol {
    /// 0 to 7, plus 8 where pixels of the opposite colour leave the screen as
    /// it is, plus 16 times the ECF pattern, 1 to 4, or 5 for all four side
    /// by side.
    pub(crate) action: u32,
    pub(crate) colour: u32,
    pub(crate) tint: u32,
    /// `OS_SetColour` gave the colour as a pixel value.
    pub(crate) literal: bool,
    /// `OS_SetColour` gave a pattern of a word a row.
    pub(crate) pattern: Option<[u32; 8]>,
}

impl Gcol {
    const fn plain(colour: u32, tint: u32) -> Self {
        Self {
            action: 0,
            colour,
            tint,
            literal: false,
            pattern: None,
        }
    }
}

/// A text colour: its number and tint, or a pixel value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TextColour {
    pub(crate) colour: u32,
    pub(crate) tint: u32,
    pub(crate) literal: bool,
}

/// The ECF patterns' default forms, a byte a row, as each depth's
/// `OS_Word` 10 reads them after a mode change.
const DEFAULT_PATTERNS: [[[u8; 2]; 4]; 3] = [
    [[0x33, 0x00], [0x33, 0xCC], [0xFF, 0xCC], [0xC0, 0x30]],
    [[0x66, 0x55], [0x66, 0x99], [0xAA, 0x99], [0xEE, 0xBB]],
    [[0x13, 0x31], [0x15, 0x51], [0x23, 0x32], [0x73, 0x37]],
];
/// The eight-bit patterns, which repeat every four rows.
const DEFAULT_PATTERNS_8: [[u8; 4]; 4] = [
    [0xFC, 0xFD, 0xFE, 0xFF],
    [0x03, 0x02, 0x01, 0x00],
    [0x23, 0x22, 0x21, 0x20],
    [0xDC, 0xDD, 0xDE, 0xDF],
];
/// The one-bit fourth pattern's diagonal, every four rows.
const DIAGONAL: [u8; 4] = [0xC0, 0x30, 0x0C, 0x03];
/// The dot pattern and its length after a mode change.
pub(crate) const DEFAULT_DOT: u64 = 0xAAAA_AAAA_AAAA_AAAA;
pub(crate) const DEFAULT_DOT_LENGTH: u32 = 8;

/// The colours the drivers keep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Colours {
    pub(crate) fore: Gcol,
    pub(crate) back: Gcol,
    pub(crate) text_fore: TextColour,
    pub(crate) text_back: TextColour,
    /// The four ECF patterns, a byte a row in the native form `OS_Word` 10
    /// reads.
    pub(crate) patterns: [[u8; 8]; 4],
    /// `VDU 23,17,4`: patterns are defined in the native form rather than
    /// the BBC Micro's.
    pub(crate) native: bool,
    /// The dot pattern, its first dot in the top bit, and its length.
    pub(crate) dot: u64,
    pub(crate) dot_length: u32,
    /// The dot a continued dotted line starts from.
    pub(crate) dot_at: u32,
    /// The ECF origin, in pixels from the screen's bottom left.
    pub(crate) ecf_origin: (i32, i32),
}

impl Colours {
    /// The colours a mode change leaves: white on black, the mode's default
    /// patterns, the default dot pattern.
    pub(crate) fn for_mode(format: &Format) -> Self {
        let white = default_white(format);
        Self {
            fore: Gcol::plain(white, 0xFF),
            back: Gcol::plain(0, 0),
            text_fore: TextColour {
                colour: if format.log2_bpp() >= 3 { 0xFF } else { white },
                tint: 0xFF,
                literal: false,
            },
            text_back: TextColour {
                colour: if format.has(Flags::TELETEXT) { 8 } else { 0 },
                tint: 0,
                literal: false,
            },
            patterns: default_patterns(format),
            native: false,
            dot: DEFAULT_DOT,
            dot_length: DEFAULT_DOT_LENGTH,
            dot_at: 0,
            ecf_origin: (0, 0),
        }
    }

    /// `VDU 20`'s colours: white on black, the actions storing.
    pub(crate) fn defaults(&mut self, format: &Format) {
        let white = default_white(format);
        self.fore = Gcol::plain(white, 0xFF);
        self.back = Gcol::plain(0, 0);
        self.text_fore = TextColour {
            colour: if format.log2_bpp() >= 3 { 0xFF } else { white },
            tint: 0xFF,
            literal: false,
        };
        self.text_back = TextColour {
            colour: if format.has(Flags::TELETEXT) { 8 } else { 0 },
            tint: 0,
            literal: false,
        };
    }

    /// `VDU 17`: a text colour, the background's from 128.
    pub(crate) fn text_colour(&mut self, format: &Format, c: u8) {
        let (target, number) = if c < 128 {
            (&mut self.text_fore, c)
        } else {
            (&mut self.text_back, c - 128)
        };
        target.colour = colour_var(format, c, number);
        target.literal = false;
    }

    /// `VDU 18`: a graphics action and colour, the background's from 128.
    pub(crate) fn graphics_colour(&mut self, format: &Format, action: u8, c: u8) {
        let (target, number) = if c < 128 {
            (&mut self.fore, c)
        } else {
            (&mut self.back, c - 128)
        };
        target.action = u32::from(action);
        target.colour = colour_var(format, c, number);
        target.literal = false;
        target.pattern = None;
    }

    /// `VDU 23,17,n,t`: a tint, 0 text foreground, 1 its background, 2 the
    /// graphics foreground, 3 its background.
    pub(crate) fn tint(&mut self, which: u8, tint: u8) {
        let tint = u32::from(tint & 0xC0);
        match which {
            0 => self.text_fore.tint = tint,
            1 => self.text_back.tint = tint,
            2 => self.fore.tint = tint,
            _ => self.back.tint = tint,
        }
    }

    /// `VDU 23,17,5`: the text colours exchanged.
    pub(crate) fn swap_text(&mut self) {
        core::mem::swap(&mut self.text_fore, &mut self.text_back);
    }

    /// `VDU 23,2` to `5`: pattern `n`, 1 to 4, from eight rows as given,
    /// in the native form or the BBC Micro's.
    pub(crate) fn define_pattern(&mut self, format: &Format, n: usize, rows: [u8; 8]) {
        let native = self.native || format.log2_bpp() >= 3;
        self.patterns[n] = if native {
            rows
        } else {
            rows.map(|row| bbc_to_native(row, format.log2_bpp()))
        };
    }

    /// `VDU 23,12` to `15`: pattern `n` from a simple form, two colours a row
    /// for four rows, or one a row in eight bits.
    pub(crate) fn define_simple(&mut self, format: &Format, n: usize, colours: [u8; 8]) {
        let bpp = format.log2_bpp();
        self.patterns[n] = if bpp >= 3 {
            colours.map(|c| simple_8(format, c))
        } else {
            let pixels = 8 >> bpp;
            let mask = (1u8 << (1 << bpp)) - 1;
            core::array::from_fn(|row| {
                let pair = [colours[2 * (row % 4)], colours[2 * (row % 4) + 1]];
                (0..pixels).fold(0, |byte, p| byte | ((pair[p % 2] & mask) << (p << bpp)))
            })
        };
    }

    /// `VDU 23,11`: the mode's default patterns, defined in the BBC Micro's
    /// form from now on.
    pub(crate) fn default_patterns(&mut self, format: &Format) {
        self.patterns = default_patterns(format);
        self.native = false;
    }

    /// The pixel value of graphics colour `gcol`, before any pattern.
    pub(crate) fn gcol_pixel(format: &Format, gcol: &Gcol) -> u32 {
        if gcol.literal {
            gcol.colour
        } else {
            pixel(format, gcol.colour, gcol.tint)
        }
    }

    /// The pixel value of text colour `colour`.
    pub(crate) fn text_pixel(format: &Format, colour: &TextColour) -> u32 {
        if colour.literal {
            colour.colour
        } else {
            pixel(format, colour.colour, colour.tint)
        }
    }

    /// The text colours' pixel values, replicated across a word.
    pub(crate) fn text_words(&self, format: &Format) -> (u32, u32) {
        (
            replicate(format, Self::text_pixel(format, &self.text_fore)),
            replicate(format, Self::text_pixel(format, &self.text_back)),
        )
    }

    /// Writes the colour blocks and the ECF offsets into zero page, as each
    /// change to the colours, the patterns or the ECF origin leaves them.
    pub(crate) fn publish(&self, os: &mut Os<'_>, format: &Format) -> Result<(), Fail> {
        let shift = (32 - ((self.ecf_origin.0.cast_unsigned() << format.log2_bpc()) & 31)) & 31;
        let y_offset = format
            .height()
            .wrapping_sub(self.ecf_origin.1.cast_unsigned())
            & 7;
        let fore_pixel = Self::gcol_pixel(format, &self.fore);
        let back_pixel = Self::gcol_pixel(format, &self.back);
        let fore = self.block(format, &self.fore, back_pixel, shift);
        let back = self.block(format, &self.back, fore_pixel, shift);
        let store = Gcol {
            action: 0,
            ..self.back
        };
        let store = self.block(format, &store, fore_pixel, shift);
        for (offset, block) in [
            (workspace::FOREGROUND, fore),
            (workspace::BACKGROUND, back),
            (workspace::BACKGROUND_STORE, store),
        ] {
            let mut bytes = [0u8; workspace::BLOCK as usize];
            for (i, [ora, eor]) in block.iter().enumerate() {
                bytes[8 * i..8 * i + 4].copy_from_slice(&ora.to_le_bytes());
                bytes[8 * i + 4..8 * i + 8].copy_from_slice(&eor.to_le_bytes());
            }
            os.write_bytes(HIGH_ZERO_PAGE + offset, &bytes)?;
        }
        os.write_word(HIGH_ZERO_PAGE + workspace::ECF_SHIFT, shift)?;
        os.write_word(HIGH_ZERO_PAGE + workspace::ECF_Y_OFFSET, y_offset)
    }

    /// The block `gcol` makes: each row's words for its action on its colour
    /// or pattern row, pixels equal to `opposite` left alone where the action
    /// asks it, then turned right by `shift` for the ECF origin.
    fn block(&self, format: &Format, gcol: &Gcol, opposite: u32, shift: u32) -> [[u32; 2]; 8] {
        let pattern = gcol.action >> 4;
        let transparent = gcol.action & 8 != 0;
        let operation = gcol.action & 7;
        let solid = replicate(format, Self::gcol_pixel(format, gcol));
        let opposite_word = replicate(format, opposite);
        core::array::from_fn(|row| {
            let word = match (gcol.pattern, pattern) {
                (Some(rows), _) => rows[row],
                (None, 1..=4) => self.pattern_word(format, (pattern - 1) as usize, row),
                (None, 5) => self.giant_word(format, row),
                (None, _) => solid,
            };
            let [mut ora, mut eor] = operate(operation, word);
            if transparent {
                let keep = !alike(format, word, opposite_word);
                ora &= keep;
                eor &= keep;
            }
            [ora.rotate_right(shift), eor.rotate_right(shift)]
        })
    }

    /// Pattern `n`'s row `row` as a word of the screen's pixels.
    fn pattern_word(&self, format: &Format, n: usize, row: usize) -> u32 {
        let byte = self.patterns[n][row];
        if format.direct() {
            return replicate(format, direct_of_internal(format, byte));
        }
        expand(format, byte)
    }

    /// The four patterns side by side: row `row` of each in turn across a
    /// word, as far as a word holds them.
    fn giant_word(&self, format: &Format, row: usize) -> u32 {
        let bytes = [0, 1, 2, 3].map(|n| self.patterns[n][row]);
        if format.log2_bpc() == format.log2_bpp() && !format.direct() {
            return u32::from_le_bytes(bytes);
        }
        self.pattern_word(format, 0, row)
    }
}

/// The colour number a `VDU 17` or `VDU 18` byte `c` leaves in the VDU
/// variable: the number within the mode's colours, or in the full-palette and
/// direct-colour modes the byte as given.
fn colour_var(format: &Format, c: u8, number: u8) -> u32 {
    if format.direct() || (format.log2_bpp() == 3 && format.has(Flags::FULL_PALETTE)) {
        return u32::from(c);
    }
    let colours = if format.log2_bpp() == 3 {
        63
    } else {
        format.ncolour()
    };
    u32::from(number) & colours
}

/// The default foreground's colour number: white, which in sixteen colours
/// is 7, the colours above it flashing, and in eight bits and more the colour
/// numbers' six-bit white.
fn default_white(format: &Format) -> u32 {
    if format.log2_bpp() >= 3 {
        63
    } else {
        format.ncolour().min(7)
    }
}

/// The pixel value colour number `colour` with `tint` is in `format`.
pub(crate) fn pixel(format: &Format, colour: u32, tint: u32) -> u32 {
    match format.log2_bpp() {
        0..=2 => colour & format.ncolour(),
        3 => u32::from(internal(colour, tint)),
        _ => direct_of_internal(format, internal(colour, tint)),
    }
}

/// The eight-bit VIDC1 form of a six-bit colour number and a tint: blue's
/// high bit at the top, then green's two, red's high bit, blue's low bit,
/// red's low bit and the tint's two.
pub(crate) const fn internal(colour: u32, tint: u32) -> u8 {
    let c = colour & 63;
    let value = (bit(c, 5) << 7)
        | (bit(c, 3) << 6)
        | (bit(c, 2) << 5)
        | (bit(c, 1) << 4)
        | (bit(c, 4) << 3)
        | (bit(c, 0) << 2)
        | ((tint >> 6) & 3);
    value.to_le_bytes()[0]
}

const fn bit(value: u32, n: u32) -> u32 {
    (value >> n) & 1
}

/// The colour number and tint an eight-bit VIDC1-form pixel holds.
pub(crate) const fn number_of_internal(pixel: u8) -> (u32, u32) {
    let p = pixel as u32;
    let colour = (bit(p, 7) << 5)
        | (bit(p, 6) << 3)
        | (bit(p, 5) << 2)
        | (bit(p, 4) << 1)
        | (bit(p, 3) << 4)
        | bit(p, 2);
    (colour, (p & 3) << 6)
}

/// The direct-colour pixel the eight-bit default palette's entry `pixel` is
/// in `format`.
pub(crate) fn direct_of_internal(format: &Format, pixel: u8) -> u32 {
    Direct::of(format).pack(tairix_vdu::palette::default_256(pixel))
}

/// How a direct-colour pixel holds its red, green and blue: which order
/// they come in, and how many bits each takes.
#[derive(Clone, Copy)]
pub(crate) struct Direct {
    swapped: bool,
    layout: Layout,
}

#[derive(Clone, Copy)]
enum Layout {
    /// Four bits each.
    Twelve,
    /// Five, six and five.
    Sixteen,
    /// Five each.
    Fifteen,
    /// Eight each.
    TwentyFour,
}

impl Direct {
    /// `format`'s.
    pub(crate) fn of(format: &Format) -> Self {
        let layout = match (format.log2_bpp(), format.ncolour()) {
            (4, 0xFFF) => Layout::Twelve,
            (4, _) if format.has(Flags::FULL_PALETTE) => Layout::Sixteen,
            (4, _) => Layout::Fifteen,
            _ => Layout::TwentyFour,
        };
        Self {
            swapped: format.has(Flags::RGB),
            layout,
        }
    }

    /// Red, green and blue packed into a pixel.
    pub(crate) fn pack(self, [r, g, b]: [u8; 3]) -> u32 {
        let (low, high) = if self.swapped { (b, r) } else { (r, b) };
        let (low, g, high) = (u32::from(low), u32::from(g), u32::from(high));
        match self.layout {
            Layout::Twelve => (low >> 4) | ((g >> 4) << 4) | ((high >> 4) << 8),
            Layout::Sixteen => (low >> 3) | ((g >> 2) << 5) | ((high >> 3) << 11),
            Layout::Fifteen => (low >> 3) | ((g >> 3) << 5) | ((high >> 3) << 10),
            Layout::TwentyFour => low | (g << 8) | (high << 16),
        }
    }

    /// A pixel's red, green and blue, each widened to eight bits.
    pub(crate) fn unpack(self, pixel: u32) -> [u32; 3] {
        let (low, g, high) = match self.layout {
            Layout::Twelve => (
                (pixel & 0xF) * 17,
                ((pixel >> 4) & 0xF) * 17,
                ((pixel >> 8) & 0xF) * 17,
            ),
            Layout::Sixteen => (
                (pixel & 0x1F) * 255 / 31,
                ((pixel >> 5) & 0x3F) * 255 / 63,
                ((pixel >> 11) & 0x1F) * 255 / 31,
            ),
            Layout::Fifteen => (
                (pixel & 0x1F) * 255 / 31,
                ((pixel >> 5) & 0x1F) * 255 / 31,
                ((pixel >> 10) & 0x1F) * 255 / 31,
            ),
            Layout::TwentyFour => (pixel & 0xFF, (pixel >> 8) & 0xFF, (pixel >> 16) & 0xFF),
        };
        if self.swapped {
            [high, g, low]
        } else {
            [low, g, high]
        }
    }
}

/// An eight-bit mode's simple pattern colour: the colour number's six bits
/// and two bits of tint above them.
fn simple_8(format: &Format, c: u8) -> u8 {
    if format.has(Flags::FULL_PALETTE) && format.log2_bpp() == 3 && format.ncolour() == 255 {
        return c;
    }
    internal(u32::from(c), u32::from(c))
}

/// A BBC Micro form pattern row as the native form: the BBC's pixels
/// across interleaved bits, the native's whole and the leftmost lowest.
const fn bbc_to_native(row: u8, log2_bpp: u32) -> u8 {
    let r = row as u32;
    let native = match log2_bpp {
        0 => r.reverse_bits() >> 24,
        1 => {
            let mut out = 0;
            let mut p = 0;
            while p < 4 {
                let colour = ((r >> (7 - p)) & 1) << 1 | ((r >> (3 - p)) & 1);
                out |= colour << (2 * p);
                p += 1;
            }
            out
        }
        2 => {
            let mut out = 0;
            let mut p = 0;
            while p < 2 {
                let colour = ((r >> (7 - p)) & 1) << 3
                    | ((r >> (5 - p)) & 1) << 2
                    | ((r >> (3 - p)) & 1) << 1
                    | ((r >> (1 - p)) & 1);
                out |= colour << (4 * p);
                p += 1;
            }
            out
        }
        _ => r,
    };
    native.to_le_bytes()[0]
}

/// The default patterns of `format`'s depth.
fn default_patterns(format: &Format) -> [[u8; 8]; 4] {
    match format.log2_bpp() {
        0 => core::array::from_fn(|n| {
            core::array::from_fn(|row| {
                if n == 3 {
                    DIAGONAL[row % 4]
                } else {
                    DEFAULT_PATTERNS[0][n][row % 2]
                }
            })
        }),
        1 | 2 => {
            let table = &DEFAULT_PATTERNS[format.log2_bpp() as usize];
            core::array::from_fn(|n| core::array::from_fn(|row| table[n][row % 2]))
        }
        _ => core::array::from_fn(|n| core::array::from_fn(|row| DEFAULT_PATTERNS_8[n][row % 4])),
    }
}

/// A pattern row's byte as a word of the screen's pixels: each pixel the
/// byte holds widened to the screen's, the whole repeated across.
fn expand(format: &Format, byte: u8) -> u32 {
    let bpp = format.log2_bpp();
    let bpc = format.log2_bpc();
    let pixels = 8u32 >> bpp;
    let mask = (1u32 << (1 << bpp)) - 1;
    let mut part = 0u32;
    for p in 0..pixels {
        let value = (u32::from(byte) >> (p << bpp)) & mask;
        part |= widen(value, bpp, bpc) << (p << bpc);
    }
    repeat(part, pixels << bpc, 32)
}

/// A pixel of `1 << bpp` bits widened to `1 << bpc` by repeating it.
const fn widen(value: u32, bpp: u32, bpc: u32) -> u32 {
    repeat(value, 1 << bpp, 1 << bpc)
}

/// A pixel value repeated across a word of `format`'s pixels.
pub(crate) fn replicate(format: &Format, value: u32) -> u32 {
    repeat(value & format.pixel_mask(), 1 << format.log2_bpp(), 32)
}

/// The low `width` bits of `part` repeated until `total` bits are filled.
const fn repeat(part: u32, width: u32, total: u32) -> u32 {
    let mut out = part;
    let mut filled = width;
    while filled < total {
        out |= out << filled;
        filled *= 2;
    }
    out
}

/// The OR and exclusive-OR words GCOL action `operation` takes on `word`.
const fn operate(operation: u32, word: u32) -> [u32; 2] {
    match operation {
        0 => [u32::MAX, !word],
        1 => [word, 0],
        2 => [!word, !word],
        3 => [0, word],
        4 => [0, u32::MAX],
        5 => [0, 0],
        6 => [word, word],
        _ => [!word, 0],
    }
}

/// A mask of the pixels where `a` and `b` hold the same value.
fn alike(format: &Format, a: u32, b: u32) -> u32 {
    let unit = 1u32 << format.log2_bpc();
    let mut mask = 0;
    let mut at = 0;
    while at < 32 {
        let bits = if unit >= 32 {
            u32::MAX
        } else {
            ((1 << unit) - 1) << at
        };
        if a & bits == b & bits {
            mask |= bits;
        }
        at += unit;
    }
    mask
}

#[cfg(test)]
mod tests;
