//! A farmed land's boundaries as they stand about the eye:
//!
//! - hedges of thorn and hazel, an oak standing in them now and then and a
//!   collapsed stretch mended with post and rail;
//! - dry-stone walls of field stones ([`drystone`]);
//! - fences of posts and rails, true where they are kept and leaning, mossed
//!   and missing rails where not;
//! - the gates hung in their gateways, shut, open or off a hinge, and the
//!   stiles its paths cross by.
//!
//! Each boundary draws from its own key, so how many a detail sets out
//! never changes what the rest of a scene draws. Beyond the reach each kind
//! stands built, it is painted on the ground where it runs.

use alloc::vec::Vec;
use core::f64::consts::FRAC_PI_2;

use tairix_countryside::boundary::{Boundary, Gap, Kind as Bound, Side, Through};
use tairix_countryside::layout::Layout;
use tairix_countryside::plane::{self, Walk};
use tairix_countryside::usage::Use;
use tairix_countryside::{self as countryside, Point};
use tairix_util::mathf;

use super::courses::{Dressing, Mason, Quarry, Weathering};
use super::landscape::{ground_of, Vantage};
use super::plants::{self, Grove, Kind, Stand};
use super::{Dice, Stage};
use crate::course::{Courses, Mark, Reach};
use crate::heightfield::Heightfield;
use crate::ground::Bounds as Painted;
use crate::land::Land;
use crate::noise::noise2;
use crate::solid::Form;
use crate::tree::Season;
use crate::vector::{Frame, Pose, Vec3};
use crate::wood::{rooted, Habit};

mod drystone;

/// How a hedge is planted: thorn the most, hazel among it.
const HAWTHORN: f64 = 0.68;

/// How many of a land's hedges have trees standing in them.
const STOOD: f64 = 0.4;

/// Set out the boundaries of `land`'s countryside about `vantage` in
/// `season`, an oak of `grove`'s standing in its hedges now and then, and
/// its woodlots stood with `grove`'s trees; `None` when the heap will not
/// hold them.
pub(super) fn set_out(
    stage: &mut Stage,
    dice: &mut Dice,
    land: &Land,
    (vantage, season, grove): (&Vantage, Season, &Grove),
) -> Option<()> {
    let Some(layout) = land.layout.as_ref() else {
        return Some(());
    };
    let standard = grove.of(Kind::Oak).or_else(|| grove.first()).map(|grown| grown.habit);
    let reach = stage.densities.bounds;
    let eye = Point::new(vantage.eye.x, vantage.eye.z);
    let hedging = Grove::new(stage, dice, (&[Kind::Hawthorn, Kind::Hazel], season), Stand::Open)?;
    let shrubs = [hedging.of(Kind::Hawthorn)?.habit, hedging.of(Kind::Hazel)?.habit];
    let quarry = dice.pick(&[Quarry::Limestone, Quarry::Sandstone, Quarry::Granite])?;
    // Field walls and fences run over the whole land, so no one height marks
    // their splashed foot: their damp shows in patches instead. Open to sun
    // and wind they dry quickly, so moss keeps to a few damp patches and
    // lichen has the rest.
    let weathering = Weathering {
        damp: dice.range(0.1, 0.4),
        drought: dice.range(0.1, 0.4),
        foot: -1.0e4,
    };
    let (walled, sawn) = (dice.range(0.6, 0.95), dice.range(0.35, 0.9));
    let stonework = stage.fieldwork(dice, quarry, (walled, weathering))?;
    let mut stone = Mason::new(stonework, dice.seed())?;
    let timberwork = stage.timberwork(dice, (sawn, weathering), None)?;
    let mut timber = Mason::new(timberwork, dice.seed())?;
    let mut near: Vec<(f64, &Boundary)> = Vec::new();
    let farthest = reach.hedges.max(reach.walls).max(reach.fences).max(reach.standards);
    for boundary in layout.boundaries() {
        let apart = plane::nearest(&boundary.line, eye).map_or(f64::INFINITY, |near| near.distance);
        if apart < farthest {
            near.try_reserve(1).ok()?;
            near.push((apart, boundary));
        }
    }
    near.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then(a.1.id.cmp(&b.1.id)));
    let mut room = budget(stage.room(), reach.share);
    woodlots(stage, land, (layout, grove), ((eye, vantage.heading), reach.standards), &mut room)?;
    for &(apart, boundary) in &near {
        let mut draws = Dice::keyed(boundary.key, 0);
        let laid = Laid {
            land,
            line: &boundary.line,
            eye,
        };
        match boundary.kind {
            Bound::Hedge if apart < reach.hedges => {
                hedge(stage, &laid, (&shrubs, standard.as_ref()), (boundary, &mut draws), (reach.hedges, &mut room), &mut timber)?;
            }
            Bound::Hedge if apart < reach.standards => {
                if let Some(standard) = &standard {
                    standards(stage, &laid, standard, (boundary, &mut draws), &mut room)?;
                }
            }
            Bound::Wall if apart < reach.walls => {
                drystone::wall(stage, &laid, (boundary, &mut draws), (reach.walls, &mut stone))?;
            }
            Bound::Fence if apart < reach.fences => {
                let kept = draws.chance(0.6);
                let length = plane::length(&boundary.line);
                fence(stage, &laid, &mut timber, &mut draws, ((0.0, length), &boundary.gaps, kept))?;
            }
            Bound::Hedge | Bound::Wall | Bound::Fence | Bound::Ditch | Bound::Open => {}
        }
        if boundary.kind != Bound::Open && apart < reach.fences {
            for gap in &boundary.gaps {
                match gap.through {
                    Through::Gateway => gate(stage, &laid, (boundary, gap), &mut timber)?,
                    Through::Path => stile(stage, &laid, (boundary, gap), (&mut timber, &mut stone))?,
                }
            }
        }
    }
    paint(stage, land, layout.boundaries())?;
    for mason in [stone, timber] {
        if !mason.is_empty() {
            let key = dice.seed();
            stage.raise(mason, Pose::new(Vec3::ZERO, Frame::WORLD), key)?;
        }
    }
    Some(())
}

