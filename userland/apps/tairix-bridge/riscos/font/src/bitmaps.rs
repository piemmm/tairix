//! A font's bitmap files as painting draws from them: an `f9999x9999` or
//! `b9999x9999` file of the size asked, a size of its `x90y45` file, and the
//! `x90y45` file's nearest size, or the master `f9999x9999` file it names,
//! scaled. Each file is read from the font's bitmap directory once and kept
//! in the store, its absence too; one that does not hold together is as good
//! as absent.

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::mem::size_of;

use crate::glyphs::{Bitmap, Refused};
use crate::module::with;
use crate::outlines::{self, Bitmaps, Depth};
use crate::resolve::Font;
use crate::scan;
use crate::units;
use crate::x90y45;
use tairix_riscos_kernel::os::{Fail, Os};
use tairix_riscos_vdu::render::WHOLE;

/// The largest bitmap file read or made.
pub(crate) const FILE_MOST: u32 = 16 << 20;
/// The pixel sizes a file's name may give.
const PIXELS_MOST: i64 = 9999;
/// The most pixels a scaled glyph spans either way.
const SCALED_MOST: usize = 4096;

/// A bitmap file a font may have.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    /// An `f` or `b` file of a size, in sixteenths of a point across and
    /// up, at a resolution in dots an inch.
    Sized {
        depth: Depth,
        points: (i32, i32),
        resolution: (i32, i32),
    },
    /// The `x90y45` file.
    Old,
    /// The `f` file the `x90y45` file names as its master bitmaps.
    Master,
}

/// A file of a master's, as the store keys it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Wanted {
    pub(crate) master: u64,
    pub(crate) kind: Kind,
}

/// A bitmap file as read.
#[derive(Debug)]
pub(crate) enum Held {
    /// There is none, or none that holds together.
    Absent,
    /// An `f` or `b` file, or the master bitmaps.
    Sized(Bitmaps),
    /// An `x90y45` file's sizes.
    Old(x90y45::Sizes),
    /// An `x90y45` file naming the `f` file of the master bitmaps.
    Named(Vec<u8>),
}

impl Held {
    /// Roughly what it holds in memory.
    pub(crate) fn bytes(&self) -> usize {
        size_of::<Self>()
            + match self {
                Self::Absent => 0,
                Self::Sized(bitmaps) => bitmaps.bytes(),
                Self::Old(sizes) => sizes.bytes(),
                Self::Named(name) => name.len(),
            }
    }
}

/// What a glyph's levels were made from, as the glyph cache keys them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Raster {
    /// The font's outline.
    Outline,
    /// A file of the size at four bits a pixel, or at one.
    Four,
    One,
    /// The `x90y45` file's own size.
    Old,
    /// A bitmap of another size, scaled.
    Scaled,
}

/// The leaf of a size's file: `f` or `b`, then its pixel sizes across and
/// up, each its sixteenths of a point at its dots an inch over 72; none
/// where either falls outside 1 to 9999.
pub(crate) fn leaf(depth: Depth, points: (i32, i32), resolution: (i32, i32)) -> Option<Vec<u8>> {
    let pixels = |points: i32, dots: i32| {
        let size =
            (i64::from(points) * i64::from(dots)).div_euclid(i64::from(units::POINTS_AN_INCH));
        (1..=PIXELS_MOST).contains(&size).then_some(size)
    };
    let (across, up) = (
        pixels(points.0, resolution.0)?,
        pixels(points.1, resolution.1)?,
    );
    let letter = match depth {
        Depth::Four => 'f',
        Depth::One => 'b',
    };
    Some(alloc::format!("{letter}{across}x{up}").into_bytes())
}

/// The file `kind` of `font`'s, as the store holds it or read now.
pub(crate) fn file(os: &mut Os<'_>, font: &Font, kind: Kind) -> Result<Rc<Held>, Fail> {
    let wanted = Wanted {
        master: font.serial,
        kind,
    };
    if let Some(held) = with(os, |state| state.store.file(&wanted))? {
        return Ok(held);
    }
    let held = Rc::new(read(os, font, kind)?);
    with(os, |state| state.store.keep_file(wanted, &held))?;
    Ok(held)
}

/// The file `kind` read from `font`'s bitmap directory.
fn read(os: &mut Os<'_>, font: &Font, kind: Kind) -> Result<Held, Fail> {
    let leaf = match kind {
        Kind::Sized {
            depth,
            points,
            resolution,
        } => match leaf(depth, points, resolution) {
            Some(leaf) => leaf,
            None => return Ok(Held::Absent),
        },
        Kind::Old => scan::OLD_BITMAPS.to_vec(),
        Kind::Master => match &*file(os, font, Kind::Old)? {
            Held::Named(name) => name.clone(),
            _ => return Ok(Held::Absent),
        },
    };
    let mut name = font.data.bitmaps.clone();
    name.extend_from_slice(&leaf);
    let Some(bytes) = scan::load(os, &name, FILE_MOST)? else {
        return Ok(Held::Absent);
    };
    Ok(match kind {
        Kind::Sized {
            depth,
            points,
            resolution,
        } => match outlines::File::read(bytes) {
            Ok(outlines::File::Bitmaps(bitmaps))
                if bitmaps.depth == depth
                    && same(bitmaps.points, points)
                    && same(bitmaps.resolution, resolution) =>
            {
                Held::Sized(bitmaps)
            }
            _ => Held::Absent,
        },
        Kind::Old => match x90y45::File::read(bytes) {
            Ok(x90y45::File::Sizes(sizes)) => Held::Old(sizes),
            Ok(x90y45::File::Master(named)) => Held::Named(named),
            Err(_) => Held::Absent,
        },
        Kind::Master => match outlines::File::read(bytes) {
            Ok(outlines::File::Bitmaps(bitmaps)) if bitmaps.depth == Depth::Four => {
                Held::Sized(bitmaps)
            }
            _ => Held::Absent,
        },
    })
}

