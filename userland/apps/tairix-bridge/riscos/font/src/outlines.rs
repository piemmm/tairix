//! A font's outlines file, `Outlines` or `Outlines<n>`, and its bitmap
//! files, `f9999x9999` at four bits a pixel and `b9999x9999` at one, read as
//! the PRM lays them out: a header, a table, and the characters in chunks of
//! 32. An outline is moves, lines and curves in design units, filled, then
//! stroke paths drawn thin, a composite character made of others; a bitmap
//! is its pixels from its bottom row up, a one-bit one perhaps packed into
//! runs. An outlines file shorter than 256 bytes names another font whose
//! outlines these are. A file is the guest's: every offset and length is
//! checked, a composite's inclusions are bounded in depth and number, and
//! what does not hold together is refused. A file's bytes are zeroed when
//! let go.

use alloc::vec::Vec;
use core::mem::size_of;

use crate::glyphs::{Bitmap, Refused};
use crate::metrics;
use crate::units;
use tairix_riscos_vdu::render::WHOLE;

/// The header's fields.
const MAGIC: &[u8; 4] = b"FONT";
const BPP: usize = 4;
const VERSION: usize = 5;
/// An outlines file's design size, a bitmap file's flags.
const DESIGN_SIZE: usize = 6;
const BITMAP_FLAGS: usize = 6;
const BBOX: usize = 8;
const CHUNK_OFFSETS: usize = 16;
const CHUNK_TABLE: usize = 16;
const CHUNK_COUNT: usize = 20;
const SCAFFOLD_ENTRIES: usize = 24;
const SCAFFOLD_FLAGS: usize = 28;
/// The scaffold flag saying the outlines are not to be anti-aliased.
const MONOCHROME: u32 = 1 << 1;
/// Where the table starts.
const TABLE: usize = 52;
/// The versions an outlines file may be, and the ones past which a chunk
/// opens with a flag word and its offsets are from its index.
const FIRST_VERSION: u8 = 4;
const LAST_VERSION: u8 = 8;
const FLAGGED_CHUNKS: u8 = 7;
/// The chunks a file before version 8 has.
const OLD_CHUNKS: usize = 8;
/// The characters a chunk holds.
pub(crate) const CHUNK: usize = 32;
/// A file shorter than this names another font.
pub(crate) const REDIRECT_BELOW: usize = 256;
/// A chunk flag word's bits: subpixel placement across and up, and the
/// bit that must be set.
const CHUNK_HORIZONTAL: u32 = 1;
const CHUNK_VERTICAL: u32 = 1 << 1;
const CHUNK_MUST: u32 = 1 << 31;
/// The sub-placements a chunk holds a character at along an axis with
/// subpixel placement.
pub(crate) const PLACEMENTS: usize = 4;
/// A character's flag bits.
mod character {
    pub(super) const WIDE: u8 = 1;
    pub(super) const ONE_BIT: u8 = 1 << 1;
    pub(super) const BLACK_FIRST: u8 = 1 << 2;
    pub(super) const OUTLINE: u8 = 1 << 3;
    pub(super) const BASE: u8 = 1 << 4;
    pub(super) const ACCENT: u8 = 1 << 5;
    pub(super) const WIDE_CODES: u8 = 1 << 6;
    /// A bitmap's packing value, nought for none.
    pub(super) const PACKING_SHIFT: u32 = 4;
}
/// A bitmap file's table: its size, then its sizes and resolutions.
const BITMAP_TABLE: u16 = 10;
/// A segment's type, in its first byte's low bits.
const SEGMENT_TYPE: u8 = 3;
const TERMINATOR: u8 = 0;
const MOVE: u8 = 1;
const LINE: u8 = 2;
/// A terminator's bits: stroke paths follow, and inclusions follow.
const STROKES_FOLLOW: u8 = 1 << 2;
const INCLUSIONS_FOLLOW: u8 = 1 << 3;
/// How deep composites nest, and how many inclusions one character draws.
const COMPOSITE_DEPTH: usize = 4;
const INCLUSIONS_MOST: usize = 64;
/// The most elements a character's fill or strokes may hold, its
/// inclusions' with it: far past any glyph a font draws.
const ELEMENTS_MOST: usize = 1 << 16;
/// The most chunks a file's characters fill: the 65536 glyphs a font's
/// metrics can index.
const CHUNKS_MOST: usize = (1 << 16) / CHUNK;

