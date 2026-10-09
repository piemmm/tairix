//! Adjustments and filters.
//!
//! An adjustment maps each colour on its own — brightness and contrast, hue
//! and saturation by range, colour balance, levels, curves, white balance,
//! posterising, a threshold, grey — so on a palette picture it maps the
//! palette. A filter makes each pixel from its neighbours — a blur,
//! sharpening, pixelation, noise, edges — which only a colour picture's pixels
//! can take. Both are held to the selection, as much of each pixel as it
//! chooses, and run on a worker.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_colour::Rgb;
use tairix_hash::FastHash;
use tairix_image::Rgba8;
use tairix_raster::{box_blur, Color, SOFTEN_PASSES};
use tairix_util::{fallible, mathf};

use crate::canvas::{Canvas, Kind, OutOfMemory, Sample, Tile, TILE};
use crate::compose::between;
use crate::mask::Mask;
use crate::shape::Bounds;
use crate::stroke::Change;
use crate::tone::{to_level, ColourBalance, Curves, HueRanges, Levels, Table, WhiteBalance};

/// One adjustment or filter and what it is set to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Filter {
    /// Brightness and contrast, each `-100..=100`.
    Brightness {
        /// Lighter or darker.
        brightness: i32,
        /// Further from or nearer to mid grey.
        contrast: i32,
    },
    /// Hue, saturation and lightness for every colour and for each range.
    HueSaturation(HueRanges),
    /// Colours moved toward red, green or blue in each band of tones.
    ColourBalance(ColourBalance),
    /// Each channel's input and output levels.
    Levels(Levels),
    /// Each channel's tone curve.
    Curves(Curves),
    /// The light the picture was lit by, corrected to daylight.
    WhiteBalance(WhiteBalance),
    /// Each channel held to so many levels.
    Posterize {
        /// The levels each channel keeps.
        levels: i32,
    },
    /// Black below a level of brightness, white from it.
    Threshold {
        /// The brightness from which a pixel is white.
        level: i32,
    },
    /// Each colour its grey.
    Desaturate,
    /// A blur reaching `radius` pixels.
    Blur {
        /// How far it reaches.
        radius: i32,
    },
    /// Sharpening: each pixel pushed away from its blurred neighbourhood.
    Sharpen {
        /// How far, in percent.
        amount: i32,
        /// How far the neighbourhood reaches.
        radius: i32,
    },
    /// Squares of one colour, each the mean of what it covers.
    Pixelate {
        /// How wide each square is.
        cell: i32,
    },
    /// Speckle, the same wherever the picture is read, from `seed`.
    Noise {
        /// How strong, in percent.
        amount: i32,
        /// What the speckle is drawn from.
        seed: u64,
    },
    /// The edges between colours lit, all else dark.
    Edges,
    /// Every colour its opposite, opacity kept: an adjustment of its own
    /// command rather than one of the menu's.
    Invert,
}

/// One number a filter is set by: what its slider says and holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Parameter {
    /// What its slider is labelled.
    pub label: &'static str,
    /// The least it holds.
    pub least: i32,
    /// The most it holds.
    pub most: i32,
}

impl Parameter {
    /// Where `value` lies along the slider, in thousandths.
    #[must_use]
    pub fn permille_of(&self, value: i32) -> u16 {
        let span = i64::from(self.most - self.least).max(1);
        let along = i64::from(value.clamp(self.least, self.most) - self.least);
        u16::try_from(along * 1000 / span).unwrap_or(1000)
    }

    /// The value `permille` thousandths along the slider, to the nearest.
    #[must_use]
    pub fn value_of(&self, permille: u16) -> i32 {
        let span = i64::from(self.most - self.least);
        let along = (i64::from(permille.min(1000)) * span + 500) / 1000;
        i32::try_from(i64::from(self.least) + along).unwrap_or(self.least)
    }

    /// The slider's steps, in thousandths: a line moves one, a page ten.
    #[must_use]
    pub fn steps(&self) -> (u16, u16) {
        let span = u16::try_from(self.most - self.least).unwrap_or(1).max(1);
        let line = 1000u16.div_ceil(span);
        (line, line.saturating_mul(10).min(1000))
    }
}

