//! How SpriteExtend's plots turn a sprite's pixel into the destination's, as
//! RISC OS 5 turns it:
//!
//! - a palettised sprite needs a translation table unless the destination
//!   has its depth and is palettised too; a palettised destination reads its
//!   colour numbers from the table as bytes, wide or not; a deep one reads a
//!   wide table's words as its pixels, and a narrow table's bytes as colours
//!   in their numbers' form, each channel's top two bits and two of tint; to
//!   a deep destination, flag bit 4 lets a palette giving every pixel value
//!   its colour stand for the table;
//! - a deep sprite on a deep destination keeps its colours, each channel
//!   narrowed or its bits repeated to widen it; on a palettised one it needs
//!   a 32K table, a 32768-byte table of colour numbers behind a guard block,
//!   and without one draws nothing;
//! - flag bit 7 makes R7 a colour mapping descriptor for a deep sprite: its
//!   transfer function is called once for each colour the sprite holds, and
//!   what it gives is drawn on a deep destination. A palettised one would
//!   need ColourTrans's colour match, so nothing is drawn there;
//! - a picture, a JPEG decoded, is drawn as a deep sprite is on a deep
//!   destination, and on a palettised one in the palette colour nearest each
//!   of its colours.

use alloc::vec::Vec;

use tairix_riscos_kernel::os::{Entry, Fail, Os, Regs};

use super::super::{NO_MEMORY, TABLE_REQUIRED};
use super::source::Source;
use super::PlotFlags;
use crate::colour::Direct;
use crate::mode::Format;

/// A 32K table's guard block: its two marker words about the table's address.
const GUARD: u32 = u32::from_le_bytes(*b"32K.");
/// The entries of a 32K table: every five-bit red, green and blue.
const THIRTY_TWO_K: usize = 1 << 15;

/// A pixel turned into the destination's.
pub(crate) enum Conversion {
    /// As it is.
    Raw,
    /// A palettised destination's colour number from a table of bytes.
    Bytes(Vec<u8>),
    /// The destination's pixel from a table of words.
    Words(Vec<u32>),
    /// A deep destination's pixel from a table of colours in colour-number
    /// form.
    Numbers(Vec<u8>, Direct),
    /// A deep destination's pixel from the sprite's palette.
    Palette(Vec<[u32; 3]>, Direct),
    /// One direct-colour form's pixel as another's.
    Direct(Direct, Direct),
    /// A palettised destination's colour number from a deep pixel's five-bit
    /// channels through a 32K table.
    ThirtyTwoK(Vec<u8>, Direct),
    /// The destination's pixel for each colour a deep sprite holds, as a
    /// colour mapping or the nearest palette colour gave it, ordered by the
    /// sprite's pixel.
    Mapped(Vec<(u32, u32)>),
    /// Nothing drawn.
    Nothing,
}

impl Conversion {
    /// The conversion a plot of `source` on `destination` with `flags` and
    /// R7 `table` makes.
    ///
    /// # Errors
    ///
    /// "A translation table is needed" where it is needed and not given, or
    /// given for a destination shallower than the sprite.
    pub(crate) fn of(
        os: &mut Os<'_>,
        source: &Source,
        destination: &Format,
        flags: PlotFlags,
        table: u32,
    ) -> Result<Self, Fail> {
        let (from, to) = (source.pixels.log2_bpp, destination.log2_bpp());
        let direct = Direct::of(destination);
        if source.pixels.paletted() {
            let entries = 1usize << source.pixels.bits();
            if destination.direct() {
                if table == 0 {
                    return Err(os.error(TABLE_REQUIRED));
                }
                if flags.table_ignorable() && source.palette.len() == entries {
                    let colours = source.palette.iter().map(|&word| rgb(word)).collect();
                    return Ok(Self::Palette(colours, direct));
                }
                if flags.wide_table() {
                    return Ok(Self::Words(words(os, table, entries)?));
                }
                return Ok(Self::Numbers(os.read_vec(table, entries)?, direct));
            }
            if table == 0 {
                return if from == to {
                    Ok(Self::Raw)
                } else {
                    Err(os.error(TABLE_REQUIRED))
                };
            }
            if to < from {
                return Err(os.error(TABLE_REQUIRED));
            }
            return Ok(Self::Bytes(os.read_vec(table, entries)?));
        }
        if flags.mapping() {
            if !destination.direct() {
                return Ok(Self::Nothing);
            }
            let colours = distinct_colours(os, source)?;
            return Ok(Self::Mapped(map_colours(
                os,
                source,
                colours,
                table,
                |rgb| direct.pack(rgb),
            )?));
        }
        if destination.direct() {
            return Ok(Self::Direct(source.direct, direct));
        }
        if table == 0
            || os.read_word(table)? != GUARD
            || os.read_word(table.wrapping_add(8))? != GUARD
        {
            return Ok(Self::Nothing);
        }
        let at = os.read_word(table.wrapping_add(4))?;
        Ok(Self::ThirtyTwoK(
            os.read_vec(at, THIRTY_TWO_K)?,
            source.direct,
        ))
    }

