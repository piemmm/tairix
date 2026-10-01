//! A complete, fail-closed decoder for RISC OS sprite area files (Acorn
//! filetype `&FF9`), and the sprite model an editor and the encoder share.
//!
//! A sprite area is a container of independent, named pictures — an
//! application's whole icon set in one file — so it decodes as a page
//! container rather than as one picture. It carries **no signature**: its
//! first word is the sprite count, and RISC OS types a file from its
//! directory entry rather than from its content. A caller therefore names
//! this format rather than having it recognised ([`crate::probe_as`],
//! [`crate::decode_as`], [`crate::Sequence::open_as`]); recognising one from
//! a structural coincidence would be a false-positive machine, and one this
//! crate would then act on.
//!
//! # Readings the format's own text does not settle
//!
//! **A sprite with no palette is shown in the desktop's colours.** RISC OS
//! resolves such a sprite against the palette the display holds, and the
//! desktop holds its own: the Wimp plots a two-colour sprite's pixels as its
//! colours 0 and 7, a four-colour one's as 0, 2, 4 and 7, and a sixteen-colour
//! one's as its sixteen standard colours — white, six greys, black, dark blue,
//! yellow, green, red, cream, dark green, orange and light blue. None of these
//! is a PC palette. At eight bits there is no table but the arrangement the
//! Programmer's Reference Manual gives a screen-memory byte — four bits per
//! channel, the low two shared as the tint — so an 8bpp sprite needs no
//! palette of its own to decode exactly. [`desktop_palette`] is that set, the
//! one definition the decoder, the encoder and an editor making a sprite read.
//!
//! **A short palette is the VIDC1 arrangement, not a truncated one.** VIDC
//! holds sixteen palette registers, so most 256-colour sprites carry sixteen
//! entries and those written by `*ScreenSave` carry sixty-four; RISC OS
//! passes the *last* sixteen to the hardware, and a pixel's top four bits
//! then override supremacy bits of the entry its low four selected. A
//! palette long enough for the depth is read straight through instead, which
//! is what a full 256-entry palette is for.
//!
//! **A mask supersedes a pixel's own alpha.** The mask is what the format
//! calls a sprite's transparency, so a file carrying both it and an alpha
//! channel is contradicting itself; taking the mask keeps one answer rather
//! than inventing arithmetic over two.
//!
//! CMYK, JPEG-data and YCbCr sprite types are refused by name rather than
//! half-read. The native walk ([`SpriteAreaReader`]) keeps any sprite it
//! cannot read as its exact bytes ([`OpaqueSprite`]), so an editor writes back
//! what it could not open rather than losing it.

use alloc::vec::Vec;
use core::fmt;

use tairix_util::fallible;

use crate::channel::{Channel, Sampler};
use crate::pages::{PageSource, Pages};
use crate::picture::{IndexDepth, Picture, Rgba8};
use crate::{DecodeError, DecodeLimits, RasterImage, RGBA_BYTES};

/// A file holds the sprite area control block without its first word — the
/// area's total size, which the file's own length already gives — so every
/// offset the file states is four greater than the position it names.
pub(crate) const AREA_HEADER_LEN: u32 = 12;
pub(crate) const AREA_OFFSET_BIAS: u32 = 4;

/// Bytes one sprite's control block occupies: the least a sprite can be.
pub const SPRITE_HEADER_LEN: u32 = 44;
const NEXT_AT: u32 = 0;
const NAME_AT: u32 = 4;
const WIDTH_WORDS_AT: u32 = 16;
const HEIGHT_AT: u32 = 20;
const FIRST_BIT_AT: u32 = 24;
const LAST_BIT_AT: u32 = 28;
const IMAGE_AT: u32 = 32;
const MASK_AT: u32 = 36;
const MODE_AT: u32 = 40;

/// Bytes one palette entry occupies: the pair of words `OS_ReadPalette`
/// returns, which differ only for a flashing colour.
pub(crate) const PALETTE_ENTRY_LEN: u32 = 8;

/// Palette entries VIDC itself holds, which is what a sprite carrying fewer
/// than its depth needs is supplying.
const VIDC_ENTRIES: u32 = 16;

/// The sprite type of 32-bit pixels, the truecolour a new sprite is written
/// in, and the channel widths and bytes of one of its pixels.
const TRUECOLOUR_TYPE: u32 = 6;
const TRUECOLOUR_PIXEL: ([u32; RGBA_BYTES], u32) = ([8, 8, 8, 8], 4);

/// Bits per pixel each numbered screen mode holds, indexed by mode number;
/// zero for mode 7, whose Teletext cells are character codes rather than
/// pixels. Numbers past this table are third-party extension modes, whose
/// depth only the module that defined them knows.
const MODE_BITS: [u8; 54] = [
    1, 2, 4, 1, 1, 2, 1, 0, 2, 4, 8, 2, 4, 8, 4, 8, 4, 4, 1, 2, 4, 8, 4, 1, 8, 1, 2, 4, 8, 1, 2, 4,
    8, 1, 2, 4, 8, 1, 2, 4, 8, 1, 2, 4, 1, 2, 4, 8, 4, 8, 1, 2, 4, 8,
];

/// Each numbered mode's `XEigFactor` and `YEigFactor`: a pixel is `1 << eig`
/// OS units across and down, which is its shape and its size on a desktop.
const MODE_EIG: [(u8, u8); 54] = [
    (1, 2),
    (2, 2),
    (3, 2),
    (1, 2),
    (2, 2),
    (3, 2),
    (2, 2),
    (2, 2),
    (1, 2),
    (2, 2),
    (3, 2),
    (1, 2),
    (1, 2),
    (2, 2),
    (1, 2),
    (1, 2),
    (1, 2),
    (1, 2),
    (1, 1),
    (1, 1),
    (1, 1),
    (1, 1),
    (0, 1),
    (1, 1),
    (1, 2),
    (1, 1),
    (1, 1),
    (1, 1),
    (1, 1),
    (1, 1),
    (1, 1),
    (1, 1),
    (1, 1),
    (1, 2),
    (1, 2),
    (1, 2),
    (1, 2),
    (1, 2),
    (1, 2),
    (1, 2),
    (1, 2),
    (1, 2),
    (1, 2),
    (1, 2),
    (1, 2),
    (1, 2),
    (1, 2),
    (2, 1),
    (2, 1),
    (2, 1),
    (2, 2),
    (2, 2),
    (2, 2),
    (2, 2),
];

/// The finest resolution an eigen factor can state: OS units per inch.
const OS_UNITS_PER_INCH: u32 = 180;

/// The largest eigen factor a RISC OS 5 mode word's two-bit fields hold.
const RO5_MAX_EIG: u8 = 3;

/// The coarsest eigen factor a pixel's shape is read from.
const MAX_EIG: u8 = 4;