/// How many of a scene's `room` objects its hedges may take, at `share`.
fn budget(room: usize, share: f64) -> usize {
    let room = u32::try_from(room).unwrap_or(u32::MAX);
    usize::try_from(mathf::round_i32((f64::from(room) * share).min(2.0e9))).unwrap_or(0)
}

/// A boundary's line over its land, and where the eye stands.
struct Laid<'a> {
    land: &'a Land,
    line: &'a [Point],
    eye: Point,
}

impl Laid<'_> {
    /// The ground's height at `at` on `stage`'s land.
    fn ground(&self, stage: &Stage, at: Point) -> f64 {
        self.land.grids.height(&stage.fields, at.x, at.y)
    }

    /// Where a thing `height` tall stands rooted at `at`.
    fn rooted(&self, stage: &Stage, at: Point, height: f64) -> f64 {
        rooted(&self.land.grids.lie(&stage.fields, at.x, at.y), height)
    }
}

/// Whether `along` a line lies within `reach` of the middle of any of
/// `gaps`, beyond half its width.
fn in_gap(gaps: &[Gap], along: f64, reach: f64) -> bool {
    gaps.iter().any(|gap| (along - gap.along).abs() < 0.5 * gap.width + reach)
}

/// The stretches of `from`..`to` along a line that `gaps`, in order along
/// it, leave standing, each at least `least` long.
fn stretches(gaps: &[Gap], (from, to): (f64, f64), least: f64) -> Option<Vec<(f64, f64)>> {
    let mut stretches = Vec::new();
    let mut start = from;
    for gap in gaps {
        let (open, close) = (gap.along - 0.5 * gap.width, gap.along + 0.5 * gap.width);
        if close <= start || open >= to {
            continue;
        }
        if open - start >= least {
            stretches.try_reserve(1).ok()?;
            stretches.push((start, open));
        }
        start = close;
    }
    if to - start >= least {
        stretches.try_reserve(1).ok()?;
        stretches.push((start, to));
    }
    Some(stretches)
}

