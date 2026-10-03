//! Changes to a whole picture: its size, its shape, which way up it is, its
//! colours and whether it can be transparent. Each builds a new canvas and
//! leaves the one it read as it was, so the old one is what undo puts back.

use alloc::vec::Vec;

use tairix_image::{desktop_palette, IndexDepth, PictureSource, Rgba8};
use tairix_raster::{resample_window, Region, Reorient, Rgba8Image};
use tairix_util::fallible;

use crate::canvas::{admissible, Canvas, CanvasBuilder, CanvasError, Kind, Sample, TILE};
use crate::colour::Nearest;
use crate::quantize::{palette_for, OPAQUE_FROM};

/// Where a picture's old pixels sit on a canvas of a new size.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Anchor {
    /// At the top left.
    TopLeft,
    /// Across the top, centred.
    Top,
    /// At the top right.
    TopRight,
    /// Down the left, centred.
    Left,
    /// In the middle.
    Centre,
    /// Down the right, centred.
    Right,
    /// At the bottom left.
    BottomLeft,
    /// Across the bottom, centred.
    Bottom,
    /// At the bottom right.
    BottomRight,
}

impl Anchor {
    /// Every anchor, row by row.
    pub const ALL: [Self; 9] = [
        Self::TopLeft,
        Self::Top,
        Self::TopRight,
        Self::Left,
        Self::Centre,
        Self::Right,
        Self::BottomLeft,
        Self::Bottom,
        Self::BottomRight,
    ];

    /// Where the old picture's top left lands on the new canvas.
    fn offset(self, old: (u32, u32), new: (u32, u32)) -> (i64, i64) {
        let spare = |new: u32, old: u32| i64::from(new) - i64::from(old);
        let (across, down) = match self {
            Self::TopLeft => (0, 0),
            Self::Top => (1, 0),
            Self::TopRight => (2, 0),
            Self::Left => (0, 1),
            Self::Centre => (1, 1),
            Self::Right => (2, 1),
            Self::BottomLeft => (0, 2),
            Self::Bottom => (1, 2),
            Self::BottomRight => (2, 2),
        };
        (
            spare(new.0, old.0) * across / 2,
            spare(new.1, old.1) * down / 2,
        )
    }
}

/// A turn, clockwise.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Turn {
    /// A quarter turn.
    Quarter,
    /// A half turn.
    Half,
    /// Three quarters: a quarter turn anticlockwise.
    ThreeQuarters,
}

/// What a palette is chosen as.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PaletteChoice {
    /// The RISC OS desktop's colours for the depth: what a sprite with no
    /// palette of its own shows.
    Desktop,
    /// The colours that suit this picture best.
    Optimised,
}

/// How a picture's pixels are to be stored.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Depth {
    /// Four bytes a pixel.
    Rgba,
    /// A palette of this depth.
    Indexed(IndexDepth),
}

/// A change to a whole picture.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Transform {
    /// Stretch or shrink the picture to `width`×`height`: each new pixel the
    /// old one nearest, or, when `smooth`, filtered from those it covers.
    Scale {
        /// The new width.
        width: u32,
        /// The new height.
        height: u32,
        /// Whether to filter rather than take the nearest pixel.
        smooth: bool,
    },
    /// Give the picture a canvas of `width`×`height`, the old pixels placed
    /// at `anchor` and the new ones `fill`.
    Resize {
        /// The new width.
        width: u32,
        /// The new height.
        height: u32,
        /// Where the old pixels go.
        anchor: Anchor,
        /// What the new pixels are.
        fill: Sample,
    },
    /// Keep only the `width`×`height` pixels from `(x, y)`.
    Crop {
        /// First column kept.
        x: u32,
        /// First row kept.
        y: u32,
        /// Columns kept.
        width: u32,
        /// Rows kept.
        height: u32,
    },
    /// Turn the picture.
    Turn(Turn),
    /// Mirror the picture left to right, or top to bottom when `vertical`.
    Flip {
        /// Top to bottom rather than left to right.
        vertical: bool,
    },
    /// Give a palette picture a mask, every pixel opaque; or take it away,
    /// the pixels it hid becoming entry `fill`.
    Mask {
        /// Whether the picture is to have a mask.
        on: bool,
        /// The entry hidden pixels become when the mask goes.
        fill: u8,
    },
    /// Store the pixels at `depth`, a palette chosen as `palette`, the error
    /// each pixel's nearest entry leaves spread to its neighbours when
    /// `dither`.
    Convert {
        /// The new depth.
        depth: Depth,
        /// How a palette is chosen.
        palette: PaletteChoice,
        /// Whether to spread the error.
        dither: bool,
    },
}

