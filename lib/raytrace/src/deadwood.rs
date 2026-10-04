//! Deadwood: the trees a wood has lost. A fallen trunk, sagging where it
//! lies, the stubs of its limbs snapped short, broken from its crown and,
//! where the wind threw it, carrying the plate of roots it tore from the
//! ground, or else broken at its foot too; and a stump, snapped or sawn, its
//! roots still flaring into the soil. A break is torn, not rounded: a
//! jagged face of the wood within, bristling with fibres, bark hanging from
//! its rim.

use alloc::vec::Vec;
use core::f64::consts::TAU;

use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::mathf;

use crate::prototype::{Building, Facet, Part, Prototype, Tube};
use crate::sample::mix32;
use crate::vector::{singles, Frame, Vec3};

/// The segments a fallen trunk is laid in.
const LOG_SEGMENTS: u32 = 10;

/// How much a fallen trunk narrows toward where its crown was.
pub(crate) const LOG_TAPER: f64 = 0.45;

/// How far a fallen trunk has sunk into the litter it lies on, as a share of
/// its radius.
pub(crate) const SUNK: f64 = 0.18;

/// The sides a broken end's face is torn in, the fibres it bristles with at
/// the least and the most, and the tatters of bark that hang from its rim.
const BREAK_SIDES: u32 = 9;
const FIBRES: (u32, u32) = (10, 16);
const TATTERS: (u32, u32) = (2, 4);

/// A piece of deadwood being made: its parts, the vertices and normals its
/// faces are cut from, its draws, and the key its last part took.
struct Timber {
    parts: Vec<Part>,
    vertices: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    dice: NonCryptoRng,
    key: u32,
}

impl Timber {
    /// A piece grown from `seed`, room made for `parts` parts and
    /// `vertices` vertices; `None` when the heap will not hold them.
    fn new(seed: u64, (parts, vertices): (usize, usize)) -> Option<Self> {
        let mut timber = Self {
            parts: Vec::new(),
            vertices: Vec::new(),
            normals: Vec::new(),
            dice: NonCryptoRng::seed_from_u64(seed),
            key: mix32(u32::try_from(seed & 0xffff_ffff).unwrap_or(0)),
        };
        timber.parts.try_reserve(parts).ok()?;
        timber.vertices.try_reserve(vertices).ok()?;
        timber.normals.try_reserve(vertices).ok()?;
        Some(timber)
    }

    fn unit(&mut self) -> f64 {
        self.dice.next_f64()
    }

    /// A count from `low` to `high`, both taken.
    fn count(&mut self, (low, high): (u32, u32)) -> u32 {
        low + self.dice.next_u32() % (high - low + 1)
    }

    fn push(&mut self, part: Part) -> Option<()> {
        self.parts.try_reserve(1).ok()?;
        self.parts.push(part);
        Some(())
    }

    /// A tube from `a` to `b`, `radii` thick at either end, `stem` along its
    /// stem there, in `material` and keyed afresh, its bark begun round it
    /// from its top as a fallen trunk lies, and its ends `open` where it
    /// broke.
    fn tube(
        &mut self,
        (a, b): (Vec3, Vec3),
        radii: (f64, f64),
        (stem, material): ((f64, f64), u16),
        open: [bool; 2],
    ) -> Option<()> {
        self.key = mix32(self.key ^ 0x9e37_79b9);
        let tube = Tube::new((a, b), (radii, stem), (material, self.key), Vec3::UP);
        self.push(Part::Tube(tube.opened(open)))
    }

    /// A vertex at `at` facing `normal`, and its index.
    fn vertex(&mut self, at: Vec3, normal: Vec3) -> Option<u32> {
        let index = u32::try_from(self.vertices.len()).ok()?;
        self.vertices.try_reserve(1).ok()?;
        self.normals.try_reserve(1).ok()?;
        self.vertices.push(singles(at));
        self.normals.push(singles(normal.normalized()));
        Some(index)
    }