/// `count` places evenly from `from` to `to`, both ends among them, no
/// further apart than `most`.
fn spaced((from, to): (f64, f64), most: f64) -> impl Iterator<Item = f64> {
    let span = to - from;
    let count = mathf::round_i32(mathf::ceil(span / most.max(1e-3)).clamp(1.0, 1.0e6));
    (0..=count).map(move |index| from + span * f64::from(index) / f64::from(count))
}

/// The frame of a thing laid along the unit way `way` of the land's plan:
/// its own `x` along it, `y` up, `z` across it to its right.
fn along(way: Point) -> Frame {
    Frame::turned(mathf::atan2(-way.y, way.x), 0.0)
}

/// `frame` tipped `angle` radians about its own `z`, raising its `x`.
fn tipped(frame: Frame, angle: f64) -> Frame {
    frame.rotated_by(Frame::about(frame.z, angle))
}

/// `frame` leant `angle` radians about its own `x`.
fn leant(frame: Frame, angle: f64) -> Frame {
    frame.rotated_by(Frame::about(frame.x, angle))
}

/// A hedge along `boundary`: shrubs of `shrubs` a stride apart, thorn the
/// most, each its own height about the hedge's and set a little off its
/// line; an oak of `standard` now and then; and, where a stretch of it
/// collapsed, a fence mended across the gap. Out to `reach` from the eye,
/// and while the hedges' `room` lasts.
fn hedge(
    stage: &mut Stage,
    laid: &Laid<'_>,
    (shrubs, standard): (&[Habit; 2], Option<&Habit>),
    (boundary, draws): (&Boundary, &mut Dice),
    (reach, room): (f64, &mut usize),
    timber: &mut Mason,
) -> Option<()> {
    let length = plane::length(laid.line);
    // Kept hedges are cut to a height, the rest left to grow out.
    let height = if draws.chance(0.7) {
        draws.range(1.7, 2.6)
    } else {
        draws.range(2.6, 3.8)
    };
    let breadth = draws.range(1.2, 2.0);
    let spacing = draws.range(40.0, 110.0);
    let mut next_standard = standard.filter(|_| draws.chance(STOOD)).map(|_| draws.range(6.0, spacing));
    let mended = draws.chance(0.15).then(|| {
        let span = draws.range(3.0, 6.5).min(0.4 * length);
        let from = draws.range(0.1, 0.9) * (length - span);
        (from, from + span)
    });
    let seed = draws.seed();
    let mut along = draws.range(0.2, 0.6);
    let mut row = 1.0;
    let mut walk = Walk::new(laid.line);
    while along < length {
        // Two rows set alternately either side of the line, as a hedge is
        // planted: one thicket, its foot never bare.
        let step = draws.range(0.4, 0.62);
        row = -row;
        let here = along;
        along += step;
        if in_gap(&boundary.gaps, here, 0.45) || mended.is_some_and(|(from, to)| (from..to).contains(&here)) {
            continue;
        }
        let Some((at, way)) = walk.at(here) else {
            continue;
        };
        if (at - laid.eye).length() > reach || *room == 0 {
            continue;
        }
        if let (Some(standard), Some(due)) = (standard, next_standard) {
            if here >= due {
                next_standard = Some(due + spacing * draws.range(0.7, 1.3));
                let tall = draws.range(0.65, 1.0) * standard.tallest();
                plant(stage, laid, standard, (at, tall), draws)?;
                *room = room.saturating_sub(1);
                continue;
            }
        }
        let habit = if draws.chance(HAWTHORN) { &shrubs[0] } else { &shrubs[1] };
        // The hedge's height wanders along it, gappy where it thins.
        let swell = 0.78 + 0.4 * noise2(here / 7.0, 0.0, seed);
        let offset = way.left() * (row * breadth * draws.range(0.08, 0.3));
        plant(stage, laid, habit, (at + offset, height * swell), draws)?;
        *room = room.saturating_sub(1);
    }
    if let Some((from, to)) = mended {
        fence(stage, laid, timber, draws, ((from, to), &[], false))?;
    }
    claim(stage, laid.line, 0.5 * breadth)
}