/// What an adjustment maps colours through, worked out once for a whole
/// picture or palette.
#[allow(
    clippy::large_enum_variant,
    reason = "one lives on the stack for the length of a filter's run; boxing the tables would allocate for nothing"
)]
enum Prepared {
    /// Each channel through its own table.
    Tables([Table; 3]),
    /// Colour balance's moves, by lightness.
    Shifts([[i16; 256]; 3]),
    /// Worked out a colour at a time.
    Direct,
}

impl Filter {
    /// Every filter as the menu offers it, at its starting settings: the
    /// adjustments, then the filters.
    pub const ALL: [Self; 14] = [
        Self::Brightness {
            brightness: 0,
            contrast: 0,
        },
        Self::HueSaturation(HueRanges::IDENTITY),
        Self::ColourBalance(ColourBalance::NEUTRAL),
        Self::Levels(Levels::IDENTITY),
        Self::Curves(Curves::IDENTITY),
        Self::WhiteBalance(WhiteBalance::NEUTRAL),
        Self::Posterize { levels: 4 },
        Self::Threshold { level: 128 },
        Self::Desaturate,
        Self::Blur { radius: 2 },
        Self::Sharpen {
            amount: 100,
            radius: 2,
        },
        Self::Pixelate { cell: 8 },
        Self::Noise {
            amount: 20,
            seed: 0x5eed,
        },
        Self::Edges,
    ];

