//! The effects: each pixel made from its neighbourhood, read from a window of
//! the picture cut with the effect's reach about the pixels it answers.
//!
//! An effect reads a place past its window's edge as the edge itself. A window
//! is cut with the effect's whole reach about what it answers wherever the
//! picture extends ([`tiled`](crate::tiled)), so the only edge an answer ever
//! sees is the picture's own, and an effect answers each pixel alike whichever
//! window it was cut as.

use alloc::vec::Vec;

use tairix_image::Rgba8;
use tairix_mix::{numbered, SplitMix64};
use tairix_raster::{box_blur16, Color, Pixel, Pixel16};
use tairix_util::{fallible, mathf};

use crate::canvas::{Canvas, OutOfMemory};
use crate::shape::Bounds;

/// The most a Gaussian blur's or an unsharp mask's radius reaches, in pixels.
pub const MOST_BLUR: i32 = 50;

/// The most an unsharp mask pushes, in percent.
pub const MOST_SHARPEN: i32 = 500;

/// The most a median's radius reaches, in pixels.
pub const MOST_MEDIAN: i32 = 50;

/// The most an emboss reads ahead of and behind a pixel, in pixels.
pub const MOST_EMBOSS: i32 = 10;

/// The strongest an emboss is, in percent.
pub const MOST_RELIEF: i32 = 500;

/// How far a uniform speckle reaches either way at full strength, in levels;
/// a Gaussian one's spread is half of it.
const NOISE_REACH: i32 = 128;

/// The pixels an effect reads: straight colours over `bounds` of a colour
/// picture, row by row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Window {
    bounds: Bounds,
    width: usize,
    height: usize,
    pixels: Vec<Rgba8>,
}

impl Window {
    /// `bounds` of `canvas`, lying within the picture, as its colours show.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] where the pixels cannot be held.
    pub fn read(canvas: &Canvas, bounds: Bounds) -> Result<Self, OutOfMemory> {
        let (width, height) = extent(bounds)?;
        let count = width.checked_mul(height).ok_or(OutOfMemory)?;
        let mut pixels = fallible::filled(count, [0u8; 4]).ok_or(OutOfMemory)?;
        if width > 0 {
            let x = u32::try_from(bounds.x0).map_err(|_| OutOfMemory)?;
            for (y, row) in (bounds.y0..).zip(pixels.chunks_exact_mut(width)) {
                canvas.row_colours(u32::try_from(y).map_err(|_| OutOfMemory)?, x, row);
            }
        }
        Ok(Self {
            bounds,
            width,
            height,
            pixels,
        })
    }

    /// The pixel at `(x, y)`, a place past the edge read as the edge.
    fn at(&self, x: i64, y: i64) -> Rgba8 {
        let at = self.offset(x, y);
        self.pixels.get(at).copied().unwrap_or([0; 4])
    }

    /// Where `(x, y)`, held to the window, lies among its pixels.
    fn offset(&self, x: i64, y: i64) -> usize {
        let b = self.bounds;
        let column = x.clamp(b.x0, (b.x1 - 1).max(b.x0)) - b.x0;
        let row = y.clamp(b.y0, (b.y1 - 1).max(b.y0)) - b.y0;
        usize::try_from(row).unwrap_or(0) * self.width + usize::try_from(column).unwrap_or(0)
    }
}

/// An effect as it is worked out: its settings, and what is made of them once
/// for a run.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    /// Box passes of these radii, together near a Gaussian.
    Gaussian([usize; 3]),
    /// Each channel pushed away from its Gaussian of `radii`.
    Unsharp {
        /// The blur's box passes.
        radii: [usize; 3],
        /// How far, in percent.
        amount: i32,
        /// The least difference from the blur, in levels, that is pushed.
        threshold: i32,
    },
    /// Each premultiplied channel the median of the square this many pixels
    /// either side.
    Median(usize),
    /// Speckle.
    Noise(Noise),
    /// Each colour channel's Sobel gradient strength.
    Edges,
    /// A grey relief, read `offset` toward the light and as far away from it.
    Emboss {
        /// Toward the light, in pixels across and down.
        offset: (i64, i64),
        /// How strong, in percent.
        amount: i32,
    },
    /// Squares this many pixels across, lined up on the picture's corner.
    Pixelate(i64),
}