/// Plant one of `habit` about `height` tall at `at`, turned any way.
fn plant(stage: &mut Stage, laid: &Laid<'_>, habit: &Habit, (at, height): (Point, f64), draws: &mut Dice) -> Option<()> {
    let base = laid.rooted(stage, at, height);
    plants::plant(stage, draws, habit, Vec3::new(at.x, base, at.y), height)
}

/// The oaks standing in a hedge out beyond where its shrubs are built,
/// where it is painted on the ground: what still stands up out of it.
fn standards(
    stage: &mut Stage,
    laid: &Laid<'_>,
    standard: &Habit,
    (boundary, draws): (&Boundary, &mut Dice),
    room: &mut usize,
) -> Option<()> {
    let length = plane::length(laid.line);
    if !draws.chance(STOOD) {
        return Some(());
    }
    let spacing = draws.range(40.0, 110.0);
    let mut along = draws.range(6.0, spacing);
    let mut walk = Walk::new(laid.line);
    while along < length && *room > 0 {
        let here = along;
        along += spacing * draws.range(0.7, 1.3);
        if in_gap(&boundary.gaps, here, 2.0) {
            continue;
        }
        let Some((at, _)) = walk.at(here) else {
            continue;
        };
        let tall = draws.range(0.65, 1.0) * standard.tallest();
        plant(stage, laid, standard, (at, tall), draws)?;
        *room = room.saturating_sub(1);
    }
    Some(())
}

/// A land being set out, as its countryside reads it: its ground as built,
/// and the water on it, to ask which field a place lies in.
struct Built<'a> {
    land: &'a Land,
    fields: &'a [Heightfield],
}

impl countryside::Ground for Built<'_> {
    fn height(&self, at: Point) -> f64 {
        self.land.grids.height(self.fields, at.x, at.y)
    }

    fn water(&self, at: Point) -> Option<f64> {
        let (ground, surface) = (self.height(at), self.land.grids.surface(self.fields, at.x, at.y));
        (surface > ground).then_some(surface)
    }

    fn lie(&self, at: Point) -> countryside::Lie {
        countryside::Lie {
            wet: self.land.grids.lie(self.fields, at.x, at.y).wet,
            ..countryside::Lie::default()
        }
    }
}

/// Whether a tree `tall` at `at` would stand so near the eye at `eye`, or so
/// near ahead of it looking along `heading`, that it walls the view off.
fn walls_off(eye: Point, heading: f64, (at, tall): (Point, f64)) -> bool {
    let apart = at - eye;
    let distance = apart.length();
    if distance < 18.0 {
        return true;
    }
    let ahead = Point::new(mathf::sin(heading), mathf::cos(heading));
    let off = mathf::atan2(apart.cross(ahead).abs(), apart.dot(ahead));
    distance < 4.0 * tall && off < 0.6
}

/// Stand every woodlot of `layout` out to `reach` from `eye` with `grove`'s
/// trees, a jittered lattice of them filling its field but for where a tree
/// would wall the view off along `heading`, while `room` lasts.
fn woodlots(
    stage: &mut Stage,
    land: &Land,
    (layout, grove): (&Layout, &Grove),
    ((eye, heading), reach): ((Point, f64), f64),
    room: &mut usize,
) -> Option<()> {
    let habits: Vec<Habit> = grove.kinds().map(|grown| grown.habit).collect();
    if habits.is_empty() {
        return Some(());
    }
    for parcel in layout.parcels() {
        if parcel.usage.used != Use::Woodlot || (parcel.field.middle - eye).length() > reach {
            continue;
        }
        let field = parcel.field.id;
        let key = (u64::from(field.holding.i.unsigned_abs()) << 40)
            ^ (u64::from(field.holding.j.unsigned_abs()) << 20)
            ^ u64::from(field.index);
        let mut draws = Dice::keyed(key, 2);
        let Some(bounds) = parcel.field.cell.bounds() else {
            continue;
        };
        let spacing = draws.range(5.5, 7.5);
        let stature = draws.range(0.6, 0.9);
        let mut z = bounds.low.y + 0.5 * spacing;
        while z < bounds.high.y && *room > 0 {
            let mut x = bounds.low.x + 0.5 * spacing;
            while x < bounds.high.x && *room > 0 {
                let at = Point::new(x + spacing * draws.range(-0.4, 0.4), z + spacing * draws.range(-0.4, 0.4));
                x += spacing;
                let built = Built {
                    land,
                    fields: &stage.fields,
                };
                if layout.parcel_at(at, &built).is_none_or(|here| here.field.id != field) {
                    continue;
                }
                let habit = &habits[usize::try_from(draws.count(0, u32::try_from(habits.len() - 1).ok()?)).ok()?];
                let tall = stature * draws.range(0.8, 1.1) * habit.tallest();
                let trunk = 0.25 + 0.025 * tall;
                if walls_off(eye, heading, (at, tall)) || !stage.clear((at.x, at.y), trunk) {
                    continue;
                }
                stage.claim((at.x, at.y), trunk)?;
                let base = rooted(&land.grids.lie(&stage.fields, at.x, at.y), tall);
                plants::plant(stage, &mut draws, habit, Vec3::new(at.x, base, at.y), tall)?;
                *room = room.saturating_sub(1);
            }
            z += spacing;
        }
    }
    Some(())
}