/// A point in design units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    /// Across.
    pub x: i32,
    /// Up.
    pub y: i32,
}

impl Point {
    /// The point `x` across and `y` up.
    #[must_use]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    const fn offset(self, by: Self) -> Self {
        Self {
            x: self.x + by.x,
            y: self.y + by.y,
        }
    }
}

/// A move, line or curve, in design units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Element {
    /// A move to a point, starting a path.
    Move(Point),
    /// A line to a point.
    Line(Point),
    /// A cubic curve through two control points to a point.
    Curve(Point, Point, Point),
}

impl Element {
    fn offset(self, by: Point) -> Self {
        match self {
            Self::Move(p) => Self::Move(p.offset(by)),
            Self::Line(p) => Self::Line(p.offset(by)),
            Self::Curve(a, b, c) => Self::Curve(a.offset(by), b.offset(by), c.offset(by)),
        }
    }
}

/// A character's outline: what it fills, what it strokes thin, and the box
/// the file gives it, its inclusions' with it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Glyph {
    pub(crate) fill: Vec<Element>,
    pub(crate) strokes: Vec<Element>,
    /// `x0, y0, x1, y1` in design units.
    pub(crate) bbox: Option<[i32; 4]>,
}

/// An outlines file that does not hold together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Malformed;

/// Why a bitmap's character was not decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Undecoded {
    /// Its data does not hold together.
    Malformed,
    /// The memory for its levels was refused.
    Refused,
}

impl From<Malformed> for Undecoded {
    fn from(_: Malformed) -> Self {
        Self::Malformed
    }
}

/// What a font file is.
#[derive(Debug)]
pub(crate) enum File {
    /// Outlines.
    Outlines(Outlines),
    /// Bitmaps of one size.
    Bitmaps(Bitmaps),
    /// The identifier of the font whose outlines to use, to its control
    /// character.
    Redirect(Vec<u8>),
}

impl File {
    /// The file `bytes`.
    pub(crate) fn read(bytes: Vec<u8>) -> Result<Self, Malformed> {
        if bytes.len() < REDIRECT_BELOW {
            let end = bytes.iter().position(|&b| b < b' ').unwrap_or(bytes.len());
            let mut name = bytes;
            name.truncate(end);
            return Ok(Self::Redirect(name));
        }
        if bytes.get(..MAGIC.len()) != Some(MAGIC.as_slice()) {
            return Err(Malformed);
        }
        match byte(&bytes, BPP)? {
            0 => Outlines::read(bytes).map(Self::Outlines),
            1 | 4 => Bitmaps::read(bytes).map(Self::Bitmaps),
            _ => Err(Malformed),
        }
    }
}

/// A file's characters in chunks of 32: its bytes, its version, and each
/// chunk's start and end in it.
#[derive(Debug)]
struct Chunks {
    bytes: Vec<u8>,
    version: u8,
    spans: Vec<(usize, usize)>,
}

impl Chunks {
    /// The chunks `bytes`' header lays out, its version checked.
    fn read(bytes: Vec<u8>) -> Result<Self, Malformed> {
        let version = byte(&bytes, VERSION)?;
        if !(FIRST_VERSION..=LAST_VERSION).contains(&version) {
            return Err(Malformed);
        }
        let (offsets_at, count) = if version < LAST_VERSION {
            (CHUNK_OFFSETS, OLD_CHUNKS)
        } else {
            let at = usize::try_from(u32_at(&bytes, CHUNK_TABLE)?).map_err(|_| Malformed)?;
            let count = usize::try_from(u32_at(&bytes, CHUNK_COUNT)?).map_err(|_| Malformed)?;
            (at, count)
        };
        if count > CHUNKS_MOST {
            return Err(Malformed);
        }
        let words = count.checked_add(1).ok_or(Malformed)?;
        if offsets_at
            .checked_add(words.checked_mul(4).ok_or(Malformed)?)
            .is_none_or(|end| end > bytes.len())
        {
            return Err(Malformed);
        }
        let mut spans = Vec::new();
        spans.try_reserve(count).map_err(|_| Malformed)?;
        for chunk in 0..count {
            let start =
                usize::try_from(u32_at(&bytes, offsets_at + 4 * chunk)?).map_err(|_| Malformed)?;
            let end = usize::try_from(u32_at(&bytes, offsets_at + 4 * chunk + 4)?)
                .map_err(|_| Malformed)?;
            if start > end || end > bytes.len() || (start != end && start < TABLE) {
                return Err(Malformed);
            }
            spans.push((start, end));
        }
        Ok(Self {
            bytes,
            version,
            spans,
        })
    }

