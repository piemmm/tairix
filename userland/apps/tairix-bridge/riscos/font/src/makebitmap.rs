//! `Font_MakeBitmap`: a font's characters drawn at one size from its
//! outlines and saved as the bitmap file painting then draws them from, in
//! the layout RISC OS 5.30's Font Manager writes: a version 8 file, its
//! table the size and resolutions, the font's name and a description, then
//! the characters in chunks of 32, each at every sub-placement asked,
//! unpacked; or that file deleted.

use alloc::vec::Vec;

use tairix_raster::scan::FillRule;
use tairix_riscos_kernel::filing::filetype;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::chunk::FONT;
use tairix_riscos_kernel::swi::X;

use crate::bitmaps::{self, FILE_MOST};
use crate::glyphs::{self, Bitmap, Placing, Refused, Scratch};
use crate::module::{call, error, raise, with};
use crate::outlines::{Depth, CHUNK, PLACEMENTS};
use crate::resolve::{self, Font};
use crate::transform::Transform;
use crate::{find, grid, scan, units};

/// `Font_MakeBitmap`'s flags.
mod make_flag {
    /// Four bits a pixel, an `f` file; else one, a `b` file.
    pub(super) const FOUR: u32 = 1;
    /// Each character at four sub-placements across, and up.
    pub(super) const ACROSS: u32 = 1 << 1;
    pub(super) const UP: u32 = 1 << 2;
    /// The file deleted, none made.
    pub(super) const DELETE: u32 = 1 << 3;
    pub(super) const KNOWN: u32 = FOUR | ACROSS | UP | DELETE;
}

/// The highest handle: R1 past it names a font.
const LAST_HANDLE: u32 = 255;
/// The file format's version written.
const VERSION: u8 = 8;
/// The header's flag saying each chunk opens with a flag word, and a
/// chunk's flag bit that must be set.
const FLAGGED: u16 = 1 << 6;
const CHUNK_MUST: u32 = 1 << 31;
/// The table a bitmap file has: its size, then the sizes and resolutions.
const TABLE: u16 = 10;
/// Where the header has the chunks' offsets, and the scaffold entries a
/// bitmap file has: its table's size alone.
const SCAFFOLD_ENTRIES: u32 = 1;
/// A character's flags: 12-bit coordinates, and one bit a pixel.
const WIDE: u8 = 1;
const ONE_BIT: u8 = 1 << 1;
/// The ranges of a character's coordinates in 8 and 12 bits.
const NARROW_RANGE: core::ops::RangeInclusive<i32> = -128..=127;
const WIDE_RANGE: core::ops::RangeInclusive<i32> = -2048..=2047;

/// `Font_MakeBitmap`: R1 a handle or a font identifier, R2 and R3 the size
/// in sixteenths of a point, R4 and R5 the resolution, R6 the flags. A font
/// named is found for the call and lost after it.
pub(crate) fn make_bitmap(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let flags = regs.r[6];
    let placed = flags & (make_flag::ACROSS | make_flag::UP) != 0;
    if flags & !make_flag::KNOWN != 0 || (placed && flags & make_flag::FOUR == 0) {
        return Err(raise(os, error::RESERVED, &[]));
    }
    if regs.r[1] <= LAST_HANDLE {
        return make(os, regs.r[1], regs, flags);
    }
    let mut find = Regs::default();
    find.r[1..6].copy_from_slice(&regs.r[1..6]);
    os.perform(X | (FONT + call::FIND_FONT), &mut find)?;
    let handle = find.r[0];
    let made = make(os, handle, regs, flags);
    let mut release = Regs::default();
    release.r[0] = handle;
    let lost = os.perform(X | (FONT + call::LOSE_FONT), &mut release);
    made.and(lost)
}