    /// What the menu calls it.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Brightness { .. } => "Brightness and contrast",
            Self::HueSaturation(_) => "Hue and saturation",
            Self::ColourBalance(_) => "Colour balance",
            Self::Levels(_) => "Levels",
            Self::Curves(_) => "Curves",
            Self::WhiteBalance(_) => "White balance",
            Self::Posterize { .. } => "Posterise",
            Self::Threshold { .. } => "Threshold",
            Self::Desaturate => "Desaturate",
            Self::Blur { .. } => "Blur",
            Self::Sharpen { .. } => "Sharpen",
            Self::Pixelate { .. } => "Pixelate",
            Self::Noise { .. } => "Add noise",
            Self::Edges => "Find edges",
            Self::Invert => "Invert colours",
        }
    }

    /// Whether `other` is the same adjustment, however each is set.
    #[must_use]
    pub fn same_kind(&self, other: &Self) -> bool {
        core::mem::discriminant(self) == core::mem::discriminant(other)
    }

    /// Whether it makes each pixel from its neighbours, which only a colour
    /// picture can take; an adjustment maps colours alone.
    #[must_use]
    pub const fn neighbourly(&self) -> bool {
        matches!(
            self,
            Self::Blur { .. }
                | Self::Sharpen { .. }
                | Self::Pixelate { .. }
                | Self::Noise { .. }
                | Self::Edges
        )
    }

    /// Whether it is set before it is applied; one with nothing to set
    /// applies at once.
    #[must_use]
    pub const fn has_settings(&self) -> bool {
        !matches!(self, Self::Desaturate | Self::Edges | Self::Invert)
    }

    /// Whether, as set, it changes nothing.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        match self {
            Self::Brightness {
                brightness,
                contrast,
            } => *brightness == 0 && *contrast == 0,
            Self::HueSaturation(ranges) => ranges.is_identity(),
            Self::ColourBalance(balance) => balance.is_identity(),
            Self::Levels(levels) => *levels == Levels::IDENTITY,
            Self::Curves(curves) => curves.is_identity(),
            Self::WhiteBalance(balance) => *balance == WhiteBalance::NEUTRAL,
            _ => false,
        }
    }

    /// The numbers it is set by, in its sliders' order; the adjustments set
    /// by channel, range or band have settings of their own.
    #[must_use]
    pub const fn parameters(&self) -> &'static [Parameter] {
        const fn p(label: &'static str, least: i32, most: i32) -> Parameter {
            Parameter { label, least, most }
        }
        const BRIGHTNESS: [Parameter; 2] = [p("Brightness", -100, 100), p("Contrast", -100, 100)];
        const POSTERIZE: [Parameter; 1] = [p("Levels", 2, 64)];
        const THRESHOLD: [Parameter; 1] = [p("Level", 0, 255)];
        const BLUR: [Parameter; 1] = [p("Radius", 1, 64)];
        const SHARPEN: [Parameter; 2] = [p("Amount", 1, 500), p("Radius", 1, 32)];
        const PIXELATE: [Parameter; 1] = [p("Size", 2, 128)];
        const NOISE: [Parameter; 1] = [p("Amount", 1, 100)];
        match self {
            Self::Brightness { .. } => &BRIGHTNESS,
            Self::Posterize { .. } => &POSTERIZE,
            Self::Threshold { .. } => &THRESHOLD,
            Self::Blur { .. } => &BLUR,
            Self::Sharpen { .. } => &SHARPEN,
            Self::Pixelate { .. } => &PIXELATE,
            Self::Noise { .. } => &NOISE,
            Self::HueSaturation(_)
            | Self::ColourBalance(_)
            | Self::Levels(_)
            | Self::Curves(_)
            | Self::WhiteBalance(_)
            | Self::Desaturate
            | Self::Edges
            | Self::Invert => &[],
        }
    }

    /// The value of number `index`.
    #[must_use]
    pub const fn value(&self, index: usize) -> i32 {
        const fn nth(values: &[i32], index: usize) -> i32 {
            if index < values.len() {
                values[index]
            } else {
                0
            }
        }
        match *self {
            Self::Brightness {
                brightness,
                contrast,
            } => nth(&[brightness, contrast], index),
            Self::Posterize { levels: v }
            | Self::Threshold { level: v }
            | Self::Blur { radius: v }
            | Self::Pixelate { cell: v }
            | Self::Noise { amount: v, .. } => nth(&[v], index),
            Self::Sharpen { amount, radius } => nth(&[amount, radius], index),
            _ => 0,
        }
    }

    /// Set number `index` to `value`, held to its bounds.
    pub fn set(&mut self, index: usize, value: i32) {
        fn put<const N: usize>(slots: [&mut i32; N], index: usize, value: i32) {
            if let Some(slot) = slots.into_iter().nth(index) {
                *slot = value;
            }
        }
        let Some(parameter) = self.parameters().get(index) else {
            return;
        };
        let v = value.clamp(parameter.least, parameter.most);
        match self {
            Self::Brightness {
                brightness,
                contrast,
            } => put([brightness, contrast], index, v),
            Self::Posterize { levels: slot }
            | Self::Threshold { level: slot }
            | Self::Blur { radius: slot }
            | Self::Pixelate { cell: slot }
            | Self::Noise { amount: slot, .. } => put([slot], index, v),
            Self::Sharpen { amount, radius } => put([amount, radius], index, v),
            _ => {}
        }
    }

    /// How far a pixel's neighbours reach, in pixels: a blur's passes reach
    /// their radius each.
    fn reach(&self) -> i64 {
        match *self {
            Self::Blur { radius } | Self::Sharpen { radius, .. } => {
                let pass = u64::try_from(radius)
                    .unwrap_or(1)
                    .div_ceil(u64::from(SOFTEN_PASSES));
                i64::try_from(pass * u64::from(SOFTEN_PASSES)).unwrap_or(0)
            }
            Self::Pixelate { cell } => i64::from(cell),
            Self::Edges => 1,
            _ => 0,
        }
    }

    /// What an adjustment maps through, worked out once.
    fn prepare(&self) -> Prepared {
        match self {
            Self::ColourBalance(balance) => Prepared::Shifts(balance.shifts()),
            Self::Levels(levels) => Prepared::Tables(levels.tables()),
            Self::Curves(curves) => Prepared::Tables(curves.tables()),
            Self::WhiteBalance(balance) => Prepared::Tables(balance.tables()),
            other => other
                .table()
                .map_or(Prepared::Direct, |table| Prepared::Tables([table; 3])),
        }
    }

    /// The colour it makes of `colour`, which keeps its alpha.
    fn map(&self, colour: Rgba8, prepared: &Prepared) -> Rgba8 {
        let [r, g, b, a] = colour;
        let mapped = match (self, prepared) {
            (_, Prepared::Tables(tables)) => [
                tables[0][usize::from(r)],
                tables[1][usize::from(g)],
                tables[2][usize::from(b)],
            ],
            (Self::ColourBalance(balance), Prepared::Shifts(shifts)) => {
                balance.map(Rgb::new(r, g, b), shifts).to_array()
            }
            (Self::Threshold { level }, _) => {
                let lit = i32::from(Color::rgb(r, g, b).luma()) >= *level;
                [if lit { u8::MAX } else { 0 }; 3]
            }
            (Self::Desaturate, _) => [Color::rgb(r, g, b).luma(); 3],
            (Self::HueSaturation(ranges), _) => ranges.map(Rgb::new(r, g, b)).to_array(),
            _ => [r, g, b],
        };
        [mapped[0], mapped[1], mapped[2], a]
    }

    /// The table an adjustment that maps each channel alike maps through.
    fn table(&self) -> Option<Table> {
        let mut table = [0u8; 256];
        for (input, slot) in (0u8..=255).zip(table.iter_mut()) {
            let v = f64::from(input);
            *slot = match *self {
                Self::Brightness {
                    brightness,
                    contrast,
                } => {
                    let contrast = f64::from(contrast.clamp(-100, 99));
                    let factor = if contrast >= 0.0 {
                        100.0 / (100.0 - contrast)
                    } else {
                        (100.0 + contrast) / 100.0
                    };
                    to_level((v - 127.5) * factor + 127.5 + f64::from(brightness) * 2.55)
                }
                Self::Posterize { levels } => {
                    let steps = f64::from(levels.max(2) - 1);
                    let band = f64::from(mathf::round_i32(v * steps / 255.0));
                    to_level(band * 255.0 / steps)
                }
                Self::Invert => 255 - input,
                _ => return None,
            };
        }
        Some(table)
    }

    /// `palette`, mapped as this adjustment maps colours; `None` for a
    /// filter, which a palette cannot take.
    #[must_use]
    pub fn mapped_palette(&self, palette: &[Rgba8]) -> Option<Vec<Rgba8>> {
        if self.neighbourly() {
            return None;
        }
        let prepared = self.prepare();
        fallible::collected(
            palette.len(),
            palette.iter().map(|&colour| self.map(colour, &prepared)),
        )
    }
}