    /// How many characters they have room for.
    fn slots(&self) -> usize {
        self.spans.len() * CHUNK
    }

    /// The bytes of character `code`'s data at sub-placement `placement`,
    /// across then up, and its chunk's end; none for a character not
    /// defined. A chunk holding sub-placements has each placement's 32
    /// characters together, across the outermost.
    fn data(
        &self,
        code: u32,
        placement: (usize, usize),
    ) -> Result<Option<(usize, usize)>, Malformed> {
        let Ok(code) = usize::try_from(code) else {
            return Ok(None);
        };
        let Some(&(start, end)) = self.spans.get(code / CHUNK) else {
            return Ok(None);
        };
        if start == end {
            return Ok(None);
        }
        let chunk = &self.bytes[start..end];
        let (index, (across, up)) = if self.version >= FLAGGED_CHUNKS {
            let flags = u32_at(chunk, 0)?;
            if flags & CHUNK_MUST == 0 {
                return Err(Malformed);
            }
            let along = |bit: u32| if flags & bit != 0 { PLACEMENTS } else { 1 };
            (4, (along(CHUNK_HORIZONTAL), along(CHUNK_VERTICAL)))
        } else {
            (0, (1, 1))
        };
        let variant = placement.0.min(across - 1) * up + placement.1.min(up - 1);
        let entry = index + 4 * (variant * CHUNK + code % CHUNK);
        let offset = usize::try_from(u32_at(chunk, entry)?).map_err(|_| Malformed)?;
        if offset == 0 {
            return Ok(None);
        }
        let base = if self.version >= FLAGGED_CHUNKS {
            index
        } else {
            0
        };
        let at = base.checked_add(offset).ok_or(Malformed)?;
        if at >= chunk.len() {
            return Err(Malformed);
        }
        Ok(Some((start + at, end)))
    }

    /// Roughly what they hold in memory.
    fn bytes(&self) -> usize {
        size_of::<Self>() + self.bytes.len() + self.spans.len() * size_of::<(usize, usize)>()
    }
}

impl Drop for Chunks {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}

/// An outlines file's characters.
#[derive(Debug)]
pub(crate) struct Outlines {
    chunks: Chunks,
    /// Design units an em.
    pub(crate) design_size: u16,
    /// The font's box in design units: `x0, y0, x1, y1`.
    pub(crate) bbox: [i32; 4],
    /// Whether its outlines are never to be anti-aliased.
    pub(crate) monochrome: bool,
}

/// The header's box: `x0, y0, x1, y1`.
fn header_box(bytes: &[u8]) -> Result<[i32; 4], Malformed> {
    let x0 = i32::from(i16_at(bytes, BBOX)?);
    let y0 = i32::from(i16_at(bytes, BBOX + 2)?);
    let width = i32::from(u16_at(bytes, BBOX + 4)?);
    let height = i32::from(u16_at(bytes, BBOX + 6)?);
    Ok([x0, y0, x0 + width, y0 + height])
}

impl Outlines {
    fn read(bytes: Vec<u8>) -> Result<Self, Malformed> {
        let design_size = u16_at(&bytes, DESIGN_SIZE)?;
        if design_size == 0 {
            return Err(Malformed);
        }
        let bbox = header_box(&bytes)?;
        let chunks = Chunks::read(bytes)?;
        let monochrome = if chunks.version >= LAST_VERSION {
            u32_at(&chunks.bytes, SCAFFOLD_ENTRIES)?;
            u32_at(&chunks.bytes, SCAFFOLD_FLAGS)? & MONOCHROME != 0
        } else {
            false
        };
        Ok(Self {
            chunks,
            design_size,
            bbox,
            monochrome,
        })
    }

