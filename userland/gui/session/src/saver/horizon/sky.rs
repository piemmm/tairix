//! The sky and the sun.
//!
//! The night deepens upward from the horizon and the sun's glow warms it
//! about the disc. The disc shades from gold at its crown to ember where it
//! sets, and across its lower part dark bands sink towards the horizon, each
//! thickening as it goes while a new one opens above them.
//!
//! Only the bands move, so a frame repaints the disc's pixels across the
//! banded part alone and lays back over them the mountains standing there,
//! from a copy of that part of the ranges drawn over transparency.

use core::ops::Range;

use tairix_parallel::JobRunner;
use tairix_raster::{blend_span, DitherRow, Pixel};
use tairix_util::mathf;
use tairix_wm::{Rect, Surface};

use super::mountains::Mountains;
use super::{column, covered, dither_biases, fill_dithered, index, paint_rows, Moment, Rgb, View};

/// The night at the top of the screen and just above the horizon, and how it
/// brightens between them: the power the share of the way down is raised to.
const ZENITH: Rgb = Rgb::new(2.0, 5.0, 13.0);
const LOW_SKY: Rgb = Rgb::new(11.0, 26.0, 60.0);
const SKY_CURVE: f64 = 2.4;

/// The sun's glow at its rim, how far it falls by a factor of `e`, in screen
/// heights, and how many such falls it is drawn out to: past them it adds
/// less than a tenth of a level.
const GLOW: Rgb = Rgb::new(64.0, 21.0, 4.0);
const GLOW_FALL: f64 = 0.045;
const GLOW_REACH: f64 = 6.5;

/// The disc at its crown and where it sets.
const CROWN: Rgb = Rgb::new(248.0, 156.0, 70.0);
const FOOT: Rgb = Rgb::new(236.0, 88.0, 48.0);

/// Where the banded part begins, in radii above the disc's centre, and how
/// many bands' periods span it.
const BANDS_FROM: f64 = 0.15;
const BANDS: f64 = 6.0;

/// The share of its period a band fills where it opens and where it sets.
const BAND_OPEN: f64 = 0.03;
const BAND_SET: f64 = 0.26;

/// What a band darkens the disc towards, and how far.
const BAND_INK: Rgb = Rgb::new(150.0, 38.0, 26.0);
const BAND_DEPTH: f64 = 0.36;

/// Seconds a band takes to sink one period.
const BAND_PERIOD: f64 = 6.5;

/// The sky, the sun, and what repaints the sun's bands.
pub(super) struct Sky {
    view: View,
    /// The banded part of the sun a frame repaints: its rows, and the
    /// columns the disc can reach among them.
    zone: Rect,
    /// The mountains over `zone`, drawn over transparency.
    overlay: Surface,
}

/// What one sky row is lit by.
struct Lit {
    y: u32,
    biases: [f64; 8],
    night: Rgb,
    disc: Rgb,
}

impl Sky {
    /// The sky of `view`, the mountains standing over the sun's banded part
    /// drawn from `mountains`; `None` when the heap will not give the copy.
    pub(super) fn new(view: View, mountains: &Mountains) -> Option<Self> {
        let zone = bands_zone(&view)?;
        let (Ok(left), Ok(top)) = (u32::try_from(zone.left()), u32::try_from(zone.top())) else {
            return None;
        };
        let mut overlay = Surface::new(zone.width, zone.height)?;
        overlay.with_origin(left, top, |overlay| mountains.draw(overlay, zone));
        Some(Self {
            view,
            zone,
            overlay,
        })
    }

    /// The rectangle a frame repaints.
    pub(super) const fn zone(&self) -> Rect {
        self.zone
    }

    /// Paint every row above the horizon, the sun's bands as they stand at
    /// `moment`, spread across `runner`.
    pub(super) fn paint(&self, surface: &mut Surface, runner: &dyn JobRunner, moment: Moment) {
        let bands = band_phase(moment);
        let reach = self.view.sun.1 + GLOW_REACH * GLOW_FALL * f64::from(self.view.height);
        paint_rows(surface, 0..self.view.horizon, runner, &|y, span| {
            let lit = self.lit(y, bands);
            fill_dithered(span, 0, lit.night, &lit.biases);
            let glowing = self.columns_within(y, reach);
            let start = usize::try_from(glowing.start).unwrap_or(usize::MAX);
            let end = usize::try_from(glowing.end).unwrap_or(usize::MAX);
            if let Some(glowing_span) = span.get_mut(start..end) {
                self.shade(&lit, glowing.start, glowing_span);
            }
        });
    }

    /// Repaint the disc's pixels across the banded part as the bands stand at
    /// `moment`, and lay the mountains standing over them back on top.
    pub(super) fn paint_bands(&self, surface: &mut Surface, moment: Moment) {
        let bands = band_phase(moment);
        let (Ok(top), Ok(left)) = (
            u32::try_from(self.zone.top()),
            u32::try_from(self.zone.left()),
        ) else {
            return;
        };
        let right = left.saturating_add(self.zone.width);
        for y in top..top.saturating_add(self.zone.height) {
            let disc = self.columns_within(y, self.view.sun.1 + 1.0);
            let columns = disc.start.max(left)..disc.end.min(right);
            if columns.is_empty() {
                continue;
            }
            let Some((first, span)) =
                surface.row_span_mut(y, columns.start, columns.end - columns.start)
            else {
                continue;
            };
            self.shade(&self.lit(y, bands), first, span);
            let wide = u32::try_from(span.len()).unwrap_or(0);
            if let Some((_, over)) = self.overlay.row_span(y - top, first - left, wide) {
                blend_span(span, over, u8::MAX, DitherRow::NEAREST, 0);
            }
        }
    }