/// What a filter could not do.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FilterError {
    /// There was not the memory for it.
    OutOfMemory,
    /// It makes pixels from their neighbours, which a palette picture's
    /// pixels, one entry each, cannot take.
    NeedsColour,
}

impl From<OutOfMemory> for FilterError {
    fn from(_: OutOfMemory) -> Self {
        Self::OutOfMemory
    }
}

/// Apply `filter` to colour picture `picture`, within `clip` where a
/// selection is held: the worker's work, answering each tile written as it
/// now stands.
///
/// # Errors
///
/// [`FilterError`] where the picture holds a palette or a buffer cannot be
/// had.
pub fn apply(
    picture: &mut Canvas,
    filter: &Filter,
    clip: Option<&Mask>,
) -> Result<Vec<(usize, Arc<Tile>)>, FilterError> {
    if !matches!(picture.kind(), Kind::Rgba) {
        return Err(FilterError::NeedsColour);
    }
    let whole = Bounds::picture(picture.width(), picture.height());
    let area = clip.map_or(whole, |clip| clip.bounds().intersection(&whole));
    if area.is_empty() {
        return Ok(Vec::new());
    }
    let made = if filter.neighbourly() {
        Some(neighbourhood(picture, filter, area)?)
    } else {
        None
    };
    let prepared = filter.prepare();
    let mut chosen = [u8::MAX; TILE as usize];
    let mut change = Change::new();
    change.repaint(picture, area, |x, y, run| {
        let chosen = &mut chosen[..run.len()];
        if let Some(clip) = clip {
            clip.row(y, x, chosen);
        }
        for ((column, sample), &share) in (x..).zip(run.iter_mut()).zip(chosen.iter()) {
            let Sample::Rgba(colour) = *sample else {
                continue;
            };
            if share == 0 {
                continue;
            }
            let filtered = match &made {
                Some(made) => made.at(column, y),
                None => filter.map(colour, &prepared),
            };
            *sample = Sample::Rgba(match share {
                u8::MAX => filtered,
                share => between(colour, filtered, share),
            });
        }
    })?;
    Ok(change.written(picture)?)
}

/// What a neighbourly filter makes of `area` of a picture: its pixels, read
/// with the reach it needs about them.
struct Made {
    bounds: Bounds,
    pixels: Vec<Rgba8>,
}

impl Made {
    fn at(&self, x: i64, y: i64) -> Rgba8 {
        let width = self.bounds.x1 - self.bounds.x0;
        usize::try_from((y - self.bounds.y0) * width + (x - self.bounds.x0))
            .ok()
            .and_then(|at| self.pixels.get(at))
            .copied()
            .unwrap_or([0; 4])
    }
}

