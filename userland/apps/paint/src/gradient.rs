//! The gradient: a blend from one ink to the other across a drag, in bands
//! or in rings, laid over the picture or over what a selection holds.
//!
//! A colour picture takes the blend itself, mixed with its alpha weighed in
//! so a colour fading to clear keeps its hue. A palette picture, whose pixels
//! are one entry each, takes an ordered dither of the two entries, so the
//! blend still reads at a distance.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_image::Rgba8;

use crate::canvas::{Canvas, Kind, OutOfMemory, Sample, Tile, TILE};
use crate::colour::Ink;
use crate::compose::between;
use crate::mask::Mask;
use crate::shape::{Bounds, Point, FX};
use crate::stroke::{lay_over, Blend, Change, Coat};
use crate::tool::GradientShape;

/// The 4×4 ordered-dither thresholds, out of 16.
const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// The shares along a gradient a pixel can lie at: one for each byte value.
const SHARES: usize = u8::MAX as usize + 1;

/// A gradient dragged across the picture.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Gradient {
    /// Where the drag began: the first ink, and a ring's centre.
    pub from: Point,
    /// Where it ended: the second ink.
    pub to: Point,
    /// How it spreads.
    pub shape: GradientShape,
    /// The ink at either end.
    pub inks: (Ink, Ink),
}

impl Gradient {
    /// Whether it spans anything: a click lays no blend.
    #[must_use]
    pub fn spans(&self) -> bool {
        self.from != self.to
    }

    /// The gradient made ready to lay over a picture of `kind`.
    #[must_use]
    pub fn on(&self, kind: &Kind) -> Laying {
        let shades = matches!(kind, Kind::Rgba).then(|| {
            let [from, to] = [self.inks.0, self.inks.1].map(|ink| match ink {
                Ink::Clear => [0; 4],
                ink => ink.shown(kind),
            });
            core::array::from_fn(|share| between(from, to, u8::try_from(share).unwrap_or(u8::MAX)))
        });
        Laying {
            gradient: *self,
            spread: self.spread(),
            masked: kind.masked(),
            shades,
        }
    }

    /// How a pixel's share is measured; `None` for a drag spanning nothing,
    /// which lays its second ink everywhere.
    fn spread(&self) -> Option<Spread> {
        let (dx, dy) = (
            i128::from(self.to.x - self.from.x),
            i128::from(self.to.y - self.from.y),
        );
        let length = dx * dx + dy * dy;
        if length == 0 {
            return None;
        }
        Some(match self.shape {
            GradientShape::Linear => Spread::Bands { dx, dy, length },
            GradientShape::Radial => {
                let radius = length.isqrt().max(1);
                Spread::Rings(core::array::from_fn(|share| {
                    let reach = (i128::try_from(share).unwrap_or(0) * radius + 254) / 255;
                    reach * reach
                }))
            }
        })
    }
}

/// How far along a gradient a pixel lies is measured.
#[allow(
    clippy::large_enum_variant,
    reason = "built once a lay or a frame and never moved on a pixel's path; boxing would allocate a frame"
)]
#[derive(Clone, Debug)]
enum Spread {
    /// Along the drag `(dx, dy)`, whose squared length is `length`.
    Bands { dx: i128, dy: i128, length: i128 },
    /// Out from its start: the squared distance at which each share begins.
    /// A share is `⌊√d²⌋·255 / ⌊√length⌋`, so comparing `d²` against where
    /// each begins finds it with no root taken a pixel.
    Rings([i128; SHARES]),
}

/// A gradient ready to lay over a picture of one kind.
#[derive(Clone, Debug)]
pub struct Laying {
    gradient: Gradient,
    spread: Option<Spread>,
    masked: bool,
    /// A colour picture's blend at each share along the gradient, mixed once
    /// so a pixel costs a lookup; a palette picture has none, taking a dither
    /// of the two entries instead.
    shades: Option<[Rgba8; SHARES]>,
}

impl Laying {
    /// How far from the first ink to the second pixel `(x, y)` lies, out of
    /// 255: along the drag for bands, out from its start for rings.
    #[must_use]
    pub fn share(&self, x: i64, y: i64) -> u8 {
        let Some(spread) = &self.spread else {
            return u8::MAX;
        };
        let (vx, vy) = (
            i128::from(x * FX + FX / 2 - self.gradient.from.x),
            i128::from(y * FX + FX / 2 - self.gradient.from.y),
        );
        match spread {
            Spread::Bands { dx, dy, length } => {
                let share = ((vx * dx + vy * dy) * 255 / length).clamp(0, 255);
                u8::try_from(share).unwrap_or(u8::MAX)
            }
            Spread::Rings(begins) => {
                let reach = vx * vx + vy * vy;
                let past = begins.partition_point(|&begin| begin <= reach);
                u8::try_from(past.saturating_sub(1)).unwrap_or(u8::MAX)
            }
        }
    }

    /// `below`, pixel `(x, y)`, with the gradient laid over it covering
    /// `cover` of it.
    #[must_use]
    pub fn laid(&self, (x, y): (i64, i64), below: Sample, cover: u8) -> Sample {
        let share = self.share(x, y);
        let (ink, blend) = if let Some(shades) = &self.shades {
            (Ink::Colour(shades[usize::from(share)]), Blend::Over)
        } else {
            let row = usize::try_from(y.rem_euclid(4)).unwrap_or(0);
            let column = usize::try_from(x.rem_euclid(4)).unwrap_or(0);
            let threshold = BAYER[row][column] * 16 + 8;
            let inks = self.gradient.inks;
            let ink = if share > threshold { inks.1 } else { inks.0 };
            (ink, Blend::Replace)
        };
        lay_over(below, Coat { ink, blend }, cover, self.masked)
    }
}

/// Lay `gradient` over `picture`, within `clip` where a selection is held:
/// the worker's work, answering each tile written as it now stands.
///
/// # Errors
///
/// [`OutOfMemory`] when a tile cannot be copied for writing.
pub fn lay(
    picture: &mut Canvas,
    gradient: &Gradient,
    clip: Option<&Mask>,
) -> Result<Vec<(usize, Arc<Tile>)>, OutOfMemory> {
    let laying = gradient.on(picture.kind());
    let area = clip.map_or(
        Bounds::picture(picture.width(), picture.height()),
        Mask::bounds,
    );
    let mut chosen = [u8::MAX; TILE as usize];
    let mut change = Change::new();
    change.repaint(picture, area, |x, y, run| {
        let chosen = &mut chosen[..run.len()];
        if let Some(clip) = clip {
            clip.row(y, x, chosen);
        }
        for ((column, sample), &cover) in (x..).zip(run.iter_mut()).zip(chosen.iter()) {
            if cover > 0 {
                *sample = laying.laid((column, y), *sample, cover);
            }
        }
    })?;
    change.written(picture)
}

#[cfg(test)]
#[path = "gradient_tests.rs"]
mod tests;