impl Effect {
    /// A Gaussian blur of standard deviation `radius` pixels.
    #[must_use]
    pub fn gaussian(radius: i32) -> Self {
        Self::Gaussian(gaussian_radii(radius))
    }

    /// An unsharp mask pushing `amount` percent away from the Gaussian of
    /// `radius` wherever the difference is `threshold` levels or more.
    #[must_use]
    pub fn unsharp(amount: i32, radius: i32, threshold: i32) -> Self {
        Self::Unsharp {
            radii: gaussian_radii(radius),
            amount: amount.clamp(0, MOST_SHARPEN),
            threshold,
        }
    }

    /// A median of the square `radius` pixels either side.
    #[must_use]
    pub fn median(radius: i32) -> Self {
        Self::Median(usize::try_from(radius.clamp(0, MOST_MEDIAN)).unwrap_or(0))
    }

    /// A relief lit from `angle` degrees anticlockwise from the right, read
    /// `height` pixels ahead and behind, at `amount` percent.
    #[must_use]
    pub fn emboss(angle: i32, height: i32, amount: i32) -> Self {
        let turn = f64::from(angle) * core::f64::consts::PI / 180.0;
        let reach = f64::from(height.clamp(1, MOST_EMBOSS));
        // Rows run down the screen, so a light from above lies a row back.
        let offset = (
            i64::from(mathf::round_i32(reach * mathf::cos(turn))),
            i64::from(mathf::round_i32(-reach * mathf::sin(turn))),
        );
        Self::Emboss {
            offset,
            amount: amount.clamp(0, MOST_RELIEF),
        }
    }

    /// Squares `cell` pixels across.
    #[must_use]
    pub fn pixelate(cell: i32) -> Self {
        Self::Pixelate(i64::from(cell.max(1)))
    }

    /// How far about a pixel it reads, in pixels.
    #[must_use]
    pub fn reach(&self) -> u32 {
        let reach = match *self {
            Self::Gaussian(radii) | Self::Unsharp { radii, .. } => radii.iter().sum(),
            Self::Median(radius) => radius,
            Self::Noise(_) => 0,
            Self::Edges => 1,
            Self::Emboss { offset, .. } => reach_of(offset),
            Self::Pixelate(cell) => usize::try_from(cell.saturating_sub(1)).unwrap_or(usize::MAX),
        };
        u32::try_from(reach).unwrap_or(u32::MAX)
    }

    /// What it makes of `inner`, a part of `window` holding the effect's whole
    /// reach about it wherever the picture extends: straight colours, row by
    /// row across `inner`.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] where its room cannot be had.
    pub fn run(&self, window: &Window, inner: Bounds) -> Result<Vec<Rgba8>, OutOfMemory> {
        match *self {
            Self::Gaussian(radii) => {
                let blurred = deep_blurred(window, radii)?;
                each(window, inner, |x, y, colour| {
                    blurred_at(&blurred, window, x, y).unwrap_or(colour)
                })
            }
            Self::Unsharp {
                radii,
                amount,
                threshold,
            } => {
                let blurred = deep_blurred(window, radii)?;
                each(window, inner, |x, y, colour| {
                    let mean = blurred_at(&blurred, window, x, y).unwrap_or(colour);
                    sharpened(colour, mean, amount, threshold)
                })
            }
            Self::Median(radius) => median(window, inner, radius),
            Self::Noise(noise) => each(window, inner, |x, y, colour| noise.speckled(x, y, colour)),
            Self::Edges => each(window, inner, |x, y, colour| edge(window, (x, y), colour)),
            Self::Emboss { offset, amount } => each(window, inner, |x, y, colour| {
                relief(window, (x, y), offset, amount, colour)
            }),
            Self::Pixelate(cell) => pixelated(window, inner, cell),
        }
    }
}