/// Run neighbourly `filter` over `area` of `picture`, reading as far about
/// it as the filter reaches.
fn neighbourhood(picture: &Canvas, filter: &Filter, area: Bounds) -> Result<Made, OutOfMemory> {
    let reach = filter.reach();
    let whole = Bounds::picture(picture.width(), picture.height());
    let read = Bounds {
        x0: area.x0 - reach,
        y0: area.y0 - reach,
        x1: area.x1 + reach,
        y1: area.y1 + reach,
    }
    .intersection(&whole);
    let width = usize::try_from(read.x1 - read.x0).map_err(|_| OutOfMemory)?;
    let height = usize::try_from(read.y1 - read.y0).map_err(|_| OutOfMemory)?;
    let count = width.checked_mul(height).ok_or(OutOfMemory)?;
    let mut straight = fallible::filled(count, [0u8; 4]).ok_or(OutOfMemory)?;
    let x0 = u32::try_from(read.x0).map_err(|_| OutOfMemory)?;
    for (y, row) in (read.y0..).zip(straight.chunks_exact_mut(width)) {
        picture.row_colours(u32::try_from(y).map_err(|_| OutOfMemory)?, x0, row);
    }
    let pixels = match *filter {
        Filter::Blur { radius } => blurred(&straight, (width, height), radius)?,
        Filter::Sharpen { amount, radius } => {
            let soft = blurred(&straight, (width, height), radius)?;
            let push = |o: u8, s: u8| {
                let pushed = i32::from(o) + (i32::from(o) - i32::from(s)) * amount / 100;
                u8::try_from(pushed.clamp(0, 255)).unwrap_or(0)
            };
            fallible::collected(
                count,
                straight
                    .iter()
                    .zip(&soft)
                    .map(|(&[r, g, b, a], &[sr, sg, sb, _])| {
                        [push(r, sr), push(g, sg), push(b, sb), a]
                    }),
            )
            .ok_or(OutOfMemory)?
        }
        Filter::Pixelate { cell } => pixelated(&straight, (width, height), read, cell)?,
        Filter::Noise { amount, seed } => fallible::collected(
            count,
            straight.iter().enumerate().map(|(at, &[r, g, b, a])| {
                let (x, y) = (
                    read.x0 + i64::try_from(at % width).unwrap_or(0),
                    read.y0 + i64::try_from(at / width).unwrap_or(0),
                );
                let mut place = [0u8; 16];
                place[..8].copy_from_slice(&x.to_le_bytes());
                place[8..].copy_from_slice(&y.to_le_bytes());
                let speckle = FastHash::hash_bytes(seed, &place).to_le_bytes();
                let shift = |v: u8, s: u8| {
                    let delta = (i32::from(s) - 128) * amount / 100;
                    u8::try_from((i32::from(v) + delta).clamp(0, 255)).unwrap_or(0)
                };
                [
                    shift(r, speckle[0]),
                    shift(g, speckle[1]),
                    shift(b, speckle[2]),
                    a,
                ]
            }),
        )
        .ok_or(OutOfMemory)?,
        _ => edges(&straight, (width, height))?,
    };
    Ok(Made {
        bounds: read,
        pixels,
    })
}

/// `straight` blurred to reach `radius` pixels: the shared box blur, run
/// enough times to be near a Gaussian, over premultiplied pixels so a clear
/// pixel lends no colour.
fn blurred(
    straight: &[Rgba8],
    (width, height): (usize, usize),
    radius: i32,
) -> Result<Vec<Rgba8>, OutOfMemory> {
    let count = straight.len();
    let mut pixels = fallible::collected(
        count,
        straight
            .iter()
            .map(|&[r, g, b, a]| Color::rgba(r, g, b, a).premultiply()),
    )
    .ok_or(OutOfMemory)?;
    let mut aux = fallible::filled(count, Color::TRANSPARENT.premultiply()).ok_or(OutOfMemory)?;
    let pass = usize::try_from(radius)
        .unwrap_or(1)
        .div_ceil(SOFTEN_PASSES as usize)
        .max(1);
    for _ in 0..SOFTEN_PASSES {
        box_blur(&mut pixels, width, height, pass, &mut aux);
    }
    fallible::collected(
        count,
        pixels.iter().map(|pixel| {
            let colour = pixel.unpremultiply();
            [colour.r, colour.g, colour.b, colour.a]
        }),
    )
    .ok_or(OutOfMemory)
}