    /// What row `y` is lit by, the bands at phase `bands`.
    fn lit(&self, y: u32, bands: f64) -> Lit {
        Lit {
            y,
            biases: dither_biases(y),
            night: self.night(y),
            disc: self.disc(y, bands),
        }
    }

    /// Shade `span`, whose first pixel is column `first` of `lit`'s row, with
    /// the sun's glow and disc over the night.
    fn shade(&self, lit: &Lit, first: u32, span: &mut [Pixel]) {
        let (centre, radius) = self.view.sun;
        let fall = GLOW_FALL * f64::from(self.view.height);
        let across = f64::from(lit.y) + 0.5 - centre;
        // The columns the disc covers whole take its light alone.
        let inner = radius - 0.5;
        let whole = if mathf::fabs(across) < inner {
            let half = mathf::sqrt(inner * inner - across * across);
            let most = u32::MAX;
            column(mathf::ceil(self.view.centre - half - 0.5), most)
                ..column(mathf::floor(self.view.centre + half - 0.5) + 1.0, most)
        } else {
            0..0
        };
        let end = first.saturating_add(u32::try_from(span.len()).unwrap_or(u32::MAX));
        let whole = whole.start.clamp(first, end)..whole.end.clamp(first, end);
        let (before, rest) = span.split_at_mut(index(whole.start - first));
        let (inside, after) = rest.split_at_mut(index(whole.end - whole.start));
        fill_dithered(inside, whole.start, lit.disc, &lit.biases);
        for (x, pixel) in (first..).zip(before).chain((whole.end..).zip(after)) {
            let distance = mathf::hypot(f64::from(x) + 0.5 - self.view.centre, across);
            let glow = GLOW * mathf::exp(-mathf::fmax(distance - radius, 0.0) / fall);
            let cover = mathf::clamp(radius - distance + 0.5, 0.0, 1.0);
            let bias = lit.biases[index(x & 7)];
            *pixel = (lit.night + glow).mix(lit.disc, cover).pixel(bias);
        }
    }

    /// The night at row `y`, before the sun's glow.
    fn night(&self, y: u32) -> Rgb {
        let down = (f64::from(y) + 0.5) / f64::from(self.view.horizon);
        ZENITH.mix(LOW_SKY, mathf::exp(SKY_CURVE * mathf::ln(down)))
    }

    /// The disc's light across row `y`, its bands at phase `bands`.
    fn disc(&self, y: u32, bands: f64) -> Rgb {
        let row = f64::from(y);
        let lit = disc_colour(&self.view, row + 0.5);
        let (centre, radius) = self.view.sun;
        let from = centre - BANDS_FROM * radius;
        if row + 1.0 <= from {
            return lit;
        }
        let depth = f64::from(self.view.horizon) - from;
        let at = |row: f64| mathf::fmax(row - from, 0.0) / depth * BANDS - bands;
        let down = mathf::clamp((row + 0.5 - from) / depth, 0.0, 1.0);
        let width = BAND_OPEN + (BAND_SET - BAND_OPEN) * down;
        lit.mix(
            BAND_INK,
            covered(at(row), at(row + 1.0), width) * BAND_DEPTH,
        )
    }

    /// The columns of row `y` within `reach` pixels of the sun's centre.
    fn columns_within(&self, y: u32, reach: f64) -> Range<u32> {
        let across = f64::from(y) + 0.5 - self.view.sun.0;
        if mathf::fabs(across) >= reach {
            return 0..0;
        }
        let half = mathf::sqrt(reach * reach - across * across);
        let width = self.view.width;
        column(mathf::floor(self.view.centre - half), width)
            ..column(mathf::ceil(self.view.centre + half), width)
    }
}

/// The disc's colour at `row` pixels down the screen, before its bands: gold
/// at its crown, ember where it sets.
pub(super) fn disc_colour(view: &View, row: f64) -> Rgb {
    let (centre, radius) = view.sun;
    let crown = centre - radius;
    let down = (row - crown) / (f64::from(view.horizon) - crown);
    CROWN.mix(FOOT, mathf::smoothstep((down - 0.2) / 0.8))
}

/// How many periods the bands have sunk at `moment`.
fn band_phase(moment: Moment) -> f64 {
    moment.time / BAND_PERIOD
}

/// The banded part of the sun on `view`: from where the bands begin down to
/// the horizon, across every column the disc can reach there.
fn bands_zone(view: &View) -> Option<Rect> {
    let (centre, radius) = view.sun;
    let top = column(mathf::floor(centre - BANDS_FROM * radius), view.horizon);
    let left = column(mathf::floor(view.centre - radius - 1.0), view.width);
    let right = column(mathf::ceil(view.centre + radius + 1.0), view.width);
    (top < view.horizon && left < right).then(|| {
        Rect::new(
            i32::try_from(left).unwrap_or(0),
            i32::try_from(top).unwrap_or(0),
            right - left,
            view.horizon - top,
        )
    })
}

#[cfg(test)]
#[path = "sky_tests.rs"]
mod tests;
