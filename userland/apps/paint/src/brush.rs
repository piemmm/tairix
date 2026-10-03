//! The brush: a round tip laid down as dabs along the pointer's path.
//!
//! A dab covers the tip's disc — its exact area at the rim where edges are
//! smoothed — held under a falloff as soft as the tip's hardness says, times
//! the flow; the stroke builds the dabs up to its opacity. Dabs fall a spacing
//! apart, the distance since the last carried from one move to the next, so a
//! stroke drawn in many small moves is laid as one drawn in a single sweep.

use crate::canvas::{Canvas, OutOfMemory};
use crate::mask::scale;
use crate::shape::{Point, Shape, FX};
use crate::stroke::Stroke;

/// The nearest dabs fall, in 256ths of a pixel: an eighth of a pixel, past
/// which a mask a pixel fine learns nothing more.
const NEAREST_STEP: i64 = FX / 8;

/// A round brush tip: how wide it is, how hard its edge, and how its paint
/// builds up.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Tip {
    /// Width, in pixels: `1..=`[`MAX_SIZE`](crate::tool::MAX_SIZE).
    pub size: u32,
    /// How much of its radius is solid before its edge falls away, in
    /// percent.
    pub hardness: u8,
    /// The most of a pixel one stroke lays, in percent.
    pub opacity: u8,
    /// How much paint each dab lays, in percent.
    pub flow: u8,
    /// How far apart dabs fall along a stroke, in percent of the width.
    pub spacing: u8,
}

impl Tip {
    /// The radius, in 256ths of a pixel: at least half a pixel.
    #[must_use]
    pub fn radius(&self) -> i64 {
        (i64::from(self.size) * FX / 2).max(FX / 2)
    }

    /// The distance between dabs, in 256ths of a pixel.
    #[must_use]
    pub fn step(&self) -> i64 {
        (i64::from(self.size) * FX * i64::from(self.spacing) / 100).max(NEAREST_STEP)
    }

    /// The tip as laid on a picture that cannot show part of a pixel: hard,
    /// laying all its paint at once.
    #[must_use]
    pub const fn whole(self) -> Self {
        Self {
            hardness: 100,
            opacity: 100,
            flow: 100,
            ..self
        }
    }

    /// The opacity as the stroke lays it, out of 255.
    #[must_use]
    pub fn opacity_255(&self) -> u8 {
        percent(self.opacity)
    }

    /// Lay one dab at `centre` on layer 0 of `stroke`, its rim smoothed when
    /// `aa`.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when a tile cannot be copied for writing.
    pub fn dab(
        &self,
        stroke: &mut Stroke,
        canvas: &mut Canvas,
        centre: Point,
        aa: bool,
    ) -> Result<(), OutOfMemory> {
        let radius = self.radius();
        let disc = Shape::Capsule {
            a: centre,
            b: centre,
            radius,
        };
        let flow = percent(self.flow);
        // A rim not smoothed is whole or nothing, so a falloff has nowhere to
        // fall.
        let inner = if aa && self.hardness < 100 {
            Some(radius * i64::from(self.hardness) / 100)
        } else {
            None
        };
        stroke.cover_shaded(canvas, 0, &disc, aa, |y, x, out| {
            if inner.is_none() && flow == u8::MAX {
                return;
            }
            let dy = y * FX + FX / 2 - centre.y;
            for (column, cover) in (x..).zip(out.iter_mut()) {
                if *cover == 0 {
                    continue;
                }
                if let Some(inner) = inner {
                    let dx = column * FX + FX / 2 - centre.x;
                    *cover = scale(*cover, falloff((dx * dx + dy * dy).isqrt(), inner, radius));
                }
                *cover = scale(*cover, flow);
            }
        })
    }
}

/// How much of a tip's paint lies `distance` from its centre, out of 255:
/// all of it within `inner`, none at `radius`, and between them a smooth
/// fall that meets both without a crease.
fn falloff(distance: i64, inner: i64, radius: i64) -> u8 {
    if distance <= inner {
        return u8::MAX;
    }
    if distance >= radius {
        return 0;
    }
    let (width, into) = (i128::from(radius - inner), i128::from(distance - inner));
    // One less the smoothstep of how far into the fall it lies.
    let fallen = into * into * (3 * width - 2 * into) * 255 / (width * width * width);
    u8::try_from(255 - fallen).unwrap_or(0)
}

/// `value` percent, out of 255.
fn percent(value: u8) -> u8 {
    u8::try_from(u32::from(value.min(100)) * 255 / 100).unwrap_or(u8::MAX)
}

/// Where dabs fall along a stroke's path: the last point it reached and how
/// far it has run since the last dab.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Path {
    last: Point,
    run: i64,
}

impl Path {
    /// A path beginning at `at`, where its first dab falls.
    #[must_use]
    pub const fn new(at: Point) -> Self {
        Self { last: at, run: 0 }
    }

    /// Where it last reached.
    #[must_use]
    pub const fn last(&self) -> Point {
        self.last
    }

    /// Carry the path on to `to` laying no dab, answering where it was: a
    /// pencil's, whose every pixel is its own.
    pub fn move_to(&mut self, to: Point) -> Point {
        self.run = 0;
        core::mem::replace(&mut self.last, to)
    }

    /// Carry the path on to `to`, handing `dab` each point a dab falls,
    /// `step` apart from the last, stopping at the first refusal.
    ///
    /// # Errors
    ///
    /// The first [`OutOfMemory`] `dab` answers; the path still reaches `to`.
    pub fn to(
        &mut self,
        to: Point,
        step: i64,
        mut dab: impl FnMut(Point) -> Result<(), OutOfMemory>,
    ) -> Result<(), OutOfMemory> {
        let from = core::mem::replace(&mut self.last, to);
        let (dx, dy) = (to.x - from.x, to.y - from.y);
        let length = (dx * dx + dy * dy).isqrt();
        let step = step.max(NEAREST_STEP);
        let mut at = step - self.run;
        let mut outcome = Ok(());
        while at <= length {
            let point = Point {
                x: from.x + dx * at / length.max(1),
                y: from.y + dy * at / length.max(1),
            };
            if outcome.is_ok() {
                outcome = dab(point);
            }
            at += step;
        }
        self.run = length - (at - step);
        outcome
    }
}

#[cfg(test)]
#[path = "brush_tests.rs"]
mod tests;
