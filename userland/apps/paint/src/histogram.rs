//! A layer's histogram: how much of it stands at each of the 256 levels of
//! red, green, blue and luma, each pixel weighed by its opacity and by how
//! much the selection chooses it, and its mean colour in linear light —
//! counted a band of rows at a time across a runner and the bands' counts
//! added, so it is the same however the rows were cut.

use alloc::vec::Vec;

use tairix_colour::srgb_to_linear;
use tairix_image::Rgba8;
use tairix_parallel::JobRunner;
use tairix_util::fallible;

use crate::canvas::{Canvas, OutOfMemory};
use crate::mask::Mask;
use crate::op::Halted;
use crate::shape::Bounds;

/// The pixels read at a time.
const RUN: usize = 256;

/// The fewest rows a band of the count is worth handing off.
const BAND_ROWS: usize = 16;

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
    /// selection is held, counted a band of rows at a time across `runner`,
    /// each band once `wanted` says it still is.
    ///
    /// # Errors
    ///
    /// [`Halted::Abandoned`] once `wanted` says no, and
    /// [`Halted::OutOfMemory`] where its room cannot be had.
    pub fn of(
        canvas: &Canvas,
        clip: Option<&Mask>,
        runner: &dyn JobRunner,
        wanted: &(dyn Fn() -> bool + Sync),
    ) -> Result<Self, Halted> {
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
        let rows = usize::try_from(area.y1 - area.y0).map_err(|_| OutOfMemory)?;
        let pieces = tairix_parallel::bands(runner, rows, BAND_ROWS);
        let per = tairix_parallel::piece_len(rows, pieces);
        let per = i64::try_from(per).map_err(|_| OutOfMemory)?;
        let band = |at: i64| {
            let first = area.y0.saturating_add(per.saturating_mul(at));
            Band::new(first, first.saturating_add(per).min(area.y1))
        };
        let Some(mut bands) = fallible::collected(pieces, (0..).map(band)) else {
            return Err(Halted::OutOfMemory);
        };
        tairix_parallel::for_each(runner, &mut bands, &|band: &mut Band| {
            band.count(canvas, clip, area, wanted);
        });
        for band in &bands {
            if let Some(halted) = band.halted {
                return Err(halted);
            }
            for (counts, counted) in histogram.levels.iter_mut().zip(&band.levels) {
                for (count, &more) in counts.iter_mut().zip(counted) {
                    *count += more;
                }
            }
            histogram.weight += band.weight;
        }
        histogram.linear = linear_sums(histogram.colours());
        Ok(histogram)
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
        let whole = exact(self.weight);
        Some(self.linear.map(|sum| sum / whole))
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

    /// How tall each of `columns` columns across stands, in thousandths of
    /// the height: the tallest level a column covers, against
    /// [`scale`](Self::scale), held to the top. The scale is found once for
    /// the whole plot, not once a column.
    pub fn columns(&self, plot: Plot, columns: u32) -> impl Iterator<Item = u16> + '_ {
        let scale = self.scale(plot);
        let counts = self.counts(plot);
        (0..columns).map(move |column| {
            if scale == 0 {
                return 0;
            }
            let start =
                usize::try_from(u64::from(column) * 256 / u64::from(columns)).unwrap_or(256);
            let end = usize::try_from((u64::from(column) + 1) * 256 / u64::from(columns))
                .unwrap_or(256)
                .max(start + 1)
                .min(256);
            let tallest = counts[start.min(255)..end]
                .iter()
                .copied()
                .max()
                .unwrap_or(0);
            u16::try_from((u128::from(tallest.min(scale)) * 1000 / u128::from(scale)).min(1000))
                .unwrap_or(1000)
        })
    }
}

/// One band of rows' counts, or why it stopped short of them.
struct Band {
    /// The rows it counts: the first, and the one past the last.
    rows: (i64, i64),
    levels: [[u64; 256]; LUMA + 1],
    weight: u64,
    halted: Option<Halted>,
}

impl Band {
    const fn new(first: i64, past: i64) -> Self {
        Self {
            rows: (first, past),
            levels: [[0; 256]; LUMA + 1],
            weight: 0,
            halted: None,
        }
    }

    /// Count `canvas`'s pixels of the band's rows across `area`, held to
    /// `clip`, once `wanted` says it still is.
    fn count(
        &mut self,
        canvas: &Canvas,
        clip: Option<&Mask>,
        area: Bounds,
        wanted: &(dyn Fn() -> bool + Sync),
    ) {
        self.halted = if wanted() {
            self.count_rows(canvas, clip, area).err().map(Halted::from)
        } else {
            Some(Halted::Abandoned)
        };
    }

    fn count_rows(
        &mut self,
        canvas: &Canvas,
        clip: Option<&Mask>,
        area: Bounds,
    ) -> Result<(), OutOfMemory> {
        let mut colours = [[0u8; 4]; RUN];
        let mut shares = [u8::MAX; RUN];
        for y in self.rows.0..self.rows.1 {
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
                    self.add(colour, share);
                }
                x += i64::try_from(run).map_err(|_| OutOfMemory)?;
            }
        }
        Ok(())
    }

    fn add(&mut self, [r, g, b, a]: Rgba8, share: u8) {
        let weight = u64::from(u32::from(a) * u32::from(share));
        if weight == 0 {
            return;
        }
        let luma = tairix_raster::Color::rgb(r, g, b).luma();
        for (counts, level) in self.levels.iter_mut().zip([r, g, b, luma]) {
            counts[usize::from(level)] += weight;
        }
        self.weight += weight;
    }
}

/// Each colour channel's levels in linear light, summed by their `colours`
/// counts: the same sum however the counting was cut.
fn linear_sums(colours: &[[u64; 256]]) -> [f64; 3] {
    let mut light = [0.0; 256];
    for (level, slot) in (0u8..=255).zip(light.iter_mut()) {
        *slot = srgb_to_linear(f64::from(level) / 255.0);
    }
    let mut sums = [0.0; 3];
    for (sum, counts) in sums.iter_mut().zip(colours) {
        *sum = light
            .iter()
            .zip(counts)
            .map(|(&level, &count)| level * exact(count))
            .sum();
    }
    sums
}

/// `value` as a float, exact to 2^53.
fn exact(value: u64) -> f64 {
    let high = u32::try_from(value >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(value & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

#[cfg(test)]
#[path = "histogram_tests.rs"]
mod tests;
