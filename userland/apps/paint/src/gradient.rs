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
use crate::mask::Mask;
use crate::shape::{Bounds, Point, FX};
use crate::stroke::{lay_over, Blend, Change, Coat};
use crate::tool::GradientShape;

/// The 4×4 ordered-dither thresholds, out of 16.
const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

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

    /// How far from the first ink to the second pixel `(x, y)` lies, out of
    /// 255: along the drag for bands, out from its start for rings.
    #[must_use]
    pub fn at(&self, x: i64, y: i64) -> u8 {
        let (dx, dy) = (
            i128::from(self.to.x - self.from.x),
            i128::from(self.to.y - self.from.y),
        );
        let (vx, vy) = (
            i128::from(x * FX + FX / 2 - self.from.x),
            i128::from(y * FX + FX / 2 - self.from.y),
        );
        let length = dx * dx + dy * dy;
        if length == 0 {
            return u8::MAX;
        }
        let share = match self.shape {
            GradientShape::Linear => (vx * dx + vy * dy) * 255 / length,
            GradientShape::Radial => (vx * vx + vy * vy).isqrt() * 255 / length.isqrt().max(1),
        };
        u8::try_from(share.clamp(0, 255)).unwrap_or(u8::MAX)
    }

    /// `below`, pixel `(x, y)` of a picture of `kind`, with the gradient
    /// laid over it covering `cover` of it.
    #[must_use]
    pub fn laid(&self, (x, y): (i64, i64), below: Sample, cover: u8, kind: &Kind) -> Sample {
        let share = self.at(x, y);
        let (ink, blend) = if matches!(kind, Kind::Rgba) {
            (Ink::Colour(mix(self.inks, share, kind)), Blend::Over)
        } else {
            let row = usize::try_from(y.rem_euclid(4)).unwrap_or(0);
            let column = usize::try_from(x.rem_euclid(4)).unwrap_or(0);
            let threshold = BAYER[row][column] * 16 + 8;
            let ink = if share > threshold {
                self.inks.1
            } else {
                self.inks.0
            };
            (ink, Blend::Replace)
        };
        lay_over(below, Coat { ink, blend }, cover, kind.masked())
    }
}

/// The colour `share` of the way from the first of `inks` to the second,
/// mixed with each one's alpha weighed in.
fn mix(inks: (Ink, Ink), share: u8, kind: &Kind) -> Rgba8 {
    let [a, b] = [inks.0, inks.1].map(|ink| match ink {
        Ink::Clear => [0; 4],
        ink => ink.shown(kind),
    });
    let (near, far) = (u32::from(255 - share), u32::from(share));
    let alpha = (u32::from(a[3]) * near + u32::from(b[3]) * far + 127) / 255;
    if alpha == 0 {
        return [0; 4];
    }
    let channel = |index: usize| {
        let weighed = u32::from(a[index]) * u32::from(a[3]) * near
            + u32::from(b[index]) * u32::from(b[3]) * far;
        u8::try_from((weighed / 255 + alpha / 2) / alpha).unwrap_or(u8::MAX)
    };
    [
        channel(0),
        channel(1),
        channel(2),
        u8::try_from(alpha).unwrap_or(u8::MAX),
    ]
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
    let kind = picture.kind().clone();
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
                *sample = gradient.laid((column, y), *sample, cover, &kind);
            }
        }
    })?;
    change.written(picture)
}

#[cfg(test)]
#[path = "gradient_tests.rs"]
mod tests;
