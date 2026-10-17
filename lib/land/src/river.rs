//! What a land's rivers make of it: their courses shaped from the drainage
//! that traced them, the beds and banks they cut, and the fans they spread
//! where they run out into standing water.

use alloc::vec::Vec;
use core::f64::consts::TAU;

use tairix_noise::{cell, noise2, smoothstep};
use tairix_terrain::drainage::{downstream, Network};
use tairix_terrain::Grid as Square;
use tairix_util::fallible;
use tairix_util::mathf::{self, power};

use crate::channel::{Banked, BROADEST};
use crate::course::{Mark, Nearest};
use crate::plan::Rivers;

/// Where a river runs out into standing water and fans the silt it carries
/// out before it: its mouth, the way it runs out, how far the fan spreads,
/// how broad the river is there, and the water's level.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Delta {
    pub(crate) apex: (f64, f64),
    pub(crate) toward: (f64, f64),
    pub(crate) length: f64,
    pub(crate) width: f64,
    pub(crate) level: f64,
}

/// How far from its middle a river `width` wide wets a grid `step` apart's
/// vertices: past its banks, as broad as its channel ever stands, by a
/// cell's diagonal, so every cell its course crosses holds water at all four
/// corners, however narrow the river and however it slants across the grid.
/// Water drawn past the banks lies under the ground there, unseen.
pub(crate) fn river_reach(width: f64, step: f64) -> f64 {
    0.5 * BROADEST * width + core::f64::consts::SQRT_2 * step
}

/// How far the water draining each coarse sample of `network`, `step`
/// apart over `square`, has run to reach it: the longest way down the
/// routing from a divide, where nothing drains into a sample, to it.
pub(crate) fn runs(network: &Network, square: Square, step: f64) -> Option<Vec<f64>> {
    let mut run = fallible::filled(network.flow.len(), 0.0)?;
    // Upstream first: every sample before the one it drains to.
    for &raw in network.order.iter().rev() {
        let index = raw as usize;
        let Some(next) = downstream(square, &network.flow, index) else {
            continue;
        };
        let reached = *run.get(index)? + step * network.flow.get(index)?.length();
        let slot = run.get_mut(next)?;
        *slot = mathf::fmax(*slot, reached);
    }
    Some(run)
}

/// How deep a river `width` across runs at bankfull: as hydraulic geometry
/// has a gravel-bed river, its breadth five times its depth for each cube
/// root of a metre of it, so a stream two to nine metres broad is six to ten
/// times as broad as it is deep.
pub(crate) fn bankfull_depth(width: f64) -> f64 {
    0.2 * power(mathf::fmax(width, 0.5), 2.0 / 3.0)
}

/// How readily ground at `height` wears under `strata`, against ordinary
/// ground's one: the top tenth of each bed is its hard cap, which wears its
/// hardness times as slowly.
pub(crate) fn erodibility(strata: Option<(f64, f64)>, height: f64) -> f64 {
    let Some((spacing, hardness)) = strata else {
        return 1.0;
    };
    let (_, within) = cell(height / spacing);
    if within > 0.9 {
        1.0 / hardness
    } else {
        1.0
    }
}

/// A lake stands where the ground lies this far below the surface its basin
/// fills to.
pub(crate) const LAKE_DEPTH: f64 = 0.8;

/// The broadest a river grows.
pub(crate) const MOST_RIVER_WIDTH: f64 = 70.0;

/// What a river's channel makes of the ground about it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Bedding {
    pub(crate) height: f64,
    /// How wet the place is, and how much of it the river's floods scour.
    pub(crate) wet: f64,
    pub(crate) scoured: f64,
    /// What the river laid down or wore away there, and how much of the say
    /// it has over that, the rest the land's own.
    pub(crate) laid: f64,
    pub(crate) say: f64,
}