    /// How many characters its chunks have room for.
    pub(crate) fn slots(&self) -> usize {
        self.chunks.slots()
    }

    /// `bbox`, `x0, y0, x1, y1` in design units, in thousandths of an em,
    /// outward.
    pub(crate) fn millems(&self, [x0, y0, x1, y1]: [i32; 4]) -> metrics::Box {
        let design = i64::from(self.design_size);
        let scale = |v: i32, up: bool| {
            let scaled = i64::from(v) * i64::from(units::MILLEMS);
            let quotient = if up {
                scaled.div_euclid(design) + i64::from(scaled.rem_euclid(design) != 0)
            } else {
                scaled.div_euclid(design)
            };
            i32::try_from(quotient).unwrap_or(if quotient < 0 { i32::MIN } else { i32::MAX })
        };
        metrics::Box {
            x0: scale(x0, false),
            y0: scale(y0, false),
            x1: scale(x1, true),
            y1: scale(y1, true),
        }
    }

    /// Roughly what it holds in memory.
    pub(crate) fn bytes(&self) -> usize {
        size_of::<Self>() + self.chunks.bytes()
    }

    /// Character `code`'s glyph; none for a character not defined.
    pub(crate) fn glyph(&self, code: u32) -> Result<Option<Glyph>, Malformed> {
        let mut glyph = Glyph::default();
        let mut inclusions = 0;
        let found = self.draw(code, Point::new(0, 0), 0, &mut inclusions, &mut glyph)?;
        Ok(found.then_some(glyph))
    }

    /// Character `code` drawn into `glyph` at `offset`: whether it is
    /// defined.
    fn draw(
        &self,
        code: u32,
        offset: Point,
        depth: usize,
        inclusions: &mut usize,
        glyph: &mut Glyph,
    ) -> Result<bool, Malformed> {
        if depth > COMPOSITE_DEPTH {
            return Err(Malformed);
        }
        let Some((start, end)) = self.chunks.data(code, (0, 0))? else {
            return Ok(false);
        };
        let mut reader = Reader {
            bytes: &self.chunks.bytes[..end],
            at: start,
            wide: false,
        };
        let flags = reader.byte()?;
        if flags & character::OUTLINE == 0 || flags & character::WIDE_CODES != 0 {
            return Err(Malformed);
        }
        reader.wide = flags & character::WIDE != 0;
        if flags & character::BASE != 0 {
            let base = u32::from(reader.byte()?);
            self.include(base, offset, depth, inclusions, glyph)?;
        }
        if flags & character::ACCENT != 0 {
            let accent = u32::from(reader.byte()?);
            let by = reader.point()?;
            self.include(accent, offset.offset(by), depth, inclusions, glyph)?;
        }
        if flags & character::BASE != 0 {
            return Ok(true);
        }
        let corner = reader.point()?.offset(offset);
        let size = reader.point()?;
        let own = [corner.x, corner.y, corner.x + size.x, corner.y + size.y];
        glyph.bbox = Some(glyph.bbox.map_or(own, |[x0, y0, x1, y1]| {
            [
                x0.min(own[0]),
                y0.min(own[1]),
                x1.max(own[2]),
                y1.max(own[3]),
            ]
        }));
        let terminator = reader.path(offset, &mut glyph.fill)?;
        let terminator = if terminator & STROKES_FOLLOW != 0 {
            reader.path(offset, &mut glyph.strokes)?
        } else {
            terminator
        };
        if terminator & INCLUSIONS_FOLLOW != 0 {
            loop {
                let included = u32::from(reader.byte()?);
                if included == 0 {
                    break;
                }
                let by = reader.point()?;
                self.include(included, offset.offset(by), depth, inclusions, glyph)?;
            }
        }
        Ok(true)
    }

    /// Character `code` drawn as part of another, counted against the
    /// inclusions one character may draw.
    fn include(
        &self,
        code: u32,
        offset: Point,
        depth: usize,
        inclusions: &mut usize,
        glyph: &mut Glyph,
    ) -> Result<(), Malformed> {
        *inclusions += 1;
        if *inclusions > INCLUSIONS_MOST {
            return Err(Malformed);
        }
        self.draw(code, offset, depth + 1, inclusions, glyph)?;
        Ok(())
    }
}