/// Claim the ground along `line`, `half` either side, so nothing grows in
/// what stands there.
fn claim(stage: &mut Stage, line: &[Point], half: f64) -> Option<()> {
    for pair in line.windows(2) {
        stage.claim_along((pair[0].x, pair[0].y), (pair[1].x, pair[1].y), half.max(0.3))?;
    }
    Some(())
}

/// A post-and-rail fence along the stretch `from`..`to` of the line, but
/// across `gaps`: posts a stride or so apart, sunk into the ground and leant
/// a little; two or three rails between each pair. One `kept` stands true and
/// whole; one not leans, its rails sag, break or are gone.
fn fence(
    stage: &mut Stage,
    laid: &Laid<'_>,
    mason: &mut Mason,
    draws: &mut Dice,
    ((from, to), gaps, kept): ((f64, f64), &[Gap], bool),
) -> Option<()> {
    let rails: &[f64] = if draws.chance(0.55) { &[0.42, 0.78, 1.08] } else { &[0.5, 1.02] };
    let (above, stride) = (draws.range(1.12, 1.32), draws.range(1.8, 2.5));
    let lean = if kept { 0.012 } else { draws.range(0.04, 0.14) };
    let mut walk = Walk::new(laid.line);
    for stretch in stretches(gaps, (from, to), 0.4)? {
        let mut last: Option<(Point, f64)> = None;
        for along in spaced(stretch, stride) {
            let Some((at, _)) = walk.at(along) else {
                continue;
            };
            let here = (at, laid.ground(stage, at));
            post(mason, here, (above, 0.065), (lean, draws))?;
            if let Some(before) = last {
                for &height in rails {
                    if kept || !draws.chance(0.18) {
                        rail(mason, (before, here), height, (kept, draws))?;
                    }
                }
            }
            last = Some(here);
        }
    }
    Some(())
}

/// A squared post at `at` on ground `ground` high, `above` of it standing
/// over it and as much again below a third of it sunk, `half` its side
/// either way, leant up to `lean` radians any way.
fn post(mason: &mut Mason, (at, ground): (Point, f64), (above, half): (f64, f64), (lean, draws): (f64, &mut Dice)) -> Option<()> {
    let sunk = 0.45 * above;
    let upright = Frame::turned(draws.range(0.0, core::f64::consts::TAU), 0.0);
    let frame = leant(tipped(upright, lean * draws.range(-1.0, 1.0)), lean * draws.range(-1.0, 1.0));
    let middle = Vec3::new(at.x, ground, at.y) + frame.y * (0.5 * (above - sunk));
    // A timber unit's grain runs along its own `x`: stood on end, up the post.
    mason.unit(
        (Pose::new(middle, tipped(frame, FRAC_PI_2)), Vec3::new(0.5 * (above + sunk), half, half)),
        Form::Block { fan: 0 },
        Dressing::Timber,
    )
}