/// Why a transform could not be made.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TransformError {
    /// The picture it would make is larger than a picture may be, or empty.
    BadSize,
    /// It does not apply to a picture of this kind.
    NotApplicable,
    /// The allocator refused the pixels.
    OutOfMemory,
}

impl From<CanvasError> for TransformError {
    fn from(err: CanvasError) -> Self {
        match err {
            CanvasError::BadSize => Self::BadSize,
            CanvasError::BadPalette => Self::NotApplicable,
            CanvasError::OutOfMemory => Self::OutOfMemory,
        }
    }
}

impl From<crate::canvas::OutOfMemory> for TransformError {
    fn from(crate::canvas::OutOfMemory: crate::canvas::OutOfMemory) -> Self {
        Self::OutOfMemory
    }
}

/// `canvas` changed by `transform`.
///
/// # Errors
///
/// [`TransformError`] where it cannot be made.
pub fn apply(canvas: &Canvas, transform: Transform) -> Result<Canvas, TransformError> {
    match transform {
        Transform::Scale {
            width,
            height,
            smooth,
        } => {
            if smooth && canvas.kind() == &Kind::Rgba {
                scale_smooth(canvas, width, height)
            } else {
                scale_nearest(canvas, width, height)
            }
        }
        Transform::Resize {
            width,
            height,
            anchor,
            fill,
        } => {
            let (dx, dy) = anchor.offset((canvas.width(), canvas.height()), (width, height));
            place(canvas, (width, height), (dx, dy), fill)
        }
        Transform::Crop {
            x,
            y,
            width,
            height,
        } => {
            let inside = x
                .checked_add(width)
                .is_some_and(|end| end <= canvas.width())
                && y.checked_add(height)
                    .is_some_and(|end| end <= canvas.height());
            if !inside {
                return Err(TransformError::BadSize);
            }
            let fill = first_sample(canvas);
            place(
                canvas,
                (width, height),
                (-i64::from(x), -i64::from(y)),
                fill,
            )
        }
        Transform::Turn(turn) => turned(canvas, turn),
        Transform::Flip { vertical } => flipped(canvas, vertical),
        Transform::Mask { on, fill } => masked(canvas, on, fill),
        Transform::Convert {
            depth,
            palette,
            dither,
        } => converted(canvas, depth, palette, dither),
    }
}

/// A sample the canvas's kind stores, for pixels a transform leaves unset.
fn first_sample(canvas: &Canvas) -> Sample {
    canvas.sample(0, 0).unwrap_or(Sample::Rgba([0; 4]))
}

/// A row of samples and one of mask, sized for `canvas`'s kind at `width`.
fn row_buffers(kind: &Kind, width: u32) -> Result<(Vec<u8>, Vec<u8>), TransformError> {
    let width = width as usize;
    let samples =
        fallible::filled(width * kind.sample_bytes(), 0u8).ok_or(TransformError::OutOfMemory)?;
    let mask = fallible::filled(if kind.masked() { width } else { 0 }, 0u8)
        .ok_or(TransformError::OutOfMemory)?;
    Ok((samples, mask))
}

fn scale_nearest(canvas: &Canvas, width: u32, height: u32) -> Result<Canvas, TransformError> {
    if !admissible(width, height) {
        return Err(TransformError::BadSize);
    }
    let kind = canvas.kind().clone();
    let bytes = kind.sample_bytes();
    let mut out = CanvasBuilder::new(width, height, kind.clone(), first_sample(canvas))?;
    let (mut from, mut from_mask) = row_buffers(&kind, canvas.width())?;
    let (mut to, mut to_mask) = row_buffers(&kind, width)?;
    let pick = |at: u32, new: u32, old: u32| {
        u32::try_from((u64::from(at) * 2 + 1) * u64::from(old) / (u64::from(new) * 2)).unwrap_or(0)
    };
    let columns: Vec<usize> = fallible::collected(
        width as usize,
        (0..width).map(|x| pick(x, width, canvas.width()) as usize),
    )
    .ok_or(TransformError::OutOfMemory)?;
    let mut read = None;
    for y in 0..height {
        let source = pick(y, height, canvas.height());
        if read != Some(source) {
            canvas.read_row(source, &mut from, &mut from_mask);
            for (x, &column) in columns.iter().enumerate() {
                to[x * bytes..(x + 1) * bytes]
                    .copy_from_slice(&from[column * bytes..(column + 1) * bytes]);
                if let (Some(slot), Some(&alpha)) = (to_mask.get_mut(x), from_mask.get(column)) {
                    *slot = alpha;
                }
            }
            read = Some(source);
        }
        out.row(y, &to, &to_mask);
    }
    Ok(out.finish())
}