/// How far an offset of `(across, down)` reaches.
fn reach_of((across, down): (i64, i64)) -> usize {
    usize::try_from(across.unsigned_abs().max(down.unsigned_abs())).unwrap_or(usize::MAX)
}

/// `bounds`' width and height.
fn extent(bounds: Bounds) -> Result<(usize, usize), OutOfMemory> {
    let width = usize::try_from(bounds.x1 - bounds.x0).map_err(|_| OutOfMemory)?;
    let height = usize::try_from(bounds.y1 - bounds.y0).map_err(|_| OutOfMemory)?;
    Ok((width, height))
}

/// `inner`'s pixels, each made by `make` from its place and its colour in
/// `window`.
fn each(
    window: &Window,
    inner: Bounds,
    make: impl Fn(i64, i64, Rgba8) -> Rgba8,
) -> Result<Vec<Rgba8>, OutOfMemory> {
    let (width, height) = extent(inner)?;
    let mut out = Vec::new();
    if !fallible::reserve(&mut out, width.checked_mul(height).ok_or(OutOfMemory)?) {
        return Err(OutOfMemory);
    }
    for y in inner.y0..inner.y1 {
        out.extend((inner.x0..inner.x1).map(|x| make(x, y, window.at(x, y))));
    }
    Ok(out)
}

/// `window` premultiplied at sixteen bits and blurred by a box pass of each of
/// `radii`, each pass rounding at sixteen bits, so a clear pixel lends no
/// colour.
fn deep_blurred(window: &Window, radii: [usize; 3]) -> Result<Vec<Pixel16>, OutOfMemory> {
    let pixels = &window.pixels;
    let deep = pixels
        .iter()
        .map(|&[r, g, b, a]| Color::rgba(r, g, b, a).premultiply16());
    let mut deep = fallible::collected(pixels.len(), deep).ok_or(OutOfMemory)?;
    if radii.iter().any(|&radius| radius > 0) {
        let mut aux = fallible::filled(pixels.len(), Pixel16::TRANSPARENT).ok_or(OutOfMemory)?;
        for radius in radii {
            box_blur16(&mut deep, window.width, window.height, radius, &mut aux);
        }
    }
    Ok(deep)
}

/// The blurred colour at `(x, y)` of `window`'s blur `blurred`.
fn blurred_at(blurred: &[Pixel16], window: &Window, x: i64, y: i64) -> Option<Rgba8> {
    let colour = blurred.get(window.offset(x, y))?.unpremultiply();
    Some([colour.r, colour.g, colour.b, colour.a])
}

/// `colour` pushed `amount` percent away from `mean`, its neighbourhood's, in
/// each channel that differs from it by `threshold` levels or more; its alpha
/// kept, and a clear pixel, which shows no colour, left as it is.
fn sharpened(colour: Rgba8, mean: Rgba8, amount: i32, threshold: i32) -> Rgba8 {
    if colour[3] == 0 {
        return colour;
    }
    let mut pushed = colour;
    for (level, mean) in pushed.iter_mut().zip(mean).take(3) {
        let difference = i32::from(*level) - i32::from(mean);
        if difference.abs() >= threshold {
            let moved = i32::from(*level) + (difference * amount + 50).div_euclid(100);
            *level = u8::try_from(moved.clamp(0, 255)).unwrap_or(0);
        }
    }
    pushed
}

/// Each colour channel's Sobel gradient strength at `(x, y)`, its alpha kept.
fn edge(window: &Window, (x, y): (i64, i64), colour: Rgba8) -> Rgba8 {
    let mut lit = colour;
    for (channel, slot) in lit.iter_mut().enumerate().take(3) {
        let v = |dx: i64, dy: i64| i32::from(window.at(x + dx, y + dy)[channel]);
        let gx = v(1, -1) + 2 * v(1, 0) + v(1, 1) - v(-1, -1) - 2 * v(-1, 0) - v(-1, 1);
        let gy = v(-1, 1) + 2 * v(0, 1) + v(1, 1) - v(-1, -1) - 2 * v(0, -1) - v(1, -1);
        *slot = u8::try_from((gx * gx + gy * gy).isqrt().min(255)).unwrap_or(u8::MAX);
    }
    lit
}

