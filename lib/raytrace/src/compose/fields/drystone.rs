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

use tairix_countryside::boundary::{self, Boundary};
use tairix_countryside::plane::{self, Walk};
use tairix_countryside::Point;
use tairix_util::mathf;

use super::{along, divided, leant, spaced, tipped, Laid};
use crate::compose::courses::Dressing;
use crate::compose::Dice;
use crate::heightfield::Heightfield;
use crate::noise::smoothstep;
use crate::solid::{Form, ROCK_FACE};
use crate::vector::{byte, power, Frame, Pose, Vec3};

mod pile;

use pile::{Pile, Stone};

/// How deep a wall is founded below the ground at its bays' ends.
pub(super) const FOUNDED: f64 = 0.12;

/// How tall a capstone stands, as shares of how tall a wall's capstones run,
/// and how far it reaches out over the wall's top either side.
pub(super) const CAPPED: (f64, f64) = (0.7, 1.15);
pub(super) const OVERHANG: (f64, f64) = (0.0, 0.06);

/// The least and the most a stone reaches about its middle at the wall's
/// foot; how much smaller they run under its top; and how far above the top
/// a stone may stand.
const REACH: (f64, f64) = (0.035, 0.2);

/// How tall a wall's typical stone stands: twice the reach of one drawn as
/// [`Battered::reach`] draws them, most of them small.
pub(super) const TYPICAL: f64 = 2.0 * (REACH.0 + (REACH.1 - REACH.0) / 2.7);
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

/// A unit of a wall as its mason is to lay it: where and how big, and its
/// form and dressing.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct Unit {
    pub(super) placing: (Pose, Vec3),
    pub(super) form: Form,
    pub(super) dressing: Dressing,
}

/// A dry-stone wall being laid along a boundary, stretch by stretch and
/// window by window, its units kept in the order laid for its mason.
#[derive(Debug)]
pub(super) struct Walling {
    shape: Battered,
    key: u64,
    stretches: Vec<(f64, f64)>,
    /// The next stretch to begin, and the one in hand.
    next: usize,
    stretch: Option<Stretch>,
    pub(super) units: Vec<Unit>,
    /// How many of its units are handed to its mason.
    pub(super) committed: usize,
}

/// A stretch of wall in hand: where it runs, its bays and tumbles, its two
/// faces as they stand and their draws, its capping, the window being raised,
/// and the window it stops before.
#[derive(Debug)]
struct Stretch {
    span: (f64, f64),
    bays: Vec<Bay>,
    tumbles: Vec<Tumble>,
    faces: [Pile; 2],
    draws: [Dice; 2],
    capping: Capping,
    window: f64,
    stop: f64,
}

impl Walling {
    /// The wall `boundary` is to be laid as, its section drawn from `draws`
    /// up to its capstones' tops standing as tall as the boundary does;
    /// `None` when the heap will not hold its stretches.
    pub(super) fn new(boundary: &Boundary, draws: &mut Dice) -> Option<Self> {
        let cap = draws.range(0.16, 0.26);
        let shape = Battered {
            height: boundary.height - cap * f64::midpoint(CAPPED.0, CAPPED.1),
            base: draws.range(0.65, 0.85),
            top: draws.range(0.32, 0.42),
            cap,
            long: draws.range(1.25, 1.9),
        };
        let length = plane::length(&boundary.line);
        Some(Self {
            shape,
            key: boundary.key,
            stretches: boundary::standing(&boundary.gaps, (0.0, length), 0.6).ok()?,
            next: 0,
            stretch: None,
            units: Vec::new(),
            committed: 0,
        })
    }

    /// How far either side of its line its foot claims the ground.
    pub(super) fn foot(&self) -> f64 {
        0.5 * self.shape.base
    }

    /// The wall's section: how high it stands to its cap, and how tall its
    /// capstones stand.
    pub(super) const fn section(&self) -> (f64, f64) {
        (self.shape.height, self.shape.cap)
    }

    /// How far each of the wall's faces stands from its middle `y` up it.
    pub(super) fn half_at(&self, y: f64) -> f64 {
        self.shape.half_at(y)
    }

    /// The stretches the wall stands in between its gaps.
    pub(super) fn spans(&self) -> &[(f64, f64)] {
        &self.stretches
    }