fn scale_smooth(canvas: &Canvas, width: u32, height: u32) -> Result<Canvas, TransformError> {
    if !admissible(width, height) {
        return Err(TransformError::BadSize);
    }
    let (w, h) = (canvas.width(), canvas.height());
    let row = w as usize * 4;
    let mut pixels = fallible::filled(row * h as usize, 0u8).ok_or(TransformError::OutOfMemory)?;
    for y in 0..h {
        let at = y as usize * row;
        canvas.read_row(y, &mut pixels[at..at + row], &mut []);
    }
    let image = Rgba8Image::new(w, h, &pixels).map_err(|_| TransformError::BadSize)?;
    let region = Region {
        x: 0,
        y: 0,
        width: w,
        height: h,
    };
    // The result is made a tile's rows at a time, each band exactly as the
    // whole would have it, so it is never held twice.
    let mut out = CanvasBuilder::new(width, height, Kind::Rgba, Sample::Rgba([0; 4]))?;
    let out_row = width as usize * 4;
    let mut band = fallible::filled(out_row * TILE.min(height) as usize, 0u8)
        .ok_or(TransformError::OutOfMemory)?;
    let mut top = 0;
    while top < height {
        let rows = TILE.min(height - top);
        let window = Region {
            x: 0,
            y: top,
            width,
            height: rows,
        };
        let band = &mut band[..out_row * rows as usize];
        resample_window(&image, region, width, height, window, band)
            .map_err(|_| TransformError::OutOfMemory)?;
        for (y, row) in (top..).zip(band.chunks_exact(out_row)) {
            out.row(y, row, &[]);
        }
        top += rows;
    }
    Ok(out.finish())
}

/// A `size` canvas of `canvas`'s kind, `fill` everywhere but where
/// `canvas`'s pixels land with their top left at `at`.
fn place(
    canvas: &Canvas,
    size: (u32, u32),
    at: (i64, i64),
    fill: Sample,
) -> Result<Canvas, TransformError> {
    let kind = canvas.kind().clone();
    let bytes = kind.sample_bytes();
    let mut out = CanvasBuilder::new(size.0, size.1, kind.clone(), fill)?;
    let (mut from, mut from_mask) = row_buffers(&kind, canvas.width())?;
    let (mut to, mut to_mask) = row_buffers(&kind, size.0)?;
    let blank = fill_row(&kind, size.0, fill)?;
    // The columns the old pixels reach on the new canvas, and where they
    // start in the old.
    let first = at.0.max(0);
    let last = (at.0 + i64::from(canvas.width())).min(i64::from(size.0));
    if first >= last {
        return Ok(out.finish());
    }
    let span = usize::try_from(last - first).unwrap_or(0);
    let (to_x, from_x) = (
        usize::try_from(first).unwrap_or(0),
        usize::try_from(first - at.0).unwrap_or(0),
    );
    for y in 0..size.1 {
        let source = i64::from(y) - at.1;
        let Ok(source) = u32::try_from(source) else {
            continue;
        };
        if source >= canvas.height() {
            continue;
        }
        canvas.read_row(source, &mut from, &mut from_mask);
        to.copy_from_slice(&blank.0);
        to_mask.copy_from_slice(&blank.1);
        to[to_x * bytes..(to_x + span) * bytes]
            .copy_from_slice(&from[from_x * bytes..(from_x + span) * bytes]);
        if kind.masked() {
            to_mask[to_x..to_x + span].copy_from_slice(&from_mask[from_x..from_x + span]);
        }
        out.row(y, &to, &to_mask);
    }
    Ok(out.finish())
}

/// A row of `width` pixels of `fill`.
fn fill_row(kind: &Kind, width: u32, fill: Sample) -> Result<(Vec<u8>, Vec<u8>), TransformError> {
    let (mut samples, mut mask) = row_buffers(kind, width)?;
    for x in 0..width as usize {
        crate::canvas::write_sample(&mut samples, &mut mask, x, fill);
    }
    Ok((samples, mask))
}