/// The relief at `(x, y)`: mid grey lifted by `amount` percent of how much
/// higher the picture stands `offset` behind the pixel than as far ahead of it
/// toward the light — a face turned to the light — its height its luma weighed
/// by its alpha, so a shape over clear ground stands up from it.
fn relief(
    window: &Window,
    (x, y): (i64, i64),
    (dx, dy): (i64, i64),
    amount: i32,
    colour: Rgba8,
) -> Rgba8 {
    let height = |px: i64, py: i64| {
        let [red, green, blue, alpha] = window.at(px, py);
        (i32::from(Color::rgb(red, green, blue).luma()) * i32::from(alpha) + 127) / 255
    };
    let rise = height(x - dx, y - dy) - height(x + dx, y + dy);
    let level = 128 + (rise * amount + 100).div_euclid(200);
    let grey = u8::try_from(level.clamp(0, 255)).unwrap_or(0);
    [grey, grey, grey, colour[3]]
}

/// `inner` as squares `cell` pixels across lined up on the picture's own
/// corner, each the premultiplied mean of what it covers.
fn pixelated(window: &Window, inner: Bounds, cell: i64) -> Result<Vec<Rgba8>, OutOfMemory> {
    let (width, height) = extent(inner)?;
    let count = width.checked_mul(height).ok_or(OutOfMemory)?;
    let mut out = fallible::filled(count, [0u8; 4]).ok_or(OutOfMemory)?;
    let cell = cell.max(1);
    let mut top = inner.y0.div_euclid(cell) * cell;
    while top < inner.y1 {
        let mut left = inner.x0.div_euclid(cell) * cell;
        while left < inner.x1 {
            let square = Bounds {
                x0: left,
                y0: top,
                x1: left + cell,
                y1: top + cell,
            };
            let mean = cell_mean(window, square.intersection(&window.bounds));
            let written = square.intersection(&inner);
            for y in written.y0..written.y1 {
                let row = usize::try_from(y - inner.y0).unwrap_or(0) * width;
                for x in written.x0..written.x1 {
                    let at = row + usize::try_from(x - inner.x0).unwrap_or(0);
                    if let Some(slot) = out.get_mut(at) {
                        *slot = mean;
                    }
                }
            }
            left += cell;
        }
        top += cell;
    }
    Ok(out)
}

/// The premultiplied mean of `square`'s pixels in `window`, as a straight
/// colour: clear where none of them shows.
fn cell_mean(window: &Window, square: Bounds) -> Rgba8 {
    let mut sum = [0u64; 4];
    let mut pixels = 0u64;
    for y in square.y0..square.y1 {
        for x in square.x0..square.x1 {
            let [r, g, b, a] = window.at(x, y);
            let weight = u64::from(a);
            sum[0] += u64::from(r) * weight;
            sum[1] += u64::from(g) * weight;
            sum[2] += u64::from(b) * weight;
            sum[3] += weight;
            pixels += 1;
        }
    }
    let weight = sum[3];
    if weight == 0 {
        return [0; 4];
    }
    let channel = |total: u64| u8::try_from((total + weight / 2) / weight).unwrap_or(u8::MAX);
    let alpha = u8::try_from((weight + pixels / 2) / pixels.max(1)).unwrap_or(u8::MAX);
    [channel(sum[0]), channel(sum[1]), channel(sum[2]), alpha]
}

/// How noise's speckle is spread about none.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Distribution {
    /// Every offset within its reach as likely.
    #[default]
    Uniform,
    /// Offsets bunched about none, each the sum of twelve uniform draws
    /// (Irwin–Hall), near a normal spread.
    Gaussian,
}

impl Distribution {
    /// Both, as a list of them reads.
    pub const ALL: [Self; 2] = [Self::Uniform, Self::Gaussian];

    /// What a list calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Uniform => "Uniform",
            Self::Gaussian => "Gaussian",
        }
    }

    /// Where it sits in [`ALL`](Self::ALL).
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Uniform => 0,
            Self::Gaussian => 1,
        }
    }
}