/// Whether a file's pair of sizes is the pair asked.
fn same(held: (u16, u16), asked: (i32, i32)) -> bool {
    i32::from(held.0) == asked.0 && i32::from(held.1) == asked.1
}

/// The size of `sizes` that is `font`'s at `resolution`: whole points the
/// same across and up.
pub(crate) fn old_size(
    sizes: &x90y45::Sizes,
    font: &Font,
    resolution: (i32, i32),
) -> Option<x90y45::Size> {
    sizes.sizes.iter().copied().find(|size| {
        let points = i64::from(size.points) * i64::from(units::SIXTEENTHS);
        points == i64::from(font.points.0)
            && points == i64::from(font.points.1)
            && i32::from(size.resolution.0) == resolution.0
            && i32::from(size.resolution.1) == resolution.1
    })
}

/// What a font scales its bitmaps from: a file and the ratios across and up
/// its pixels are scaled by.
pub(crate) struct Scalable {
    held: Rc<Held>,
    /// The `x90y45` file's size scaled from, where not the master bitmaps.
    size: Option<x90y45::Size>,
    pub(crate) scale: (Ratio, Ratio),
}

/// A scale as a ratio of whole numbers, both above nought: the wanted size
/// over the size held, each its sixteenths of a point times its dots an
/// inch, so that scaling is exact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ratio {
    pub(crate) wanted: i64,
    pub(crate) held: i64,
}

/// The largest term a scale's ratio may have in its lowest terms: far past
/// any size a glyph's bitmap is scaled to or from, and small enough that a
/// pixel's place times a term fits a word's square.
const RATIO_MOST: i64 = i32::MAX as i64;

impl Ratio {
    /// `wanted` over `held`, in lowest terms; none unless both are above
    /// nought and neither then passes [`RATIO_MOST`].
    pub(crate) fn of(wanted: i64, held: i64) -> Option<Self> {
        if wanted <= 0 || held <= 0 {
            return None;
        }
        let divisor = gcd(wanted, held);
        let ratio = Self {
            wanted: wanted / divisor,
            held: held / divisor,
        };
        (ratio.wanted <= RATIO_MOST && ratio.held <= RATIO_MOST).then_some(ratio)
    }
}

/// The greatest common divisor of two numbers above nought.
const fn gcd(mut a: i64, mut b: i64) -> i64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl Scalable {
    /// Character `code`'s pixels, scaled.
    pub(crate) fn glyph(&self, code: u32) -> Result<Option<Bitmap>, Refused> {
        let source = match (&*self.held, self.size) {
            (Held::Old(sizes), Some(size)) => sizes.glyph(&size, code)?,
            (Held::Sized(bitmaps), None) => bitmaps.glyph(code, (0, 0))?,
            _ => None,
        };
        match source {
            Some(source) => scaled(&source, self.scale),
            None => Ok(None),
        }
    }
}

/// What `font` at `resolution` scales its bitmaps from, as the PRM gives
/// it: the master bitmaps its `x90y45` file names, else the size of that
/// file nearest it up, the larger of two as near; none where it has no such
/// file.
pub(crate) fn scalable(
    os: &mut Os<'_>,
    font: &Font,
    resolution: (i32, i32),
) -> Result<Option<Scalable>, Fail> {
    let wanted = (
        i64::from(font.points.0) * i64::from(resolution.0),
        i64::from(font.points.1) * i64::from(resolution.1),
    );
    let scale =
        |held: (i64, i64)| Some((Ratio::of(wanted.0, held.0)?, Ratio::of(wanted.1, held.1)?));
    let old = file(os, font, Kind::Old)?;
    match &*old {
        Held::Old(sizes) => {
            let held = |size: &x90y45::Size| {
                let points = i64::from(size.points) * i64::from(units::SIXTEENTHS);
                (
                    points * i64::from(size.resolution.0),
                    points * i64::from(size.resolution.1),
                )
            };
            let nearest = sizes.sizes.iter().copied().min_by_key(|size| {
                (
                    (held(size).1 - wanted.1).abs(),
                    core::cmp::Reverse(size.points),
                )
            });
            Ok(nearest.and_then(|size| {
                Some(Scalable {
                    scale: scale(held(&size))?,
                    held: Rc::clone(&old),
                    size: Some(size),
                })
            }))
        }
        Held::Named(_) => {
            let master = file(os, font, Kind::Master)?;
            let Held::Sized(bitmaps) = &*master else {
                return Ok(None);
            };
            let held = (
                i64::from(bitmaps.points.0) * i64::from(bitmaps.resolution.0),
                i64::from(bitmaps.points.1) * i64::from(bitmaps.resolution.1),
            );
            Ok(scale(held).map(|scale| Scalable {
                held: Rc::clone(&master),
                size: None,
                scale,
            }))
        }
        Held::Absent | Held::Sized(_) => Ok(None),
    }
}