/// A rail from post `a` to post `b`, each with the ground's height there,
/// `height` above the ground between them: laid along its posts' face, true
/// where the fence is `kept`, sagging or snapped and hanging where not.
fn rail(mason: &mut Mason, (a, b): ((Point, f64), (Point, f64)), height: f64, (kept, draws): (bool, &mut Dice)) -> Option<()> {
    let span = (b.0 - a.0).length();
    if span < 0.3 {
        return Some(());
    }
    let way = (b.0 - a.0) * (1.0 / span);
    let rise = mathf::atan2(b.1 - a.1, span);
    let face = way.left() * 0.09;
    let (snapped, sag) = if kept {
        (false, 0.0)
    } else {
        (draws.chance(0.12), draws.range(-0.06, 0.02))
    };
    let length = if snapped { draws.range(0.45, 0.7) * span } else { span + 0.12 };
    let droop = if snapped { draws.range(0.3, 0.9) } else { sag };
    let start = Vec3::new(a.0.x + face.x, a.1 + height, a.0.y + face.y);
    let frame = tipped(along(way), rise + droop);
    let middle = start + frame.x * (0.5 * length);
    mason.unit(
        (Pose::new(middle, frame), Vec3::new(0.5 * length, 0.045, 0.019)),
        Form::Block { fan: 0 },
        Dressing::Timber,
    )
}

/// The field gate hung in `gap` of `boundary`: a hanging post and a shutting
/// post either side, and between them five bars braced on two stiles, shut
/// across the gap, swung open into the field, or dropped off a hinge.
fn gate(stage: &mut Stage, laid: &Laid<'_>, (boundary, gap): (&Boundary, &Gap), mason: &mut Mason) -> Option<()> {
    let mut draws = Dice::keyed(gap.key, 0);
    let half = 0.5 * gap.width;
    let mut walk = Walk::new(laid.line);
    let (Some((hinge, way)), Some((latch, _))) = (walk.at(gap.along - half), walk.at(gap.along + half)) else {
        return Some(());
    };
    let (hinge_ground, latch_ground) = (laid.ground(stage, hinge), laid.ground(stage, latch));
    post(mason, (hinge, hinge_ground), (1.45, 0.1), (0.01, &mut draws))?;
    post(mason, (latch, latch_ground), (1.3, 0.08), (0.02, &mut draws))?;
    // It swings into whichever side is a field, and is hung clear of the
    // ground at its hinge; a turn about the vertical swings it rightward.
    let into = if matches!(boundary.left, Side::Field(_)) { -1.0 } else { 1.0 };
    let width = (latch - hinge).length() - 0.2;
    let state = draws.unit();
    let mut frame = along(way);
    if state < 0.35 {
        frame = frame.rotated_by(Frame::about(Vec3::UP, into * draws.range(1.2, 1.9)));
    } else if state < 0.5 {
        // Off its top hinge, its far end down on the ground.
        let drop = mathf::atan2(0.85 - 0.3 * draws.unit(), width);
        frame = tipped(frame, -drop);
    }
    let foot = Vec3::new(hinge.x, hinge_ground + 0.1, hinge.y) + frame.x * 0.11;
    let piece = |mason: &mut Mason, (from, to): ((f64, f64), (f64, f64)), (deep, thick): (f64, f64)| {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = mathf::hypot(dx, dy);
        let tilt = mathf::atan2(dy, dx);
        let middle = foot + frame.x * (0.5 * (from.0 + to.0)) + frame.y * (0.5 * (from.1 + to.1));
        mason.unit(
            (Pose::new(middle, tipped(frame, tilt)), Vec3::new(0.5 * length, deep, thick)),
            Form::Block { fan: 0 },
            Dressing::Timber,
        )
    };
    piece(mason, ((0.0, 0.0), (0.0, 1.1)), (0.05, 0.04))?;
    piece(mason, ((width, 0.05), (width, 1.06)), (0.035, 0.035))?;
    for (index, &height) in [0.12, 0.34, 0.56, 0.78, 1.04].iter().enumerate() {
        let deep = if index == 4 { 0.055 } else { 0.035 };
        piece(mason, ((0.0, height), (width, height)), (deep, 0.02))?;
    }
    piece(mason, ((0.08, 0.14), (0.62 * width, 1.0)), (0.03, 0.018))?;
    claim(stage, &[hinge, latch], 0.4)
}