    /// Close the open end of a limb broken off at `centre`, `radius` thick,
    /// its wood running on along the unit `out`: a face of torn `wood`
    /// jagged deeper toward its heart, bristling with the fibres the break
    /// tore out, and tatters of `bark` hanging from its rim.
    fn broken(
        &mut self,
        (centre, out, radius): (Vec3, Vec3, f64),
        (bark, wood): (u16, u16),
    ) -> Option<()> {
        let (u, v) = across(out);
        let radial = |around: f64| u * mathf::cos(around) + v * mathf::sin(around);
        let sides = f64::from(BREAK_SIDES);
        let start = TAU * self.unit();
        // The rim, meeting the tube where it opens; a ring torn out within
        // it; and the heart, torn furthest.
        let rim = self.vertices.len();
        for side in 0..BREAK_SIDES {
            let around = start + TAU * f64::from(side) / sides;
            self.vertex(centre + radial(around) * radius, out + radial(around) * 0.8)?;
        }
        for side in 0..BREAK_SIDES {
            let around = start + TAU * (f64::from(side) + 0.5 + 0.3 * (self.unit() - 0.5)) / sides;
            let torn = radius * (0.05 + 0.35 * self.unit());
            let lean = self.unit() - 0.3;
            self.vertex(
                centre + radial(around) * (0.55 * radius) + out * torn,
                out + radial(around) * (0.5 * lean),
            )?;
        }
        let deepest = radius * (0.1 + 0.4 * self.unit());
        let heart = self.vertex(centre + out * deepest, out)?;
        let index = |offset: usize, side: u32| {
            u32::try_from(offset)
                .ok()
                .map(|first| first + side % BREAK_SIDES)
        };
        let ring = rim + BREAK_SIDES as usize;
        for side in 0..BREAK_SIDES {
            let (here, next) = (index(rim, side)?, index(rim, side + 1)?);
            let (inner, beyond) = (index(ring, side)?, index(ring, side + 1)?);
            for corners in [
                [here, next, inner],
                [inner, next, beyond],
                [inner, beyond, heart],
            ] {
                self.push(Part::Facet(Facet {
                    corners,
                    material: Some(wood),
                }))?;
            }
        }
        // Splinters stand longest about the rim, where the wood tore last.
        for _ in 0..self.count(FIBRES) {
            let around = TAU * self.unit();
            let reach = radius * (0.35 + 0.6 * mathf::sqrt(self.unit()));
            let from = centre + radial(around) * reach + out * (0.05 * radius * self.unit());
            let splay = (self.unit() - 0.5, self.unit() - 0.5);
            let toward =
                (out + radial(around) * 0.08 + (u * splay.0 + v * splay.1) * 0.12).normalized();
            let long = radius * (0.15 + 0.7 * (reach / radius) * self.unit());
            let thick = radius * (0.04 + 0.06 * self.unit());
            self.tube(
                (from, from + toward * long),
                (thick, 0.15 * thick),
                ((0.0, long), wood),
                [false; 2],
            )?;
        }
        for _ in 0..self.count(TATTERS) {
            let around = TAU * self.unit();
            let from = centre + radial(around) * radius - out * (0.3 * radius * self.unit());
            let toward = (out * 0.8 + radial(around) * 0.35).normalized();
            let long = radius * (0.4 + 0.8 * self.unit());
            self.tube(
                (from, from + toward * long),
                (0.12 * radius, 0.03 * radius),
                ((0.0, long), bark),
                [false; 2],
            )?;
        }
        Some(())
    }

    fn finish(self) -> Option<Building> {
        Prototype::building(self.parts, self.vertices, self.normals)
    }
}

/// Two unit directions square to the unit `axis` and to each other.
fn across(axis: Vec3) -> (Vec3, Vec3) {
    let reference = if axis.y.abs() < 0.9 {
        Vec3::UP
    } else {
        Vec3::new(1.0, 0.0, 0.0)
    };
    let u = axis.cross(reference).normalized();
    (u, axis.cross(u).normalized())
}