/// How many bits a bitmap file's pixels take.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Depth {
    One,
    Four,
}

/// A bitmap file's characters, all at one size.
#[derive(Debug)]
pub(crate) struct Bitmaps {
    chunks: Chunks,
    pub(crate) depth: Depth,
    /// Whether it holds each character at four sub-placements across, and
    /// up.
    pub(crate) placements: (bool, bool),
    /// Its size in sixteenths of a point, across and up.
    pub(crate) points: (u16, u16),
    /// Its dots an inch, across and up.
    pub(crate) resolution: (u16, u16),
}

/// A bitmap file's flag bits: sub-placement across and up, and the chunks'
/// flag words, which a file of version 7 or later has and an earlier one
/// has not.
mod bitmap_flag {
    pub(super) const ACROSS: u16 = 1;
    pub(super) const UP: u16 = 1 << 1;
    pub(super) const FLAGGED: u16 = 1 << 6;
    pub(super) const KNOWN: u16 = ACROSS | UP | FLAGGED;
}

impl Bitmaps {
    fn read(bytes: Vec<u8>) -> Result<Self, Malformed> {
        let depth = match byte(&bytes, BPP)? {
            1 => Depth::One,
            4 => Depth::Four,
            _ => return Err(Malformed),
        };
        let flags = u16_at(&bytes, BITMAP_FLAGS)?;
        if flags & !bitmap_flag::KNOWN != 0 || u16_at(&bytes, TABLE)? != BITMAP_TABLE {
            return Err(Malformed);
        }
        let table = |at: usize| u16_at(&bytes, TABLE + 2 + 2 * at);
        let (points, resolution) = ((table(0)?, table(2)?), (table(1)?, table(3)?));
        let chunks = Chunks::read(bytes)?;
        if (flags & bitmap_flag::FLAGGED != 0) != (chunks.version >= FLAGGED_CHUNKS) {
            return Err(Malformed);
        }
        Ok(Self {
            chunks,
            depth,
            placements: (
                flags & bitmap_flag::ACROSS != 0,
                flags & bitmap_flag::UP != 0,
            ),
            points,
            resolution,
        })
    }

    /// Roughly what it holds in memory.
    pub(crate) fn bytes(&self) -> usize {
        size_of::<Self>() + self.chunks.bytes()
    }

    /// Character `code`'s pixels at sub-placement `placement`, across then
    /// up, as levels; none for a character not defined, one with no pixels,
    /// or one whose data does not hold together.
    pub(crate) fn glyph(
        &self,
        code: u32,
        placement: (usize, usize),
    ) -> Result<Option<Bitmap>, Refused> {
        match self.decoded(code, placement) {
            Ok(bitmap) => Ok(bitmap),
            Err(Undecoded::Malformed) => Ok(None),
            Err(Undecoded::Refused) => Err(Refused),
        }
    }

    fn decoded(&self, code: u32, placement: (usize, usize)) -> Result<Option<Bitmap>, Undecoded> {
        let Some((start, end)) = self.chunks.data(code, placement)? else {
            return Ok(None);
        };
        let mut reader = Reader {
            bytes: &self.chunks.bytes[..end],
            at: start,
            wide: false,
        };
        let flags = reader.byte()?;
        if flags & character::OUTLINE != 0 {
            return Err(Undecoded::Malformed);
        }
        reader.wide = flags & character::WIDE != 0;
        let corner = reader.point()?;
        let size = reader.point()?;
        let (Ok(width), Ok(height)) = (usize::try_from(size.x), usize::try_from(size.y)) else {
            return Err(Undecoded::Malformed);
        };
        if width == 0 || height == 0 {
            return Ok(None);
        }
        let mut levels = Vec::new();
        let pixels = width.checked_mul(height).ok_or(Malformed)?;
        levels
            .try_reserve_exact(pixels)
            .map_err(|_| Undecoded::Refused)?;
        levels.resize(pixels, 0);
        let packing = u32::from(flags) >> character::PACKING_SHIFT;
        let data = &reader.bytes[reader.at..];
        match (flags & character::ONE_BIT != 0, packing) {
            (false, 0) => four_bit(data, width, &mut levels)?,
            (true, 0) => one_bit(data, width, &mut levels)?,
            (true, packing) => packed(
                data,
                packing,
                flags & character::BLACK_FIRST != 0,
                width,
                &mut levels,
            )?,
            (false, _) => return Err(Undecoded::Malformed),
        }
        Ok(Some(Bitmap {
            left: corner.x,
            top: corner.y + size.y - 1,
            width,
            levels,
        }))
    }
}

