//! Deadwood: the trees a wood has lost. A fallen trunk, sagging where it
//! lies, the stubs of its limbs snapped short and, where the wind threw it,
//! the plate of roots it tore from the ground; and a stump, snapped or
//! sawn, its roots still flaring into the soil.

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
const LOG_TAPER: f64 = 0.45;

/// How far a fallen trunk has sunk into the litter it lies on, as a share of
/// its radius.
const SUNK: f64 = 0.18;

/// A tube from `a` to `b`, `radii` thick at either end, `stem` along its
/// stem there, keyed `key`, its bark begun round it from its top, as a
/// fallen trunk lies.
fn tube(
    a: Vec3,
    b: Vec3,
    radii: (f64, f64),
    (stem, material, key): ((f64, f64), u16, u32),
) -> Part {
    Part::Tube(Tube::new((a, b), (radii, stem), (material, key), Vec3::UP))
}

/// A fallen trunk `length` long and `radius` thick at its foot, lying along
/// its frame's `z` on the ground its `y` stands up from, in bark `bark`:
/// thrown by the wind, its roots torn up with it, if `thrown`, and snapped
/// off at its foot otherwise; grown from `seed`, its hierarchy still to
/// build. `None` when the heap will not hold it.
pub(crate) fn log(
    length: f64,
    radius: f64,
    (bark, thrown): (u16, bool),
    seed: u64,
) -> Option<Building> {
    let mut dice = NonCryptoRng::seed_from_u64(seed);
    let mut key = mix32(u32::try_from(seed & 0xffff_ffff).unwrap_or(0));
    let mut next_key = || {
        key = mix32(key ^ 0x9e37_79b9);
        key
    };
    let mut parts: Vec<Part> = Vec::new();
    parts.try_reserve(64).ok()?;
    let step = length / f64::from(LOG_SEGMENTS);
    let resting = |radius: f64| (1.0 - SUNK) * radius;
    let mut at = Vec3::new(0.0, resting(radius), 0.0);
    let mut heading = 0.0f64;
    // The trunk as it lies: its axis at each segment's end, and its radius there.
    let mut axis = [(at, radius); LOG_SEGMENTS as usize + 1];
    for segment in 0..LOG_SEGMENTS {
        let along = f64::from(segment + 1) / f64::from(LOG_SEGMENTS);
        let thick = radius * (1.0 - LOG_TAPER * along);
        heading += 0.06 * (dice.next_f64() - 0.5);
        let end = Vec3::new(
            at.x + mathf::sin(heading) * step,
            resting(thick),
            at.z + mathf::cos(heading) * step,
        );
        let (r0, travelled) = (
            axis.get(segment as usize).map_or(radius, |&(_, r)| r),
            f64::from(segment) * step,
        );
        parts.push(tube(
            at,
            end,
            (r0, thick),
            ((travelled, travelled + step), bark, next_key()),
        ));
        if let Some(slot) = axis.get_mut(segment as usize + 1) {
            *slot = (end, thick);
        }
        at = end;
    }
    let stubs = 4 + dice.next_u32() % 7;
    parts.try_reserve(stubs as usize + 24).ok()?;
    for _ in 0..stubs {
        // Most of its limbs were toward where its crown was.
        let along = 0.25 + 0.75 * mathf::sqrt(dice.next_f64());
        let index = usize::try_from(mathf::round_i32(mathf::floor(
            along * f64::from(LOG_SEGMENTS),
        )))
        .unwrap_or(0);
        let &(centre, thick) = axis.get(index.min(LOG_SEGMENTS as usize))?;
        let around = TAU * dice.next_f64();
        let (side, up) = (mathf::cos(around), mathf::sin(around));
        // A limb that pointed down broke off short against the ground.
        let reach = if up < -0.3 { 0.3 } else { 1.0 } * thick * (2.0 + 6.0 * dice.next_f64());
        let out = Vec3::new(side, up, 0.7).normalized();
        let from = centre + Vec3::new(side, up, 0.0) * (0.7 * thick);
        let stub = thick * (0.22 + 0.2 * dice.next_f64());
        parts.push(tube(
            from,
            from + out * reach,
            (stub, 0.35 * stub),
            ((0.0, reach), bark, next_key()),
        ));
    }
    let foot = axis.first()?.0;
    if thrown {
        // The plate of roots the trunk tore up as it fell, standing on edge.
        let roots = 10 + dice.next_u32() % 7;
        for index in 0..roots {
            let around = TAU * (f64::from(index) + 0.4 * dice.next_f64()) / f64::from(roots);
            let reach = radius * (2.2 + 2.0 * dice.next_f64());
            let out = Vec3::new(
                mathf::cos(around),
                mathf::sin(around).max(-0.9 * (1.0 - SUNK)),
                -0.25,
            );
            let end = foot + out * reach;
            parts.push(tube(
                foot,
                end,
                (0.45 * radius, 0.06 * radius),
                ((0.0, reach), bark, next_key()),
            ));
        }
    } else {
        // The splintered end where it snapped.
        let splinters = 4 + dice.next_u32() % 4;
        for _ in 0..splinters {
            let around = TAU * dice.next_f64();
            let out =
                Vec3::new(0.35 * mathf::cos(around), 0.35 * mathf::sin(around), -1.0).normalized();
            let from =
                foot + Vec3::new(mathf::cos(around), mathf::sin(around), 0.0) * (0.5 * radius);
            let reach = radius * (0.4 + 1.2 * dice.next_f64());
            parts.push(tube(
                from,
                from + out * reach,
                (0.22 * radius, 0.02 * radius),
                ((0.0, reach), bark, next_key()),
            ));
        }
    }
    Prototype::building(parts, Vec::new(), Vec::new())
}