/// The bitmap file of handle `handle`'s font at `regs`' size made, or
/// deleted, as `flags` say: at the mode's resolution where none is given.
fn make(os: &mut Os<'_>, handle: u32, regs: &Regs, flags: u32) -> Result<(), Fail> {
    let font = resolve::font(os, handle)?;
    let points = (regs.r[2].cast_signed(), regs.r[3].cast_signed());
    if points.0 == 0 || points.1 == 0 {
        return Err(raise(os, error::ZERO_SIZE, &[]));
    }
    if points.0 < 0 || points.1 < 0 {
        return Err(raise(os, error::NO_DATA, &[&font.data.identifier]));
    }
    let resolution = find::resolved(os, (regs.r[4].cast_signed(), regs.r[5].cast_signed()))?;
    let depth = if flags & make_flag::FOUR != 0 {
        Depth::Four
    } else {
        Depth::One
    };
    let Some(leaf) = bitmaps::leaf(depth, points, resolution) else {
        return Err(raise(os, error::UNNAMED_SIZE, &[]));
    };
    let mut name = font.data.bitmaps.clone();
    name.extend_from_slice(&leaf);
    if flags & make_flag::DELETE != 0 {
        scan::delete(os, &name)?;
    } else {
        let size = Size {
            depth,
            points,
            resolution,
            placements: (flags & make_flag::ACROSS != 0, flags & make_flag::UP != 0),
        };
        let bytes = match file(&font, &size) {
            Ok(bytes) => bytes,
            Err(Unmade::TooBig | Unmade::Refused) => return Err(os.no_memory()),
        };
        let directory = &font.data.bitmaps;
        if directory.len() > font_directory(&font).len() {
            scan::create_directory(os, directory.strip_suffix(b".").unwrap_or(directory))?;
        }
        scan::save(os, &name, filetype::FONT, &bytes)?;
    }
    with(os, |state| state.store.forget_drawn())
}

/// The directory `font` was found in, its trailing full stop included.
fn font_directory(font: &Font) -> &[u8] {
    let bitmaps = &font.data.bitmaps;
    let inner = bitmaps.strip_suffix(b".").unwrap_or(bitmaps);
    inner
        .iter()
        .rposition(|&b| b == b'.')
        .map_or(&bitmaps[..], |at| &bitmaps[..=at])
}

/// The size a file is made at.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Size {
    pub(crate) depth: Depth,
    /// In sixteenths of a point, across and up.
    pub(crate) points: (i32, i32),
    /// In dots an inch.
    pub(crate) resolution: (i32, i32),
    /// Whether each character is held at four sub-placements across, and
    /// up.
    pub(crate) placements: (bool, bool),
}

/// Why a file was not made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unmade {
    /// It would be larger than a font file read may be, or a character
    /// larger than the format's coordinates reach.
    TooBig,
    /// The memory it needs was refused.
    Refused,
}

impl From<Refused> for Unmade {
    fn from(_: Refused) -> Self {
        Self::Refused
    }
}

/// A glyph's pixels at one of a file's sub-placements, across then up.
pub(crate) struct GlyphPixels {
    pub(crate) glyph: u32,
    pub(crate) placement: (usize, usize),
    pub(crate) bitmap: Bitmap,
}

/// One glyph's record in a file: the glyph, the index of its
/// sub-placement, its bytes and its box.
struct Record {
    glyph: usize,
    variant: usize,
    bytes: Vec<u8>,
    bounds: Bounds,
}

/// The bitmap file of the glyphs `font`'s codes reach, each drawn from its
/// outline at `size` and held at its own index.
pub(crate) fn file(font: &Font, size: &Size) -> Result<Vec<u8>, Unmade> {
    let (across, up) = placements(size);
    let mut glyphs = Vec::new();
    glyphs
        .try_reserve_exact(font.data.defined().count())
        .map_err(|_| Unmade::Refused)?;
    glyphs.extend(font.data.defined().map(|(_, glyph)| glyph));
    glyphs.sort_unstable();
    glyphs.dedup();
    let mut scratch = Scratch::default();
    let drawn = glyphs
        .into_iter()
        .flat_map(|glyph| {
            (0..across * up).map(move |variant| (glyph, (variant / up, variant % up)))
        })
        .filter_map(
            |(glyph, placement)| match drawn(font, glyph, size, placement, &mut scratch) {
                Ok(Some(bitmap)) => Some(Ok(GlyphPixels {
                    glyph,
                    placement,
                    bitmap,
                })),
                Ok(None) => None,
                Err(unmade) => Some(Err(unmade)),
            },
        );
    laid_out(&font.data.identifier, size, drawn)
}

/// The sub-placements a file at `size` holds each glyph at, across and up.
fn placements(size: &Size) -> (usize, usize) {
    let along = |placed: bool| if placed { PLACEMENTS } else { 1 };
    (along(size.placements.0), along(size.placements.1))
}