/// The sixteen colours of the desktop's standard palette, in Wimp colour
/// order.
const WIMP_COLOURS: [[u8; 3]; 16] = [
    [0xFF, 0xFF, 0xFF],
    [0xDD, 0xDD, 0xDD],
    [0xBB, 0xBB, 0xBB],
    [0x99, 0x99, 0x99],
    [0x77, 0x77, 0x77],
    [0x55, 0x55, 0x55],
    [0x33, 0x33, 0x33],
    [0x00, 0x00, 0x00],
    [0x00, 0x44, 0x99],
    [0xEE, 0xEE, 0x00],
    [0x00, 0xCC, 0x00],
    [0xDD, 0x00, 0x00],
    [0xEE, 0xEE, 0xBB],
    [0x55, 0x88, 0x00],
    [0xFF, 0xBB, 0x00],
    [0x00, 0xBB, 0xFF],
];

/// The Wimp colours a two-colour sprite's pixels are plotted as.
const DESKTOP_2: [[u8; 3]; 2] = [WIMP_COLOURS[0], WIMP_COLOURS[7]];

/// The Wimp colours a four-colour sprite's pixels are plotted as.
const DESKTOP_4: [[u8; 3]; 4] = [
    WIMP_COLOURS[0],
    WIMP_COLOURS[2],
    WIMP_COLOURS[4],
    WIMP_COLOURS[7],
];

/// The screen-memory byte's own arrangement at eight bits: bit 0 and 1 the
/// tint every channel shares, then red bit 2, blue bit 2, red bit 3, green
/// bits 2 and 3, and blue bit 3.
const DESKTOP_256: [[u8; 3]; 256] = tint_palette();

const fn tint_palette() -> [[u8; 3]; 256] {
    let mut table = [[0u8; 3]; 256];
    let mut byte = 0u32;
    while byte < 256 {
        let tint = byte & 0x3;
        let red = (byte >> 4 & 1) << 3 | (byte >> 2 & 1) << 2 | tint;
        let green = (byte >> 6 & 1) << 3 | (byte >> 5 & 1) << 2 | tint;
        let blue = (byte >> 7 & 1) << 3 | (byte >> 3 & 1) << 2 | tint;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a nibble times seventeen is at most 255"
        )]
        {
            table[byte as usize] = [
                (red * 0x11) as u8,
                (green * 0x11) as u8,
                (blue * 0x11) as u8,
            ];
        }
        byte += 1;
    }
    table
}

/// The colours a paletted sprite with no palette of its own is shown in:
/// the desktop's, exactly as the Wimp plots such a sprite.
#[must_use]
pub fn desktop_palette(depth: IndexDepth) -> &'static [[u8; 3]] {
    match depth {
        IndexDepth::One => &DESKTOP_2,
        IndexDepth::Two => &DESKTOP_4,
        IndexDepth::Four => &WIMP_COLOURS,
        IndexDepth::Eight => &DESKTOP_256,
    }
}

/// The word at `at`, or a refusal where the input holds no word there.
fn word(bytes: &[u8], at: u32) -> Result<u32, DecodeError> {
    usize::try_from(at)
        .ok()
        .and_then(|at| crate::le_u32(bytes, at))
        .ok_or(DecodeError::SpriteTruncated)
}

/// The `len` bytes at `base + offset`.
fn slice_at(bytes: &[u8], base: u32, offset: u32, len: u32) -> Result<&[u8], DecodeError> {
    let start = base
        .checked_add(offset)
        .ok_or(DecodeError::SpriteTruncated)?;
    let end = start.checked_add(len).ok_or(DecodeError::SpriteTruncated)?;
    let (Ok(start), Ok(end)) = (usize::try_from(start), usize::try_from(end)) else {
        return Err(DecodeError::SpriteTruncated);
    };
    bytes.get(start..end).ok_or(DecodeError::SpriteTruncated)
}

/// Which colour a packed pixel's lowest field carries, and whether its
/// highest is alpha rather than an unused byte.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Order {
    pub(crate) red_lowest: bool,
    pub(crate) alpha: bool,
}

/// The channel order every sprite mode word but a RISC OS 5 one implies.
const VIDC_ORDER: Order = Order {
    red_lowest: true,
    alpha: false,
};

/// How one pixel's bits sit in a row.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Layout {
    /// A palette index of 1, 2, 4, or 8 bits, least significant pixel
    /// leftmost.
    Indexed { bits: u32 },
    /// A little-endian value of 2, 3, or 4 bytes, its channels cut out by
    /// fields the sprite type fixes.
    Packed {
        bytes: u32,
        channels: [Channel; RGBA_BYTES],
    },
}

impl Layout {
    pub(crate) const fn bits(self) -> u32 {
        match self {
            Self::Indexed { bits } => bits,
            Self::Packed { bytes, .. } => bytes * 8,
        }
    }
}

/// How wide one pixel of a sprite's mask is, and so how it is read.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum MaskDepth {
    /// An old-format sprite's mask is the image's own depth and shares its
    /// row layout; only whether a pixel's bits are all clear is read.
    Image,
    /// A new-format sprite's mask is one bit per pixel, starting at bit zero
    /// of rows of its own.
    Bit,
    /// A wide mask is eight bits per pixel, read as alpha.
    Alpha,
}

/// What a sprite mode word says about the pixels it describes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Mode {
    pub(crate) layout: Layout,
    pub(crate) mask: MaskDepth,
    /// A numbered mode, the only kind whose rows may begin part way into
    /// their first word.
    pub(crate) numbered: bool,
    /// The pixel's eigen factors across and down.
    pub(crate) eig: (u8, u8),
}

/// Cut a packed pixel's channels out of the widths a sprite type fixes,
/// lowest field first.
fn packed_layout(widths: [u32; RGBA_BYTES], bytes: u32, order: Order) -> Layout {
    let mid = Channel::fixed(widths[0], widths[1]);
    let low = Channel::fixed(0, widths[0]);
    let high = Channel::fixed(widths[0] + widths[1], widths[2]);
    let (red, blue) = if order.red_lowest {
        (low, high)
    } else {
        (high, low)
    };
    let top = if order.alpha {
        Channel::fixed(widths[0] + widths[1] + widths[2], widths[3])
    } else {
        Channel::ABSENT
    };
    Layout::Packed {
        bytes,
        channels: [red, mid, blue, top],
    }
}

/// The pixel format a sprite type names.
fn layout_for(sprite_type: u32, order: Order) -> Result<Layout, DecodeError> {
    let (widths, bytes) = match sprite_type {
        1 => return Ok(Layout::Indexed { bits: 1 }),
        2 => return Ok(Layout::Indexed { bits: 2 }),
        3 => return Ok(Layout::Indexed { bits: 4 }),
        4 => return Ok(Layout::Indexed { bits: 8 }),
        5 => ([5, 5, 5, 1], 2),
        TRUECOLOUR_TYPE => TRUECOLOUR_PIXEL,
        8 => ([8, 8, 8, 0], 3),
        10 => ([5, 6, 5, 0], 2),
        16 => ([4, 4, 4, 4], 2),
        _ => return Err(DecodeError::SpriteUnsupportedType),
    };
    // Asking for alpha where the format has no fourth field is a mode word
    // contradicting itself, and dropping the flag would hide that.
    if order.alpha && widths[3] == 0 {
        return Err(DecodeError::SpriteInvalidModeWord);
    }
    Ok(packed_layout(widths, bytes, order))
}