    /// How the wall's stretch `index`, `span`, is laid, drawn from its own
    /// key; `None` when the heap will not hold where it tumbled.
    pub(super) fn plan(&self, index: usize, span: (f64, f64)) -> Option<Plan> {
        let mut draws = Dice::keyed(self.key, index + 1);
        let spacing = draws.range(1.3, 2.0);
        let tumbles = tumbled(span, &mut draws)?;
        Some(Plan {
            span,
            spacing,
            tumbles,
            seed: draws.wide(),
        })
    }

    /// Lay on along `laid`'s line over `fields`, out to `reach` from the eye,
    /// until about `stones` more are settled; whether it is whole, or `None`
    /// when the heap will not hold it.
    pub(super) fn step(
        &mut self,
        (laid, fields): (&Laid<'_>, &[Heightfield]),
        (reach, stones): (f64, usize),
    ) -> Option<bool> {
        let mut settled = 0usize;
        while settled < stones {
            let Some(stretch) = self.stretch.as_mut() else {
                let Some(&span) = self.stretches.get(self.next) else {
                    return Some(true);
                };
                // A stretch none of which lies within reach of the eye is
                // passed over.
                let plan = self.plan(self.next, span)?;
                let bays = found(span, (laid, fields), (plan.spacing, reach))?;
                match bays.iter().rev().find(|bay| bay.near).map(|bay| bay.to) {
                    Some(last) => {
                        self.stretch = Some(Stretch::new(&self.shape, (bays, plan), last)?);
                    }
                    None => self.next += 1,
                }
                continue;
            };
            let (whole, spent) = stretch.raise(&self.shape, &mut self.units, stones - settled)?;
            settled += spent.max(1);
            if !whole {
                return Some(false);
            }
            let stretch = self.stretch.take()?;
            stretch.hearting(&self.shape, &mut self.units)?;
            for tumble in &stretch.tumbles {
                stretch.fallen(&self.shape, (laid, fields), *tumble, &mut self.units)?;
            }
            self.next += 1;
        }
        Some(self.next >= self.stretches.len() && self.stretch.is_none())
    }
}

/// How a stretch of wall is laid, drawn from its own key: where it runs, how
/// long its bays run, where it has tumbled, and what its stones are drawn
/// under.
#[derive(Debug)]
pub(super) struct Plan {
    span: (f64, f64),
    spacing: f64,
    tumbles: Vec<Tumble>,
    seed: u64,
}

impl Plan {
    /// How high a wall `height` tall still stands `x` along the stretch.
    pub(super) fn standing(&self, height: f64, x: f64) -> f64 {
        standing(&self.tumbles, height, x)
    }

