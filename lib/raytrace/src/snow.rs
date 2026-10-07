//! Snow as the wind leaves it. A fall is moved before it settles: lifted
//! from ground standing exposed to the wind and dropped where the ground
//! upwind shelters it, so it lies deep in the lee of every rise (Winstral,
//! Elder and Davis, "Spatial Snow Modeling of Wind-Redistributed Snow Using
//! Terrain-Based Parameters", 2002) and thin wherever the wind quickens, over
//! crests and up the slopes it climbs (Liston and Elder, "A Meteorological
//! Distribution System for High-Resolution Terrestrial Modeling (`MicroMet`)",
//! 2006). Where the wind keeps working the scoured snow it carves it into
//! dunes and sastrugi, ridges running along it.

use tairix_util::mathf;

use crate::noise::{noise2, resolved, ridged2, smoothstep};
use crate::vector::{byte, real, tanh};

/// The snow on a land, and the wind that laid it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Snowpack {
    /// The compass angle the wind blew toward.
    pub(crate) heading: f64,
    /// How deep the snow fell before the wind moved it, in metres.
    pub(crate) fallen: f64,
    pub(crate) seed: u32,
}

/// How far upwind the ground is read for shelter, out to Winstral et al.'s
/// hundred metres, nearer readings closer together; and the fan of headings
/// about the wind's, fifteen degrees either side, that shelter is the mean of
/// as the wind wanders.
const UPWIND: [f64; 8] = [4.0, 7.0, 11.0, 17.0, 26.0, 40.0, 62.0, 100.0];
const FAN: [f64; 3] = [-0.261_799_387_799_149_4, 0.0, 0.261_799_387_799_149_4];

/// How much the wind quickens over ground curving up `CREST_REACH` about a
/// place, and up a slope it climbs, as `MicroMet` weighs the two, here against
/// the shelter they offset, in radians of upwind slope.
const CREST_REACH: f64 = 50.0;
const CREST: f64 = 2.0;
const CLIMB: f64 = 0.3;

/// The share of the fall the wind moves at most: what a place in the lee of
/// a steep rise gains, and one exposed on a crest loses; the shelter, in
/// radians, over which that comes on; and the crust a scoured place keeps,
/// which the wind cannot lift.
const MOVED: f64 = 1.4;
const SHELTER_SCALE: f64 = 0.2;
const CRUST: f64 = 0.06;

/// How unevenly the snow fell, and how broad its patches are, in metres.
const UNEVEN: f64 = 0.15;
const FALL_PATCH: f64 = 70.0;

/// The slopes, as rise over run, over which snow stops lying, sloughing off
/// past its angle of repose.
const SLOUGHS: (f64, f64) = (0.75, 1.1);

/// The forms the wind carves scoured snow into: snow dunes, and the
/// sastrugi on and between them. Each is how far apart its crests run
/// across the wind, how many times as long as that they run along it, and how
/// deep it is cut where the wind scours hardest, in metres (Filhol and Sturm,
/// "Snow bedforms: A review, new data, and a formation model", 2015).
const DUNES: Form = Form {
    spacing: 9.0,
    elongation: 2.5,
    depth: 0.22,
    seed: 0x5d,
};
const SASTRUGI: Form = Form {
    spacing: 1.6,
    elongation: 4.4,
    depth: 0.16,
    seed: 0x5a,
};

/// How broad the patches of scoured snow the wind carves are, in metres.
const CARVED_PATCH: f64 = 30.0;

/// A form the wind carves.
struct Form {
    spacing: f64,
    elongation: f64,
    depth: f64,
    seed: u32,
}

/// The deepest drift a grid keeps the depth of; one deeper is kept at it.
pub(crate) const DEEPEST: f64 = 6.0;

/// A boundary the wind crosses as it moves the snow: how tall it stands, and
/// how much of the wind passes through it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Barrier {
    pub(crate) height: f64,
    pub(crate) porosity: f64,
}

/// How far down the wind from a barrier, in its heights, the drift it drops
/// runs out: a solid wall's banked against its lee face, a porous hedge's
/// lower and longer, peaking clear of it where its heights put that; and how
/// many times the depth that fell a drift may gather against one (Tabler,
/// "Snow Fence Guide", 1991).
const LEE: (f64, f64) = (9.0, 16.0);
const PORE_PEAK: f64 = 3.5;
const GATHERED: f64 = 5.0;

/// How much of the fall the wind scours from a barrier's windward foot, and
/// how far out from it, in its heights; and the low ridge it drops upwind of
/// that: as a share of the drift, how far out, and how broad.
const SCOUR: (f64, f64) = (0.75, 0.7);
const RIDGE: (f64, f64, f64) = (0.25, 2.2, 0.9);

/// How far from a barrier `height` tall the snow it moves lies.
pub(crate) fn reach(height: f64) -> f64 {
    LEE.1 * height
}