/// Add noise's settings.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Noise {
    /// How strong, `0..=100` percent: at 100 a uniform offset reaches 128
    /// levels either way, and a Gaussian one has a spread of 64.
    pub amount: i32,
    /// How its offsets are spread.
    pub distribution: Distribution,
    /// Whether one offset moves all three channels, so the speckle is grey.
    pub monochrome: bool,
    /// What the speckle is drawn from.
    pub seed: u64,
}

impl Noise {
    /// Speckle at its starting strength: uniform, in colour.
    pub const START: Self = Self {
        amount: 20,
        distribution: Distribution::Uniform,
        monochrome: false,
        seed: 0x5eed,
    };

    /// `colour` at `(x, y)` with its speckle laid on; a clear pixel, which
    /// shows no colour, is left as it is.
    #[must_use]
    pub fn speckled(&self, x: i64, y: i64, colour: Rgba8) -> Rgba8 {
        if colour[3] == 0 || self.amount <= 0 {
            return colour;
        }
        let offsets = self.offsets(x, y);
        let mut speckled = colour;
        for (level, offset) in speckled.iter_mut().zip(offsets) {
            *level = u8::try_from((i32::from(*level) + offset).clamp(0, 255)).unwrap_or(0);
        }
        speckled
    }

    /// The offsets, in levels, the three channels of pixel `(x, y)` take:
    /// drawn from the seed and the place alone, in integers, so they are the
    /// same on every target and however the picture is cut.
    #[must_use]
    pub fn offsets(&self, x: i64, y: i64) -> [i32; 3] {
        let place = (y.unsigned_abs() << 32) | (x.unsigned_abs() & u64::from(u32::MAX));
        let mut draws = SplitMix64::new(numbered(self.seed, place));
        let reach = self.amount.clamp(0, 100) * NOISE_REACH / 100;
        let mut draw = || match self.distribution {
            Distribution::Uniform => uniform(draws.next_u64(), reach),
            Distribution::Gaussian => irwin_hall(draws.next_u64(), draws.next_u64(), reach),
        };
        if self.monochrome {
            [draw(); 3]
        } else {
            [draw(), draw(), draw()]
        }
    }
}

/// A uniform offset within `reach` levels either way: the top sixteen bits of
/// `word` spread over the `2 · reach + 1` offsets.
fn uniform(word: u64, reach: i32) -> i32 {
    let offsets = u64::try_from(2 * reach + 1).unwrap_or(1);
    i32::try_from(((word >> 48) * offsets) >> 16).unwrap_or(0) - reach
}

/// A Gaussian offset spread `reach / 2` levels: the eight bytes of `first` and
/// the low four of `second` summed (Irwin–Hall), whose mean is 1530 and whose
/// spread is within a hundredth of 256.
fn irwin_hall(first: u64, second: u64, reach: i32) -> i32 {
    let low = second.to_le_bytes();
    let bytes = first
        .to_le_bytes()
        .into_iter()
        .chain(low.into_iter().take(4));
    let sum: i32 = bytes.map(i32::from).sum();
    ((sum - 1530) * reach + 256).div_euclid(512)
}

/// The radii of the three box passes nearest a Gaussian of standard deviation
/// `sigma` pixels, held to [`MOST_BLUR`]: Kovesi's widths ("Fast
/// Almost-Gaussian Filtering", DICTA 2010, after Wells, IEEE PAMI 1986), the
/// widest odd width `w` with `w² <= 12σ²/3 + 1` for the first passes and `w + 2`
/// for the rest, as many of each as brings their variances `(w² − 1) / 12`
/// nearest `σ²`.
#[must_use]
pub fn gaussian_radii(sigma: i32) -> [usize; 3] {
    const PASSES: u64 = 3;
    let sigma = u64::from(sigma.clamp(0, MOST_BLUR).unsigned_abs());
    let twelve_variance = 12 * sigma * sigma;
    let ideal = (twelve_variance / PASSES + 1).isqrt();
    let narrow = if ideal.is_multiple_of(2) {
        ideal - 1
    } else {
        ideal
    };
    // Kovesi's (12σ² − n·w² − 4n·w − 3n) / (−4w − 4), rounded; never negative,
    // as w <= √(12σ²/n + 1) < w + 2.
    let over = (PASSES * narrow * narrow + 4 * PASSES * narrow + 3 * PASSES)
        .saturating_sub(twelve_variance);
    let per_pass = 4 * narrow + 4;
    let narrow_passes = ((2 * over + per_pass) / (2 * per_pass)).min(PASSES);
    let mut radii = [0; 3];
    for (pass, radius) in (0..).zip(&mut radii) {
        let width = if pass < narrow_passes {
            narrow
        } else {
            narrow + 2
        };
        *radius = usize::try_from((width - 1) / 2).unwrap_or(0);
    }
    radii
}

