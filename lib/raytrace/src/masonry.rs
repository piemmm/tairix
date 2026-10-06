//! The colour of masonry: a structure's stones, its bricks and the mortar
//! they are laid in, each unit its own shade and each weathered where it
//! stands.
//!
//! A unit's key gives it its shade and its hue, so no two stones or bricks
//! of a wall match; a field stone's wander further, and it is blotched, now
//! and then stained rust, as it lay half buried in the land, weathering for
//! a while of its own. Weathering follows where water goes: rain streaks a
//! face below what it runs off, grime and biofilm blacken old stone in
//! patches and along the joints water creeps into, soot and gypsum crust in
//! black where rain never washes, algae greens the foot of a wall and its
//! damp side, and a reclaimed brick keeps the lime mortar of the wall it was
//! taken from.

use crate::noise::{noise3, smoothstep};
use crate::pigment::{speckle, Spot};
use crate::sample::{mix32, unit};
use crate::vector::Vec3;

/// What a unit of masonry is.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Unit {
    /// A dressed or split stone.
    Stone,
    /// A field stone, gathered off the land as it lay: each weathered its own
    /// way, and blotched over its faces by what it lay in.
    Field,
    /// A fired brick: `burnt` of those whose end shows overfired dark,
    /// `reclaimed` of them taken from an older wall.
    Brick { burnt: f64, reclaimed: f64 },
    /// The mortar units are bedded in: lime, and its sand.
    Mortar,
}

/// Masonry of one stone, clay or mortar.
#[derive(Clone, Debug)]
pub(crate) struct Masonry {
    /// The two colours each unit's shade lies between.
    pub(crate) bases: [Vec3; 2],
    /// Its minerals' or its grit's flecks, and how many span a metre.
    pub(crate) flecks: [Vec3; 2],
    pub(crate) grain: f64,
    /// How far each unit's shade wanders from the rest.
    pub(crate) shade: f64,
    /// How weathered it stands, `0.0..=1.0`.
    pub(crate) weathering: f64,
    /// How damp the place is, `0.0..=1.0`, and the height in the
    /// structure's frame below which splash keeps it so.
    pub(crate) damp: f64,
    pub(crate) foot: f64,
    pub(crate) unit: Unit,
    pub(crate) seed: u32,
}

/// The black crust soot and gypsum lay on stone where rain never washes it.
const CRUST: Vec3 = Vec3::new(0.035, 0.032, 0.028);

/// Green algae on damp stone.
const ALGAE: Vec3 = Vec3::new(0.06, 0.09, 0.03);

/// The grime and biofilm that blacken old stone where water lingers.
const GRIME: Vec3 = Vec3::new(0.06, 0.06, 0.05);

/// Old lime mortar left on a reclaimed brick.
const OLD_MORTAR: Vec3 = Vec3::new(0.55, 0.53, 0.48);

/// How much further field stones' shades wander than quarried ones': they
/// came from many beds, not one.
const GATHERED: f64 = 1.8;

/// What iron in the soil a stone lay in stains its colour by.
const RUST: Vec3 = Vec3::new(1.12, 0.88, 0.66);

impl Masonry {
    /// The colour at `spot`: the unit its mark names, at the place on it
    /// its coordinates name.
    pub(crate) fn colour(&self, spot: &Spot) -> Vec3 {
        let key = mix32(spot.mark ^ self.seed);
        let shade = match self.unit {
            Unit::Field => GATHERED * self.shade,
            Unit::Stone | Unit::Brick { .. } | Unit::Mortar => self.shade,
        };
        let tint = 1.0 + shade * (2.0 * unit(mix32(key ^ 1)) - 1.0);
        // Each bed of a quarry has its own hue, some warmer, some greyer.
        let warmth = shade * (2.0 * unit(mix32(key ^ 2)) - 1.0);
        let hue = Vec3::new(1.0 + 0.5 * warmth, 1.0, 1.0 - 0.8 * warmth);
        let base = self.bases[0].lerp(self.bases[1], unit(key)) * hue * tint;
        let mut colour = speckle(
            spot.p * self.grain,
            (base, self.flecks.map(|fleck| fleck * tint)),
            key,
            spot.width * self.grain,
        );
        match self.unit {
            Unit::Brick { burnt, reclaimed } => colour = brick(colour, spot, (burnt, reclaimed), key),
            Unit::Field => colour = blotched(colour, spot, key),
            Unit::Stone | Unit::Mortar => {}
        }
        self.weathered(colour, spot, key)
    }