    /// Whether the bay `x` along the stretch lies in is laid stone by stone:
    /// whether either of its ends, read on `walk`, lies within `reach` of
    /// `eye`.
    pub(super) fn laid(&self, (walk, eye): (&mut Walk<'_>, Point), x: f64, reach: f64) -> bool {
        let (from, to) = self.span;
        let count = divided(self.span, self.spacing);
        let bay = mathf::floor((x - from) / (to - from) * f64::from(count))
            .clamp(0.0, f64::from(count - 1));
        let first = mathf::round_i32(bay);
        let end = |index: i32| from + (to - from) * f64::from(index) / f64::from(count);
        [end(first), end(first + 1)]
            .into_iter()
            .any(|along| walk.at(along).is_some_and(|(at, _)| within(eye, at, reach)))
    }
}

/// Whether `at` lies within `reach` of `eye`, as a bay's end is to be laid.
fn within(eye: Point, at: Point, reach: f64) -> bool {
    (at - eye).length() <= reach
}

impl Stretch {
    /// A stretch of a wall to `shape`, founded in `bays` and laid to `plan`,
    /// as far as the window past the one holding `last`; `None` when the
    /// heap will not hold its faces.
    fn new(shape: &Battered, (bays, plan): (Vec<Bay>, Plan), last: f64) -> Option<Self> {
        let (long, span, seed) = (shape.long, plan.span, plan.seed);
        let packed = (span.0 / long, span.1 / long);
        let mut capping = Capping {
            draws: Dice::keyed(seed, 2),
            at: span.0,
            end: span.1,
        };
        capping.at += capping.draws.range(0.0, 0.08);
        Some(Self {
            span,
            bays,
            tumbles: plan.tumbles,
            faces: [Pile::new(packed)?, Pile::new(packed)?],
            draws: [Dice::keyed(seed, 0), Dice::keyed(seed, 1)],
            capping,
            window: span.0,
            stop: span.0 + WINDOW * (mathf::floor((last - span.0).max(0.0) / WINDOW) + 2.0),
        })
    }
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
        f64::midpoint(self.base, (self.top - self.base) * share)
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
        let wanted = scale * (REACH.0 + (REACH.1 - REACH.0) * power(draws.unit(), 1.7));
        let deepest = self.deepest(low + wanted) / DEPTH.0;
        wanted
            .min(0.5 * (self.height + PROUD - low))
            .min(deepest)
            .max(LEAST)
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

impl Stretch {
    /// Raise the stretch's next windows of both faces to the top of a wall to
    /// `shape`, then set the capstones a window behind, where the top stands
    /// finished either side of each, into `units`, until `budget` stones are
    /// settled — kept or not — or the stretch stops; whether its stones are
    /// all laid, and how many it settled.
    ///
    /// A face is packed with its length squeezed by the stones' `long`, so the
    /// round stones it settles stand for flat ones that long, as an affine copy
    /// of a packing still is one, each resting in its nook.
    fn raise(
        &mut self,
        shape: &Battered,
        units: &mut Vec<Unit>,
        budget: usize,
    ) -> Option<(bool, usize)> {
        let long = shape.long;
        let (_, end) = self.span;
        let mut settled = 0usize;
        while self.window < end.min(self.stop) {
            if settled >= budget {
                return Some((false, settled));
            }
            let edge = (self.window + WINDOW).min(end);
            for (side, (face, draws)) in FACES
                .into_iter()
                .zip(self.faces.iter_mut().zip(&mut self.draws))
            {
                while let Some((at, low)) = face
                    .lowest((self.window / long, edge / long))
                    .filter(|&(_, low)| low < shape.height - DIP)
                {
                    let r = shape.reach(low, draws);
                    let dropped = face.x_of(at) + r * draws.range(-0.6, 0.6);
                    // A stone that would rest standing proud of the top is not
                    // set, and the notch it could not fill is left a void.
                    settled += 1;
                    match face
                        .rest(r, dropped)
                        .filter(|&(_, y)| y + r <= shape.height + PROUD)
                    {
                        Some((x, y)) => {
                            let stone = Stone { x, y, r };
                            face.set(stone)?;
                            set(
                                (&self.bays, &self.tumbles),
                                shape,
                                face_stone(shape, (stone, side), draws),
                                units,
                            )?;
                        }
                        None => face.bridge(at, BRIDGE),
                    }
                }
            }
            let (bays, tumbles) = (&self.bays, &self.tumbles);
            self.capping
                .lay(&self.faces, self.window, shape, &mut |placed| {
                    set((bays, tumbles), shape, placed, units)
                })?;
            self.window = edge;
        }
        if self.window >= end {
            let (bays, tumbles) = (&self.bays, &self.tumbles);
            self.capping.lay(&self.faces, end, shape, &mut |placed| {
                set((bays, tumbles), shape, placed, units)
            })?;
        }
        Some((true, settled))
    }
}

/// `stone` set in the face `side` of a wall to `shape`, drawn from `draws`:
/// as deep into the wall as it is broad or so, turned to show its face, and
/// set by it flush with the batter, a centimetre or so proud or back, tipped
/// in the face as it settled but never twisted out of it.
fn face_stone(shape: &Battered, (stone, side): (Stone, f64), draws: &mut Dice) -> Placed {
    let half = shape.half_at(stone.y);
    let deep = (stone.r * draws.range(DEPTH.0, DEPTH.1))
        .min(shape.deepest(stone.y))
        .max(0.6 * stone.r);
    let proud = draws.range(-0.02, 0.01);
    let extent = Vec3::new(
        shape.long * stone.r * draws.range(BROAD.0, BROAD.1),
        stone.r * draws.range(TALL.0, TALL.1),
        deep,
    );
    let outward = if side < 0.0 {
        core::f64::consts::PI
    } else {
        0.0
    };
    let settle = (
        draws.range(-0.25, 0.25),
        -shape.batter() + draws.range(-0.04, 0.04),
        outward + draws.range(-0.08, 0.08),
    );
    Placed {
        middle: Vec3::new(
            shape.long * stone.x,
            stone.y,
            side * (half - ROCK_FACE * deep + proud),
        ),
        half: extent,
        settle,
        form: broken(draws),
    }
}

/// A stretch's capstones being laid: where the next stands along it, drawn
/// from `draws`, and where the stretch ends.
#[derive(Debug)]
struct Capping {
    draws: Dice,
    at: f64,
    end: f64,
}

impl Capping {
    /// Set the capstones starting short of `before` along the finished top of
    /// both `faces` of a wall to `shape`: big stones laid across the top one
    /// after another, each bedded among the stones under it, a few long gone.
    fn lay(
        &mut self,
        faces: &[Pile; 2],
        before: f64,
        shape: &Battered,
        lay: &mut dyn FnMut(Placed) -> Option<()>,
    ) -> Option<()> {
        while self.at < before {
            let draws = &mut self.draws;
            let long = draws.range(0.16, 0.34).min(self.end - self.at);
            let tall = shape.cap * draws.range(CAPPED.0, CAPPED.1);
            let (wide, gap, gone) = (
                draws.range(OVERHANG.0, OVERHANG.1),
                draws.range(0.0, 0.04),
                draws.chance(0.1),
            );
            let settle = (
                draws.range(-0.15, 0.15),
                draws.range(-0.06, 0.06),
                draws.range(-0.15, 0.15),
            );
            let form = broken(draws);
            let span = (self.at, self.at + long);
            self.at = span.1 + gap;
            if gone || long < 0.06 {
                continue;
            }
            let packed = (span.0 / shape.long, span.1 / shape.long);
            let ((high_a, mean_a), (high_b, mean_b)) = (faces[0].top(packed), faces[1].top(packed));
            let (high, mean) = (high_a.max(high_b), f64::midpoint(mean_a, mean_b));
            let seat = high - 0.35 * (high - mean);
            lay(Placed {
                middle: Vec3::new(f64::midpoint(span.0, span.1), seat + 0.5 * tall - 0.02, 0.0),
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
        self.foot
            + self.frame.x * ((local.x - self.from) * self.stretch)
            + self.frame.y * local.y
            + self.frame.z * local.z
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

/// The bays the stretch `(start, end)` of `laid`'s line over `fields` is
/// founded in, about `spacing` long, each on the ground at its two ends and
/// within `reach` of the eye where either is.
fn found(
    (start, end): (f64, f64),
    (laid, fields): (&Laid<'_>, &[Heightfield]),
    (spacing, reach): (f64, f64),
) -> Option<Vec<Bay>> {
    let mut bays = Vec::new();
    let mut walk = Walk::new(laid.line);
    let mut before: Option<(f64, Point, f64)> = None;
    for x in spaced((start, end), spacing) {
        let Some((at, _)) = walk.at(x) else {
            continue;
        };
        let ground = laid.ground(fields, at);
        if let Some((from, back, low)) = before {
            let chord = at - back;
            let (run, rise) = (chord.length(), ground - low);
            if run > 1e-6 && x > from {
                let near = within(laid.eye, back, reach) || within(laid.eye, at, reach);
                bays.try_reserve(1).ok()?;
                bays.push(Bay {
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
    Some(bays)
}

/// Where the stretch `(start, end)` has tumbled, drawn from `draws`: a few
/// metres every few tens, fallen to a quarter or half its height.
fn tumbled((start, end): (f64, f64), draws: &mut Dice) -> Option<Vec<Tumble>> {
    let mut tumbles = Vec::new();
    let mut at = start + draws.range(5.0, 60.0);
    while at < end {
        let long = draws.range(1.2, 3.2);
        let tumble = Tumble {
            from: at,
            to: (at + long).min(end),
            low: draws.range(0.25, 0.55),
            seed: draws.wide(),
        };
        tumbles.try_reserve(1).ok()?;
        tumbles.push(tumble);
        at += long + draws.range(15.0, 75.0);
    }
    Some(tumbles)
}

/// The bay of `bays` that `x` along its stretch lies in.
fn bay_at(bays: &[Bay], x: f64) -> Option<&Bay> {
    bays.get(bays.partition_point(|bay| bay.to <= x))
}

/// `placed` set in its bay of `bays` into `units`, where that lies within
/// reach and a wall to `shape` still stands there for its `tumbles`: a whole
/// stretch keeps every stone and its capstones, a tumbled one none above
/// what is left of it.
fn set(
    (bays, tumbles): (&[Bay], &[Tumble]),
    shape: &Battered,
    placed: Placed,
    units: &mut Vec<Unit>,
) -> Option<()> {
    let (x, top) = (placed.middle.x, placed.middle.y + placed.half.y);
    let stands = standing(tumbles, shape.height, x);
    if stands < shape.height && top > stands + 0.03 {
        return Some(());
    }
    let Some(&bay) = bay_at(bays, x).filter(|bay| bay.near) else {
        return Some(());
    };
    let (tip, lean, turn) = placed.settle;
    let frame = leant(tipped(bay.frame, tip), lean).rotated_by(Frame::about(bay.frame.y, turn));
    units.try_reserve(1).ok()?;
    units.push(Unit {
        placing: (Pose::new(bay.place(placed.middle), frame), placed.half),
        form: placed.form,
        dressing: Dressing::Field,
    });
    Some(())
}

impl Stretch {
    /// How high a wall to `shape` still stands `x` along the stretch.
    fn standing(&self, shape: &Battered, x: f64) -> f64 {
        standing(&self.tumbles, shape.height, x)
    }

    /// The hearting packed between the faces of each bay within reach into
    /// `units`: one rough core, battered as a wall to `shape`'s faces are,
    /// behind the shallowest stone either face holds and below the capstones
    /// and wherever the wall stands lowest over the bay, short of the
    /// stretch's ends, where it would show.
    fn hearting(&self, shape: &Battered, units: &mut Vec<Unit>) -> Option<()> {
        let (start, end) = self.span;
        for bay in self.bays.iter().filter(|bay| bay.near) {
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
                .fold(shape.height, |low, x| low.min(self.standing(shape, x)));
            let top = lowest - 0.12;
            let (foot, head) = (
                shape.half_at(0.0) - 0.13,
                (shape.half_at(top) - 0.13).max(0.03),
            );
            if top < 0.1 || foot <= 0.0 {
                continue;
            }
            let fan =
                i8::try_from(mathf::round_i32(100.0 * (head - foot) / (head + foot))).unwrap_or(0);
            let frame = Frame {
                x: bay.frame.z,
                y: bay.frame.y,
                z: -bay.frame.x,
            };
            let half = Vec3::new(
                f64::midpoint(foot, head),
                0.5 * top,
                0.5 * (to - from) * bay.stretch,
            );
            let middle = bay.place(Vec3::new(f64::midpoint(from, to), 0.5 * top, 0.0));
            units.try_reserve(1).ok()?;
            units.push(Unit {
                placing: (Pose::new(middle, frame), half),
                form: Form::Block { fan },
                dressing: Dressing::Rubble,
            });
        }
        Some(())
    }

    /// The stones `tumble` shed from a wall to `shape`, lying on the ground of
    /// `laid`'s land beside it within reach, most to the side it fell toward,
    /// into `units`.
    fn fallen(
        &self,
        shape: &Battered,
        (laid, fields): (&Laid<'_>, &[Heightfield]),
        tumble: Tumble,
        units: &mut Vec<Unit>,
    ) -> Option<()> {
        let mut draws = Dice::keyed(tumble.seed, 0);
        let missing = (tumble.to - tumble.from) * (1.0 - tumble.low) * shape.height;
        let count = mathf::round_i32(missing * 28.0).max(0);
        let toward = draws.sign();
        for _ in 0..count {
            let x = draws.range(tumble.from - 0.3, tumble.to + 0.3);
            let side = if draws.chance(0.75) { toward } else { -toward };
            let across = side * (0.5 * shape.base + draws.range(0.05, 1.1));
            let reach = shape.reach(0.0, &mut draws);
            let extent = Vec3::new(
                reach * draws.range(0.9, 1.1),
                reach * draws.range(0.6, 0.9),
                reach * draws.range(0.8, 1.2),
            );
            let (yaw, tip, lean) = (
                draws.range(0.0, core::f64::consts::TAU),
                draws.range(-0.3, 0.3),
                draws.range(-0.3, 0.3),
            );
            let form = broken(&mut draws);
            let Some(&bay) =
                bay_at(&self.bays, x.clamp(tumble.from, tumble.to)).filter(|bay| bay.near)
            else {
                continue;
            };
            let beside = bay.place(Vec3::new(x, 0.0, across));
            let at = Point::new(beside.x, beside.z);
            let ground = laid.ground(fields, at);
            let frame = leant(tipped(Frame::turned(yaw, 0.0), tip), lean);
            units.try_reserve(1).ok()?;
            units.push(Unit {
                placing: (
                    Pose::new(Vec3::new(at.x, ground + 0.6 * extent.y, at.y), frame),
                    extent,
                ),
                form,
                dressing: Dressing::Field,
            });
        }
        Some(())
    }
}

#[cfg(test)]
#[path = "drystone_tests.rs"]
mod tests;