    /// The destination's pixel for colour `colour`, if one is drawn.
    pub(crate) fn pixel(&self, colour: u32) -> Option<u32> {
        let index = colour as usize;
        match self {
            Self::Raw => Some(colour),
            Self::Bytes(table) => table.get(index).map(|&b| u32::from(b)),
            Self::Words(table) => table.get(index).copied(),
            Self::Numbers(table, direct) => {
                table.get(index).map(|&e| direct.pack_nibbles(numbers(e)))
            }
            Self::Palette(colours, direct) => colours
                .get(index)
                .map(|&c| direct.pack(c.map(|v| v.to_le_bytes()[0]))),
            Self::Direct(from, to) => {
                Some(to.pack(from.unpack_replicated(colour).map(|v| v.to_le_bytes()[0])))
            }
            Self::ThirtyTwoK(table, from) => {
                let [r, g, b] = from.unpack_replicated(colour);
                let index = (r >> 3) | (g >> 3) << 5 | (b >> 3) << 10;
                table.get(index as usize).map(|&b| u32::from(b))
            }
            Self::Mapped(colours) => colours
                .binary_search_by_key(&colour, |&(from, _)| from)
                .ok()
                .and_then(|at| colours.get(at))
                .map(|&(_, to)| to),
            Self::Nothing => None,
        }
    }
}

impl Conversion {
    /// How a picture's pixels become the destination's: as a deep sprite's
    /// on a deep destination, else each the palette colour `palette` holds
    /// nearest it; through the colour mapping descriptor at `mapping` first
    /// where one is given.
    pub(crate) fn of_picture(
        os: &mut Os<'_>,
        source: &Source,
        destination: &Format,
        palette: &[[u8; 3]],
        mapping: Option<u32>,
    ) -> Result<Self, Fail> {
        let direct = Direct::of(destination);
        if destination.direct() && mapping.is_none() {
            return Ok(Self::Direct(source.direct, direct));
        }
        let colours = distinct_colours(os, source)?;
        let to = |rgb: [u8; 3]| {
            if destination.direct() {
                direct.pack(rgb)
            } else {
                nearest(palette, rgb)
            }
        };
        let colours = match mapping {
            Some(descriptor) => map_colours(os, source, colours, descriptor, to)?,
            None => colours
                .into_iter()
                .map(|(from, _)| {
                    (
                        from,
                        to(source
                            .direct
                            .unpack_replicated(from)
                            .map(|v| v.to_le_bytes()[0])),
                    )
                })
                .collect(),
        };
        Ok(Self::Mapped(colours))
    }
}

/// The index of the colour of `palette` nearest `rgb`, the first of any
/// equally near.
pub(crate) fn nearest(palette: &[[u8; 3]], rgb: [u8; 3]) -> u32 {
    let distance = |entry: &[u8; 3]| -> u32 {
        entry
            .iter()
            .zip(rgb)
            .map(|(&a, b)| u32::from(a.abs_diff(b)).pow(2))
            .sum()
    };
    let (mut best, mut least) = (0, u32::MAX);
    for (index, entry) in (0u32..).zip(palette) {
        let d = distance(entry);
        if d < least {
            (best, least) = (index, d);
        }
    }
    best
}

/// The groups of near colours [`NearestKept`] keeps an answer for: each
/// channel's top four bits.
const KEPT: usize = 1 << 12;