    /// `colour` weathered where `spot` stands: mottled as its face erodes,
    /// streaked below where rain runs off, grimed in patches and along its
    /// joints, crusted black under what shelters it, and greened at its foot
    /// and where it stays damp.
    fn weathered(&self, colour: Vec3, spot: &Spot, key: u32) -> Vec3 {
        let (p, normal, w) = (spot.p, spot.normal, self.weathering);
        let shows = |size: f64| 1.0 - smoothstep(0.3 * size, 1.5 * size, spot.width);
        let mottle = 1.0 + 0.18 * w * shows(0.05) * noise3(p * 14.0, key ^ 0x5a);
        // Water running down an upright face draws streaks along its fall.
        let upright = 1.0 - smoothstep(0.35, 0.8, normal.y.abs());
        let streaks = noise3(Vec3::new(p.x * 9.0, p.y * 0.6, p.z * 9.0), self.seed ^ 0x5b);
        let streaked = w * upright * smoothstep(0.1, 0.6, streaks);
        // Grime and biofilm gather where water lingers: in patches, along
        // the joints it creeps into, and on some beds more than others.
        let (across, up) = spot.uv;
        let joint = upright * smoothstep(0.72, 1.0, across.abs().max(up.abs()));
        let patchy = smoothstep(
            -0.15,
            0.55,
            0.65 * noise3(p * 0.8, self.seed ^ 0x5d) + 0.35 * noise3(p * 3.4, self.seed ^ 0x5e),
        );
        let dirty = unit(mix32(key ^ 0x5f));
        let grimed = w * (0.5 * patchy + 0.3 * dirty * dirty + 0.35 * joint).min(1.0);
        // What faces down is never washed: soot settles there and crusts.
        let sheltered = w * smoothstep(-0.05, -0.6, normal.y);
        let algae = self.damp
            * w
            * ((1.0 - smoothstep(self.foot, self.foot + 0.6, p.y))
                .max(0.5 * smoothstep(0.0, 0.8, noise3(p * 2.2, self.seed ^ 0x5c))));
        // Age greys and darkens a face under the grime and the microbial
        // film it gathers, most on what faces the sky and holds the rain; a
        // field stone weathered as long again as it lay in the land, each
        // for its own while.
        let film = 1.0 - 0.38 * smoothstep(0.3, 0.9, normal.y);
        let patina = Vec3::splat(colour.luminance()) * Vec3::new(0.8, 0.78, 0.72) * film;
        let aged = match self.unit {
            Unit::Field => 0.4 + 1.2 * unit(mix32(key ^ 0x60)),
            Unit::Stone | Unit::Brick { .. } | Unit::Mortar => 1.0,
        };
        (colour * mottle)
            .lerp(patina, (0.55 * w * aged).min(1.0))
            .lerp(colour * 0.62, 0.55 * streaked)
            .lerp(GRIME, 0.5 * grimed)
            .lerp(CRUST, 0.8 * sheltered)
            .lerp(ALGAE, 0.6 * algae)
    }
}

/// A brick's `colour` as it was fired and laid: its end darker where it
/// faced the fire, a few `burnt` through to the glaze, and a `reclaimed`
/// brick still carrying the old mortar of the wall it came from along the
/// edges of its faces, where it was bedded.
fn brick(colour: Vec3, spot: &Spot, (burnt, reclaimed): (f64, f64), key: u32) -> Vec3 {
    let (across, up) = spot.uv;
    let end = spot.along < 0.5;
    // Its middle was fired cooler than its ends.
    let fired = if end {
        0.86
    } else {
        0.86 + 0.14 * (1.0 - smoothstep(0.55, 1.0, across.abs()))
    };
    let mut colour = colour * fired;
    if end && unit(mix32(key ^ 0xb7)) < burnt {
        colour = colour.lerp(Vec3::new(0.05, 0.035, 0.04), 0.75);
    }
    if unit(mix32(key ^ 0xb8)) < reclaimed {
        let edge = smoothstep(0.72, 0.97, up.abs().max(across.abs()));
        let patchy = smoothstep(-0.1, 0.5, noise3(spot.p * 60.0, key ^ 0xb9));
        colour = colour.lerp(OLD_MORTAR, 0.85 * edge * patchy);
    }
    colour
}

/// A field stone's `colour` as it lay half buried in the land: paler and
/// darker in patches over its faces, a few stones stained rust by the iron in
/// the soil, each blotch settling to its mean where `spot` is too wide to
/// show it.
fn blotched(colour: Vec3, spot: &Spot, key: u32) -> Vec3 {
    let shows = |size: f64| 1.0 - smoothstep(0.3 * size, 1.5 * size, spot.width);
    let p = spot.p;
    let patches = 0.6 * shows(0.11) * noise3(p * 9.0, key ^ 0x61)
        + 0.4 * shows(0.03) * noise3(p * 33.0, key ^ 0x62);
    let rusty = unit(mix32(key ^ 0x63));
    let stained = rusty * rusty * smoothstep(-0.1, 0.5, noise3(p * 4.0, key ^ 0x64));
    (colour * (1.0 + 0.32 * patches)).lerp(colour * RUST, 0.6 * stained)
}

#[cfg(test)]
#[path = "masonry_tests.rs"]
mod tests;