/// `data`'s pixels at four bits each, the low nibble first, rows bottom to
/// top, into `levels`, rows top to bottom.
fn four_bit(data: &[u8], width: usize, levels: &mut [u8]) -> Result<(), Malformed> {
    let rows = levels.len() / width;
    for (i, level) in (0..levels.len()).zip(upward(levels, width, rows)) {
        let byte = *data.get(i / 2).ok_or(Malformed)?;
        *level = if i % 2 == 0 { byte & 0xF } else { byte >> 4 };
    }
    Ok(())
}

/// `data`'s pixels at a bit each, the lowest first, rows bottom to top,
/// into `levels`, rows top to bottom.
fn one_bit(data: &[u8], width: usize, levels: &mut [u8]) -> Result<(), Malformed> {
    let rows = levels.len() / width;
    for (i, level) in (0..levels.len()).zip(upward(levels, width, rows)) {
        let byte = *data.get(i / 8).ok_or(Malformed)?;
        *level = if byte >> (i % 8) & 1 != 0 { WHOLE } else { 0 };
    }
    Ok(())
}

/// `levels`' pixels in a file's order: its rows from the bottom up, each
/// from its left.
fn upward(levels: &mut [u8], width: usize, rows: usize) -> impl Iterator<Item = &mut u8> {
    let mut by_row: Vec<&mut [u8]> = levels.chunks_mut(width).collect();
    by_row.truncate(rows);
    by_row.into_iter().rev().flat_map(|row| row.iter_mut())
}

/// The most nibbles a long run's zeros may count: more would be a run past
/// any bitmap a file can describe, which is at most 4095 pixels each way.
const LONG_RUN_MOST: u32 = 6;

/// A packed bitmap's numbers, a nibble at a time, the low nibble of each
/// byte first.
struct Nibbles<'a> {
    data: &'a [u8],
    at: usize,
}

impl Nibbles<'_> {
    fn next(&mut self) -> Result<u32, Malformed> {
        let byte = *self.data.get(self.at / 2).ok_or(Malformed)?;
        let nibble = if self.at.is_multiple_of(2) {
            byte & 0xF
        } else {
            byte >> 4
        };
        self.at += 1;
        Ok(u32::from(nibble))
    }

    /// A packed number at packing value `packing`, or a row repeat: a run's
    /// length, or how many times the row the next run starts in repeats.
    fn number(&mut self, packing: u32) -> Result<Packed, Malformed> {
        match self.next()? {
            REPEAT => {
                let first = self.next()?;
                Ok(Packed::Repeat(self.run(packing, first)?))
            }
            REPEAT_ONCE => Ok(Packed::Repeat(1)),
            first => Ok(Packed::Run(self.run(packing, first)?)),
        }
    }

    /// The run whose first nibble is `first`: a repeat's nibble here is
    /// malformed, as is a long run past any bitmap.
    fn run(&mut self, packing: u32, first: u32) -> Result<u32, Malformed> {
        match first {
            0 => {
                let mut zeros = 1;
                let mut value = loop {
                    let nibble = self.next()?;
                    if nibble != 0 {
                        break nibble;
                    }
                    zeros += 1;
                    if zeros > LONG_RUN_MOST {
                        return Err(Malformed);
                    }
                };
                for _ in 0..zeros {
                    value = value * 16 + self.next()?;
                }
                (value + (13 - packing) * 16 + packing + 1)
                    .checked_sub(16)
                    .ok_or(Malformed)
            }
            1..=13 if first <= packing => Ok(first),
            1..=13 => Ok((first - packing - 1) * 16 + self.next()? + packing + 1),
            _ => Err(Malformed),
        }
    }
}

/// A packed number's nibbles that make the next number a row repeat, and a
/// row repeated once.
const REPEAT: u32 = 14;
const REPEAT_ONCE: u32 = 15;

/// What a packed number is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Packed {
    Run(u32),
    Repeat(u32),
}