/// The channel order mode-flags bits 12 to 15 name. Only the RGB family is a
/// depth this decoder claims; the CMYK and YCbCr families are colour spaces
/// of their own.
fn data_format(flags: u32) -> Result<Order, DecodeError> {
    if flags >> 4 & 0x3 != 0 {
        return Err(DecodeError::SpriteUnsupportedType);
    }
    Ok(Order {
        red_lowest: flags >> 6 & 1 == 0,
        alpha: flags >> 7 & 1 == 1,
    })
}

/// What a numbered screen mode says about its pixels.
fn numbered_mode(mode: u32) -> Result<Mode, DecodeError> {
    let index = usize::try_from(mode).map_err(|_| DecodeError::SpriteUnknownMode)?;
    let bits = MODE_BITS
        .get(index)
        .copied()
        .filter(|&bits| bits != 0)
        .ok_or(DecodeError::SpriteUnknownMode)?;
    Ok(Mode {
        layout: Layout::Indexed {
            bits: u32::from(bits),
        },
        mask: MaskDepth::Image,
        numbered: true,
        eig: MODE_EIG[index],
    })
}

/// The eigen factor a resolution of `dpi` states: the finest whose OS-unit
/// pitch it does not exceed.
fn eig_of_dpi(dpi: u32) -> u8 {
    let mut eig = 0u8;
    while eig < MAX_EIG && OS_UNITS_PER_INCH >> eig > dpi {
        eig += 1;
    }
    eig
}

/// Decode a sprite mode word.
///
/// A word under 256 is a screen mode number; one with bit 0 clear is a
/// pointer to a mode selector block, which no file can hold; the rest are
/// the two sprite mode word formats, told apart by whether bits 27 to 30 are
/// all set.
pub(crate) fn read_mode(value: u32) -> Result<Mode, DecodeError> {
    if value < 256 {
        return numbered_mode(value);
    }
    if value & 1 == 0 {
        return Err(DecodeError::SpriteInvalidModeWord);
    }
    let (sprite_type, order, eig) = if value >> 27 & 0xF == 0xF {
        // A RISC OS 5 word: a fixed pattern in bits 27-30, 16-19, and 0-3,
        // the eigen factors in bits 4-7, a seven-bit type, and mode-flags
        // bits 8-15.
        if value & 0x000F_000F != 1 {
            return Err(DecodeError::SpriteInvalidModeWord);
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "each eigen factor is a two-bit field"
        )]
        let eig = ((value >> 4 & 0x3) as u8, (value >> 6 & 0x3) as u8);
        (value >> 20 & 0x7F, data_format(value >> 8 & 0xFF)?, eig)
    } else {
        // A RISC OS 3.5 word: a four-bit type over two 13-bit DPI fields,
        // neither of which a valid word leaves zero.
        let (xdpi, ydpi) = (value >> 1 & 0x1FFF, value >> 14 & 0x1FFF);
        if xdpi == 0 || ydpi == 0 {
            return Err(DecodeError::SpriteInvalidModeWord);
        }
        (
            value >> 27 & 0xF,
            VIDC_ORDER,
            (eig_of_dpi(xdpi), eig_of_dpi(ydpi)),
        )
    };
    Ok(Mode {
        layout: layout_for(sprite_type, order)?,
        // Bit 31 widens the mask from one bit per pixel to eight.
        mask: if value >> 31 & 1 == 1 {
            MaskDepth::Alpha
        } else {
            MaskDepth::Bit
        },
        numbered: false,
        eig,
    })
}

/// A sprite's name as its control block holds it: up to twelve bytes,
/// ending at the first control character or space.
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct SpriteName {
    bytes: [u8; SpriteName::MAX_LEN],
    len: u8,
}

impl SpriteName {
    /// The most bytes a name holds.
    pub const MAX_LEN: usize = 12;

    /// A name for a new sprite: one to twelve printable ASCII characters and
    /// no space, lower-cased as RISC OS stores them. `None` otherwise.
    #[must_use]
    pub fn new(name: &str) -> Option<Self> {
        let raw = name.as_bytes();
        if raw.is_empty() || raw.len() > Self::MAX_LEN || !raw.iter().all(u8::is_ascii_graphic) {
            return None;
        }
        let mut bytes = [0u8; Self::MAX_LEN];
        for (slot, byte) in bytes.iter_mut().zip(raw) {
            *slot = byte.to_ascii_lowercase();
        }
        Some(Self {
            bytes,
            len: u8::try_from(raw.len()).ok()?,
        })
    }

    /// A name exactly as it was read: at most twelve bytes, none of them a
    /// control character or a space, kept in the case it was written in.
    /// `None` otherwise.
    #[must_use]
    pub fn from_bytes(raw: &[u8]) -> Option<Self> {
        if raw.len() > Self::MAX_LEN || raw.iter().any(|&byte| byte <= b' ') {
            return None;
        }
        Some(Self::from_control_block(raw))
    }

    /// The name a control block's twelve name bytes spell.
    pub(crate) fn from_control_block(raw: &[u8]) -> Self {
        let mut bytes = [0u8; Self::MAX_LEN];
        let mut len = 0u8;
        for (slot, &byte) in bytes.iter_mut().zip(raw) {
            if byte <= b' ' {
                break;
            }
            *slot = byte;
            len += 1;
        }
        Self { bytes, len }
    }

    /// The name's bytes, as written to a control block before its padding.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    /// Whether RISC OS would take the two for the same sprite: names match
    /// without regard to case.
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        self.as_bytes().eq_ignore_ascii_case(other.as_bytes())
    }
}

impl fmt::Display for SpriteName {
    /// A byte outside printable ASCII is shown as `?`: a file's name is
    /// untrusted and is never passed through as control text.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &byte in self.as_bytes() {
            let shown = if byte.is_ascii_graphic() {
                char::from(byte)
            } else {
                '?'
            };
            fmt::Write::write_char(f, shown)?;
        }
        Ok(())
    }
}

impl fmt::Debug for SpriteName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SpriteName({self})")
    }
}

/// How a sprite's pixels are laid out, as an editor needs to know it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SpriteLayout {
    /// Palette indices of this depth.
    Indexed(IndexDepth),
    /// Colour held in the pixel itself.
    Direct {
        /// Bits one pixel occupies: 16, 24 or 32.
        bits_per_pixel: u32,
        /// Whether the pixel's top field is alpha.
        alpha_channel: bool,
    },
}

impl SpriteLayout {
    /// Bits one pixel occupies.
    #[must_use]
    pub const fn bits_per_pixel(self) -> u32 {
        match self {
            Self::Indexed(depth) => depth.bits(),
            Self::Direct { bits_per_pixel, .. } => bits_per_pixel,
        }
    }