/// The file the glyphs `drawn` make, the font's name `identifier`, at
/// `size`, in the layout RISC OS 5.30 writes: each glyph's record packed
/// as it comes, then the chunks laid out in index order.
pub(crate) fn laid_out(
    identifier: &[u8],
    size: &Size,
    drawn: impl IntoIterator<Item = Result<GlyphPixels, Unmade>>,
) -> Result<Vec<u8>, Unmade> {
    let (across, up) = placements(size);
    let mut records = Vec::new();
    let mut total = 0usize;
    for placed in drawn {
        let GlyphPixels {
            glyph,
            placement,
            bitmap,
        } = placed?;
        let (bytes, bounds) = record(size.depth, &bitmap).ok_or(Unmade::TooBig)?;
        total = total.saturating_add(bytes.len());
        if total > FILE_MOST as usize {
            return Err(Unmade::TooBig);
        }
        records.try_reserve(1).map_err(|_| Unmade::Refused)?;
        records.push(Record {
            glyph: usize::try_from(glyph).map_err(|_| Unmade::TooBig)?,
            variant: placement.0.min(across - 1) * up + placement.1.min(up - 1),
            bytes,
            bounds,
        });
    }
    let chunks = records
        .iter()
        .map(|r| r.glyph / CHUNK + 1)
        .max()
        .unwrap_or(0);
    let bounds = records
        .iter()
        .map(|r| r.bounds)
        .reduce(Bounds::union)
        .unwrap_or_default();
    let mut out = header(identifier, size, bounds, chunks).ok_or(Unmade::TooBig)?;
    let offsets = out.len();
    let entries = across * up * CHUNK;
    let index_bytes = 4 + 4 * entries;
    let length = offsets + 4 * (chunks + 1) + chunks * index_bytes + total;
    if length > FILE_MOST as usize {
        return Err(Unmade::TooBig);
    }
    out.try_reserve_exact(length - out.len())
        .map_err(|_| Unmade::Refused)?;
    out.resize(offsets + 4 * (chunks + 1), 0);
    records.sort_by_key(|r| (r.glyph / CHUNK, r.variant, r.glyph));
    let chunk_flags = CHUNK_MUST | u32::from(size.placements.0) | u32::from(size.placements.1) << 1;
    let mut records = records.iter().peekable();
    for chunk in 0..chunks {
        let start = out.len();
        put_word(&mut out, offsets + 4 * chunk, word(start));
        out.extend_from_slice(&chunk_flags.to_le_bytes());
        out.resize(start + index_bytes, 0);
        while let Some(record) = records.next_if(|r| r.glyph / CHUNK == chunk) {
            let entry = start + 4 + 4 * (record.variant * CHUNK + record.glyph % CHUNK);
            let from_index = out.len() - start - 4;
            put_word(&mut out, entry, word(from_index));
            out.extend_from_slice(&record.bytes);
        }
    }
    let end = out.len();
    put_word(&mut out, offsets + 4 * chunks, word(end));
    Ok(out)
}

/// A length within a file, which is no longer than a font file may be, as
/// a word.
fn word(length: usize) -> u32 {
    u32::try_from(length).unwrap_or(u32::MAX)
}

/// The header and table of a file at `size`: the characters within
/// `bounds`, in `chunks` chunks, the font's name `identifier` and a
/// description after.
fn header(identifier: &[u8], size: &Size, bounds: Bounds, chunks: usize) -> Option<Vec<u8>> {
    let narrow = |v: i32| u16::try_from(v).ok();
    let signed = |v: i32| i16::try_from(v).ok();
    let mut out = Vec::new();
    out.try_reserve(128).ok()?;
    out.extend_from_slice(b"FONT");
    out.push(match size.depth {
        Depth::Four => 4,
        Depth::One => 1,
    });
    out.push(VERSION);
    let placed = u16::from(size.placements.0) | u16::from(size.placements.1) << 1;
    out.extend_from_slice(&(FLAGGED | placed).to_le_bytes());
    for value in [signed(bounds.x0)?, signed(bounds.y0)?] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    for value in [
        narrow(bounds.x1 - bounds.x0)?,
        narrow(bounds.y1 - bounds.y0)?,
    ] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    let offsets_at = out.len();
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&u32::try_from(chunks).ok()?.to_le_bytes());
    out.extend_from_slice(&SCAFFOLD_ENTRIES.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    out.extend_from_slice(&TABLE.to_le_bytes());
    for value in [
        size.points.0,
        size.resolution.0,
        size.points.1,
        size.resolution.1,
    ] {
        out.extend_from_slice(&narrow(value)?.to_le_bytes());
    }
    out.extend_from_slice(identifier);
    out.push(0);
    out.extend_from_slice(&description(size));
    out.push(0);
    out.resize(out.len().next_multiple_of(4), 0);
    let table = word(out.len());
    put_word(&mut out, offsets_at, table);
    Some(out)
}