fn turned(canvas: &Canvas, turn: Turn) -> Result<Canvas, TransformError> {
    let how = match turn {
        Turn::Quarter => Reorient::QuarterTurnRight,
        Turn::Half => Reorient::HalfTurn,
        Turn::ThreeQuarters => Reorient::QuarterTurnLeft,
    };
    let (width, height) = how.applied_size(canvas.width(), canvas.height());
    let mut out = CanvasBuilder::new(width, height, canvas.kind().clone(), first_sample(canvas))?;
    let mut line = fallible::filled(width as usize, first_sample(canvas))
        .ok_or(TransformError::OutOfMemory)?;
    // Each row turned out is one row or column of the picture, read whole and
    // written whole rather than a pixel at a time.
    let back = how.inverse();
    for y in 0..height {
        let (x0, y0) = back.place(0, y, width, height);
        let (x1, y1) = back.place(width - 1, y, width, height);
        if y0 == y1 {
            canvas.row_samples(y0, x0.min(x1), &mut line);
        } else {
            canvas.column_samples(x0, y0.min(y1), &mut line);
        }
        if x0 > x1 || y0 > y1 {
            line.reverse();
        }
        out.set_row(y, &line);
    }
    Ok(out.finish())
}

fn flipped(canvas: &Canvas, vertical: bool) -> Result<Canvas, TransformError> {
    let kind = canvas.kind().clone();
    let bytes = kind.sample_bytes();
    let (width, height) = (canvas.width(), canvas.height());
    let mut out = CanvasBuilder::new(width, height, kind.clone(), first_sample(canvas))?;
    let (mut from, mut from_mask) = row_buffers(&kind, width)?;
    let (mut to, mut to_mask) = row_buffers(&kind, width)?;
    for y in 0..height {
        let source = if vertical { height - 1 - y } else { y };
        canvas.read_row(source, &mut from, &mut from_mask);
        if vertical {
            out.row(y, &from, &from_mask);
            continue;
        }
        let w = width as usize;
        for x in 0..w {
            to[x * bytes..(x + 1) * bytes]
                .copy_from_slice(&from[(w - 1 - x) * bytes..(w - x) * bytes]);
        }
        for x in 0..to_mask.len() {
            to_mask[x] = from_mask[w - 1 - x];
        }
        out.row(y, &to, &to_mask);
    }
    Ok(out.finish())
}

fn masked(canvas: &Canvas, on: bool, fill: u8) -> Result<Canvas, TransformError> {
    let Kind::Indexed {
        depth,
        palette,
        masked,
    } = canvas.kind()
    else {
        return Err(TransformError::NotApplicable);
    };
    if *masked == on || usize::from(fill) >= palette.len() {
        return Err(TransformError::NotApplicable);
    }
    let kind = Kind::Indexed {
        depth: *depth,
        palette: palette.clone(),
        masked: on,
    };
    let (width, height) = (canvas.width(), canvas.height());
    let mut out = CanvasBuilder::new(width, height, kind.clone(), Sample::Index(fill, 255))?;
    let (mut samples, mut mask) = row_buffers(canvas.kind(), width)?;
    let opaque = fallible::filled(width as usize, 255u8).ok_or(TransformError::OutOfMemory)?;
    for y in 0..height {
        canvas.read_row(y, &mut samples, &mut mask);
        if on {
            out.row(y, &samples, &opaque);
        } else {
            for (index, &alpha) in samples.iter_mut().zip(&mask) {
                if alpha < OPAQUE_FROM {
                    *index = fill;
                }
            }
            out.row(y, &samples, &[]);
        }
    }
    Ok(out.finish())
}

/// The palette a conversion to `depth` uses.
fn palette_of(
    canvas: &Canvas,
    depth: IndexDepth,
    choice: PaletteChoice,
) -> Result<Vec<Rgba8>, TransformError> {
    match choice {
        PaletteChoice::Desktop => {
            let colours = desktop_palette(depth);
            fallible::collected(
                colours.len(),
                colours.iter().map(|&[r, g, b]| [r, g, b, 255]),
            )
            .ok_or(TransformError::OutOfMemory)
        }
        PaletteChoice::Optimised => {
            palette_for(canvas, depth.colours()).map_err(|_| TransformError::OutOfMemory)
        }
    }
}

fn converted(
    canvas: &Canvas,
    depth: Depth,
    choice: PaletteChoice,
    dither: bool,
) -> Result<Canvas, TransformError> {
    let (width, height) = (canvas.width(), canvas.height());
    let depth = match depth {
        Depth::Rgba => {
            let mut colours = alloc::vec::Vec::new();
            if !fallible::grow_to(&mut colours, width as usize, [0u8; 4]) {
                return Err(TransformError::OutOfMemory);
            }
            let mut out = CanvasBuilder::new(width, height, Kind::Rgba, Sample::Rgba([0; 4]))?;
            for y in 0..height {
                canvas.row_colours(y, 0, &mut colours);
                out.row(y, colours.as_flattened(), &[]);
            }
            return Ok(out.finish());
        }
        Depth::Indexed(depth) => depth,
    };
    indexed(canvas, depth, &palette_of(canvas, depth, choice)?, dither)
}

