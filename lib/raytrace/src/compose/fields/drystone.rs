//! A dry-stone wall of field stones, laid as they came off the land: each
//! face built up haphazard, every stone let fall where the work stands lowest
//! to settle in the nook it fits ([`pile`]), the largest at the foot and the
//! smallest under the top, which is capped with big stones laid across it;
//! hearting packed between the faces. A field stone is broken angular along
//! the planes it split on, never squared. Here and there a stretch has
//! tumbled, its stones down beside it.
//!
//! A stretch is laid window by window from where it starts, each window
//! finished before the next is begun and every stone drawn from the
//! stretch's own keys, so where a detail stops building never changes a stone
//! nearer the eye. A wall's stones are solids rather than strewn rocks: tens
//! of thousands stand in one structure, each sized to the place it fills.

use alloc::vec::Vec;

use tairix_countryside::boundary::Boundary;
use tairix_countryside::plane::{self, Walk};
use tairix_countryside::Point;
use tairix_util::mathf;

use super::{along, claim, leant, spaced, stretches, tipped, Laid};
use crate::compose::courses::{Dressing, Mason};
use crate::compose::{Dice, Stage};
use crate::noise::smoothstep;
use crate::solid::{Form, ROCK_FACE};
use crate::vector::{byte, power, Frame, Pose, Vec3};

mod pile;

use pile::{Pile, Stone};

/// How deep a wall is founded below the ground at its bays' ends.
const FOUNDED: f64 = 0.12;

/// The least and the most a stone reaches about its middle at the wall's
/// foot; how much smaller they run under its top; and how far above the top
/// a stone may stand.
const REACH: (f64, f64) = (0.035, 0.2);
const TOPMOST: f64 = 0.55;
const PROUD: f64 = 0.04;

/// The least a stone reaches, however near the top it is set.
const LEAST: f64 = 0.025;

/// How far below the top the work may still dip between the stones under
/// it, once it is done; and how much a notch no stone settles in is raised
/// by, left a void.
const DIP: f64 = 0.05;
const BRIDGE: f64 = 0.02;

/// How long a stretch of the work is raised to its top before the next.
const WINDOW: f64 = 1.2;

/// The two faces of a wall, across it: its left, and its right.
const FACES: [f64; 2] = [-1.0, 1.0];

/// How far the ends of a breach run, from where it has tumbled to its low
/// back up to where the wall stands whole.
const BREACH: f64 = 0.5;

/// How worn a field stone's arrises are, as shares of its least half
/// extent; and how many planes it broke along, its bed and its face among
/// them.
const WORN: (f64, f64) = (0.1, 0.25);
const FRACTURES: (u32, u32) = (6, 9);

/// How broad, how tall and how deep a stone is drawn, as shares of how far
/// it reaches in the face it settled in: about the room it settled in, so
/// once its corners are broken off it meets its neighbours along its sides.
const BROAD: (f64, f64) = (0.98, 1.08);
const TALL: (f64, f64) = (0.96, 1.04);
const DEPTH: (f64, f64) = (0.85, 1.25);

/// Lay the dry-stone wall `boundary` along `laid`'s line, out to `reach` from
/// the eye, by `mason`, its section drawn from `draws`.
pub(super) fn wall(
    stage: &mut Stage,
    laid: &Laid<'_>,
    (boundary, draws): (&Boundary, &mut Dice),
    (reach, mason): (f64, &mut Mason),
) -> Option<()> {
    let shape = Battered {
        height: draws.range(1.05, 1.45),
        base: draws.range(0.65, 0.85),
        top: draws.range(0.32, 0.42),
        cap: draws.range(0.16, 0.26),
        long: draws.range(1.25, 1.9),
    };
    let mut waller = Waller {
        mason,
        shape,
        bays: Vec::new(),
        tumbles: Vec::new(),
    };
    let length = plane::length(laid.line);
    for (index, stretch) in stretches(&boundary.gaps, (0.0, length), 0.6)?.into_iter().enumerate() {
        let mut draws = Dice::keyed(boundary.key, index + 1);
        waller.stretch(stage, laid, stretch, (reach, &mut draws))?;
    }
    claim(stage, laid.line, 0.5 * shape.base)
}

/// A field stone's form drawn from `draws`: broken angular, its arrises
/// blunted by the weather.
fn broken(draws: &mut Dice) -> Form {
    Form::Rock {
        round: byte(draws.range(WORN.0, WORN.1)),
        facets: u8::try_from(draws.count(FRACTURES.0, FRACTURES.1)).unwrap_or(u8::MAX),
    }
}

