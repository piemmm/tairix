//! The colour of sawn timber: posts, rails and boards, each its own shade,
//! its grain running its length, knotted and checked, silvered as the
//! weather takes it, greened at its foot and blotched with lichen, its paint
//! flaking where it was painted.
//!
//! A unit lies with its length along its own `x`, so on its long faces the
//! grain runs with the face's first coordinate and streaks across its
//! second, and its end shows its rings.

use tairix_util::mathf;

use crate::noise::{noise3, smoothstep};
use crate::pigment::Spot;
use crate::sample::{mix32, unit};
use crate::vector::Vec3;

/// Timber of one wood, weathered as long as it has stood.
#[derive(Clone, Debug)]
pub(crate) struct Timber {
    /// The two colours each unit's fresh shade lies between.
    pub(crate) bases: [Vec3; 2],
    /// How weathered it stands, `0.0..=1.0`: fresh-sawn to silver-grey.
    pub(crate) weathering: f64,
    /// How damp the place is, `0.0..=1.0`, and the height in the
    /// structure's frame below which splash keeps it so.
    pub(crate) damp: f64,
    pub(crate) foot: f64,
    /// The paint it was given, if any, and how much of it has flaked away.
    pub(crate) paint: Option<(Vec3, f64)>,
    pub(crate) seed: u32,
}

/// Weathered timber's silver-grey.
const SILVER: Vec3 = Vec3::new(0.34, 0.33, 0.31);

/// Lichen's grey-green crust on old timber.
const LICHEN: Vec3 = Vec3::new(0.42, 0.45, 0.36);

/// Green algae on damp timber.
const ALGAE: Vec3 = Vec3::new(0.07, 0.1, 0.04);

/// What a timber unit's ends are, where its pigment reads its face.
const END: f64 = 0.0;

impl Timber {
    /// The colour at `spot`: the unit its mark names, read along its grain.
    pub(crate) fn colour(&self, spot: &Spot) -> Vec3 {
        let key = mix32(spot.mark ^ self.seed);
        let base = self.bases[0].lerp(self.bases[1], unit(key)) * (0.9 + 0.2 * unit(mix32(key ^ 1)));
        let (along, across) = spot.uv;
        let fine = 1.0 - smoothstep(0.004, 0.03, spot.width);
        let grain = if spot.along == END {
            // The end grain: rings about the heart, off its middle.
            let (dx, dy) = (along - 0.3 * (unit(mix32(key ^ 2)) - 0.5), across - 0.3 * (unit(mix32(key ^ 3)) - 0.5));
            let rings = 0.5 + 0.5 * mathf::sin(28.0 * mathf::hypot(dx, dy) + 6.0 * unit(key));
            0.82 + 0.18 * rings * fine
        } else {
            let streak = noise3(Vec3::new(across * 14.0, along * 0.6, 0.0), key ^ 4);
            let fibre = noise3(Vec3::new(across * 60.0, along * 3.0, 0.0), key ^ 5);
            0.85 + 0.15 * (0.7 * streak + 0.3 * fibre * fine)
        };
        let mut colour = base * grain;
        colour = self.knotted(colour, spot, key);
        colour = self.painted(colour, spot, key);
        self.weathered(colour, spot, key)
    }

    /// `colour` with the dark knots a branch left where the unit's key
    /// places them, and the checks its drying split along its grain.
    fn knotted(&self, colour: Vec3, spot: &Spot, key: u32) -> Vec3 {
        if spot.along == END {
            return colour;
        }
        let (along, across) = spot.uv;
        let mut colour = colour;
        for knot in 0..3 {
            let salt = mix32(key ^ (0x40 + knot));
            if unit(salt) > 0.55 {
                continue;
            }
            let (at, side) = (2.0 * unit(mix32(salt ^ 1)) - 1.0, 1.6 * unit(mix32(salt ^ 2)) - 0.8);
            let apart = mathf::hypot((along - at) * 6.0, (across - side) * 1.2);
            colour = colour.lerp(colour * 0.45, 1.0 - smoothstep(0.08, 0.16, apart));
        }
        // A check opens the more the weather has worked the timber.
        let check = unit(mix32(key ^ 0x50));
        if check < 0.25 + 0.5 * self.weathering {
            let line = 1.6 * unit(mix32(key ^ 0x51)) - 0.8;
            let open = 1.0 - smoothstep(0.004, 0.012, (across - line).abs());
            let reach = 1.0 - smoothstep(0.4, 0.9, (along - (2.0 * unit(mix32(key ^ 0x52)) - 1.0)).abs());
            colour = colour.lerp(colour * 0.25, open * reach * self.weathering.max(0.2));
        }
        colour
    }

    /// `colour` under the unit's paint, where it was painted: flaked away in
    /// patches the more it has weathered, the bare wood showing.
    fn painted(&self, colour: Vec3, spot: &Spot, key: u32) -> Vec3 {
        let Some((paint, flaked)) = self.paint else {
            return colour;
        };
        let patches = noise3(spot.p * 9.0, key ^ 0x60) + 0.35 * noise3(spot.p * 31.0, key ^ 0x61);
        let held = smoothstep(flaked - 0.15, flaked + 0.15, 0.5 + 0.5 * patches);
        let tone = 0.92 + 0.08 * unit(mix32(key ^ 0x62));
        colour.lerp(paint * tone, held)
    }

    /// `colour` weathered where `spot` stands: silvered as sun and rain
    /// bleach it, crusted with lichen on what faces the sky, and greened at
    /// its foot and where it stays damp.
    fn weathered(&self, colour: Vec3, spot: &Spot, key: u32) -> Vec3 {
        let (p, w) = (spot.p, self.weathering);
        let silvered = w * (0.55 + 0.45 * unit(mix32(key ^ 0x70)));
        let sky = smoothstep(0.2, 0.8, spot.normal.y);
        let lichen = w * w * sky * smoothstep(0.25, 0.7, noise3(p * 4.0, self.seed ^ 0x71));
        let algae = self.damp
            * w
            * (1.0 - smoothstep(self.foot, self.foot + 0.35, p.y))
                .max(0.4 * smoothstep(0.1, 0.8, noise3(p * 2.6, self.seed ^ 0x72)));
        colour
            .lerp(SILVER * (0.85 + 0.3 * unit(mix32(key ^ 0x73))), silvered)
            .lerp(LICHEN, 0.6 * lichen)
            .lerp(ALGAE, 0.55 * algae)
    }
}

#[cfg(test)]
#[path = "timber_tests.rs"]
mod tests;