/// The stile a path crosses `boundary` by in `gap`: a step on two short
/// posts beside a hedge or fence, or a squeeze between two upright stones
/// in a wall.
fn stile(stage: &mut Stage, laid: &Laid<'_>, (boundary, gap): (&Boundary, &Gap), (timber, stone): (&mut Mason, &mut Mason)) -> Option<()> {
    let mut draws = Dice::keyed(gap.key, 1);
    let Some((at, way)) = plane::at(laid.line, gap.along) else {
        return Some(());
    };
    let ground = laid.ground(stage, at);
    if boundary.kind == Bound::Wall {
        // Two slabs set on end as they were found, broken angular.
        for side in [-1.0, 1.0] {
            let place = at + way * (side * 0.28);
            let tall = draws.range(1.0, 1.25);
            let frame = leant(along(way), draws.range(-0.05, 0.05));
            let form = Form::Rock {
                round: draws.count(10, 40).try_into().unwrap_or(25),
                facets: draws.count(4, 6).try_into().unwrap_or(5),
            };
            stone.unit(
                (
                    Pose::new(Vec3::new(place.x, ground + 0.5 * tall - 0.2, place.y), frame),
                    Vec3::new(0.11, 0.5 * tall + 0.2, 0.3),
                ),
                form,
                Dressing::Field,
            )?;
        }
        return Some(());
    }
    for side in [-1.0, 1.0] {
        let place = at + way * (side * 0.45);
        post(timber, (place, laid.ground(stage, place)), (0.95, 0.055), (0.03, &mut draws))?;
    }
    let step = Vec3::new(at.x, ground + 0.38, at.y);
    timber.unit(
        (Pose::new(step, along(way)), Vec3::new(0.55, 0.025, 0.12)),
        Form::Block { fan: 0 },
        Dressing::Timber,
    )?;
    claim(stage, &[at - way * 0.6, at + way * 0.6], 0.5)
}

/// Paint every hedge and wall of `boundaries` on `land`'s ground where it
/// runs, but across its gaps: what shows of it where it is not built.
fn paint<'a>(stage: &mut Stage, land: &Land, boundaries: impl IntoIterator<Item = &'a Boundary>) -> Option<()> {
    let (mut hedges, mut walls): (Vec<Vec<Mark>>, Vec<Vec<Mark>>) = (Vec::new(), Vec::new());
    for boundary in boundaries {
        let (into, width) = match boundary.kind {
            Bound::Hedge => (&mut hedges, 2.2),
            Bound::Wall => (&mut walls, 0.75),
            Bound::Fence | Bound::Ditch | Bound::Open => continue,
        };
        let length = plane::length(&boundary.line);
        for standing in stretches(&boundary.gaps, (0.0, length), 0.5)? {
            let course = stretch(&boundary.line, standing, width)?;
            into.try_reserve(1).ok()?;
            into.push(course);
        }
    }
    let ((cx, cz), reach) = (land.grids.centre, land.grids.reach);
    let square = ((cx - reach, cz - reach), 2.0 * reach);
    let marked = Reach {
        per_width: 1.0,
        beyond: 2.0,
    };
    let painted = Painted {
        hedges: Courses::new(&hedges, square, marked)?,
        walls: Courses::new(&walls, square, marked)?,
    };
    if let Some(ground) = ground_of(stage, land) {
        ground.bounds = Some(painted);
    }
    Some(())
}

/// The marks of `line` from `from` to `to` along it, `width` broad.
fn stretch(line: &[Point], (from, to): (f64, f64), width: f64) -> Option<Vec<Mark>> {
    let mark = |at: Point| Mark {
        x: at.x,
        z: at.y,
        width,
        ..Mark::default()
    };
    let mut marks = Vec::new();
    marks.try_reserve(line.len() + 2).ok()?;
    marks.push(mark(plane::at(line, from)?.0));
    let mut walked = 0.0;
    for pair in line.windows(2) {
        walked += (pair[1] - pair[0]).length();
        if walked > from && walked < to {
            marks.push(mark(pair[1]));
        }
    }
    marks.push(mark(plane::at(line, to)?.0));
    Some(marks)
}

#[cfg(test)]
#[path = "fields_tests.rs"]
mod tests;
