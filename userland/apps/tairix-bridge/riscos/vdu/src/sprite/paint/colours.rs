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
//!   what it gives is drawn on a deep destination, or found in a palettised
//!   one's 32K table;
//! - a picture, a JPEG decoded, is drawn as a deep sprite is on a deep
//!   destination, and on a palettised one each of its colours is found in
//!   the 32K table.

use alloc::rc::Rc;
use alloc::vec::Vec;

use tairix_colour::Rgb;
use tairix_riscos_kernel::os::{Entry, Fail, Os, Regs};

use super::super::extend::State;
use super::super::{NO_MEMORY, TABLE_REQUIRED};
use super::source::{Source, Transparency};
use super::PlotFlags;
use crate::colour::Direct;
use crate::colourtrans::palettes;
use crate::inverse::{self, fifteen, CELLS, GUARD};
use crate::mode::Format;

/// A palettised destination's colours, each first flash state's, and the
/// 32K table colours are found in, kept by SpriteExtend until they differ.
pub(crate) struct Palettised {
    pub(crate) colours: Vec<Rgb>,
    table: Rc<Vec<u8>>,
}

impl Palettised {
    /// The current destination's, as ColourTrans reads them, a sprite with
    /// no palette its mode's default; none for a direct-colour one.
    pub(crate) fn now(os: &mut Os<'_>) -> Result<Option<Self>, Fail> {
        let against = palettes::destination_colours(os)?;
        let Some((_, colours)) = against.palette() else {
            return Ok(None);
        };
        let palettised = match os.extension::<State>() {
            Some(state) => state
                .inverse
                .table(colours)
                .and_then(|(table, _)| Self::with_table(colours, table)),
            None => Self::of(colours),
        };
        match palettised {
            Some(palettised) => Ok(Some(palettised)),
            None => Err(os.error(NO_MEMORY)),
        }
    }

    /// `colours` with their 32K table, none where there is no room.
    pub(crate) fn of(colours: &[Rgb]) -> Option<Self> {
        Self::with_table(colours, Rc::new(inverse::table(colours)?))
    }

    fn with_table(colours: &[Rgb], table: Rc<Vec<u8>>) -> Option<Self> {
        let mut kept = Vec::new();
        kept.try_reserve_exact(colours.len()).ok()?;
        kept.extend_from_slice(colours);
        Some(Self {
            colours: kept,
            table,
        })
    }

    /// The colour number the 32K table gives `colour`.
    pub(crate) fn number(&self, colour: Rgb) -> u32 {
        self.number_of_cell(fifteen(colour))
    }

    /// The colour number the 32K table gives fifteen-bit colour `cell`.
    pub(crate) fn number_of_cell(&self, cell: usize) -> u32 {
        self.table.get(cell).map_or(0, |&number| u32::from(number))
    }

    /// Colour number `number`'s colour, black past the palette.
    pub(crate) fn colour(&self, number: u32) -> Rgb {
        self.colours
            .get(number as usize)
            .copied()
            .unwrap_or(Rgb::BLACK)
    }
}

/// How a plot turns its pixels into the destination's: the conversion, and
/// a palettised destination's colours where the plot blends or maps onto
/// them.
pub(crate) struct Converted {
    pub(crate) conversion: Conversion,
    pub(crate) palettised: Option<Palettised>,
}

impl Converted {
    /// A plot of `source` on `destination` with `flags` and R7 `table`.
    ///
    /// # Errors
    ///
    /// As [`Conversion::of`].
    pub(crate) fn of(
        os: &mut Os<'_>,
        source: &Source,
        destination: &Format,
        flags: PlotFlags,
        table: u32,
    ) -> Result<Self, Fail> {
        let mapped = flags.mapping() && !source.pixels.paletted();
        let palettised = if !destination.direct() && (blends(source, flags) || mapped) {
            Palettised::now(os)?
        } else {
            None
        };
        let conversion =
            Conversion::of(os, source, destination, palettised.as_ref(), flags, table)?;
        Ok(Self {
            conversion,
            palettised,
        })
    }

    /// A picture's plot on `destination`, through the colour mapping
    /// descriptor at `mapping` where one is given.
    pub(crate) fn of_picture(
        os: &mut Os<'_>,
        source: &Source,
        destination: &Format,
        mapping: Option<u32>,
    ) -> Result<Self, Fail> {
        let palettised = if destination.direct() {
            None
        } else {
            Palettised::now(os)?
        };
        let conversion =
            Conversion::of_picture(os, source, destination, palettised.as_ref(), mapping)?;
        Ok(Self {
            conversion,
            palettised,
        })
    }
}

/// Whether a plot of `source` with `flags` blends: translucent, or alpha
/// asked for from a sprite that has it.
pub(crate) fn blends(source: &Source, flags: PlotFlags) -> bool {
    flags.translucency() != 0 || alpha_blended(source, flags)
}

/// Whether a plot of `source` with `flags` blends by the sprite's alpha.
pub(crate) const fn alpha_blended(source: &Source, flags: PlotFlags) -> bool {
    flags.masked()
        && matches!(
            source.transparency,
            Transparency::Wide | Transparency::Channel(_)
        )
}

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
    /// The destination's pixel for each colour a deep sprite or a picture
    /// holds, through a colour mapping or a 32K table, ordered by the
    /// source's pixel.
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
    /// "A translation table is needed" where it is needed and not given.
    fn of(
        os: &mut Os<'_>,
        source: &Source,
        destination: &Format,
        palettised: Option<&Palettised>,
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
            return Ok(Self::Bytes(os.read_vec(table, entries)?));
        }
        if flags.mapping() {
            let colours = distinct_colours(os, source)?;
            let mapped = match (palettised, destination.direct()) {
                (Some(palettised), _) => map_colours(os, source, colours, table, |rgb| {
                    palettised.number(Rgb::new(rgb[0], rgb[1], rgb[2]))
                })?,
                (None, true) => map_colours(os, source, colours, table, |rgb| direct.pack(rgb))?,
                (None, false) => return Ok(Self::Nothing),
            };
            return Ok(Self::Mapped(mapped));
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
        Ok(Self::ThirtyTwoK(os.read_vec(at, CELLS)?, source.direct))
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
                let [r, g, b] = from.unpack_replicated(colour).map(|v| v.to_le_bytes()[0]);
                table.get(fifteen(Rgb::new(r, g, b))).map(|&b| u32::from(b))
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
    /// on a deep destination, else each found in `palettised`'s 32K table;
    /// through the colour mapping descriptor at `mapping` first where one is
    /// given.
    fn of_picture(
        os: &mut Os<'_>,
        source: &Source,
        destination: &Format,
        palettised: Option<&Palettised>,
        mapping: Option<u32>,
    ) -> Result<Self, Fail> {
        let direct = Direct::of(destination);
        match (palettised, destination.direct()) {
            (None, false) => return Ok(Self::Nothing),
            (None, true) if mapping.is_none() => return Ok(Self::Direct(source.direct, direct)),
            _ => {}
        }
        let colours = distinct_colours(os, source)?;
        let to = |rgb: [u8; 3]| match palettised {
            Some(palettised) => palettised.number(Rgb::new(rgb[0], rgb[1], rgb[2])),
            None => direct.pack(rgb),
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