/// A fallen trunk `length` long and `radius` thick at its foot, lying along
/// its frame's `z` on the ground its `y` stands up from, in bark `bark`:
/// thrown by the wind, its roots torn up with it, if `thrown`, and broken
/// off at its foot otherwise, and its crown broken from it, each break torn
/// in `wood`; grown from `seed`, its hierarchy still to build. `None` when
/// the heap will not hold it.
pub(crate) fn log(
    length: f64,
    radius: f64,
    (bark, wood, thrown): (u16, u16, bool),
    seed: u64,
) -> Option<Building> {
    let faces = 2 * (2 * BREAK_SIDES as usize + 1);
    let mut timber = Timber::new(seed, (128, faces))?;
    let step = length / f64::from(LOG_SEGMENTS);
    let resting = |radius: f64| (1.0 - SUNK) * radius;
    let mut at = Vec3::new(0.0, resting(radius), 0.0);
    let mut heading = 0.0f64;
    // The trunk as it lies: its axis at each segment's end, and its radius there.
    let mut axis = [(at, radius); LOG_SEGMENTS as usize + 1];
    for segment in 0..LOG_SEGMENTS {
        let along = f64::from(segment + 1) / f64::from(LOG_SEGMENTS);
        let thick = radius * (1.0 - LOG_TAPER * along);
        heading += 0.06 * (timber.unit() - 0.5);
        let end = Vec3::new(
            at.x + mathf::sin(heading) * step,
            resting(thick),
            at.z + mathf::cos(heading) * step,
        );
        let (r0, travelled) = (
            axis.get(segment as usize).map_or(radius, |&(_, r)| r),
            f64::from(segment) * step,
        );
        // Open where it broke: at its foot, unless its roots tore up with
        // it, and where its crown broke away.
        let open = [segment == 0 && !thrown, segment + 1 == LOG_SEGMENTS];
        timber.tube(
            (at, end),
            (r0, thick),
            ((travelled, travelled + step), bark),
            open,
        )?;
        if let Some(slot) = axis.get_mut(segment as usize + 1) {
            *slot = (end, thick);
        }
        at = end;
    }
    let stubs = timber.count((4, 10));
    for _ in 0..stubs {
        // Most of its limbs were toward where its crown was.
        let along = 0.25 + 0.75 * mathf::sqrt(timber.unit());
        let index = usize::try_from(mathf::round_i32(mathf::floor(
            along * f64::from(LOG_SEGMENTS),
        )))
        .unwrap_or(0);
        let &(centre, thick) = axis.get(index.min(LOG_SEGMENTS as usize))?;
        let around = TAU * timber.unit();
        let (side, up) = (mathf::cos(around), mathf::sin(around));
        // A limb that pointed down broke off short against the ground.
        let reach = if up < -0.3 { 0.3 } else { 1.0 } * thick * (2.0 + 6.0 * timber.unit());
        let out = Vec3::new(side, up, 0.7).normalized();
        let from = centre + Vec3::new(side, up, 0.0) * (0.7 * thick);
        let stub = thick * (0.22 + 0.2 * timber.unit());
        timber.tube(
            (from, from + out * reach),
            (stub, 0.35 * stub),
            ((0.0, reach), bark),
            [false; 2],
        )?;
    }
    let (foot, second) = (axis.first()?.0, axis.get(1)?.0);
    if thrown {
        // The plate of roots the trunk tore up as it fell, standing on edge.
        let roots = timber.count((10, 16));
        for index in 0..roots {
            let around = TAU * (f64::from(index) + 0.4 * timber.unit()) / f64::from(roots);
            let reach = radius * (2.2 + 2.0 * timber.unit());
            let out = Vec3::new(
                mathf::cos(around),
                mathf::sin(around).max(-0.9 * (1.0 - SUNK)),
                -0.25,
            );
            timber.tube(
                (foot, foot + out * reach),
                (0.45 * radius, 0.06 * radius),
                ((0.0, reach), bark),
                [false; 2],
            )?;
        }
    } else {
        timber.broken((foot, (foot - second).normalized(), radius), (bark, wood))?;
    }
    let last = LOG_SEGMENTS as usize;
    let (&(tip, thick), before) = (axis.get(last)?, axis.get(last - 1)?.0);
    timber.broken((tip, (tip - before).normalized(), thick), (bark, wood))?;
    timber.finish()
}

/// How a stump's top was left: snapped off and torn, or sawn flat.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Top {
    Snapped,
    Sawn,
}

/// The sides a sawn stump's face is cut in.
const FACE_SIDES: u32 = 14;

/// A stump `height` tall and `radius` across, its roots flaring into the
/// ground, its bark `bark` and its top as `top` left it, torn or sawn in
/// `wood`; grown from `seed`, its hierarchy still to build. `None` when the
/// heap will not hold it.
pub(crate) fn stump(
    height: f64,
    radius: f64,
    (top, bark, wood): (Top, u16, u16),
    seed: u64,
) -> Option<Building> {
    let faces = (2 * BREAK_SIDES).max(FACE_SIDES) as usize + 1;
    let mut timber = Timber::new(seed, (48 + FACE_SIDES as usize, faces))?;
    let lean = Frame::turned(TAU * timber.unit(), 0.06 * timber.unit());
    let crown = lean.y * height;
    let top_radius = 0.92 * radius;
    timber.tube(
        (Vec3::new(0.0, -0.2 * radius, 0.0), crown),
        (radius, top_radius),
        ((0.0, height), bark),
        [false, true],
    )?;
    let roots = timber.count((5, 7));
    let turn = TAU * timber.unit();
    for index in 0..roots {
        let around = turn + TAU * f64::from(index) / f64::from(roots) + 0.3 * (timber.unit() - 0.5);
        let out = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around));
        let from = out * (0.4 * radius) + Vec3::UP * (0.6 * radius);
        let to = out * (radius * (2.0 + 1.4 * timber.unit())) - Vec3::UP * (0.5 * radius);
        let reach = (to - from).length();
        timber.tube(
            (from, to),
            (0.55 * radius, 0.12 * radius),
            ((0.0, reach), bark),
            [false; 2],
        )?;
    }
    match top {
        Top::Snapped => timber.broken((crown, lean.y, top_radius), (bark, wood))?,
        Top::Sawn => {
            // The sawn face: a fan about the middle closing its open top.
            let middle = timber.vertex(crown, lean.y)?;
            for side in 0..FACE_SIDES {
                let around = TAU * f64::from(side) / f64::from(FACE_SIDES);
                let rim = crown
                    + (lean.x * mathf::cos(around) + lean.z * mathf::sin(around)) * top_radius;
                timber.vertex(rim, lean.y)?;
            }
            for side in 0..FACE_SIDES {
                let (a, b) = (middle + 1 + side, middle + 1 + (side + 1) % FACE_SIDES);
                timber.push(Part::Facet(Facet {
                    corners: [middle, b, a],
                    material: Some(wood),
                }))?;
            }
        }
    }
    timber.finish()
}

#[cfg(test)]
#[path = "deadwood_tests.rs"]
mod tests;
