//! A layer's histogram: how much of it stands at each of the 256 levels of
//! red, green, blue and luma, each pixel weighed by its opacity and by how
//! much the selection chooses it, and its mean colour in linear light.

use alloc::vec::Vec;

use tairix_colour::srgb_to_linear;
use tairix_image::Rgba8;
use tairix_util::fallible;

use crate::canvas::{Canvas, OutOfMemory};
use crate::mask::Mask;
use crate::shape::Bounds;

/// The pixels read at a time.
const RUN: usize = 256;

/// Which of a histogram's counts is shown.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Plot {
    /// Red.
    Red,
    /// Green.
    Green,
    /// Blue.
    Blue,
    /// The eye's weighing of the three.
    #[default]
    Luma,
}

/// Where each plot's counts stand among a histogram's.
const RED: usize = 0;
const LUMA: usize = 3;

/// A layer's histogram.
#[derive(Clone, Debug, PartialEq)]
pub struct Histogram {
    /// Red, green, blue and luma: each level's weight.
    levels: Vec<[u64; 256]>,
    /// Each linear channel summed, weighed as the counts are.
    linear: [f64; 3],
    weight: u64,
}

impl Histogram {
    /// The histogram of `canvas`, held to what `clip` chooses where a
    /// selection is held.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] where its room cannot be had.
    pub fn of(canvas: &Canvas, clip: Option<&Mask>) -> Result<Self, OutOfMemory> {
        let mut histogram = Self {
            levels: fallible::filled(LUMA + 1, [0; 256]).ok_or(OutOfMemory)?,
            linear: [0.0; 3],
            weight: 0,
        };
        let whole = Bounds::picture(canvas.width(), canvas.height());
        let area = clip.map_or(whole, |clip| clip.bounds().intersection(&whole));
        if area.is_empty() {
            return Ok(histogram);
        }
        let mut linear = [0.0; 256];
        for (level, slot) in (0u8..=255).zip(linear.iter_mut()) {
            *slot = srgb_to_linear(f64::from(level) / 255.0);
        }
        let mut colours = [[0u8; 4]; RUN];
        let mut shares = [u8::MAX; RUN];
        for y in area.y0..area.y1 {
            let row = u32::try_from(y).map_err(|_| OutOfMemory)?;
            let mut x = area.x0;
            while x < area.x1 {
                let run = usize::try_from(area.x1 - x).map_or(RUN, |left| left.min(RUN));
                let column = u32::try_from(x).map_err(|_| OutOfMemory)?;
                canvas.row_colours(row, column, &mut colours[..run]);
                if let Some(clip) = clip {
                    clip.row(y, x, &mut shares[..run]);
                }
                for (&colour, &share) in colours[..run].iter().zip(&shares[..run]) {
                    histogram.count(colour, share, &linear);
                }
                x += i64::try_from(run).map_err(|_| OutOfMemory)?;
            }
        }
        Ok(histogram)
    }

    fn count(&mut self, [r, g, b, a]: Rgba8, share: u8, linear: &[f64; 256]) {
        let weight = u64::from(u32::from(a) * u32::from(share));
        if weight == 0 {
            return;
        }
        let luma = tairix_raster::Color::rgb(r, g, b).luma();
        for (counts, level) in self.levels.iter_mut().zip([r, g, b, luma]) {
            counts[usize::from(level)] += weight;
        }
        let share = f64::from(u32::from(a) * u32::from(share));
        for (sum, level) in self.linear.iter_mut().zip([r, g, b]) {
            *sum += linear[usize::from(level)] * share;
        }
        self.weight += weight;
    }

    /// The counts `plot` shows, each level's weight.
    #[must_use]
    pub fn counts(&self, plot: Plot) -> &[u64; 256] {
        let at = match plot {
            Plot::Red => RED,
            Plot::Green => RED + 1,
            Plot::Blue => RED + 2,
            Plot::Luma => LUMA,
        };
        &self.levels[at]
    }

    /// Red, green and blue's counts.
    #[must_use]
    pub fn colours(&self) -> &[[u64; 256]] {
        &self.levels[RED..RED + 3]
    }

    /// The mean colour in linear light, or `None` where nothing was counted.
    #[must_use]
    pub fn mean(&self) -> Option<[f64; 3]> {
        if self.weight == 0 {
            return None;
        }
        let whole = self.linear_weight();
        Some(self.linear.map(|sum| sum / whole))
    }

    /// The whole weight as a float, exact to 2^53.
    fn linear_weight(&self) -> f64 {
        let high = u32::try_from(self.weight >> 32).unwrap_or(u32::MAX);
        let low = u32::try_from(self.weight & u64::from(u32::MAX)).unwrap_or(u32::MAX);
        f64::from(high) * 4_294_967_296.0 + f64::from(low)
    }

    /// The tallest count `plot` is drawn against: the tallest of the levels
    /// between black and white, so a spike of clipped pixels at either end
    /// does not flatten the rest; the ends' own where nothing lies between.
    #[must_use]
    pub fn scale(&self, plot: Plot) -> u64 {
        let counts = self.counts(plot);
        let between = counts[1..255].iter().copied().max().unwrap_or(0);
        if between > 0 {
            between
        } else {
            counts.iter().copied().max().unwrap_or(0)
        }
    }

    /// How tall column `column` of `columns` across stands, in thousandths
    /// of the height: the tallest level the column covers, against
    /// [`scale`](Self::scale), held to the top.
    #[must_use]
    pub fn column(&self, plot: Plot, column: u32, columns: u32) -> u16 {
        let scale = self.scale(plot);
        if scale == 0 || columns == 0 {
            return 0;
        }
        let start = usize::try_from(u64::from(column) * 256 / u64::from(columns)).unwrap_or(256);
        let end = usize::try_from((u64::from(column) + 1) * 256 / u64::from(columns))
            .unwrap_or(256)
            .max(start + 1)
            .min(256);
        let tallest = self.counts(plot)[start.min(255)..end]
            .iter()
            .copied()
            .max()
            .unwrap_or(0);
        u16::try_from((u128::from(tallest.min(scale)) * 1000 / u128::from(scale)).min(1000))
            .unwrap_or(1000)
    }
}

#[cfg(test)]
#[path = "histogram_tests.rs"]
mod tests;