    /// The least a `width` by `height` sprite of this layout occupies in its
    /// file: its control block and its rows, each a whole number of words.
    /// A mask, a palette and left-hand wastage only add to it; `None` where
    /// the size overflows.
    #[must_use]
    pub fn least_stored_bytes(self, width: u32, height: u32) -> Option<u64> {
        let row_bits = u64::from(width).checked_mul(u64::from(self.bits_per_pixel()))?;
        row_bits
            .div_ceil(32)
            .checked_mul(4)?
            .checked_mul(u64::from(height))?
            .checked_add(u64::from(SPRITE_HEADER_LEN))
    }
}

/// A sprite's mode: a numbered screen mode or a sprite mode word this crate
/// reads.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SpriteMode {
    value: u32,
    mode: Mode,
}

impl SpriteMode {
    /// `value` as a mode, or `None` where it names no layout this crate
    /// reads.
    #[must_use]
    pub fn from_value(value: u32) -> Option<Self> {
        read_mode(value).ok().map(|mode| Self { value, mode })
    }

    /// A mode for a new paletted sprite of `depth` whose pixels have the
    /// eigen factors `eig`: the numbered mode every RISC OS reads when one
    /// matches and no alpha mask is asked for, else a RISC OS 3.5 mode word.
    #[must_use]
    pub fn indexed(depth: IndexDepth, eig: (u8, u8), alpha_mask: bool) -> Self {
        if !alpha_mask {
            let numbered = (0u32..)
                .zip(MODE_BITS.iter().zip(MODE_EIG))
                .find(|&(_, (&bits, mode_eig))| u32::from(bits) == depth.bits() && mode_eig == eig)
                .map(|(number, _)| number);
            if let Some(mode) = numbered.and_then(Self::from_value) {
                return mode;
            }
        }
        let sprite_type = match depth {
            IndexDepth::One => 1,
            IndexDepth::Two => 2,
            IndexDepth::Four => 3,
            IndexDepth::Eight => 4,
        };
        let layout = Layout::Indexed { bits: depth.bits() };
        Self::word_for(sprite_type, layout, eig, alpha_mask)
    }

    /// A mode for a new truecolour sprite: 32-bit pixels in a RISC OS 3.5
    /// mode word, with an eight-bit alpha mask when `alpha_mask`.
    #[must_use]
    pub fn truecolour(eig: (u8, u8), alpha_mask: bool) -> Self {
        let (widths, bytes) = TRUECOLOUR_PIXEL;
        let layout = packed_layout(widths, bytes, VIDC_ORDER);
        Self::word_for(TRUECOLOUR_TYPE, layout, eig, alpha_mask)
    }

    /// A RISC OS 3.5 mode word of `sprite_type`, whose pixels are `layout`,
    /// at the resolutions `eig` states.
    fn word_for(sprite_type: u32, layout: Layout, eig: (u8, u8), alpha_mask: bool) -> Self {
        let eig = (eig.0.min(MAX_EIG), eig.1.min(MAX_EIG));
        let dpi = |eig: u8| OS_UNITS_PER_INCH >> eig;
        let value = u32::from(alpha_mask) << 31
            | sprite_type << 27
            | dpi(eig.1) << 14
            | dpi(eig.0) << 1
            | 1;
        let mask = if alpha_mask {
            MaskDepth::Alpha
        } else {
            MaskDepth::Bit
        };
        let mode = Mode {
            layout,
            mask,
            numbered: false,
            eig,
        };
        Self { value, mode }
    }

    /// The value a control block holds.
    #[must_use]
    pub const fn value(&self) -> u32 {
        self.value
    }

    /// Whether this is a numbered screen mode, which every RISC OS reads.
    #[must_use]
    pub const fn is_numbered(&self) -> bool {
        self.mode.numbered
    }

    /// How the pixels are laid out.
    #[must_use]
    pub fn layout(&self) -> SpriteLayout {
        match self.mode.layout {
            Layout::Indexed { bits } => {
                SpriteLayout::Indexed(IndexDepth::from_bits(bits).unwrap_or(IndexDepth::Eight))
            }
            Layout::Packed { bytes, channels } => SpriteLayout::Direct {
                bits_per_pixel: bytes * 8,
                alpha_channel: channels[3].present(),
            },
        }
    }

    /// The pixel's eigen factors across and down.
    #[must_use]
    pub const fn eig(&self) -> (u8, u8) {
        self.mode.eig
    }

    /// A pixel's shape, width to height, in lowest terms: `(1, 2)` for a
    /// pixel twice as tall as it is wide.
    #[must_use]
    pub fn pixel_aspect(&self) -> (u32, u32) {
        let (x, y) = self.mode.eig;
        let least = x.min(y);
        (1 << (x - least), 1 << (y - least))
    }

    /// Whether the mask is eight bits of alpha rather than binary.
    #[must_use]
    pub fn alpha_mask(&self) -> bool {
        self.mode.mask == MaskDepth::Alpha
    }

    /// This mode with pixels of eigen factors `eig`: the same layout and
    /// mask, the word's resolution restated — a numbered mode moving to the
    /// numbered mode or mode word that says the new shape.
    #[must_use]
    pub fn with_eig(&self, eig: (u8, u8)) -> Self {
        if self.mode.eig == eig {
            return *self;
        }
        if let (true, SpriteLayout::Indexed(depth)) = (self.is_numbered(), self.layout()) {
            return Self::indexed(depth, eig, self.alpha_mask());
        }
        let (value, eig) = if self.value >> 27 & 0xF == 0xF {
            // A RISC OS 5 word holds each factor in two bits.
            let eig = (eig.0.min(RO5_MAX_EIG), eig.1.min(RO5_MAX_EIG));
            let value = self.value & !(0xF << 4) | u32::from(eig.0) << 4 | u32::from(eig.1) << 6;
            (value, eig)
        } else {
            let eig = (eig.0.min(MAX_EIG), eig.1.min(MAX_EIG));
            let dpi = |eig: u8| OS_UNITS_PER_INCH >> eig;
            let value =
                self.value & !(0x1FFF << 14 | 0x1FFF << 1) | dpi(eig.1) << 14 | dpi(eig.0) << 1;
            (value, eig)
        };
        Self {
            value,
            mode: Mode { eig, ..self.mode },
        }
    }

    /// This mode with an eight-bit alpha mask, or a binary one: the same
    /// layout and pixel shape, as a mode word where a numbered mode cannot
    /// carry an alpha mask.
    #[must_use]
    pub fn with_alpha_mask(&self, alpha: bool) -> Self {
        if self.alpha_mask() == alpha {
            return *self;
        }
        if let (true, SpriteLayout::Indexed(depth)) = (self.is_numbered(), self.layout()) {
            return Self::indexed(depth, self.eig(), alpha);
        }
        let value = if alpha {
            self.value | 1 << 31
        } else {
            self.value & !(1 << 31)
        };
        Self::from_value(value).unwrap_or(*self)
    }

    pub(crate) const fn parsed(&self) -> Mode {
        self.mode
    }
}