/// A dry-stone wall's section: its height to its cap, its breadth at its foot
/// and at its top, how tall its capstones stand, and how many times longer
/// than tall its stones run along it, laid flat as they lie best.
#[derive(Copy, Clone, Debug)]
struct Battered {
    height: f64,
    base: f64,
    top: f64,
    cap: f64,
    long: f64,
}

impl Battered {
    /// How far each face stands from the wall's middle `y` up it.
    fn half_at(&self, y: f64) -> f64 {
        let share = (y / self.height).clamp(0.0, 1.0);
        0.5 * (self.base + (self.top - self.base) * share)
    }

    /// How far each face leans in from upright.
    fn batter(&self) -> f64 {
        mathf::atan2(0.5 * (self.base - self.top), self.height)
    }

    /// How far a stone let down onto work standing `low`, drawn from
    /// `draws`, reaches: most of them small and a few large, the largest at
    /// the foot; none so large it would stand far above the top, nor broader
    /// than the wall lets it lie deep, so every one is about as deep as it is
    /// broad.
    fn reach(&self, low: f64, draws: &mut Dice) -> f64 {
        let scale = 1.0 - (1.0 - TOPMOST) * (low / self.height).clamp(0.0, 1.0);
        let drawn = scale * (REACH.0 + (REACH.1 - REACH.0) * power(draws.unit(), 1.7));
        let deepest = self.deepest(low + drawn) / DEPTH.0;
        drawn.min(0.5 * (self.height + PROUD - low)).min(deepest).max(LEAST)
    }

    /// The deepest a face's stone set `y` up the wall lies into it: half as
    /// deep as the wall is broad there, so the two faces' stones meet in the
    /// hearting rather than in each other.
    fn deepest(&self, y: f64) -> f64 {
        0.42 * self.half_at(y) - 0.01
    }
}

/// A stone of a stretch, in the stretch's own frame: its middle `x` along it,
/// `y` up from its foot and `z` across it to its right; `half` its size each
/// way; how it settles, tipped along the wall, leant across it and turned
/// about the upright; and its form.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Placed {
    middle: Vec3,
    half: Vec3,
    settle: (f64, f64, f64),
    form: Form,
}

/// Lay the stones of a stretch of wall to `shape` from `start` to `end`, as
/// far as the window past the one holding `last`, each drawn from the
/// streams keyed from `seed`: each window's faces raised to the top, then the
/// capstones set a window behind, where the top stands finished either side
/// of each. Every stone is handed to `lay`; the faces as they then stand are
/// returned.
///
/// A face is packed with its length squeezed by the stones' `long`, so the
/// round stones it settles stand for flat ones that long, as an affine copy
/// of a packing still is one, each resting in its nook.
fn courses(
    shape: &Battered,
    ((start, end), last): ((f64, f64), f64),
    seed: u64,
    lay: &mut dyn FnMut(Placed) -> Option<()>,
) -> Option<[Pile; 2]> {
    let long = shape.long;
    let packed = (start / long, end / long);
    let mut faces = [Pile::new(packed)?, Pile::new(packed)?];
    let mut draws = [Dice::keyed(seed, 0), Dice::keyed(seed, 1)];
    let mut capping = Capping {
        draws: Dice::keyed(seed, 2),
        at: start,
        end,
    };
    capping.at += capping.draws.range(0.0, 0.08);
    let stop = start + WINDOW * (mathf::floor((last - start).max(0.0) / WINDOW) + 2.0);
    let mut window = start;
    while window < end.min(stop) {
        let edge = (window + WINDOW).min(end);
        for (side, (face, draws)) in FACES.into_iter().zip(faces.iter_mut().zip(&mut draws)) {
            while let Some((at, low)) = face.lowest((window / long, edge / long)).filter(|&(_, low)| low < shape.height - DIP) {
                let r = shape.reach(low, draws);
                let dropped = face.x_of(at) + r * draws.range(-0.6, 0.6);
                // A stone that would rest standing proud of the top is not
                // set, and the notch it could not fill is left a void.
                match face.rest(r, dropped).filter(|&(_, y)| y + r <= shape.height + PROUD) {
                    Some((x, y)) => {
                        let stone = Stone { x, y, r };
                        face.set(stone)?;
                        lay(face_stone(shape, (stone, side), draws))?;
                    }
                    None => face.bridge(at, BRIDGE),
                }
            }
        }
        capping.lay(&faces, window, shape, lay)?;
        window = edge;
    }
    if window >= end {
        capping.lay(&faces, end, shape, lay)?;
    }
    Some(faces)
}

