//! The brush: a round tip laid down as dabs along the pointer's path.
//!
//! A dab covers the tip's disc — its exact area at the rim where edges are
//! smoothed — held under a falloff as soft as the tip's hardness says, times
//! the flow; the stroke builds the dabs up to its opacity. Dabs fall a spacing
//! apart, the distance since the last carried from one move to the next, so a
//! stroke drawn in many small moves is laid as one drawn in a single sweep.
//!
//! An airbrush also lays a dab for each interval it is held ([`Spray`]), where
//! the pointer was at that instant. That is known only afterwards: from the
//! next report, which says where it went, or from a silence long enough that
//! it can only have been held still.

use tairix_colour::Fraction;
use tairix_raster::ROUND_NEAREST;

use crate::canvas::{Canvas, OutOfMemory};
use crate::mask::scale;
use crate::shape::{Point, Shape, FX};
use crate::stroke::Stroke;

/// The nearest dabs fall, in 256ths of a pixel: an eighth of a pixel, past
/// which a mask a pixel fine learns nothing more.
const NEAREST_STEP: i64 = FX / 8;

/// How often a held airbrush lays another dab.
pub const AIRBRUSH_INTERVAL_NS: u64 = 25_000_000;

/// How long the pointer goes unreported before an airbrush takes it to have
/// been held still: longer than a report of it moving spends reaching the
/// window, even under load.
pub const AIRBRUSH_STILL_NS: u64 = 100_000_000;

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
        Fraction::from_percent(u32::from(self.opacity)).byte()
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
        self.dabs(stroke, canvas, centre, aa, 1)
    }

    /// Lay `passes` dabs at `centre` as one, building what they would one
    /// after another, so their cost does not grow with their number.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`], as [`dab`](Self::dab).
    pub fn dabs(
        &self,
        stroke: &mut Stroke,
        canvas: &mut Canvas,
        centre: Point,
        aa: bool,
        passes: u32,
    ) -> Result<(), OutOfMemory> {
        if passes == 0 {
            return Ok(());
        }
        let radius = self.radius();
        let disc = Shape::Capsule {
            a: centre,
            b: centre,
            radius,
        };
        let flow = Fraction::from_percent(u32::from(self.flow)).byte();
        // A rim not smoothed is whole or nothing, so a falloff has nowhere to
        // fall.
        let inner = if aa && self.hardness < 100 {
            Some(radius * i64::from(self.hardness) / 100)
        } else {
            None
        };
        stroke.cover_shaded(canvas, 0, &disc, aa, |y, x, out| {
            if inner.is_none() && flow == u8::MAX && passes == 1 {
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
                *cover = built_up(scale(*cover, flow), passes);
            }
        })
    }
}

/// What `passes` passes each covering `cover` of a pixel lay together, out of
/// 255: each builds its share of what those before it left, so what they leave
/// is one pass's share left raised to their number.
fn built_up(cover: u8, passes: u32) -> u8 {
    const ONE: u64 = 1 << 16;
    if passes == 1 || cover == 0 || cover == u8::MAX {
        return cover;
    }
    let mut share = (u64::from(u8::MAX - cover) * ONE + u64::from(ROUND_NEAREST)) / 255;
    let mut left = ONE;
    let mut count = passes;
    while count > 0 {
        if count & 1 == 1 {
            left = (left * share + ONE / 2) >> 16;
        }
        share = (share * share + ONE / 2) >> 16;
        count >>= 1;
    }
    let left = (left * 255 + ONE / 2) >> 16;
    u8::MAX - u8::try_from(left).unwrap_or(u8::MAX)
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
        // A spacing shrunk mid-stroke can leave more run than a step: the dab
        // it owes falls where this move begins, never behind it.
        let mut at = (step - self.run).max(0);
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

/// When a held airbrush's own dabs fall: one each [`AIRBRUSH_INTERVAL_NS`] the
/// button is held, laid where the pointer was at that instant once that is
/// known.
///
/// Laying one where the pointer was last reported, as soon as it falls, piles
/// the paint of every interval between two reports onto the first of them
/// when the pointer was in fact moving.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Spray {
    /// When the pointer was last reported.
    reported_ns: u64,
    /// When the next dab falls.
    next_ns: u64,
}