impl Snowpack {
    /// How sheltered `(x, z)` stands from the wind on the ground `ground`
    /// gives, in radians: the steepest slope up to the ground upwind, the mean
    /// of it over the wind's fan, less what the wind quickens by over a crest
    /// and up a slope — positive in the lee of a rise, negative where the wind
    /// scours.
    pub(crate) fn shelter(&self, ground: &dyn Fn(f64, f64) -> f64, (x, z): (f64, f64)) -> f64 {
        let here = ground(x, z);
        let mut upwind = 0.0;
        for turn in FAN {
            let angle = self.heading + turn;
            let (sin, cos) = (mathf::sin(angle), mathf::cos(angle));
            // The arctangent is monotonic, so the steepest rise is found first
            // and turned into an angle once.
            let steepest = UPWIND
                .iter()
                .map(|&distance| (ground(x - sin * distance, z - cos * distance) - here) / distance)
                .fold(f64::NEG_INFINITY, f64::max);
            upwind += mathf::atan(steepest);
        }
        let (sin, cos) = (mathf::sin(self.heading), mathf::cos(self.heading));
        let reach = CREST_REACH;
        let (ahead, behind) = (
            ground(x + sin * reach, z + cos * reach),
            ground(x - sin * reach, z - cos * reach),
        );
        let (left, right) = (
            ground(x - cos * reach, z + sin * reach),
            ground(x + cos * reach, z - sin * reach),
        );
        let crest = (here - 0.25 * (ahead + behind + left + right)) / reach;
        let climb = (ahead - behind) / (2.0 * reach);
        upwind / real(FAN.len()) - CREST * crest - CLIMB * climb
    }

    /// How much deeper than `open`, its depth in the open, snow lies
    /// `downwind` metres down the wind from `barrier` — upwind of it where
    /// negative — the wind crossing it as squarely as `square` has it, one
    /// across it and nought along it: banked in its lee as far as the snow
    /// that fell can fill, and scoured at its windward foot inside a low
    /// ridge. Negative where scoured, never past bare ground.
    pub(crate) fn drifted(
        &self,
        barrier: Barrier,
        (downwind, square): (f64, f64),
        open: f64,
    ) -> f64 {
        let Barrier { height, porosity } = barrier;
        let across = smoothstep(0.15, 0.6, square.abs());
        if across <= 0.0 || height <= 0.0 {
            return 0.0;
        }
        // What blows through a barrier drops the snow it carries further
        // down the wind, and the more of it the less the barrier gathers.
        let solid = (1.0 - porosity) * (1.0 - porosity);
        let gathered = height.min(GATHERED * self.fallen) * (1.0 - porosity * porosity);
        let out = downwind / height;
        let change = if out >= 0.0 {
            let banked = (1.0 - out / LEE.0).max(0.0);
            let peaked = (1.0 - out / LEE.1).max(0.0)
                * (out / PORE_PEAK)
                * mathf::exp(1.0 - out / PORE_PEAK);
            let drift = gathered * (solid * banked * banked + (1.0 - solid) * peaked);
            (drift - open).max(0.0)
        } else {
            let (foot, ridge) = (-out / SCOUR.1, (-out - RIDGE.1) / RIDGE.2);
            let scoured = open * SCOUR.0 * solid * mathf::exp(-foot * foot);
            RIDGE.0 * gathered * mathf::exp(-ridge * ridge) - scoured
        };
        across * change
    }

    /// How deep the snow lies at `(x, z)`, `shelter` sheltered on ground
    /// `slope` steep.
    pub(crate) fn depth(&self, shelter: f64, (x, z): (f64, f64), slope: f64) -> f64 {
        let fell =
            self.fallen * (1.0 + UNEVEN * noise2(x / FALL_PATCH, z / FALL_PATCH, self.seed ^ 0x5f));
        let moved = (1.0 + MOVED * tanh(shelter / SHELTER_SCALE)).max(CRUST);
        let holds = 1.0 - smoothstep(SLOUGHS.0, SLOUGHS.1, slope);
        (fell * moved * holds).clamp(0.0, DEEPEST)
    }

    /// How far the wind's forms cut into snow `depth` deep at `(x, z)`, a
    /// negative height, on a grid `step` apart: in patches where the wind
    /// scoured the snow, running along its heading, each form only as fine as
    /// the grid resolves it, and none in the soft drifts it dropped. They are
    /// cut from the snow, never through it.
    pub(crate) fn carved(&self, (x, z): (f64, f64), depth: f64, step: f64) -> f64 {
        let scoured = smoothstep(1.0, 0.35, depth / self.fallen.max(1e-6));
        if scoured <= 0.0 {
            return 0.0;
        }
        let patch = smoothstep(
            -0.2,
            0.4,
            noise2(x / CARVED_PATCH, z / CARVED_PATCH, self.seed ^ 0x5b),
        );
        let (sin, cos) = (mathf::sin(self.heading), mathf::cos(self.heading));
        let (along, across) = (x * sin + z * cos, x * cos - z * sin);
        let cut: f64 = [DUNES, SASTRUGI]
            .iter()
            .map(|form| {
                let kept = resolved(step, form.spacing);
                if kept <= 0.0 {
                    return 0.0;
                }
                let crests = ridged2(
                    along / (form.spacing * form.elongation),
                    across / form.spacing,
                    self.seed ^ form.seed,
                    2,
                );
                form.depth * kept * (1.0 - crests)
            })
            .sum();
        -(scoured * patch * cut).min(0.8 * depth)
    }
}

/// Snow `depth` metres deep as a land's grid keeps it, finest about nought,
/// where a few centimetres decide what shows through it.
pub(crate) fn kept(depth: f64) -> u8 {
    byte(mathf::sqrt(depth.max(0.0) / DEEPEST))
}

/// The depth a grid's snow channel keeps, the channel read back as a share.
pub(crate) fn depth_of(channel: f64) -> f64 {
    DEEPEST * channel * channel
}

/// How much of what stands `tall` above the ground snow `depth` deep hides:
/// none on bare ground, all once the snow lies over it.
pub(crate) fn buries(depth: f64, tall: f64) -> f64 {
    smoothstep(0.25 * tall, tall, depth)
}

#[cfg(test)]
#[path = "snow_tests.rs"]
mod tests;