/// `stone` set in the face `side` of a wall to `shape`, drawn from `draws`:
/// as deep into the wall as it is broad or so, turned to show its face, and
/// set by it flush with the batter, a centimetre or so proud or back, tipped
/// in the face as it settled but never twisted out of it.
fn face_stone(shape: &Battered, (stone, side): (Stone, f64), draws: &mut Dice) -> Placed {
    let half = shape.half_at(stone.y);
    let deep = (stone.r * draws.range(DEPTH.0, DEPTH.1)).min(shape.deepest(stone.y)).max(0.6 * stone.r);
    let proud = draws.range(-0.02, 0.01);
    let size = Vec3::new(shape.long * stone.r * draws.range(BROAD.0, BROAD.1), stone.r * draws.range(TALL.0, TALL.1), deep);
    let outward = if side < 0.0 { core::f64::consts::PI } else { 0.0 };
    let settle = (
        draws.range(-0.25, 0.25),
        -shape.batter() + draws.range(-0.04, 0.04),
        outward + draws.range(-0.08, 0.08),
    );
    Placed {
        middle: Vec3::new(shape.long * stone.x, stone.y, side * (half - ROCK_FACE * deep + proud)),
        half: size,
        settle,
        form: broken(draws),
    }
}

/// A stretch's capstones being laid: where the next stands along it, drawn
/// from `draws`, and where the stretch ends.
struct Capping {
    draws: Dice,
    at: f64,
    end: f64,
}

impl Capping {
    /// Set the capstones starting short of `before` along the finished top of
    /// both `faces` of a wall to `shape`: big stones laid across the top one
    /// after another, each bedded among the stones under it, a few long gone.
    fn lay(&mut self, faces: &[Pile; 2], before: f64, shape: &Battered, lay: &mut dyn FnMut(Placed) -> Option<()>) -> Option<()> {
        while self.at < before {
            let draws = &mut self.draws;
            let long = draws.range(0.16, 0.34).min(self.end - self.at);
            let tall = shape.cap * draws.range(0.7, 1.15);
            let (wide, gap, gone) = (draws.range(0.0, 0.06), draws.range(0.0, 0.04), draws.chance(0.1));
            let settle = (draws.range(-0.15, 0.15), draws.range(-0.06, 0.06), draws.range(-0.15, 0.15));
            let form = broken(draws);
            let span = (self.at, self.at + long);
            self.at = span.1 + gap;
            if gone || long < 0.06 {
                continue;
            }
            let packed = (span.0 / shape.long, span.1 / shape.long);
            let ((high_a, mean_a), (high_b, mean_b)) = (faces[0].top(packed), faces[1].top(packed));
            let (high, mean) = (high_a.max(high_b), 0.5 * (mean_a + mean_b));
            let seat = high - 0.35 * (high - mean);
            lay(Placed {
                middle: Vec3::new(0.5 * (span.0 + span.1), seat + 0.5 * tall - 0.02, 0.0),
                half: Vec3::new(0.5 * long, 0.5 * tall, shape.half_at(shape.height) + wide),
                settle,
                form,
            })?;
        }
        Some(())
    }
}

/// A bay of a stretch of wall, founded on the ground: from `from` to `to`
/// along the stretch, its foot where it starts and the frame it is laid in,
/// its `x` along the wall rising with the ground, `y` up it and `z` across
/// it to its right; how much longer it runs on the ground than along the
/// line's plan; and whether it lies within reach of the eye.
#[derive(Copy, Clone, Debug)]
struct Bay {
    from: f64,
    to: f64,
    foot: Vec3,
    frame: Frame,
    stretch: f64,
    near: bool,
}

impl Bay {
    /// Where `local`, in its stretch's own frame, stands in the bay.
    fn place(&self, local: Vec3) -> Vec3 {
        self.foot + self.frame.x * ((local.x - self.from) * self.stretch) + self.frame.y * local.y + self.frame.z * local.z
    }
}