/// `canvas` stored at `depth` in `palette`, each pixel its nearest entry
/// and the error that leaves spread to its neighbours when `dither`; a pixel
/// less than half opaque is masked out.
///
/// # Errors
///
/// [`TransformError`] where the picture cannot be held, or `palette` is not
/// one `depth` indexes.
pub fn indexed(
    canvas: &Canvas,
    depth: IndexDepth,
    palette: &[Rgba8],
    dither: bool,
) -> Result<Canvas, TransformError> {
    let (width, height) = (canvas.width(), canvas.height());
    let mut colours = alloc::vec::Vec::new();
    if !fallible::grow_to(&mut colours, width as usize, [0u8; 4]) {
        return Err(TransformError::OutOfMemory);
    }
    let masked = canvas.has_transparency();
    let mut nearest = Nearest::new(palette).ok_or(TransformError::OutOfMemory)?;
    let kind = Kind::Indexed {
        depth,
        palette: fallible::collected(palette.len(), palette.iter().copied())
            .ok_or(TransformError::OutOfMemory)?,
        masked,
    };
    let mut out = CanvasBuilder::new(width, height, kind, Sample::Index(0, 255))?;
    let (mut samples, mut mask) = (
        fallible::filled(width as usize, 0u8).ok_or(TransformError::OutOfMemory)?,
        fallible::filled(if masked { width as usize } else { 0 }, 0u8)
            .ok_or(TransformError::OutOfMemory)?,
    );
    let mut diffusion = if dither {
        Some(Diffusion::new(width).ok_or(TransformError::OutOfMemory)?)
    } else {
        None
    };
    for y in 0..height {
        canvas.row_colours(y, 0, &mut colours);
        if let Some(diffusion) = diffusion.as_mut() {
            diffusion.next_row();
        }
        for (x, colour) in colours.iter().enumerate() {
            let shown = colour[3] >= OPAQUE_FROM;
            if let Some(slot) = mask.get_mut(x) {
                *slot = if shown { 255 } else { 0 };
            }
            if !shown {
                samples[x] = 0;
                continue;
            }
            let want = match diffusion.as_ref() {
                Some(diffusion) => diffusion.adjusted(x, *colour),
                None => [colour[0], colour[1], colour[2], 255],
            };
            let index = nearest.find(want);
            samples[x] = index;
            if let Some(diffusion) = diffusion.as_mut() {
                diffusion.spread(x, want, palette[usize::from(index)]);
            }
        }
        out.row(y, &samples, &mask);
    }
    Ok(out.finish())
}

/// Floyd–Steinberg error diffusion: what each pixel's nearest entry missed
/// by, spread to the pixels right of it and below.
struct Diffusion {
    /// Error carried into this row and the next, three channels a pixel,
    /// one pixel of margin at each end.
    this: Vec<[i32; 3]>,
    next: Vec<[i32; 3]>,
}

impl Diffusion {
    fn new(width: u32) -> Option<Self> {
        let len = width as usize + 2;
        Some(Self {
            this: fallible::filled(len, [0; 3])?,
            next: fallible::filled(len, [0; 3])?,
        })
    }

    fn next_row(&mut self) {
        core::mem::swap(&mut self.this, &mut self.next);
        self.next.fill([0; 3]);
    }

    /// `colour` with the error carried to pixel `x`, opaque.
    fn adjusted(&self, x: usize, colour: Rgba8) -> Rgba8 {
        let carried = self.this[x + 1];
        let channel = |i: usize| {
            u8::try_from((i32::from(colour[i]) + carried[i] / 16).clamp(0, 255)).unwrap_or(0)
        };
        [channel(0), channel(1), channel(2), 255]
    }

    /// Spread what `chosen` missed `wanted` by, at pixel `x`.
    fn spread(&mut self, x: usize, wanted: Rgba8, chosen: Rgba8) {
        for channel in 0..3 {
            let error = i32::from(wanted[channel]) - i32::from(chosen[channel]);
            self.this[x + 2][channel] += error * 7;
            self.next[x][channel] += error * 3;
            self.next[x + 1][channel] += error * 5;
            self.next[x + 2][channel] += error;
        }
    }
}

#[cfg(test)]
#[path = "transform_tests.rs"]
mod tests;