/// How a paletted sprite's colours are stated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpritePalette {
    /// The file holds no palette; the depth's desktop colours apply.
    Implied,
    /// The palette exactly as the file held it, entry pairs and all, which is
    /// written back unchanged.
    Stored(Vec<u8>),
    /// Write the picture's own colours, one entry for each the depth names.
    Full,
}

impl SpritePalette {
    /// Most bytes a stored palette holds: an entry pair for each colour the
    /// deepest index names.
    pub const MAX_STORED_BYTES: usize = IndexDepth::Eight.colours() * PALETTE_ENTRY_LEN as usize;

    /// Whether `raw` is a palette kept as [`Stored`](Self::Stored): at least
    /// one whole entry pair, and no more than any depth indexes.
    #[must_use]
    pub const fn stores(raw: &[u8]) -> bool {
        !raw.is_empty()
            && raw.len().is_multiple_of(PALETTE_ENTRY_LEN as usize)
            && raw.len() <= Self::MAX_STORED_BYTES
    }
}

/// Whether a sprite kept as its `bytes` can be written back: a whole control
/// block, and a whole number of words, since every sprite after it is laid on
/// its end and would otherwise start off a word boundary.
#[must_use]
pub const fn opaque_sprite_writes_back(bytes: &[u8]) -> bool {
    bytes.len() >= SPRITE_HEADER_LEN as usize && bytes.len().is_multiple_of(4)
}

/// A sprite read into the representation its file stores.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sprite {
    /// Its name.
    pub name: SpriteName,
    /// Its mode.
    pub mode: SpriteMode,
    /// How its colours are stated; [`SpritePalette::Implied`] for a direct
    /// colour sprite.
    pub palette: SpritePalette,
    /// Whether it carries a mask. A paletted sprite's mask is the picture's
    /// own alpha plane; a direct one's is folded into its alpha.
    pub masked: bool,
    /// Its pixels.
    pub picture: Picture,
}

/// A sprite this crate cannot read, held as its exact bytes, control block
/// first, so it can be written back unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpaqueSprite {
    /// Its name.
    pub name: SpriteName,
    /// Why it could not be read.
    pub reason: DecodeError,
    /// Its bytes.
    pub bytes: Vec<u8>,
}

/// One sprite of an area, as the native walk answers it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpriteEntry {
    /// A sprite read into pixels.
    Picture(Sprite),
    /// A sprite kept as its bytes.
    Opaque(OpaqueSprite),
}

/// One sprite's control block, every field already validated against the
/// others.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Header {
    mode: Mode,
    width: u32,
    height: u32,
    /// Bits of the first word of a row that precede its first pixel.
    left_wastage: u32,
    /// Bytes one row of the image occupies, rows being word-aligned.
    stride: u32,
    image_at: u32,
    /// Where the mask sits, or `None` where the sprite has none.
    mask_at: Option<u32>,
    palette_entries: u32,
    /// Bytes this sprite occupies, control block included.
    length: u32,
}

impl Header {
    fn area(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// The pixel width the wastage fields leave, refusing a row that does not
/// hold a whole number of pixels.
///
/// A pixel never straddles a word at a depth that divides 32, so wastage
/// there has to land on a pixel boundary; at 24 bits, which does not, a row
/// simply holds whole pixels and starts at bit zero.
fn pixel_width(words_less_one: u32, left: u32, last: u32, bits: u32) -> Result<u32, DecodeError> {
    if left > 31 || last > 31 {
        return Err(DecodeError::SpriteInvalidWastage);
    }
    let boundary = if 32_u32.is_multiple_of(bits) { bits } else { 8 };
    if !left.is_multiple_of(boundary) {
        return Err(DecodeError::SpriteInvalidWastage);
    }
    let used = (u64::from(words_less_one) * 32 + u64::from(last) + 1)
        .checked_sub(u64::from(left))
        .filter(|used| *used > 0 && used.is_multiple_of(u64::from(bits)))
        .ok_or(DecodeError::SpriteInvalidWastage)?;
    u32::try_from(used / u64::from(bits)).map_err(|_| DecodeError::DimensionsOverflow)
}

/// Read the control block at `at`, and with it the sprite's true geometry.
fn read_header(bytes: &[u8], at: u32) -> Result<Header, DecodeError> {
    let field = |offset: u32| {
        at.checked_add(offset)
            .ok_or(DecodeError::SpriteTruncated)
            .and_then(|at| word(bytes, at))
    };
    let length = field(NEXT_AT)?;
    if length < SPRITE_HEADER_LEN {
        return Err(DecodeError::SpriteBadArea);
    }
    let mode = read_mode(field(MODE_AT)?)?;
    let bits = mode.layout.bits();
    let words_less_one = field(WIDTH_WORDS_AT)?;
    let height = field(HEIGHT_AT)?
        .checked_add(1)
        .ok_or(DecodeError::DimensionsOverflow)?;
    let left_wastage = field(FIRST_BIT_AT)?;
    // Only a numbered mode's rows may begin part way into their first word;
    // every sprite mode word requires the first pixel at bit zero.
    if !mode.numbered && left_wastage != 0 {
        return Err(DecodeError::SpriteInvalidWastage);
    }
    let width = pixel_width(words_less_one, left_wastage, field(LAST_BIT_AT)?, bits)?;
    let image_at = field(IMAGE_AT)?;
    let mask_field = field(MASK_AT)?;
    let palette_bytes = image_at
        .min(mask_field)
        .checked_sub(SPRITE_HEADER_LEN)
        .ok_or(DecodeError::SpriteBadArea)?;
    Ok(Header {
        mode,
        width,
        height,
        left_wastage,
        stride: words_less_one
            .checked_add(1)
            .and_then(|words| words.checked_mul(4))
            .ok_or(DecodeError::DimensionsOverflow)?,
        image_at,
        mask_at: (mask_field != image_at).then_some(mask_field),
        // A palette whose length is not a whole number of entries states
        // nothing this decoder can read, so the depth's own colours stand.
        palette_entries: if palette_bytes.is_multiple_of(PALETTE_ENTRY_LEN) {
            palette_bytes / PALETTE_ENTRY_LEN
        } else {
            0
        },
        length,
    })
}

/// Bytes one row of a mask occupies at `bits` per pixel, word-aligned.
pub(crate) fn mask_stride(width: u32, bits: u32) -> Result<u32, DecodeError> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(bits))
        .and_then(|total| total.checked_add(31))
        .ok_or(DecodeError::DimensionsOverflow)?
        / 32
        * 4;
    u32::try_from(bytes).map_err(|_| DecodeError::DimensionsOverflow)
}

/// The colour every logical colour of an indexed sprite resolves to.
///
/// Resolved once per sprite rather than per pixel: at eight bits the VIDC
/// arrangement makes a pixel's colour a pure function of its byte, so
/// tabulating it leaves the row loop one indexed load.
pub(crate) struct IndexedPalette {
    pub(crate) entries: [Rgba8; 256],
}