/// `bitmap` scaled by `scale` across and up: each pixel's level the share
/// of it its source pixels cover, weighed by their levels, rounded to the
/// nearest, halves up; none where it would be empty or wider or taller than
/// a glyph may be.
pub(crate) fn scaled(
    bitmap: &Bitmap,
    (across, up): (Ratio, Ratio),
) -> Result<Option<Bitmap>, Refused> {
    if bitmap.width == 0 {
        return Ok(None);
    }
    let rows = bitmap.levels.len() / bitmap.width;
    let Some(bottom) = i32::try_from(rows)
        .ok()
        .and_then(|rows| bitmap.top.checked_sub(rows)?.checked_add(1))
    else {
        return Ok(None);
    };
    let (Some(columns), Some(lines)) = (
        Weights::of(bitmap.left, bitmap.width, across)?,
        Weights::of(bottom, rows, up)?,
    ) else {
        return Ok(None);
    };
    let width = columns.count();
    let mut levels = Vec::new();
    levels
        .try_reserve_exact(width.checked_mul(lines.count()).ok_or(Refused)?)
        .map_err(|_| Refused)?;
    let whole = i128::from(across.held) * i128::from(up.held);
    for line in (0..lines.count()).rev() {
        for column in 0..width {
            let mut sum = 0i128;
            for &(row, row_share) in lines.under(line) {
                let pixels = &bitmap.levels[(rows - 1 - row) * bitmap.width..][..bitmap.width];
                for &(at, share) in columns.under(column) {
                    sum += i128::from(pixels[at]) * i128::from(row_share) * i128::from(share);
                }
            }
            let level = (2 * sum + whole) / (2 * whole);
            levels.push(u8::try_from(level.clamp(0, i128::from(WHOLE))).unwrap_or(WHOLE));
        }
    }
    let count = i32::try_from(lines.count()).unwrap_or(i32::MAX);
    Ok(Some(Bitmap {
        left: columns.first,
        top: lines.first.saturating_add(count) - 1,
        width,
        levels,
    }))
}

/// Pixels along one axis scaled: the first scaled pixel, and under each
/// scaled pixel the source pixels with the share of it each covers, in
/// parts of the ratio's held size.
struct Weights {
    first: i32,
    /// Where each scaled pixel's sources start in `sources`, and where the
    /// last ends.
    starts: Vec<usize>,
    sources: Vec<(usize, i64)>,
}

impl Weights {
    /// `count` pixels from `first`, scaled by `scale`; none where they would
    /// span more than a glyph may.
    fn of(first: i32, count: usize, scale: Ratio) -> Result<Option<Self>, Refused> {
        let Ok(length) = i64::try_from(count) else {
            return Ok(None);
        };
        if count == 0 {
            return Ok(None);
        }
        let Ratio { wanted, held } = scale;
        let start = i64::from(first);
        let low = (start * wanted).div_euclid(held);
        let high = ((start + length) * wanted + held - 1).div_euclid(held);
        let Ok(scaled) = usize::try_from(high - low) else {
            return Ok(None);
        };
        if scaled == 0 || scaled > SCALED_MOST {
            return Ok(None);
        }
        let per = usize::try_from(held.div_euclid(wanted) + 2).map_or(count, |per| per.min(count));
        let mut starts = Vec::new();
        starts.try_reserve_exact(scaled + 1).map_err(|_| Refused)?;
        let mut sources = Vec::new();
        sources
            .try_reserve_exact(scaled.checked_mul(per).ok_or(Refused)?)
            .map_err(|_| Refused)?;
        for pixel in (low..).take(scaled) {
            starts.push(sources.len());
            let (from, to) = (pixel * held, (pixel + 1) * held);
            let first_cell = from.div_euclid(wanted).max(start);
            let last_cell = (to - 1).div_euclid(wanted).min(start + length - 1);
            for cell in first_cell..=last_cell {
                let covered = to.min((cell + 1) * wanted) - from.max(cell * wanted);
                if covered > 0 {
                    sources.push((usize::try_from(cell - start).unwrap_or(0), covered));
                }
            }
        }
        starts.push(sources.len());
        Ok(Some(Self {
            first: i32::try_from(low).unwrap_or(i32::MIN),
            starts,
            sources,
        }))
    }

    fn count(&self) -> usize {
        self.starts.len() - 1
    }

    fn under(&self, pixel: usize) -> &[(usize, i64)] {
        &self.sources[self.starts[pixel]..self.starts[pixel + 1]]
    }
}

#[cfg(test)]
mod tests;