impl Spray {
    /// An airbrush pressed at `at_ns`, whose press lays its first dab.
    #[must_use]
    pub const fn new(at_ns: u64) -> Self {
        Self {
            reported_ns: at_ns,
            next_ns: at_ns.saturating_add(AIRBRUSH_INTERVAL_NS),
        }
    }

    /// When the next dab is known to have fallen where the pointer was last
    /// reported, if no report comes first.
    #[must_use]
    pub const fn due_ns(&self) -> u64 {
        self.next_ns.saturating_add(AIRBRUSH_STILL_NS)
    }

    /// The pointer reported at `to` at `at_ns`, having been reported at `from`
    /// before: hand `dab` each place the dabs fallen since are laid, and how
    /// many fall there.
    ///
    /// It is taken to have stood at `from` and then moved at a steady pace,
    /// for no longer than [`AIRBRUSH_STILL_NS`]: a longer silence was it held
    /// still first.
    ///
    /// # Errors
    ///
    /// The first [`OutOfMemory`] `dab` answers; the dabs are spent all the same.
    pub fn reported(
        &mut self,
        from: Point,
        to: Point,
        at_ns: u64,
        mut dab: impl FnMut(Point, u32) -> Result<(), OutOfMemory>,
    ) -> Result<(), OutOfMemory> {
        let moving = if from == to {
            0
        } else {
            at_ns
                .saturating_sub(self.reported_ns)
                .min(AIRBRUSH_STILL_NS)
        };
        self.reported_ns = self.reported_ns.max(at_ns);
        let began = at_ns - moving;
        let mut outcome = match self.take(began) {
            0 => Ok(()),
            held => dab(from, held),
        };
        let first = self.next_ns;
        for nth in 0..self.take(at_ns) {
            let into = first.saturating_add(u64::from(nth) * AIRBRUSH_INTERVAL_NS) - began;
            let point = Point {
                x: along(from.x, to.x, into, moving),
                y: along(from.y, to.y, into, moving),
            };
            if outcome.is_ok() {
                outcome = dab(point, 1);
            }
        }
        outcome
    }

    /// Nothing reported by `now_ns`: hand `dab` how many dabs the silence
    /// shows fell with the pointer held still at `at`, where it was last
    /// reported.
    ///
    /// # Errors
    ///
    /// The [`OutOfMemory`] `dab` answers; the dabs are spent all the same.
    pub fn held(
        &mut self,
        at: Point,
        now_ns: u64,
        dab: impl FnOnce(Point, u32) -> Result<(), OutOfMemory>,
    ) -> Result<(), OutOfMemory> {
        match self.take(now_ns.saturating_sub(AIRBRUSH_STILL_NS)) {
            0 => Ok(()),
            passes => dab(at, passes),
        }
    }

    /// How many dabs fall by `until_ns`, each then spent.
    fn take(&mut self, until_ns: u64) -> u32 {
        let Some(after) = until_ns.checked_sub(self.next_ns) else {
            return 0;
        };
        let count = after / AIRBRUSH_INTERVAL_NS + 1;
        self.next_ns = self
            .next_ns
            .saturating_add(count.saturating_mul(AIRBRUSH_INTERVAL_NS));
        u32::try_from(count).unwrap_or(u32::MAX)
    }
}

/// The coordinate `into` of `over` of the way from `from` to `to`.
fn along(from: i64, to: i64, into: u64, over: u64) -> i64 {
    let span = i128::from(to) - i128::from(from);
    let moved = span * i128::from(into) / i128::from(over.max(1));
    i64::try_from(i128::from(from) + moved).unwrap_or(to)
}

#[cfg(test)]
#[path = "brush_tests.rs"]
mod tests;