impl IndexedPalette {
    /// Resolve `bits`-deep logical colours against the sprite's own palette,
    /// or against the depth's desktop colours where it carries none this
    /// decoder can use.
    pub(crate) fn new(bits: u32, palette: &[u8], entries: u32) -> Self {
        let depth = IndexDepth::from_bits(bits).unwrap_or(IndexDepth::Eight);
        let count = depth.colours();
        let mut table = [[0, 0, 0, u8::MAX]; 256];
        // Fewer entries than the depth needs is the VIDC1 arrangement rather
        // than a short palette: the last sixteen are the hardware registers,
        // and a pixel's top four bits override supremacy bits of the entry
        // its low four select.
        let vidc = entries < u32::try_from(count).unwrap_or(u32::MAX);
        let base = entries.saturating_sub(VIDC_ENTRIES);
        let desktop = desktop_palette(depth);
        for (index, slot) in (0u32..).zip(table.iter_mut().take(count)) {
            let [r, g, b] = match (vidc, entries >= VIDC_ENTRIES) {
                (false, _) => Self::stored(palette, index),
                (true, true) => Self::supremacy(Self::stored(palette, base + (index & 0xF)), index),
                (true, false) => desktop[index as usize],
            };
            *slot = [r, g, b, u8::MAX];
        }
        Self { entries: table }
    }

    /// The colour stored at `index`. An entry is `&BBGGRR00`, so it lands in
    /// memory as an unused byte and then red, green, and blue.
    fn stored(palette: &[u8], index: u32) -> [u8; 3] {
        let at = usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_mul(PALETTE_ENTRY_LEN as usize));
        let Some(entry) = at
            .and_then(|at| palette.get(at..))
            .and_then(<[u8]>::first_chunk::<4>)
        else {
            return [0, 0, 0];
        };
        [entry[1], entry[2], entry[3]]
    }

    /// Override the supremacy bits a 256-colour pixel's top four carry: red
    /// bit three, green bits two and three, and blue bit three.
    fn supremacy(rgb: [u8; 3], pixel: u32) -> [u8; 3] {
        let widen = |nibble: u32| u8::try_from(nibble * 0x11).unwrap_or(u8::MAX);
        [
            widen((u32::from(rgb[0]) >> 4 & 0x7) | (pixel >> 4 & 1) << 3),
            widen((u32::from(rgb[1]) >> 4 & 0x3) | (pixel >> 5 & 1) << 2 | (pixel >> 6 & 1) << 3),
            widen((u32::from(rgb[2]) >> 4 & 0x7) | (pixel >> 7 & 1) << 3),
        ]
    }
}

/// Read the `bits`-wide field at bit offset `bit` of a row, pixels never
/// straddling a byte at any depth this reads.
fn field_at(src: &[u8], bit: u64, bits: u32) -> u32 {
    let mask = u32::MAX >> (u32::BITS - bits);
    let byte = usize::try_from(bit / 8)
        .ok()
        .and_then(|at| src.get(at))
        .copied()
        .unwrap_or(0);
    (u32::from(byte) >> (bit % 8)) & mask
}

/// Extract one row of indexed pixels, least significant pixel leftmost.
fn indices_row(bits: u32, left: u32, src: &[u8], dst: &mut [u8]) {
    let mut bit = u64::from(left);
    for index in dst {
        *index = u8::try_from(field_at(src, bit, bits)).unwrap_or(0);
        bit += u64::from(bits);
    }
}

/// Expand one row of packed pixels.
fn expand_packed(bytes: usize, samplers: &[Sampler; RGBA_BYTES], src: &[u8], dst: &mut [u8]) {
    for (x, pixel) in dst.as_chunks_mut::<RGBA_BYTES>().0.iter_mut().enumerate() {
        let raw = src
            .get(x * bytes..)
            .and_then(|rest| rest.get(..bytes))
            .unwrap_or_default()
            .iter()
            .rev()
            .fold(0u32, |value, &byte| value << 8 | u32::from(byte));
        for (channel, sampler) in pixel.iter_mut().zip(samplers) {
            *channel = sampler.sample(raw);
        }
    }
}

/// A sprite's mask rows, and how one of them is read.
struct MaskRows<'a> {
    rows: &'a [u8],
    bits: u32,
    stride: usize,
    left: u32,
    alpha: bool,
}

impl<'a> MaskRows<'a> {
    fn new(header: &Header, mask: &'a [u8]) -> Result<Self, DecodeError> {
        // Only an old-format mask shares the image's row layout; a new-format
        // one starts at bit zero of rows of its own width. Either way a mask
        // pixel is at most a byte wide, so it never straddles one.
        let (bits, stride, left) = match header.mode.mask {
            MaskDepth::Image => (
                header.mode.layout.bits(),
                header.stride,
                header.left_wastage,
            ),
            MaskDepth::Bit => (1, mask_stride(header.width, 1)?, 0),
            MaskDepth::Alpha => (8, mask_stride(header.width, 8)?, 0),
        };
        let total = stride
            .checked_mul(header.height)
            .ok_or(DecodeError::DimensionsOverflow)?;
        Ok(Self {
            rows: slice_at(mask, 0, 0, total)?,
            bits,
            stride: stride as usize,
            left,
            alpha: header.mode.mask == MaskDepth::Alpha,
        })
    }

    /// Row `y`'s alpha values into `out`: a wide mask's own values, a binary
    /// one's as fully clear or fully solid.
    fn read(&self, y: usize, out: &mut [u8]) {
        let src = self
            .rows
            .get(y * self.stride..(y + 1) * self.stride)
            .unwrap_or_default();
        let mut bit = u64::from(self.left);
        for alpha in out {
            let value = field_at(src, bit, self.bits);
            // Every bit of a binary mask pixel is set or clear together, so
            // any set bit means the pixel is solid.
            *alpha = if self.alpha {
                u8::try_from(value).unwrap_or(u8::MAX)
            } else if value == 0 {
                0
            } else {
                u8::MAX
            };
            bit += u64::from(self.bits);
        }
    }
}

/// A validated sprite and the byte ranges its pixels come from.
struct Located<'a> {
    header: Header,
    sprite: &'a [u8],
    image: &'a [u8],
    mask: Option<MaskRows<'a>>,
}

/// Read and bound the sprite whose control block sits at `at`.
fn locate<'a>(bytes: &'a [u8], at: u32, limits: &DecodeLimits) -> Result<Located<'a>, DecodeError> {
    let header = read_header(bytes, at)?;
    limits.check(header.width, header.height)?;
    // Everything a sprite states an offset for lies inside the sprite, so
    // the whole decode reads within its own extent.
    let sprite = slice_at(bytes, at, 0, header.length)?;
    let image = slice_at(
        sprite,
        0,
        header.image_at,
        header
            .stride
            .checked_mul(header.height)
            .ok_or(DecodeError::DimensionsOverflow)?,
    )?;
    let mask = match header.mask_at {
        Some(mask_at) => {
            let from = usize::try_from(mask_at)
                .ok()
                .and_then(|at| sprite.get(at..))
                .ok_or(DecodeError::SpriteTruncated)?;
            Some(MaskRows::new(&header, from)?)
        }
        None => None,
    };
    Ok(Located {
        header,
        sprite,
        image,
        mask,
    })
}