/// [`nearest`] for one palette, its last answer in each group of near
/// colours kept, so a blend of a few colours searches the palette once for
/// each rather than once a pixel.
pub(crate) struct NearestKept<'a> {
    palette: &'a [[u8; 3]],
    kept: Vec<Option<([u8; 3], u8)>>,
}

impl<'a> NearestKept<'a> {
    /// The palette's answers, none kept yet; no room is taken for an empty
    /// palette, which no plot searches. None where the room is refused.
    pub(crate) fn of(palette: &'a [[u8; 3]]) -> Option<Self> {
        let mut kept = Vec::new();
        if !palette.is_empty() {
            kept.try_reserve_exact(KEPT).ok()?;
            kept.resize(KEPT, None);
        }
        Some(Self { palette, kept })
    }

    /// The palette.
    pub(crate) const fn palette(&self) -> &'a [[u8; 3]] {
        self.palette
    }

    /// The index of the palette's colour nearest `rgb`, as [`nearest`]
    /// gives it.
    pub(crate) fn index(&mut self, rgb: [u8; 3]) -> u32 {
        let group = usize::from(rgb[0] >> 4) << 8
            | usize::from(rgb[1] >> 4) << 4
            | usize::from(rgb[2] >> 4);
        let Some(slot) = self.kept.get_mut(group) else {
            return nearest(self.palette, rgb);
        };
        if let Some((kept, index)) = *slot {
            if kept == rgb {
                return u32::from(index);
            }
        }
        let index = nearest(self.palette, rgb);
        *slot = u8::try_from(index).ok().map(|index| (rgb, index));
        index
    }
}

/// Each colour `source` holds, once, in order, with room for its pixel.
fn distinct_colours(os: &mut Os<'_>, source: &Source) -> Result<Vec<(u32, u32)>, Fail> {
    let mut colours = Vec::new();
    let count =
        usize::try_from(u64::from(source.width) * u64::from(source.height)).unwrap_or(usize::MAX);
    if colours.try_reserve_exact(count).is_err() {
        return Err(os.error(NO_MEMORY));
    }
    for row in 0..source.height {
        colours.extend((0..source.width).map(|x| (source.colour(x, row), 0)));
    }
    colours.sort_unstable_by_key(|&(from, _)| from);
    colours.dedup_by_key(|&mut (from, _)| from);
    Ok(colours)
}

/// `colours`, each through the transfer function of the colour mapping
/// descriptor at `descriptor`, as `to` gives the result's pixel.
fn map_colours(
    os: &mut Os<'_>,
    source: &Source,
    mut colours: Vec<(u32, u32)>,
    descriptor: u32,
    to: impl Fn([u8; 3]) -> u32,
) -> Result<Vec<(u32, u32)>, Fail> {
    let workspace = os.read_word(descriptor)?;
    let function = os.read_word(descriptor.wrapping_add(4))?;
    let bits = os.caller_bits();
    for (from, onto) in &mut colours {
        let [r, g, b] = source.direct.unpack_replicated(*from);
        let mut regs = Regs::default();
        regs.r[0] = (r << 8) | (g << 16) | (b << 24);
        regs.r[12] = workspace;
        os.call_routine(function, bits, Entry::Supervisor, &mut regs)?;
        *onto = to(rgb(regs.r[0]).map(|v| v.to_le_bytes()[0]));
    }
    Ok(colours)
}

/// The four-bit red, green and blue a colour-number-form byte names: each
/// channel's top two bits above the shared two of tint.
const fn numbers(e: u8) -> [u32; 3] {
    let tint = (e & 3) as u32;
    [
        e as u32 & 0xF,
        ((e >> 4) & 3) as u32 * 4 + tint,
        ((e >> 6) & 3) as u32 * 4 + tint,
    ]
}

/// A palette word's red, green and blue.
const fn rgb(word: u32) -> [u32; 3] {
    [(word >> 8) & 0xFF, (word >> 16) & 0xFF, word >> 24]
}

/// `entries` words from `address`.
fn words(os: &mut Os<'_>, address: u32, entries: usize) -> Result<Vec<u32>, Fail> {
    let mut table = Vec::new();
    if table.try_reserve_exact(entries).is_err() {
        return Err(os.error(NO_MEMORY));
    }
    for i in 0..entries {
        table.push(os.read_word(address.wrapping_add(4 * u32::try_from(i).unwrap_or(0)))?);
    }
    Ok(table)
}
