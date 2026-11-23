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
use tairix_vdu::palette::default_256;

use super::super::extend::State;
use super::super::format::Pixels;
use super::super::{NO_MEMORY, TABLE_REQUIRED};
use super::source::{Checked, Source, Transparency};
use super::{Destination, PlotFlags};
use crate::colour::{internal, Direct};
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
    /// A plot of `source` on `destination` with `flags` and R7 `table`, and
    /// the destination as it stands after. Making it may run guest code — the
    /// colour mapping, a claimant of `PaletteV`, a module asked about a mode —
    /// which may move the destination, or leave it in another form, onto
    /// which the plot draws nothing.
    ///
    /// # Errors
    ///
    /// As [`Conversion::of`].
    pub(crate) fn of(
        os: &mut Os<'_>,
        source: &Source,
        destination: &Destination,
        flags: PlotFlags,
        table: u32,
    ) -> Result<(Self, Destination), Fail> {
        let format = &destination.format;
        let mapped = mapped(&source.pixels, flags);
        let palettised = if !format.direct() && (blends(source, flags) || mapped) {
            Palettised::now(os)?
        } else {
            None
        };
        let conversion = Conversion::of(os, source, format, palettised.as_ref(), flags, table)?;
        Self::after(os, conversion, palettised, destination)
    }

    /// As [`Converted::of`], a picture's plot through the colour mapping
    /// descriptor at `mapping` where one is given.
    pub(crate) fn of_picture(
        os: &mut Os<'_>,
        source: &Source,
        destination: &Destination,
        mapping: Option<u32>,
    ) -> Result<(Self, Destination), Fail> {
        let format = &destination.format;
        let palettised = if format.direct() {
            None
        } else {
            Palettised::now(os)?
        };
        let conversion = Conversion::of_picture(os, source, format, palettised.as_ref(), mapping)?;
        Self::after(os, conversion, palettised, destination)
    }

    /// `conversion` made for `before`, and the destination as it stands now:
    /// nothing drawn where its pixels are no longer those it was made for.
    fn after(
        os: &mut Os<'_>,
        conversion: Conversion,
        palettised: Option<Palettised>,
        before: &Destination,
    ) -> Result<(Self, Destination), Fail> {
        let now = Destination::now(os)?;
        let conversion = if now.format.same_pixels(&before.format) {
            conversion
        } else {
            Conversion::Nothing
        };
        let converted = Self {
            conversion,
            palettised,
        };
        Ok((converted, now))
    }

    /// Whether the plot draws anything.
    pub(crate) const fn draws(&self) -> bool {
        !matches!(self.conversion, Conversion::Nothing)
    }
}

/// The sprite `checked` turned into `destination`'s pixels with `flags` and
/// R7 `table` before any of its rows are read but a mapped plot's, every one
/// of which the mapping is given: the sprite as read, the conversion, and the
/// destination as the guest code making it left it, which the rows to read
/// are chosen against.
pub(crate) fn converted_first(
    os: &mut Os<'_>,
    checked: &Checked<'_>,
    destination: &Destination,
    flags: PlotFlags,
    table: u32,
) -> Result<(Source, Converted, Destination), Fail> {
    let rows = if mapped(&checked.pixels, flags) {
        0..checked.height
    } else {
        0..0
    };
    let source = checked.coloured(os, rows)?;
    let (converted, destination) = Converted::of(os, &source, destination, flags, table)?;
    Ok((source, converted, destination))
}