/// `natural` ground `near` a river whose channel and banks are `banked`
/// there: its bed within its brims, and its banks rising beyond to the land, never lower
/// beside the river than its levees raise them, their faces softened across
/// a grid `step`; how wet the place is, the margin of its bed its low water
/// leaves bare soaked but not under; how much of it its floods scour, all
/// of its bed and a fringe up its banks where plants thin toward it, the
/// fringe's edge wandering by `wander` either way; and what the river laid
/// down or wore away, as far as its banks' faces reach.
pub(crate) fn river_bed(
    banked: &Banked,
    near: &Nearest,
    natural: f64,
    (step, wander): (f64, f64),
) -> Bedding {
    let section = &banked.section;
    let half = section.half;
    let width = 2.0 * half;
    let distance = near.distance;
    let across = near.side * distance;
    let beside = 1.0 - smoothstep(half + width, half + 2.5 * width + 2.0 * step, distance);
    let top = mathf::fmax(
        natural,
        natural + (section.brim + FREEBOARD - natural) * beside,
    );
    let height = banked.ground(across, top, step);
    // Soaked within the brim where the low water bares the bed, damp up the
    // banks above it.
    let margin = 1.0 - (1.0 - SOAKED) * smoothstep(0.0, BARE, height - section.water);
    let bank = DAMP_BANK * (1.0 - smoothstep(half, half + 2.0 * width + 3.0 * step, distance));
    let wet =
        margin + (bank - margin) * smoothstep(half, half + mathf::fmax(2.0 * step, 0.3), distance);
    let fringe = mathf::fmax((FRINGE * width).clamp(0.4, 2.0), step);
    // Pioneer plants take the tops of its bars, high over its low water, in
    // the patches its last flood spared.
    let pioneers =
        PIONEERS * smoothstep(0.12, 0.45, height - section.water) * smoothstep(-0.2, 0.5, wander);
    let scoured =
        (1.0 - smoothstep(
            half - 0.25 * fringe,
            half + fringe,
            distance + 0.6 * fringe * wander,
        )) * (1.0 - pioneers);
    Bedding {
        height,
        wet,
        scoured,
        laid: banked.laid(across),
        say: say(banked, near, step),
    }
}

/// How much of the say a river's channel and banks, `banked`, have over the
/// ground `near` them, on a grid `step` apart: all of it within their faces,
/// none beyond.
pub(crate) fn say(banked: &Banked, near: &Nearest, step: f64) -> f64 {
    let faces = banked.section.half + banked.bank_reach(near.side * near.distance, step);
    1.0 - smoothstep(faces, faces + 2.0 * step, near.distance)
}

/// How high above a river's water its bare bed has drained.
const BARE: f64 = 0.05;
/// How wet a river's bare bed stays: soaked, though no water stands on it.
pub const SOAKED: f64 = 0.78;
/// How damp a river's banks are above its brim, too dry to be its bed.
pub const DAMP_BANK: f64 = 0.55;

/// How much of its bars' tops pioneer plants take, at most.
const PIONEERS: f64 = 0.3;

/// How far up a river's banks its floods thin the plants, as a share of its
/// breadth.
const FRINGE: f64 = 0.15;
/// How far apart the turns of that fringe's edge lie, in metres.
pub(crate) const SCOUR_WANDER: f64 = 2.5;

/// How many coarse samples' ground a stream drains where its channel
/// begins: a few hundred metres of slope, as hillslopes run before their
/// water gathers enough to cut.
pub(crate) const CHANNEL_HEAD: f64 = 12.0;

/// How far beyond a river's banks a road's bridge reaches before it meets
/// the land again.
pub(crate) const BRIDGE_REACH: f64 = 2.5;

/// How far above the water a bridge's deck stands, at the least.
pub(crate) const CLEARANCE: f64 = 2.5;

/// The least a river's banks stand above its water.
const FREEBOARD: f64 = 0.35;