impl Located<'_> {
    fn pixel_count(&self) -> Result<usize, DecodeError> {
        usize::try_from(self.header.area()).map_err(|_| DecodeError::DimensionsOverflow)
    }

    /// The palette bytes the control block states.
    fn palette_bytes(&self) -> Result<&[u8], DecodeError> {
        slice_at(
            self.sprite,
            0,
            SPRITE_HEADER_LEN,
            self.header
                .palette_entries
                .checked_mul(PALETTE_ENTRY_LEN)
                .ok_or(DecodeError::SpriteTruncated)?,
        )
    }

    fn rows(&self) -> impl Iterator<Item = &[u8]> {
        self.image.chunks_exact(self.header.stride as usize)
    }

    /// The sprite as straight-alpha RGBA8.
    fn rgba(&self) -> Result<RasterImage, DecodeError> {
        let width = self.header.width as usize;
        let row_bytes = width
            .checked_mul(RGBA_BYTES)
            .ok_or(DecodeError::DimensionsOverflow)?;
        let mut out = fallible::filled(
            self.pixel_count()?
                .checked_mul(RGBA_BYTES)
                .ok_or(DecodeError::DimensionsOverflow)?,
            0u8,
        )
        .ok_or(DecodeError::OutOfMemory)?;
        let mut scratch = fallible::filled(width, 0u8).ok_or(DecodeError::OutOfMemory)?;
        match self.header.mode.layout {
            Layout::Indexed { bits } => {
                let palette =
                    IndexedPalette::new(bits, self.palette_bytes()?, self.header.palette_entries);
                for (dst, src) in out.chunks_exact_mut(row_bytes).zip(self.rows()) {
                    indices_row(bits, self.header.left_wastage, src, &mut scratch);
                    for (pixel, &index) in
                        dst.as_chunks_mut::<RGBA_BYTES>().0.iter_mut().zip(&scratch)
                    {
                        *pixel = palette.entries[usize::from(index)];
                    }
                }
            }
            Layout::Packed { bytes, channels } => {
                let samplers = channels.map(Sampler::new);
                for (dst, src) in out.chunks_exact_mut(row_bytes).zip(self.rows()) {
                    expand_packed(bytes as usize, &samplers, src, dst);
                }
            }
        }
        if let Some(mask) = &self.mask {
            for (y, dst) in out.chunks_exact_mut(row_bytes).enumerate() {
                mask.read(y, &mut scratch);
                for (pixel, &alpha) in dst.as_chunks_mut::<RGBA_BYTES>().0.iter_mut().zip(&scratch)
                {
                    pixel[3] = alpha;
                }
            }
        }
        Ok(RasterImage::from_parts(
            self.header.width,
            self.header.height,
            out,
        ))
    }

    /// The sprite in the representation its file stores.
    fn native(&self, name: SpriteName, value: u32) -> Result<Sprite, DecodeError> {
        let (width, height) = (self.header.width, self.header.height);
        let (picture, palette) = match self.header.mode.layout {
            Layout::Indexed { bits } => self.indexed(bits)?,
            Layout::Packed { .. } => (
                Picture::rgba(width, height, self.rgba()?.into_pixels())
                    .map_err(|_| DecodeError::DimensionsOverflow)?,
                SpritePalette::Implied,
            ),
        };
        Ok(Sprite {
            name,
            mode: SpriteMode {
                value,
                mode: self.header.mode,
            },
            palette,
            masked: self.mask.is_some(),
            picture,
        })
    }

    /// A paletted sprite's indices, mask plane and palette.
    fn indexed(&self, bits: u32) -> Result<(Picture, SpritePalette), DecodeError> {
        let depth = IndexDepth::from_bits(bits).ok_or(DecodeError::SpriteUnsupportedType)?;
        let raw = self.palette_bytes()?;
        let resolved = IndexedPalette::new(bits, raw, self.header.palette_entries);
        let palette = fallible::collected(
            depth.colours(),
            resolved.entries.iter().copied().take(depth.colours()),
        )
        .ok_or(DecodeError::OutOfMemory)?;
        let count = self.pixel_count()?;
        let row = self.header.width as usize;
        let mut indices = fallible::filled(count, 0u8).ok_or(DecodeError::OutOfMemory)?;
        for (dst, src) in indices.chunks_exact_mut(row).zip(self.rows()) {
            indices_row(bits, self.header.left_wastage, src, dst);
        }
        let plane = match &self.mask {
            Some(mask) => {
                let mut plane = fallible::filled(count, 0u8).ok_or(DecodeError::OutOfMemory)?;
                for (y, dst) in plane.chunks_exact_mut(row).enumerate() {
                    mask.read(y, dst);
                }
                Some(plane)
            }
            None => None,
        };
        // A palette longer than any depth indexes states nothing a stored
        // copy could keep faithfully, so its resolved colours are what is
        // written back.
        let form = if self.header.palette_entries == 0 {
            SpritePalette::Implied
        } else if SpritePalette::stores(raw) {
            SpritePalette::Stored(
                fallible::collected(raw.len(), raw.iter().copied())
                    .ok_or(DecodeError::OutOfMemory)?,
            )
        } else {
            SpritePalette::Full
        };
        let picture = Picture::indexed(
            self.header.width,
            self.header.height,
            depth,
            palette,
            indices,
            plane,
        )
        .map_err(|_| DecodeError::DimensionsOverflow)?;
        Ok((picture, form))
    }
}

/// Decode the sprite whose control block sits at `at`.
fn decode_sprite(bytes: &[u8], at: u32, limits: &DecodeLimits) -> Result<RasterImage, DecodeError> {
    locate(bytes, at, limits)?.rgba()
}

/// A sprite area's own header: how many sprites it declares, where they
/// start, and where they end.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct AreaHeader {
    count: u32,
    first: u32,
    end: u32,
}

fn read_area(bytes: &[u8]) -> Result<AreaHeader, DecodeError> {
    let count = word(bytes, 0)?;
    if count == 0 {
        return Err(DecodeError::SpriteNoSprites);
    }
    let first = word(bytes, 4)?
        .checked_sub(AREA_OFFSET_BIAS)
        .ok_or(DecodeError::SpriteBadArea)?;
    let end = word(bytes, 8)?
        .checked_sub(AREA_OFFSET_BIAS)
        .ok_or(DecodeError::SpriteBadArea)?;
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    if first < AREA_HEADER_LEN || first > end || end > len {
        return Err(DecodeError::SpriteBadArea);
    }
    Ok(AreaHeader { count, first, end })
}

/// Where the sprite after the one at `at` begins.
///
/// Every step advances by at least a control block and stops at the area's
/// end, so a chain can neither loop nor outrun the file.
fn next_sprite(bytes: &[u8], area: &AreaHeader, at: u32) -> Result<u32, DecodeError> {
    let length = word(
        bytes,
        at.checked_add(NEXT_AT).ok_or(DecodeError::SpriteBadArea)?,
    )?;
    if length < SPRITE_HEADER_LEN {
        return Err(DecodeError::SpriteBadArea);
    }
    let next = at.checked_add(length).ok_or(DecodeError::SpriteBadArea)?;
    if next > area.end {
        return Err(DecodeError::SpriteBadArea);
    }
    Ok(next)
}