/// A stretch of wall fallen to `low` of its height from `from` to `to`, its
/// stones lying beside it drawn from `seed`.
#[derive(Copy, Clone, Debug)]
struct Tumble {
    from: f64,
    to: f64,
    low: f64,
    seed: u64,
}

/// How high a wall `height` tall still stands `x` along a stretch that has
/// tumbled where `tumbles` have.
fn standing(tumbles: &[Tumble], height: f64, x: f64) -> f64 {
    tumbles.iter().fold(height, |standing, tumble| {
        let out = (tumble.from - x).max(x - tumble.to).max(0.0);
        let whole = smoothstep(0.0, BREACH, out);
        standing.min(height * (tumble.low + (1.0 - tumble.low) * whole))
    })
}

/// A wall being laid: by `mason` to `shape`, over the bays and the tumbles
/// of the stretch at hand.
struct Waller<'a> {
    mason: &'a mut Mason,
    shape: Battered,
    bays: Vec<Bay>,
    tumbles: Vec<Tumble>,
}

impl Waller<'_> {
    /// Lay the stretch `(start, end)` of the wall along `laid`'s line, out to
    /// `reach` from the eye, from `draws`.
    fn stretch(&mut self, stage: &Stage, laid: &Laid<'_>, (start, end): (f64, f64), (reach, draws): (f64, &mut Dice)) -> Option<()> {
        self.found(stage, laid, (start, end), (draws.range(1.3, 2.0), reach))?;
        let Some(last) = self.bays.iter().rev().find(|bay| bay.near).map(|bay| bay.to) else {
            return Some(());
        };
        self.tumble((start, end), draws)?;
        let (shape, seed) = (self.shape, draws.wide());
        courses(&shape, ((start, end), last), seed, &mut |placed| self.set(placed))?;
        self.hearting((start, end))?;
        for index in 0..self.tumbles.len() {
            if let Some(&tumble) = self.tumbles.get(index) {
                self.fallen(stage, laid, tumble)?;
            }
        }
        Some(())
    }

    /// Found the bays of the stretch `(start, end)` of `laid`'s line, about
    /// `spacing` long, each on the ground at its two ends and within `reach`
    /// of the eye where either is.
    fn found(&mut self, stage: &Stage, laid: &Laid<'_>, (start, end): (f64, f64), (spacing, reach): (f64, f64)) -> Option<()> {
        self.bays.clear();
        let mut walk = Walk::new(laid.line);
        let mut before: Option<(f64, Point, f64)> = None;
        for x in spaced((start, end), spacing) {
            let Some((at, _)) = walk.at(x) else {
                continue;
            };
            let ground = laid.ground(stage, at);
            if let Some((from, back, low)) = before {
                let chord = at - back;
                let (run, rise) = (chord.length(), ground - low);
                if run > 1e-6 && x > from {
                    let near = (back - laid.eye).length() <= reach || (at - laid.eye).length() <= reach;
                    self.bays.try_reserve(1).ok()?;
                    self.bays.push(Bay {
                        from,
                        to: x,
                        foot: Vec3::new(back.x, low - FOUNDED, back.y),
                        frame: tipped(along(chord * (1.0 / run)), mathf::atan2(rise, run)),
                        stretch: mathf::hypot(run, rise) / (x - from),
                        near,
                    });
                }
            }
            before = Some((x, at, ground));
        }
        Some(())
    }

    /// Draw where the stretch `(start, end)` has tumbled: a few metres every
    /// few tens, fallen to a quarter or half its height.
    fn tumble(&mut self, (start, end): (f64, f64), draws: &mut Dice) -> Option<()> {
        self.tumbles.clear();
        let mut at = start + draws.range(5.0, 60.0);
        while at < end {
            let long = draws.range(1.2, 3.2);
            let tumble = Tumble {
                from: at,
                to: (at + long).min(end),
                low: draws.range(0.25, 0.55),
                seed: draws.wide(),
            };
            self.tumbles.try_reserve(1).ok()?;
            self.tumbles.push(tumble);
            at += long + draws.range(15.0, 75.0);
        }
        Some(())
    }

    /// How high the wall still stands `x` along its stretch.
    fn standing(&self, x: f64) -> f64 {
        standing(&self.tumbles, self.shape.height, x)
    }

    /// The bay `x` along the stretch lies in.
    fn bay_at(&self, x: f64) -> Option<&Bay> {
        self.bays.get(self.bays.partition_point(|bay| bay.to <= x))
    }

    /// Set `placed` in its bay, where that lies within reach and the wall
    /// still stands there.
    fn set(&mut self, placed: Placed) -> Option<()> {
        let (x, top) = (placed.middle.x, placed.middle.y + placed.half.y);
        if top > self.standing(x) + 0.03 {
            return Some(());
        }
        let Some(&bay) = self.bay_at(x).filter(|bay| bay.near) else {
            return Some(());
        };
        let (tip, lean, turn) = placed.settle;
        let frame = leant(tipped(bay.frame, tip), lean).rotated_by(Frame::about(bay.frame.y, turn));
        self.mason.unit((Pose::new(bay.place(placed.middle), frame), placed.half), placed.form, Dressing::Field)
    }

    /// The hearting packed between the faces of each bay of the stretch
    /// `(start, end)` within reach: one rough core, battered as the faces are,
    /// behind the shallowest stone either face holds and below the capstones
    /// and wherever the wall stands lowest over the bay, short of the
    /// stretch's ends, where it would show.
    fn hearting(&mut self, (start, end): (f64, f64)) -> Option<()> {
        for index in 0..self.bays.len() {
            let Some(&bay) = self.bays.get(index).filter(|bay| bay.near) else {
                continue;
            };
            let (from, to) = (bay.from.max(start + 0.3), bay.to.min(end - 0.3));
            if to - from < 0.1 {
                continue;
            }
            let lowest = self
                .tumbles
                .iter()
                .flat_map(|tumble| [tumble.from, tumble.to])
                .map(|x| x.clamp(from, to))
                .chain([from, to])
                .fold(self.shape.height, |low, x| low.min(self.standing(x)));
            let top = lowest - 0.12;
            let (foot, head) = (self.shape.half_at(0.0) - 0.13, (self.shape.half_at(top) - 0.13).max(0.03));
            if top < 0.1 || foot <= 0.0 {
                continue;
            }
            let fan = i8::try_from(mathf::round_i32(100.0 * (head - foot) / (head + foot))).unwrap_or(0);
            let frame = Frame {
                x: bay.frame.z,
                y: bay.frame.y,
                z: -bay.frame.x,
            };
            let half = Vec3::new(0.5 * (foot + head), 0.5 * top, 0.5 * (to - from) * bay.stretch);
            let middle = bay.place(Vec3::new(0.5 * (from + to), 0.5 * top, 0.0));
            self.mason.unit((Pose::new(middle, frame), half), Form::Block { fan }, Dressing::Rubble)?;
        }
        Some(())
    }

    /// The stones `tumble` shed, lying on the ground beside it within reach,
    /// most to the side it fell toward.
    fn fallen(&mut self, stage: &Stage, laid: &Laid<'_>, tumble: Tumble) -> Option<()> {
        let mut draws = Dice::keyed(tumble.seed, 0);
        let shape = self.shape;
        let missing = (tumble.to - tumble.from) * (1.0 - tumble.low) * shape.height;
        let count = mathf::round_i32(missing * 28.0).max(0);
        let toward = draws.sign();
        for _ in 0..count {
            let x = draws.range(tumble.from - 0.3, tumble.to + 0.3);
            let side = if draws.chance(0.75) { toward } else { -toward };
            let across = side * (0.5 * shape.base + draws.range(0.05, 1.1));
            let reach = shape.reach(0.0, &mut draws);
            let size = Vec3::new(reach * draws.range(0.9, 1.1), reach * draws.range(0.6, 0.9), reach * draws.range(0.8, 1.2));
            let (yaw, tip, lean) = (draws.range(0.0, core::f64::consts::TAU), draws.range(-0.3, 0.3), draws.range(-0.3, 0.3));
            let form = broken(&mut draws);
            let Some(&bay) = self.bay_at(x.clamp(tumble.from, tumble.to)).filter(|bay| bay.near) else {
                continue;
            };
            let beside = bay.place(Vec3::new(x, 0.0, across));
            let at = Point::new(beside.x, beside.z);
            let ground = laid.ground(stage, at);
            let frame = leant(tipped(Frame::turned(yaw, 0.0), tip), lean);
            self.mason.unit(
                (Pose::new(Vec3::new(at.x, ground + 0.6 * size.y, at.y), frame), size),
                form,
                Dressing::Field,
            )?;
        }
        Some(())
    }
}

#[cfg(test)]
#[path = "drystone_tests.rs"]
mod tests;
