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
//!
//! Masonry too far off to be laid unit by unit is [`Massed`]: its face is
//! cut into cells a unit across, each coloured as a unit, which settle to
//! the mean of them wherever a cell is finer than a pixel.

use tairix_util::mathf;

use crate::heightfield::{Grows, QUANTITIES};
use crate::noise::{cells3, noise3, smoothstep};
use crate::pigment::{lying, speckle, Spot, SNOW};
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
    pub(crate) massed: Option<Massed>,
    /// How much of what faces the sky the snow lying on it covers.
    pub(crate) snow: f64,
}

/// Masonry set too far off to lay unit by unit: cells `size` across stand
/// for its units, and settle to `faces` on an upright face and `tops` on one
/// facing the sky, the mean of them and the voids between them.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Massed {
    size: f64,
    faces: Vec3,
    tops: Vec3,
}

/// How far into a massed cell from its edge the void between it and the
/// next reaches, as a share of a cell, and how dark it is: as deep in shade
/// as a field-stone wall's face is between and under its stones, measured
/// against walls laid stone by stone; and how many places a massed face's
/// mean is read at.
const VOID: (f64, f64) = (0.35, 0.85);
const MEAN_READS: u32 = 1024;

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
    /// its coordinates name; where it is massed, the cell it lies in.
    pub(crate) fn colour(&self, spot: &Spot) -> Vec3 {
        let Some(massed) = &self.massed else {
            return self.unit_colour(spot, mix32(spot.mark ^ self.seed));
        };
        let mean = massed
            .faces
            .lerp(massed.tops, smoothstep(0.35, 0.8, spot.normal.y));
        let resolved = 1.0 - smoothstep(0.3, 1.5, spot.width / massed.size);
        if resolved <= 0.0 {
            return mean;
        }
        mean.lerp(self.cell_colour(spot, massed.size), resolved)
    }

    /// This masonry set too far off to lay unit by unit, its units `size`
    /// across.
    pub(crate) fn massed(&self, size: f64) -> Self {
        let laid = Self {
            massed: None,
            ..self.clone()
        };
        let mean = |normal: Vec3| {
            let mut sum = Vec3::ZERO;
            for read in 0..MEAN_READS {
                let draw =
                    |salt: u32| unit(mix32(read.wrapping_mul(0x9E37_79B9) ^ salt ^ self.seed));
                let p = Vec3::new(draw(1), draw(2), draw(3)) * (64.0 * size);
                let spot = Spot {
                    p,
                    normal,
                    height: p.y,
                    width: 1e-4 * size,
                    mark: 0,
                    along: 0.0,
                    uv: (0.0, 0.0),
                    girth: 0.0,
                    instance: 0,
                    front: true,
                    ground: [0.0; QUANTITIES],
                    grows: Grows::default(),
                    thatch: 0.0,
                    cover: None,
                };
                sum += laid.cell_colour(&spot, size);
            }
            sum * (1.0 / f64::from(MEAN_READS))
        };
        let (faces, tops) = (mean(Vec3::new(1.0, 0.0, 0.0)), mean(Vec3::UP));
        Self {
            massed: Some(Massed { size, faces, tops }),
            ..laid
        }
    }

    /// The colour at `spot` of the cell `size` across it lies in: a unit's,
    /// darkened in the void between it and the next.
    fn cell_colour(&self, spot: &Spot, size: f64) -> Vec3 {
        let found = cells3(spot.p * (1.0 / size), self.seed ^ 0x66, 1.0);
        let edge = mathf::sqrt(found.second) - mathf::sqrt(found.nearest);
        let void = 1.0 - smoothstep(0.0, VOID.0, edge);
        self.unit_colour(spot, mix32(found.id ^ self.seed)) * (1.0 - VOID.1 * void)
    }

    /// The colour at `spot` of the unit keyed `key`.
    fn unit_colour(&self, spot: &Spot, key: u32) -> Vec3 {
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
            Unit::Brick { burnt, reclaimed } => {
                colour = brick(colour, spot, (burnt, reclaimed), key);
            }
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
            .lerp(SNOW, lying(self.snow, normal))
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
