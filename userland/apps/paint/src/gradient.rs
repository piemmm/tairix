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
            GradientShape::Linear => Spread {
                measure: Measure::Bands {
                    dx,
                    dy,
                    per_column: 255 * i128::from(FX) * dx,
                },
                begins: core::array::from_fn(|share| match i128::try_from(share) {
                    Ok(share) if share > 0 => share * length,
                    _ => i128::MIN,
                }),
            },
            GradientShape::Radial => {
                let radius = length.isqrt().max(1);
                Spread {
                    measure: Measure::Rings,
                    begins: core::array::from_fn(|share| {
                        let reach = (i128::try_from(share).unwrap_or(0) * radius + 254) / 255;
                        reach * reach
                    }),
                }
            }
        })
    }
}

/// How far along a gradient a pixel lies is measured: its reach, and the
/// least reach at which each share begins, so a share is read off a table
/// rather than divided or rooted out a pixel.
#[derive(Clone, Debug)]
struct Spread {
    measure: Measure,
    /// Ascending, the first no greater than any reach.
    begins: [i128; SHARES],
}

/// What a pixel's reach is.
#[derive(Copy, Clone, Debug)]
enum Measure {
    /// Its offset projected on the drag `(dx, dy)`, times 255: a share is
    /// `reach / length`, so share `k` begins at `k · length`. `per_column`
    /// is what a column further along adds.
    Bands {
        dx: i128,
        dy: i128,
        per_column: i128,
    },
    /// Its squared distance from the start: a share is
    /// `⌊√d²⌋·255 / ⌊√length⌋`.
    Rings,
}

/// The most shares a pixel steps from the last read before its share is
/// searched for instead.
const NEARBY: usize = 4;

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
    /// Row `y`, its pixels to be read left to right.
    #[must_use]
    pub const fn along(&self, y: i64) -> Along<'_> {
        Along {
            laying: self,
            y,
            last: None,
        }
    }
}

/// One row of a [`Laying`], read at columns that never go back: each
/// pixel's share is found from the last one's, so a run costs an addition
/// and a comparison a pixel.
pub struct Along<'a> {
    laying: &'a Laying,
    y: i64,
    /// The column read last, its offset across from the start, its reach,
    /// and its share.
    last: Option<(i64, i128, i128, usize)>,
}

impl Along<'_> {
    /// How far from the first ink to the second pixel `x` lies, out of 255:
    /// along the drag for bands, out from its start for rings.
    pub fn share(&mut self, x: i64) -> u8 {
        let Some(spread) = &self.laying.spread else {
            return u8::MAX;
        };
        let from = self.laying.gradient.from;
        let vx = i128::from(x * FX + FX / 2 - from.x);
        let (reach, share) = match self.last {
            Some((column, _, reach, share)) if column == x => (reach, share),
            Some((column, before, reach, share)) if column < x => {
                let step = x - column;
                let reach = reach
                    + match spread.measure {
                        Measure::Bands { per_column, .. } if step == 1 => per_column,
                        Measure::Bands { per_column, .. } => per_column * i128::from(step),
                        Measure::Rings => i128::from(step * FX) * (vx + before),
                    };
                (reach, walked(&spread.begins, share, reach))
            }
            _ => {
                let vy = i128::from(self.y * FX + FX / 2 - from.y);
                let reach = match spread.measure {
                    Measure::Bands { dx, dy, .. } => 255 * (vx * dx + vy * dy),
                    Measure::Rings => vx * vx + vy * vy,
                };
                (reach, searched(&spread.begins, reach))
            }
        };
        self.last = Some((x, vx, reach, share));
        u8::try_from(share).unwrap_or(u8::MAX)
    }

    /// `below`, pixel `x`, with the gradient laid over it covering `cover`
    /// of it. Every pixel of a run is passed, covered or not, so the next
    /// one's share is still found from this one's.
    pub fn laid(&mut self, x: i64, below: Sample, cover: u8) -> Sample {
        let share = self.share(x);
        if cover == 0 {
            return below;
        }
        let laying = self.laying;
        let (ink, blend) = if let Some(shades) = &laying.shades {
            (Ink::Colour(shades[usize::from(share)]), Blend::Over)
        } else {
            let row = usize::try_from(self.y.rem_euclid(4)).unwrap_or(0);
            let column = usize::try_from(x.rem_euclid(4)).unwrap_or(0);
            let threshold = BAYER[row][column] * 16 + 8;
            let inks = laying.gradient.inks;
            let ink = if share > threshold { inks.1 } else { inks.0 };
            (ink, Blend::Replace)
        };
        lay_over(below, Coat { ink, blend }, cover, laying.masked)
    }
}

/// The share `reach` lies in, found by stepping from `from` while it is a
/// few away, and searched for when it is further.
fn walked(begins: &[i128; SHARES], from: usize, reach: i128) -> usize {
    let mut share = from;
    for _ in 0..NEARBY {
        if begins.get(share + 1).is_some_and(|&next| next <= reach) {
            share += 1;
        } else if share > 0 && begins.get(share).is_some_and(|&begin| begin > reach) {
            share -= 1;
        } else {
            return share;
        }
    }
    searched(begins, reach)
}

/// The share `reach` lies in: the last whose beginning it has reached.
fn searched(begins: &[i128; SHARES], reach: i128) -> usize {
    begins
        .partition_point(|&begin| begin <= reach)
        .saturating_sub(1)
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
        let mut along = laying.along(y);
        for ((column, sample), &cover) in (x..).zip(run.iter_mut()).zip(chosen.iter()) {
            *sample = along.laid(column, *sample, cover);
        }
    })?;
    change.written(picture)
}

#[cfg(test)]
#[path = "gradient_tests.rs"]
mod tests;