/// What a file says of its size: `16x16 points at 90x90 dpi`.
fn description(size: &Size) -> Vec<u8> {
    let points = |sixteenths: i32| sixteenths / units::SIXTEENTHS;
    alloc::format!(
        "{}x{} points at {}x{} dpi",
        points(size.points.0),
        points(size.points.1),
        size.resolution.0,
        size.resolution.1
    )
    .into_bytes()
}

/// Glyph `glyph` of `font`'s drawn at `size`, a quarter of a pixel on for
/// each step of `shift` across and up, its blank edges trimmed; none where
/// it has no outline or no pixels.
fn drawn(
    font: &Font,
    glyph: u32,
    size: &Size,
    shift: (usize, usize),
    scratch: &mut Scratch,
) -> Result<Option<Bitmap>, Unmade> {
    let Some(outline) = font.data.outline(glyph) else {
        return Ok(None);
    };
    let placing = Placing {
        points: size.points,
        transform: Transform::IDENTITY,
        resolution: size.resolution,
        shift: (grid::shift(shift.0), grid::shift(shift.1)),
    };
    let traced = glyphs::trace(&outline, &placing);
    let Some(reach) = traced.reach() else {
        return Ok(None);
    };
    let reaches = |from: i32, to: i32| {
        WIDE_RANGE.contains(&from)
            && to
                .checked_sub(from)
                .is_some_and(|span| WIDE_RANGE.contains(&span))
    };
    if !(reaches(reach.left, reach.right) && reaches(reach.bottom, reach.top)) {
        return Err(Unmade::TooBig);
    }
    let rule = if font.data.non_zero() {
        FillRule::NonZero
    } else {
        FillRule::EvenOdd
    };
    match traced.levels(&reach, rule, size.depth == Depth::One, scratch)? {
        Some(bitmap) => Ok(bitmap.trimmed()?),
        None => Ok(None),
    }
}

/// A character's box in pixels: bottom left in, top right out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Bounds {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl Bounds {
    fn union(self, other: Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }
}

/// `bitmap`'s record in a file at `depth`: its flags, its box in 8-bit
/// coordinates or, where they do not reach, 12-bit ones, and its pixels
/// from its bottom row up, the record padded to a word; none where even
/// 12 bits do not reach.
fn record(depth: Depth, bitmap: &Bitmap) -> Option<(Vec<u8>, Bounds)> {
    let width = i32::try_from(bitmap.width).ok()?;
    let rows = bitmap.levels.len() / bitmap.width;
    let height = i32::try_from(rows).ok()?;
    let (x0, y0) = (bitmap.left, bitmap.top - height + 1);
    let coordinates = [x0, y0, width, height];
    let wide = !coordinates.iter().all(|v| NARROW_RANGE.contains(v));
    if wide && !coordinates.iter().all(|v| WIDE_RANGE.contains(v)) {
        return None;
    }
    let mut out = Vec::new();
    let pixels = match depth {
        Depth::Four => bitmap.levels.len().div_ceil(2),
        Depth::One => bitmap.levels.len().div_ceil(8),
    };
    out.try_reserve(8 + pixels).ok()?;
    out.push((u8::from(wide) * WIDE) | if depth == Depth::One { ONE_BIT } else { 0 });
    if wide {
        for pair in [(x0, y0), (width, height)] {
            let packed =
                (pair.0.cast_unsigned() & 0xFFF) | ((pair.1.cast_unsigned() & 0xFFF) << 12);
            out.extend_from_slice(&packed.to_le_bytes()[..3]);
        }
    } else {
        out.extend(coordinates.iter().map(|&v| v.to_le_bytes()[0]));
    }
    let upward = bitmap.levels.chunks(bitmap.width).rev().flatten();
    match depth {
        Depth::Four => {
            let mut low = None;
            for &level in upward {
                match low.take() {
                    None => low = Some(level & 0xF),
                    Some(first) => out.push(first | (level & 0xF) << 4),
                }
            }
            out.extend(low);
        }
        Depth::One => {
            let (mut byte, mut bits) = (0u8, 0);
            for &level in upward {
                byte |= u8::from(level != 0) << bits;
                bits += 1;
                if bits == 8 {
                    out.push(byte);
                    (byte, bits) = (0, 0);
                }
            }
            if bits > 0 {
                out.push(byte);
            }
        }
    }
    out.resize(out.len().next_multiple_of(4), 0);
    let bounds = Bounds {
        x0,
        y0,
        x1: x0 + width,
        y1: y0 + height,
    };
    Some((out, bounds))
}

/// `value` written as a little-endian word at `at` in `out`.
fn put_word(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests;