/// `straight`, read over `read`, as squares `cell` pixels across lined up on
/// the picture's own corner, each the premultiplied mean of what it covers.
fn pixelated(
    straight: &[Rgba8],
    (width, height): (usize, usize),
    read: Bounds,
    cell: i32,
) -> Result<Vec<Rgba8>, OutOfMemory> {
    let cell = i64::from(cell.max(1));
    let mut out = fallible::filled(straight.len(), [0u8; 4]).ok_or(OutOfMemory)?;
    let (x1, y1) = (
        read.x0 + i64::try_from(width).unwrap_or(0),
        read.y0 + i64::try_from(height).unwrap_or(0),
    );
    let mut top = read.y0.div_euclid(cell) * cell;
    while top < y1 {
        let mut left = read.x0.div_euclid(cell) * cell;
        while left < x1 {
            let (cx0, cy0) = (left.max(read.x0), top.max(read.y0));
            let (cx1, cy1) = ((left + cell).min(x1), (top + cell).min(y1));
            let index = |x: i64, y: i64| {
                usize::try_from((y - read.y0) * i64::try_from(width).unwrap_or(0) + (x - read.x0))
                    .unwrap_or(0)
            };
            let mut sum = [0u64; 4];
            for y in cy0..cy1 {
                for x in cx0..cx1 {
                    let [r, g, b, a] = straight[index(x, y)];
                    let a64 = u64::from(a);
                    sum[0] += u64::from(r) * a64;
                    sum[1] += u64::from(g) * a64;
                    sum[2] += u64::from(b) * a64;
                    sum[3] += a64;
                }
            }
            let pixels = u64::try_from((cx1 - cx0) * (cy1 - cy0)).unwrap_or(1).max(1);
            let mean = if sum[3] == 0 {
                [0; 4]
            } else {
                let channel =
                    |total: u64| u8::try_from((total + sum[3] / 2) / sum[3]).unwrap_or(u8::MAX);
                let alpha = u8::try_from((sum[3] + pixels / 2) / pixels).unwrap_or(u8::MAX);
                [channel(sum[0]), channel(sum[1]), channel(sum[2]), alpha]
            };
            for y in cy0..cy1 {
                for x in cx0..cx1 {
                    out[index(x, y)] = mean;
                }
            }
            left += cell;
        }
        top += cell;
    }
    Ok(out)
}

/// `straight`'s edges: each channel's Sobel gradient strength, its alpha
/// kept, the picture's own edge replicated outward.
fn edges(straight: &[Rgba8], (width, height): (usize, usize)) -> Result<Vec<Rgba8>, OutOfMemory> {
    let mut out = fallible::filled(straight.len(), [0u8; 4]).ok_or(OutOfMemory)?;
    let at = |x: isize, y: isize, channel: usize| {
        let x = usize::try_from(x.clamp(0, isize::try_from(width).unwrap_or(1) - 1)).unwrap_or(0);
        let y = usize::try_from(y.clamp(0, isize::try_from(height).unwrap_or(1) - 1)).unwrap_or(0);
        i32::from(straight[y * width + x][channel])
    };
    for y in 0..height {
        for x in 0..width {
            let (xi, yi) = (
                isize::try_from(x).unwrap_or(0),
                isize::try_from(y).unwrap_or(0),
            );
            let mut pixel = straight[y * width + x];
            for (channel, slot) in pixel.iter_mut().enumerate().take(3) {
                let v = |dx: isize, dy: isize| at(xi + dx, yi + dy, channel);
                let gx = v(1, -1) + 2 * v(1, 0) + v(1, 1) - v(-1, -1) - 2 * v(-1, 0) - v(-1, 1);
                let gy = v(-1, 1) + 2 * v(0, 1) + v(1, 1) - v(-1, -1) - 2 * v(0, -1) - v(1, -1);
                let strength = (gx * gx + gy * gy).isqrt();
                *slot = u8::try_from(strength.min(255)).unwrap_or(u8::MAX);
            }
            out[y * width + x] = pixel;
        }
    }
    Ok(out)
}

#[cfg(test)]
#[path = "filter_tests.rs"]
mod tests;