/// What a walk of the whole area measured, without decoding a pixel.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Measured {
    largest: u32,
    width: u32,
    height: u32,
}

/// A sprite area, as the pages a walk decodes.
///
/// The control blocks form a chain rather than a table, so the last one
/// located is kept: a sequential walk then costs one step per page instead
/// of re-walking the chain for each.
pub(crate) struct Area {
    header: AreaHeader,
    located: (u32, u32),
}

impl Area {
    /// Validate the area's header and every link of its chain, reading no
    /// control block beyond its length.
    fn chain(bytes: &[u8]) -> Result<Self, DecodeError> {
        let header = read_area(bytes)?;
        let mut at = header.first;
        for _ in 0..header.count {
            if at >= header.end {
                return Err(DecodeError::SpriteBadArea);
            }
            at = next_sprite(bytes, &header, at)?;
        }
        Ok(Self {
            header,
            located: (0, header.first),
        })
    }

    /// Validate the area's chain and measure its sprites, decoding none.
    ///
    /// A control block that will not parse is fatal, because the chain is
    /// what finds the next sprite. A sprite whose *mode* this decoder does
    /// not claim is not: it is passed over here and refused only if it is
    /// asked for, exactly as one page of an icon file is. Where no sprite
    /// could be measured, the first refusal one raised is the answer,
    /// because that names a real reason.
    fn open(bytes: &[u8]) -> Result<(Self, Measured), DecodeError> {
        let area = Self::chain(bytes)?;
        let mut at = area.header.first;
        let mut measured: Option<Measured> = None;
        let mut refusal: Option<DecodeError> = None;
        for index in 0..area.header.count {
            match read_header(bytes, at) {
                Ok(sprite) => {
                    if measured.is_none_or(|best| {
                        sprite.area() > u64::from(best.width) * u64::from(best.height)
                    }) {
                        measured = Some(Measured {
                            largest: index,
                            width: sprite.width,
                            height: sprite.height,
                        });
                    }
                }
                Err(err) => refusal = refusal.or(Some(err)),
            }
            at = next_sprite(bytes, &area.header, at)?;
        }
        let measured = measured.ok_or_else(|| refusal.unwrap_or(DecodeError::SpriteNoSprites))?;
        Ok((area, measured))
    }

    /// Where the control block of the sprite at `index` begins.
    fn locate(&mut self, bytes: &[u8], index: u32) -> Result<u32, DecodeError> {
        let (mut from, mut at) = if index >= self.located.0 {
            self.located
        } else {
            (0, self.header.first)
        };
        while from < index {
            at = next_sprite(bytes, &self.header, at)?;
            from += 1;
        }
        self.located = (index, at);
        Ok(at)
    }
}

impl PageSource for Area {
    fn count(&self) -> u32 {
        self.header.count
    }

    fn decode(
        &mut self,
        bytes: &[u8],
        index: u32,
        limits: &DecodeLimits,
    ) -> Result<RasterImage, DecodeError> {
        let at = self.locate(bytes, index)?;
        decode_sprite(bytes, at, limits)
    }
}

/// Read the geometry of the picture [`decode`] would answer, from control
/// blocks alone.
pub(crate) fn probe(bytes: &[u8]) -> Result<(u32, u32), DecodeError> {
    let (_, measured) = Area::open(bytes)?;
    Ok((measured.width, measured.height))
}

/// Decode the area's largest sprite at its natural size.
///
/// An area holds pictures rather than one picture at several sizes, so "the
/// picture" is a convention here: the largest is the only choice that never
/// silently answers a thumbnail, and it is the one a single-sprite file — a
/// saved screen, a lone icon — holds anyway. A caller that wants the others
/// walks the sequence.
pub(crate) fn decode(bytes: &[u8], limits: &DecodeLimits) -> Result<RasterImage, DecodeError> {
    let (mut area, measured) = Area::open(bytes)?;
    area.decode(bytes, measured.largest, limits)
}

/// Validate the area and measure its sprites, decoding none of them.
pub(crate) fn pages(bytes: &[u8], limits: &DecodeLimits) -> Result<Pages<Area>, DecodeError> {
    let (area, measured) = Area::open(bytes)?;
    Ok(Pages::new(area, limits, measured.width, measured.height))
}

/// A sprite area read one sprite at a time, each in the representation its
/// file stores.
///
/// Only the chain is validated when the area opens, so an area none of whose
/// sprites this crate can read still opens: each such sprite answers as its
/// bytes, and an editor keeps it.
pub struct SpriteAreaReader<B> {
    bytes: B,
    area: Area,
    limits: DecodeLimits,
}

impl<B: AsRef<[u8]>> SpriteAreaReader<B> {
    /// Validate `bytes` as a sprite area's header and chain.
    ///
    /// # Errors
    ///
    /// A refusal of the area header or of any link in the chain.
    pub fn open(bytes: B, limits: &DecodeLimits) -> Result<Self, DecodeError> {
        let area = Area::chain(bytes.as_ref())?;
        Ok(Self {
            bytes,
            area,
            limits: *limits,
        })
    }

    /// How many sprites the area holds.
    #[must_use]
    pub fn count(&self) -> u32 {
        self.area.header.count
    }

    /// The sprite at `index`, read into pixels, or kept as its bytes where
    /// that cannot be done; `None` past the last sprite.
    ///
    /// # Errors
    ///
    /// [`DecodeError::OutOfMemory`] where the allocator refuses a buffer:
    /// that is the machine's condition rather than the sprite's, so it is
    /// not answered as an opaque sprite.
    pub fn sprite(&mut self, index: u32) -> Result<Option<SpriteEntry>, DecodeError> {
        if index >= self.count() {
            return Ok(None);
        }
        let bytes = self.bytes.as_ref();
        let at = self.area.locate(bytes, index)?;
        let length = word(bytes, at)?;
        let name = SpriteName::from_control_block(slice_at(
            bytes,
            at,
            NAME_AT,
            u32::try_from(SpriteName::MAX_LEN).unwrap_or(0),
        )?);
        let read = word(bytes, at.saturating_add(MODE_AT)).and_then(|value| {
            locate(bytes, at, &self.limits).and_then(|located| located.native(name, value))
        });
        match read {
            Ok(sprite) => Ok(Some(SpriteEntry::Picture(sprite))),
            Err(DecodeError::OutOfMemory) => Err(DecodeError::OutOfMemory),
            Err(reason) => {
                let raw = slice_at(bytes, at, 0, length)?;
                let kept = fallible::collected(raw.len(), raw.iter().copied())
                    .ok_or(DecodeError::OutOfMemory)?;
                Ok(Some(SpriteEntry::Opaque(OpaqueSprite {
                    name,
                    reason,
                    bytes: kept,
                })))
            }
        }
    }
}

#[cfg(test)]
#[path = "sprite_tests.rs"]
pub(crate) mod tests;