/// How a stump's top was left: snapped off in splinters, or sawn flat.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Top {
    Snapped,
    Sawn,
}

/// The sides a sawn stump's face is cut in.
const FACE_SIDES: u32 = 14;

/// A stump `height` tall and `radius` across, its roots flaring into the
/// ground, its bark `bark` and its top as `top` left it, a sawn face in
/// `wood`; grown from `seed`, its hierarchy still to build. `None` when the
/// heap will not hold it.
pub(crate) fn stump(
    height: f64,
    radius: f64,
    (top, bark, wood): (Top, u16, u16),
    seed: u64,
) -> Option<Building> {
    let mut dice = NonCryptoRng::seed_from_u64(seed);
    let mut key = mix32(u32::try_from(seed >> 32).unwrap_or(0));
    let mut next_key = || {
        key = mix32(key ^ 0x85eb_ca6b);
        key
    };
    let mut parts: Vec<Part> = Vec::new();
    parts.try_reserve(32 + FACE_SIDES as usize).ok()?;
    let lean = Frame::turned(TAU * dice.next_f64(), 0.06 * dice.next_f64());
    let crown = lean.y * height;
    // A tube's end is rounded by its radius, which a sawn face must stand
    // clear of.
    let end = match top {
        Top::Sawn => crown - lean.y * (0.92 * radius),
        Top::Snapped => crown,
    };
    parts.push(tube(
        Vec3::new(0.0, -0.2 * radius, 0.0),
        end,
        (radius, 0.92 * radius),
        ((0.0, height), bark, next_key()),
    ));
    let roots = 5 + dice.next_u32() % 3;
    let turn = TAU * dice.next_f64();
    for index in 0..roots {
        let around =
            turn + TAU * f64::from(index) / f64::from(roots) + 0.3 * (dice.next_f64() - 0.5);
        let out = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around));
        let from = out * (0.4 * radius) + Vec3::UP * (0.6 * radius);
        let to = out * (radius * (2.0 + 1.4 * dice.next_f64())) - Vec3::UP * (0.5 * radius);
        let reach = (to - from).length();
        parts.push(tube(
            from,
            to,
            (0.55 * radius, 0.12 * radius),
            ((0.0, reach), bark, next_key()),
        ));
    }
    let mut vertices = Vec::new();
    let mut normals = Vec::new();
    match top {
        Top::Snapped => {
            let splinters = 5 + dice.next_u32() % 4;
            for _ in 0..splinters {
                let around = TAU * dice.next_f64();
                let rim = crown
                    + (lean.x * mathf::cos(around) + lean.z * mathf::sin(around)) * (0.6 * radius);
                let reach = radius * (0.5 + 1.5 * dice.next_f64());
                let tip = rim + (lean.y + (rim - crown) * (0.6 / radius)).normalized() * reach;
                parts.push(tube(
                    rim,
                    tip,
                    (0.25 * radius, 0.02 * radius),
                    ((height, height + reach), bark, next_key()),
                ));
            }
        }
        Top::Sawn => {
            // The sawn face: a fan about the middle, a little inside the bark.
            vertices.try_reserve(FACE_SIDES as usize + 1).ok()?;
            normals.try_reserve(FACE_SIDES as usize + 1).ok()?;
            let up = singles(lean.y);
            vertices.push(singles(crown + lean.y * 0.002));
            normals.push(up);
            for side in 0..FACE_SIDES {
                let around = TAU * f64::from(side) / f64::from(FACE_SIDES);
                let rim = crown
                    + (lean.x * mathf::cos(around) + lean.z * mathf::sin(around)) * (0.9 * radius);
                vertices.push(singles(rim + lean.y * 0.002));
                normals.push(up);
            }
            for side in 0..FACE_SIDES {
                let (a, b) = (side + 1, (side + 1) % FACE_SIDES + 1);
                parts.push(Part::Facet(Facet {
                    corners: [0, b, a],
                    material: wood,
                }));
            }
        }
    }
    Prototype::building(parts, vertices, normals)
}

#[cfg(test)]
#[path = "deadwood_tests.rs"]
mod tests;
