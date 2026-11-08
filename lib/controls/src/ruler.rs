//! A ruler along one edge of a measured view.
//!
//! Its figures stand at the multiples of one step — 1, 2 or 5 times a power
//! of ten units, the least of those whose figures stand far enough apart to
//! read — each with a tick across the ruler, and each spelt with exactly the
//! decimals the step needs, so no two figures ever read alike. A ruler draws a
//! count of figures bounded by its own length, however far the view it
//! measures has scrolled.

use core::fmt::Write;

use tairix_geometry::{Rect, Scale};
use tairix_inline::ArrayString;
use tairix_raster::{Color, Surface};
use tairix_theme::{TextRole, Theme};
use tairix_util::mathf::{ceil, floor, ln, power, round_i32};

use crate::paint::{fill_area, role_font};

/// The least distance between two figures, in logical pixels.
const FIGURE_SPACING: u32 = 60;

/// Room for the longest figure a ruler spells: a sign, every whole digit an
/// `f64` holds, a point and the decimals a step can need.
type Figure = ArrayString<48>;

/// Which way a ruler runs.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RulerAxis {
    /// Along the top of the view, figures left to right.
    Across,
    /// Down its side, figures top to bottom.
    Down,
}

/// The step between a ruler's figures, in units, and the decimals that spell
/// every multiple of it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RulerStep {
    /// The step.
    pub step: f64,
    /// The decimals a multiple of the step is spelt with.
    pub decimals: usize,
}

/// A ruler over a view drawn at a scale of device pixels to units.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Ruler {
    per_unit: f64,
    zero: f64,
}

impl Ruler {
    /// A ruler over a view drawn at `per_unit` device pixels a unit, whose
    /// zero falls `zero` device pixels along from the ruler's leading edge;
    /// `None` for a scale that is not positive and finite.
    #[must_use]
    pub fn new(per_unit: f64, zero: f64) -> Option<Self> {
        (per_unit.is_finite() && per_unit > 0.0 && zero.is_finite())
            .then_some(Self { per_unit, zero })
    }

    /// The least of 1, 2 and 5 times a power of ten units whose figures stand
    /// at least `spacing` device pixels apart.
    #[must_use]
    pub fn step(&self, spacing: f64) -> RulerStep {
        let least = spacing / self.per_unit;
        let exponent = round_i32(floor(ln(least) / core::f64::consts::LN_10));
        let decade = power(10.0, f64::from(exponent));
        let (multiple, exponent) = [1.0, 2.0, 5.0]
            .into_iter()
            .find(|multiple| multiple * decade >= least)
            .map_or((1.0, exponent.saturating_add(1)), |multiple| {
                (multiple, exponent)
            });
        RulerStep {
            step: multiple * power(10.0, f64::from(exponent)),
            decimals: usize::try_from(exponent.saturating_neg().max(0)).unwrap_or(0),
        }
    }

    /// Paint the ruler into `bounds`, running along `axis`: its plate, then a
    /// tick and a figure at every step that falls on it.
    pub fn paint(
        &self,
        surface: &mut Surface,
        bounds: Rect,
        axis: RulerAxis,
        (scale, theme): (Scale, &Theme),
    ) {
        let palette = theme.palette();
        fill_area(surface, bounds, Color::from(palette.surface));
        let face = role_font(theme, scale, TextRole::Caption);
        let ink = Color::from(palette.on_surface_muted);
        let spacing = f64::from(scale.scale_length(FIGURE_SPACING).max(1));
        let RulerStep { step, decimals } = self.step(spacing);
        let length = f64::from(match axis {
            RulerAxis::Across => bounds.width,
            RulerAxis::Down => bounds.height,
        });
        let inset = i32::try_from(scale.scale_length(2).max(1)).unwrap_or(1);
        let first = floor((-self.zero / self.per_unit) / step);
        let last = ceil(((length - self.zero) / self.per_unit) / step);
        // Figures stand at least the spacing apart, so the ruler's own length
        // bounds how many there are.
        let most = round_i32(floor(length / spacing)).saturating_add(2);
        for mark in 0..most {
            let index = first + f64::from(mark);
            if index > last {
                break;
            }
            let value = index * step;
            let at = round_i32(self.zero + value * self.per_unit);
            let figure = spelt(value, decimals);
            let (tick, text) = match axis {
                RulerAxis::Across => {
                    let x = bounds.left().saturating_add(at);
                    (
                        Rect::new(x, bounds.top(), 1, bounds.height),
                        (x.saturating_add(inset), bounds.top()),
                    )
                }
                RulerAxis::Down => {
                    let y = bounds.top().saturating_add(at);
                    (
                        Rect::new(bounds.left(), y, bounds.width, 1),
                        (
                            bounds.left().saturating_add(inset / 2),
                            y.saturating_add(inset / 2),
                        ),
                    )
                }
            };
            fill_area(surface, tick.intersection(&bounds), ink);
            // A figure wider than a ruler down the side is cut at its edge,
            // never drawn over the view beside it.
            surface.with_clip(
                u32::try_from(bounds.left()).unwrap_or(0),
                u32::try_from(bounds.top()).unwrap_or(0),
                bounds.width,
                bounds.height,
                |surface| {
                    face.draw_text(surface, text.0, text.1, figure.as_str(), ink);
                },
            );
        }
    }
}

/// `value` spelt with `decimals` places and no trailing zeros, zero never
/// signed.
fn spelt(value: f64, decimals: usize) -> Figure {
    let mut figure = Figure::new();
    let value = if value == 0.0 { 0.0 } else { value };
    if write!(figure, "{value:.decimals$}").is_err() {
        return Figure::new();
    }
    if decimals > 0 {
        let kept = figure
            .as_str()
            .trim_end_matches('0')
            .trim_end_matches('.')
            .len();
        figure.truncate(kept);
    }
    figure
}

#[cfg(test)]
#[path = "ruler_tests.rs"]
mod tests;