/// `course` set wandering from side to side across its level stretches under
/// `seed`: each point moved across the untouched course, by how far that
/// course has run to it, so no move bends the next; `None` when the heap will
/// not hold it.
pub(crate) fn meandering(course: &[Mark], rivers: Rivers, seed: u32) -> Option<Vec<Mark>> {
    let mut wandering = fallible::collected(course.len(), course.iter().copied())?;
    let mut travelled = 0.0;
    for (moved, run) in wandering.iter_mut().skip(1).zip(course.windows(3)) {
        let [last, here, next] = [run[0], run[1], run[2]];
        travelled += mathf::hypot(here.x - last.x, here.z - last.z);
        let (dx, dz) = (next.x - last.x, next.z - last.z);
        let length = mathf::fmax(mathf::hypot(dx, dz), 1e-9);
        let fall = mathf::fmax(last.level - next.level, 0.0) / length;
        // Wandering most where the valley floor is flattest.
        let level = 1.0 - smoothstep(0.002, 0.03, fall);
        let wavelength = 11.0 * mathf::fmax(here.width, 2.0);
        let sway = rivers.meander
            * here.width
            * level
            * mathf::sin(
                TAU * travelled / wavelength + noise2(travelled / (4.0 * wavelength), 0.3, seed),
            );
        moved.x += -dz / length * sway;
        moved.z += dx / length * sway;
    }
    Some(wandering)
}

/// The fan a river whose course is `course` lays where it runs out into
/// water standing at `level`, if its mouth has a heading to fan along.
pub(crate) fn delta(course: &[Mark], level: f64) -> Option<Delta> {
    let (mouth, before) = (course.last()?, course.get(course.len().checked_sub(4)?)?);
    let (dx, dz) = (mouth.x - before.x, mouth.z - before.z);
    let length = mathf::hypot(dx, dz);
    if length < 1e-6 || !level.is_finite() {
        return None;
    }
    Some(Delta {
        apex: (mouth.x, mouth.z),
        toward: (dx / length, dz / length),
        length: 40.0 + 7.0 * mouth.width,
        width: mouth.width,
        level,
    })
}

/// The ground at `(x, z)` with `delta`'s fan laid over `height`: built up a
/// little above the water near the mouth, shelving under it toward its
/// fringe, and cut by the distributaries it spreads through; and how much
/// of the place is the river's fresh silt.
pub(crate) fn delta_bed(delta: &Delta, (x, z): (f64, f64), height: f64) -> (f64, f64) {
    let (px, pz) = (x - delta.apex.0, z - delta.apex.1);
    let along = px * delta.toward.0 + pz * delta.toward.1;
    let across = px * delta.toward.1 - pz * delta.toward.0;
    // Back up the river a little too, where its mouth widens.
    if along < -delta.width || along > delta.length {
        return (height, 0.0);
    }
    let reach = (along / delta.length).clamp(0.0, 1.0);
    let spread = delta.width + mathf::fmax(along, 0.0) * FAN_SPREAD;
    if across.abs() > spread {
        return (height, 0.0);
    }
    let edge = smoothstep(spread, 0.6 * spread, across.abs())
        * smoothstep(delta.length, 0.8 * delta.length, along);
    let surface = delta.level + 0.45 - 1.4 * reach;
    // The distributaries fan out from the mouth, each a channel a third as
    // broad as the river, angled evenly across the fan: the place's angle
    // off the fan's middle, as a share of the fan, in channels.
    let out = mathf::fmax(along, delta.width);
    let half = mathf::atan(FAN_SPREAD);
    let share = f64::midpoint(mathf::atan2(across, out) / half, 1.0) * DISTRIBUTARIES;
    let (_, within) = cell(share);
    let off = (within - 0.5).abs() * 2.0 * half / DISTRIBUTARIES * out;
    let channel = 1.0 - smoothstep(0.1 * delta.width, 0.2 * delta.width, off);
    let fan = surface - 0.8 * channel;
    (height + (fan - height) * edge, edge * (1.0 - reach))
}

/// How far a delta's fan spreads either way for every unit it runs out.
const FAN_SPREAD: f64 = 0.7;

/// How many channels a delta's river divides into across its fan.
const DISTRIBUTARIES: f64 = 4.0;

#[cfg(test)]
#[path = "river_tests.rs"]
mod tests;