/// One channel's level counts, at 256 fine levels and 16 coarse ones each the
/// sum of 16 fine: the histogram the constant-time median keeps for every
/// column and for the kernel (Perreault and Hébert, "Median Filtering in
/// Constant Time", IEEE Transactions on Image Processing 16(9), 2007).
#[derive(Clone)]
struct Counts {
    coarse: [u16; 16],
    fine: [u16; 256],
}

impl Counts {
    /// Nothing counted.
    const NONE: Self = Self {
        coarse: [0; 16],
        fine: [0; 256],
    };

    /// Count one more of `level`.
    fn add(&mut self, level: u8) {
        let coarse = &mut self.coarse[usize::from(level >> 4)];
        *coarse = coarse.wrapping_add(1);
        let fine = &mut self.fine[usize::from(level)];
        *fine = fine.wrapping_add(1);
    }

    /// Count one fewer of `level`.
    fn remove(&mut self, level: u8) {
        let coarse = &mut self.coarse[usize::from(level >> 4)];
        *coarse = coarse.wrapping_sub(1);
        let fine = &mut self.fine[usize::from(level)];
        *fine = fine.wrapping_sub(1);
    }

    /// Take in every count `entering` holds and give up every count `leaving`
    /// holds.
    fn slide(&mut self, entering: &Self, leaving: &Self) {
        let coarse = entering.coarse.iter().zip(&leaving.coarse);
        for (held, (enter, leave)) in self.coarse.iter_mut().zip(coarse) {
            *held = held.wrapping_add(*enter).wrapping_sub(*leave);
        }
        let fine = entering.fine.iter().zip(&leaving.fine);
        for (held, (enter, leave)) in self.fine.iter_mut().zip(fine) {
            *held = held.wrapping_add(*enter).wrapping_sub(*leave);
        }
    }

    /// The level at which the counts, from black up, pass `rank`: the coarse
    /// levels walked first, then the fine levels of the one it falls in.
    fn level_past(&self, rank: u32) -> u8 {
        let mut seen = 0u32;
        for (high, &count) in self.coarse.iter().enumerate() {
            let through = seen + u32::from(count);
            if through > rank {
                for (low, &fine) in self.fine.iter().skip(high * 16).take(16).enumerate() {
                    seen += u32::from(fine);
                    if seen > rank {
                        return u8::try_from(high * 16 + low).unwrap_or(u8::MAX);
                    }
                }
            }
            seen = through;
        }
        u8::MAX
    }
}

/// Count a pixel's four channel `levels` into `counts` by `change`.
fn count(counts: &mut [Counts], levels: [u8; 4], change: fn(&mut Counts, u8)) {
    for (counts, level) in counts.iter_mut().zip(levels) {
        change(counts, level);
    }
}