/// The packing value past which a nibble's value is a repeat, not a run.
const PACKING_MOST: u32 = 13;

/// `data`'s pixels packed into runs of alternate colours at packing value
/// `packing`, the first black where `black_first`, rows bottom to top, a row
/// repeated as a repeat count says, into `levels`, rows top to bottom.
fn packed(
    data: &[u8],
    packing: u32,
    black_first: bool,
    width: usize,
    levels: &mut [u8],
) -> Result<(), Undecoded> {
    if packing > PACKING_MOST {
        return Err(Undecoded::Malformed);
    }
    let rows = levels.len() / width;
    let mut nibbles = Nibbles { data, at: 0 };
    let mut black = black_first;
    let mut row = 0;
    let mut column = 0;
    let mut repeat = 0u32;
    let mut line = Vec::new();
    line.try_reserve_exact(width)
        .map_err(|_| Undecoded::Refused)?;
    line.resize(width, 0);
    while row < rows {
        let mut run = match nibbles.number(packing)? {
            Packed::Repeat(count) => {
                repeat = count;
                continue;
            }
            Packed::Run(run) => usize::try_from(run).map_err(|_| Undecoded::Malformed)?,
        };
        while run > 0 && row < rows {
            let here = run.min(width - column);
            line[column..column + here].fill(if black { WHOLE } else { 0 });
            column += here;
            run -= here;
            if column == width {
                for _ in 0..=repeat {
                    if row == rows {
                        break;
                    }
                    let top_first = rows - 1 - row;
                    levels[top_first * width..(top_first + 1) * width].copy_from_slice(&line);
                    row += 1;
                }
                repeat = 0;
                column = 0;
            }
        }
        black = !black;
    }
    Ok(())
}

/// A character's data read in order.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    /// Whether coordinates are 12-bit, two to three bytes.
    wide: bool,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, Malformed> {
        let value = byte(self.bytes, self.at)?;
        self.at += 1;
        Ok(value)
    }

    /// A point: two 8-bit coordinates, or two 12-bit ones in three bytes.
    fn point(&mut self) -> Result<Point, Malformed> {
        if self.wide {
            let a = u32::from(self.byte()?);
            let b = u32::from(self.byte()?);
            let c = u32::from(self.byte()?);
            let packed = a | b << 8 | c << 16;
            Ok(Point::new(
                sign_extend(packed & 0xFFF, 12),
                sign_extend(packed >> 12, 12),
            ))
        } else {
            let x = i32::from(self.byte()?.cast_signed());
            let y = i32::from(self.byte()?.cast_signed());
            Ok(Point::new(x, y))
        }
    }

    /// A path's moves, lines and curves into `out`, each at `offset`, to
    /// its terminator: the terminator.
    fn path(&mut self, offset: Point, out: &mut Vec<Element>) -> Result<u8, Malformed> {
        loop {
            let segment = self.byte()?;
            let element = match segment & SEGMENT_TYPE {
                TERMINATOR => return Ok(segment),
                MOVE => Element::Move(self.point()?),
                LINE => Element::Line(self.point()?),
                _ => Element::Curve(self.point()?, self.point()?, self.point()?),
            };
            if out.len() >= ELEMENTS_MOST {
                return Err(Malformed);
            }
            out.try_reserve(1).map_err(|_| Malformed)?;
            out.push(element.offset(offset));
        }
    }
}

/// `value`'s low `bits` bits, sign-extended.
const fn sign_extend(value: u32, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((value << shift).cast_signed()) >> shift
}

fn byte(bytes: &[u8], at: usize) -> Result<u8, Malformed> {
    bytes.get(at).copied().ok_or(Malformed)
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, Malformed> {
    let pair = bytes
        .get(at..at.checked_add(2).ok_or(Malformed)?)
        .ok_or(Malformed)?;
    Ok(u16::from_le_bytes([pair[0], pair[1]]))
}

fn i16_at(bytes: &[u8], at: usize) -> Result<i16, Malformed> {
    u16_at(bytes, at).map(u16::cast_signed)
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, Malformed> {
    let word = bytes
        .get(at..at.checked_add(4).ok_or(Malformed)?)
        .ok_or(Malformed)?;
    Ok(u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
}

#[cfg(test)]
mod tests;