/// Whether a plot of a sprite of `pixels` with `flags` takes its colours
/// through the guest's colour mapping: every colour the sprite holds, so
/// every row is read before it.
pub(crate) const fn mapped(pixels: &Pixels, flags: PlotFlags) -> bool {
    flags.mapping() && !pixels.paletted()
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
    /// One direct-colour form's pixel as another's.
    Direct(Direct, Direct),
    /// A palettised destination's colour number from a deep pixel's five-bit
    /// channels through a 32K table.
    ThirtyTwoK(Rc<Vec<u8>>, Direct),
    /// The destination's pixel for each colour a deep sprite or a picture
    /// holds, through a colour mapping, ordered by the source's pixel and
    /// keyed by the bits the second word keeps: a pixel's colour, not its
    /// alpha or spare bits.
    Mapped(Vec<(u32, u32)>, u32),
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
                    let mut pixels = Vec::new();
                    if pixels.try_reserve_exact(entries).is_err() {
                        return Err(os.error(NO_MEMORY));
                    }
                    pixels.extend(
                        source
                            .palette
                            .iter()
                            .map(|&word| direct.pack(Rgb::from_riscos_word(word).to_array())),
                    );
                    return Ok(Self::Words(pixels));
                }
                if flags.wide_table() {
                    return Ok(Self::Words(words(os, table, entries)?));
                }
                return Ok(Self::Words(numbered(os, table, entries, direct)?));
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
        if mapped(&source.pixels, flags) {
            let colours = distinct_colours(os, source)?;
            let mapped = match (palettised, destination.direct()) {
                (Some(palettised), _) => map_colours(os, source, colours, table, |rgb| {
                    palettised.number(Rgb::new(rgb[0], rgb[1], rgb[2]))
                })?,
                (None, true) => map_colours(os, source, colours, table, |rgb| direct.pack(rgb))?,
                (None, false) => return Ok(Self::Nothing),
            };
            return Ok(Self::Mapped(mapped, source.direct.colour_bits()));
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
            Rc::new(os.read_vec(at, CELLS)?),
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
            Self::Direct(from, to) => {
                Some(to.pack(from.unpack_replicated(colour).map(|v| v.to_le_bytes()[0])))
            }
            Self::ThirtyTwoK(table, from) => {
                let [r, g, b] = from.unpack_replicated(colour).map(|v| v.to_le_bytes()[0]);
                table.get(fifteen(Rgb::new(r, g, b))).map(|&b| u32::from(b))
            }
            Self::Mapped(colours, kept) => colours
                .binary_search_by_key(&(colour & kept), |&(from, _)| from)
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
        let descriptor = match (palettised, destination.direct(), mapping) {
            (None, false, _) => return Ok(Self::Nothing),
            (None, true, None) => return Ok(Self::Direct(source.direct, direct)),
            // Each pixel found in the 32K table as it is drawn: no colour
            // needs gathering where no descriptor maps them first.
            (Some(palettised), _, None) => {
                return Ok(Self::ThirtyTwoK(
                    Rc::clone(&palettised.table),
                    source.direct,
                ));
            }
            (_, _, Some(descriptor)) => descriptor,
        };
        let colours = distinct_colours(os, source)?;
        let to = |rgb: [u8; 3]| match palettised {
            Some(palettised) => palettised.number(Rgb::new(rgb[0], rgb[1], rgb[2])),
            None => direct.pack(rgb),
        };
        let colours = map_colours(os, source, colours, descriptor, to)?;
        Ok(Self::Mapped(colours, source.direct.colour_bits()))
    }
}

/// Each colour the rows read of `source` hold, its colour's bits alone, once,
/// in order, with room for its pixel.
fn distinct_colours(os: &mut Os<'_>, source: &Source) -> Result<Vec<(u32, u32)>, Fail> {
    let kept = source.direct.colour_bits();
    let mut colours = Vec::new();
    let rows = source.rows();
    let count = usize::try_from(u64::from(source.width) * u64::from(rows.end - rows.start))
        .unwrap_or(usize::MAX);
    if colours.try_reserve_exact(count).is_err() {
        return Err(os.error(NO_MEMORY));
    }
    for row in rows {
        colours.extend((0..source.width).map(|x| (source.colour(x, row) & kept, 0)));
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
        let [r, g, b] = source
            .direct
            .unpack_replicated(*from)
            .map(|v| v.to_le_bytes()[0]);
        let mut regs = Regs::default();
        regs.r[0] = Rgb::new(r, g, b).riscos_word();
        regs.r[12] = workspace;
        os.call_routine(function, bits, Entry::Supervisor, &mut regs)?;
        *onto = to(Rgb::from_riscos_word(regs.r[0]).to_array());
    }
    Ok(colours)
}

/// The `entries` bytes at `address`, each a colour number and its tint, as
/// `direct`'s pixels: the default 256-colour palette's colours, widened as
/// RISC OS 5 widens a colour number's.
fn numbered(
    os: &mut Os<'_>,
    address: u32,
    entries: usize,
    direct: Direct,
) -> Result<Vec<u32>, Fail> {
    let bytes = os.read_vec(address, entries)?;
    let mut pixels = Vec::new();
    if pixels.try_reserve_exact(entries).is_err() {
        return Err(os.error(NO_MEMORY));
    }
    pixels.extend(bytes.iter().map(|&number| {
        let pixel = internal(u32::from(number >> 2), u32::from(number & 3) << 6);
        direct.pack_nibbles(default_256(pixel).map(|channel| u32::from(channel >> 4)))
    }));
    Ok(pixels)
}

/// `entries` words from `address`.
fn words(os: &mut Os<'_>, address: u32, entries: usize) -> Result<Vec<u32>, Fail> {
    let bytes = os.read_vec(address, 4 * entries)?;
    let mut table = Vec::new();
    if table.try_reserve_exact(entries).is_err() {
        return Err(os.error(NO_MEMORY));
    }
    table.extend(
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&word| u32::from_le_bytes(word)),
    );
    Ok(table)
}