/// Each channel of `inner` the median of the square `radius` either side of
/// it over premultiplied levels, so every colour channel stays within its
/// alpha: every column's counts slid a row at a time and the kernel's a column
/// at a time, so a pixel costs the same at any radius.
fn median(window: &Window, inner: Bounds, radius: usize) -> Result<Vec<Rgba8>, OutOfMemory> {
    if radius == 0 {
        return each(window, inner, |_, _, colour| colour);
    }
    let premultiplied = window.pixels.iter().map(|&[r, g, b, a]| {
        let pixel = Color::rgba(r, g, b, a).premultiply();
        [pixel.r, pixel.g, pixel.b, pixel.a]
    });
    let levels = fallible::collected(window.pixels.len(), premultiplied).ok_or(OutOfMemory)?;
    let level_at = |x: i64, y: i64| levels.get(window.offset(x, y)).copied().unwrap_or([0; 4]);
    let reach = i64::try_from(radius).map_err(|_| OutOfMemory)?;
    let side = u32::try_from(2 * radius + 1).map_err(|_| OutOfMemory)?;
    let rank = side * side / 2;
    let held = window.width.checked_mul(4).ok_or(OutOfMemory)?;
    let mut columns = fallible::filled(held, Counts::NONE).ok_or(OutOfMemory)?;
    let (width, height) = extent(inner)?;
    let mut out = Vec::new();
    if !fallible::reserve(&mut out, width.checked_mul(height).ok_or(OutOfMemory)?) {
        return Err(OutOfMemory);
    }
    let bounds = window.bounds;
    for y in inner.y0..inner.y1 {
        for (x, counts) in (bounds.x0..).zip(columns.as_chunks_mut::<4>().0) {
            if y == inner.y0 {
                for dy in -reach..=reach {
                    count(counts, level_at(x, y + dy), Counts::add);
                }
            } else {
                count(counts, level_at(x, y - 1 - reach), Counts::remove);
                count(counts, level_at(x, y + reach), Counts::add);
            }
        }
        let column = |x: i64| {
            let held = x.clamp(bounds.x0, (bounds.x1 - 1).max(bounds.x0)) - bounds.x0;
            let at = usize::try_from(held).unwrap_or(0) * 4;
            columns.get(at..at + 4).unwrap_or_default()
        };
        let mut kernel: [Counts; 4] = core::array::from_fn(|_| Counts::NONE);
        for dx in -reach..=reach {
            for (kept, own) in kernel.iter_mut().zip(column(inner.x0 + dx)) {
                kept.slide(own, &Counts::NONE);
            }
        }
        for x in inner.x0..inner.x1 {
            if x > inner.x0 {
                let moving = column(x + reach).iter().zip(column(x - 1 - reach));
                for (kept, (entering, leaving)) in kernel.iter_mut().zip(moving) {
                    kept.slide(entering, leaving);
                }
            }
            let [r, g, b, a] = [0, 1, 2, 3].map(|channel| kernel[channel].level_past(rank));
            let colour = Pixel { r, g, b, a }.unpremultiply();
            out.push([colour.r, colour.g, colour.b, colour.a]);
        }
    }
    Ok(out)
}

/// A colour picture whose every pixel differs and whose every fifth is half
/// clear: what the operations' tests run over, so a slip reading or writing
/// one pixel cannot hide behind a flat field.
#[cfg(test)]
pub(crate) fn patterned(width: u32, height: u32) -> Canvas {
    use crate::canvas::{CanvasBuilder, Kind, Sample};
    let mut built = CanvasBuilder::new(width, height, Kind::Rgba, Sample::Rgba([0; 4]))
        .expect("a picture the tests can hold");
    for y in 0..height {
        for x in 0..width {
            let level =
                |k: u32| u8::try_from((x * (7 + 2 * k) + y * (13 + k) + k * 41) % 256).unwrap_or(0);
            let alpha = if (x + y).is_multiple_of(5) { 128 } else { 255 };
            built.set(x, y, Sample::Rgba([level(0), level(1), level(2), alpha]));
        }
    }
    built.finish()
}

/// Every pixel of `canvas`, row by row.
#[cfg(test)]
pub(crate) fn pixels(canvas: &Canvas) -> Vec<Rgba8> {
    let mut all = Vec::new();
    for y in 0..canvas.height() {
        for x in 0..canvas.width() {
            all.push(canvas.colour_at(x, y).unwrap_or([0; 4]));
        }
    }
    all
}

#[cfg(test)]
#[path = "effect_tests.rs"]
mod tests;
