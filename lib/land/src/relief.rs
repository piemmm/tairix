//! Landforms: the height functions a land is drawn from before any water
//! wears it, and the relief a land lays them out as.

use tairix_noise::{fbm2, noise2, ridged2, smoothstep};
use tairix_util::mathf;

/// A shape of land.
#[derive(Clone, Debug)]
pub enum Landform {
    /// Rolling hills.
    Hills {
        /// How far apart their swells lie.
        scale: f64,
        /// How high they rise.
        height: f64,
        /// The seed they are drawn under.
        seed: u32,
    },
    /// Ridged mountains rising from a valley floor about the origin.
    Mountains {
        /// How far it is from ridge to ridge.
        scale: f64,
        /// How high they rise.
        height: f64,
        /// How broad the floor is.
        floor: f64,
        /// The seed they are drawn under.
        seed: u32,
    },
    /// Dunes, their ridges across the wind: a long windward slope, a steep
    /// slip face.
    Dunes {
        /// How far apart their ridges lie.
        scale: f64,
        /// How high they rise.
        height: f64,
        /// The compass heading the wind blows toward.
        heading: f64,
        /// The seed they are drawn under.
        seed: u32,
    },
    /// Land rising from the sea, falling away beneath it beyond.
    Island {
        /// Its middle.
        centre: (f64, f64),
        /// How far from its middle it rises.
        radius: f64,
        /// How high it rises.
        height: f64,
        /// The seed it is drawn under.
        seed: u32,
    },
    /// Flat-topped mesas in terraces, with canyons cut between them.
    Mesas {
        /// How far apart they stand.
        scale: f64,
        /// How high they rise.
        height: f64,
        /// How many terraces they rise in.
        steps: f64,
        /// The seed they are drawn under.
        seed: u32,
    },
    /// A river valley between hills.
    Valley {
        /// The compass heading it runs toward.
        heading: f64,
        /// How broad its floor is.
        floor: f64,
        /// How high the hills about it rise.
        height: f64,
        /// The seed it is drawn under.
        seed: u32,
    },
}

impl Landform {
    /// Whether it is a landform at all: every length a positive finite
    /// number, every height and heading finite.
    #[must_use]
    pub fn fits(&self) -> bool {
        let length = |value: f64| value.is_finite() && value > 0.0;
        match *self {
            Self::Hills { scale, height, .. } => length(scale) && height.is_finite(),
            Self::Mountains {
                scale,
                height,
                floor,
                ..
            } => length(scale) && height.is_finite() && floor.is_finite() && floor >= 0.0,
            Self::Dunes {
                scale,
                height,
                heading,
                ..
            } => length(scale) && height.is_finite() && heading.is_finite(),
            Self::Island {
                centre,
                radius,
                height,
                ..
            } => {
                centre.0.is_finite() && centre.1.is_finite() && length(radius) && height.is_finite()
            }
            Self::Mesas {
                scale,
                height,
                steps,
                ..
            } => length(scale) && height.is_finite() && length(steps),
            Self::Valley {
                heading,
                floor,
                height,
                ..
            } => heading.is_finite() && length(floor) && height.is_finite(),
        }
    }

    /// The seed the landform is drawn under.
    #[must_use]
    pub const fn seed(&self) -> u32 {
        match *self {
            Self::Hills { seed, .. }
            | Self::Mountains { seed, .. }
            | Self::Dunes { seed, .. }
            | Self::Island { seed, .. }
            | Self::Mesas { seed, .. }
            | Self::Valley { seed, .. } => seed,
        }
    }

    /// A height the land never falls below.
    #[must_use]
    pub fn lowest(&self) -> f64 {
        match *self {
            Self::Hills { .. } | Self::Mountains { .. } => 0.0,
            Self::Dunes { height, .. } => -0.1 * height,
            Self::Island { height, .. } => -0.25 * height,
            Self::Mesas { height, .. } => -0.02 * height,
            Self::Valley { height, .. } => -0.01 * height,
        }
    }

    /// The land's height at `(x, z)`.
    #[must_use]
    pub fn height(&self, x: f64, z: f64) -> f64 {
        match *self {
            Self::Hills {
                scale,
                height,
                seed,
            } => {
                let (u, v) = warp(x / scale, z / scale, seed, 0.35);
                height * (0.5 + 0.5 * fbm2(u, v, seed, (7, 0.5, 2.03)))
            }
            Self::Mountains {
                scale,
                height,
                floor,
                seed,
            } => mountains((x, z), (scale, height, floor), seed),
            Self::Dunes {
                scale,
                height,
                heading,
                seed,
            } => dunes((x, z), (scale, height, heading), seed),
            Self::Island {
                centre,
                radius,
                height,
                seed,
            } => island((x, z), (centre, radius, height), seed),
            Self::Mesas {
                scale,
                height,
                steps,
                seed,
            } => mesas((x, z), (scale, height, steps), seed),
            Self::Valley {
                heading,
                floor,
                height,
                seed,
            } => valley((x, z), (heading, floor, height), seed),
        }
    }
}

fn mountains((x, z): (f64, f64), (scale, height, floor): (f64, f64, f64), seed: u32) -> f64 {
    let (u, v) = warp(x / scale, z / scale, seed, 0.25);
    let ridges = ridged2(u, v, seed, 8);
    let rolling = 0.5 + 0.5 * fbm2(u * 0.5, v * 0.5, seed ^ 0xb, (4, 0.5, 2.0));
    let distance = mathf::sqrt(x * x + z * z);
    let rise = smoothstep(0.3 * floor, 1.4 * floor, distance);
    height * (0.08 * rolling + rise * (0.25 * rolling + 0.75 * ridges * ridges))
}

/// How far `(x, z)` lies along the compass `heading` from the origin, and
/// how far to its right: the axes a landform with a heading is drawn along,
/// the way every scene reads a heading.
fn axes((x, z): (f64, f64), heading: f64) -> (f64, f64) {
    let (sin, cos) = (mathf::sin(heading), mathf::cos(heading));
    (x * sin + z * cos, x * cos - z * sin)
}

fn dunes((x, z): (f64, f64), (scale, height, heading): (f64, f64, f64), seed: u32) -> f64 {
    let (along, across) = axes((x, z), heading);
    let (along, across) = (along / scale, across / scale);
    // The crest line wanders, so the ridges are not ruled lines.
    let bent = along + 0.8 * noise2(across * 0.35, along * 0.1, seed);
    let phase = bent - mathf::floor(bent);
    // Windward over most of the wave, the slip face over the rest.
    let profile = if phase < 0.78 {
        smoothstep(0.0, 1.0, phase / 0.78)
    } else {
        1.0 - smoothstep(0.0, 1.0, (phase - 0.78) / 0.22)
    };
    let (broad, fine) = (4.0 * scale, scale);
    let swell = 0.6 + 0.4 * fbm2(x / broad, z / broad, seed ^ 3, (3, 0.5, 2.0));
    height * profile * swell + 0.1 * height * fbm2(x / fine, z / fine, seed ^ 7, (4, 0.5, 2.0))
}

fn island((x, z): (f64, f64), (centre, radius, height): ((f64, f64), f64, f64), seed: u32) -> f64 {
    let (dx, dz) = (x - centre.0, z - centre.1);
    let (u, v) = warp(x / radius, z / radius, seed, 0.4);
    let shape = 0.5 + 0.5 * fbm2(u * 1.5, v * 1.5, seed, (7, 0.5, 2.0));
    let distance = mathf::sqrt(dx * dx + dz * dz) / radius;
    let falloff = 1.0 - smoothstep(0.35, 1.15, distance + 0.35 * (shape - 0.5));
    height * (falloff * (0.35 + 0.65 * shape)) - 0.25 * height * (1.0 - falloff)
}

fn mesas((x, z): (f64, f64), (scale, height, steps): (f64, f64, f64), seed: u32) -> f64 {
    let (u, v) = warp(x / scale, z / scale, seed, 0.3);
    let raw = 0.5 + 0.5 * fbm2(u, v, seed, (6, 0.5, 2.0));
    // Terraces: each level a flat top, joined to the next by a steep face.
    let level = raw * steps;
    let whole = mathf::floor(level);
    let terraced = (whole + smoothstep(0.82, 1.0, level - whole)) / steps;
    let fine = 0.1 * scale;
    height * terraced + 0.02 * height * fbm2(x / fine, z / fine, seed ^ 5, (3, 0.5, 2.0))
}

fn valley((x, z): (f64, f64), (heading, floor, height): (f64, f64, f64), seed: u32) -> f64 {
    let (along, across) = axes((x, z), heading);
    // The river's course meanders down the valley, and the spurs between the
    // side valleys stand out from its walls by turns.
    let course = across - 0.6 * floor * noise2(along / (4.0 * floor), 0.5, seed);
    let spurs = 0.4 * floor * noise2(along / (1.7 * floor), 3.5, seed ^ 0x51);
    let out = mathf::fmax(course.abs() - spurs, 0.0);
    // Each side climbs from the floor to the upland over the run its height
    // needs at the slope soil keeps to, so even at its steepest it stands.
    let run = mathf::fmax(height / SIDE_SLOPE, 1.5 * floor);
    let sides = smoothstep(0.5 * floor, 0.5 * floor + run, out);
    let broad = mathf::fmax(run, 3.0 * floor);
    let hills = 0.5 + 0.5 * fbm2(x / broad, z / broad, seed ^ 9, (7, 0.5, 2.0));
    height * (0.03 + sides * (0.35 + 0.65 * hills)) - 0.04 * height * (1.0 - sides)
}

/// A valley side's mean slope, as rise over run: steep, yet under what soil
/// stands at even where the side is steepest, half again as steep.
const SIDE_SLOPE: f64 = 0.55;

/// Land drawn over a disc: a landform on a regional slope, levelled in a
/// clearing at the disc's middle where a scene's pieces stand, and either
/// running on past the disc or settling toward its edge to a level `rim` —
/// itself on the slope — where whatever lies beyond takes over.
#[derive(Clone, Debug)]
pub struct Terrain {
    /// The landform drawn.
    pub form: Landform,
    /// The landform's height taken as nought: its own height where a scene's
    /// pieces stand, so the land about them lies as it would.
    pub datum: f64,
    /// The disc's middle.
    pub centre: (f64, f64),
    /// How far the disc reaches from its middle.
    pub radius: f64,
    /// The level the land settles to toward the disc's edge, if it does
    /// rather than running on past it.
    pub rim: Option<f64>,
    /// How the land falls across the disc, in metres per metre along x and
    /// z: the slope its rivers run down to leave it, levelling off beyond.
    pub tilt: (f64, f64),
    /// The clearing's level and radius, if it has one.
    pub clearing: Option<(f64, f64)>,
}

impl Terrain {
    /// Whether it is a relief at all: its landform one, its datum, middle,
    /// rim and tilt finite, its radius a positive finite length, and its
    /// clearing a finite level and radius where it has one.
    #[must_use]
    pub fn fits(&self) -> bool {
        let finite = |values: &[f64]| values.iter().all(|value| value.is_finite());
        self.form.fits()
            && finite(&[
                self.datum,
                self.centre.0,
                self.centre.1,
                self.tilt.0,
                self.tilt.1,
            ])
            && self.radius.is_finite()
            && self.radius > 0.0
            && self.rim.is_none_or(f64::is_finite)
            && self.clearing.is_none_or(|(level, radius)| {
                level.is_finite() && radius.is_finite() && radius >= 0.0
            })
    }

    /// The land's height at `(x, z)`.
    #[must_use]
    pub fn height(&self, x: f64, z: f64) -> f64 {
        let (dx, dz) = (x - self.centre.0, z - self.centre.1);
        let distance = mathf::hypot(dx, dz);
        let fall = self.fall(dx, dz);
        let height = self.level((x, z), self.form.height(x, z) - self.datum + fall);
        match self.rim {
            Some(rim) => {
                height
                    + (rim + fall - height)
                        * smoothstep(0.82 * self.radius, 0.99 * self.radius, distance)
            }
            None => height,
        }
    }

    /// How far the regional slope has fallen `(dx, dz)` from the middle: as
    /// the tilt has it well within the disc, easing to a level by its edge,
    /// so land running on past the disc lies level rather than climbing to
    /// the sky.
    fn fall(&self, dx: f64, dz: f64) -> f64 {
        let slope = mathf::hypot(self.tilt.0, self.tilt.1);
        if slope <= 0.0 {
            return 0.0;
        }
        let along = (self.tilt.0 * dx + self.tilt.1 * dz) / (slope * self.radius);
        let reached = along.abs();
        let eased = if reached <= EASE {
            reached
        } else {
            let past = mathf::exp(-2.0 * (reached - EASE) / (1.0 - EASE));
            EASE + (1.0 - EASE) * (1.0 - past) / (1.0 + past)
        };
        slope * self.radius * eased.copysign(along)
    }

    /// The clearing the land is levelled in, if it has one.
    #[must_use]
    pub fn clearing(&self) -> Option<Clearing> {
        self.clearing.map(|(level, radius)| Clearing {
            centre: self.centre,
            level,
            radius,
            seed: self.form.seed() ^ 0xc1ea,
        })
    }

    fn level(&self, (x, z): (f64, f64), height: f64) -> f64 {
        self.clearing().map_or(height, |clearing| {
            clearing.level + (height - clearing.level) * clearing.keep(x, z)
        })
    }

    /// A height the land never falls below.
    #[must_use]
    pub fn lowest(&self) -> f64 {
        let clearing = self.clearing.map_or(f64::INFINITY, |(level, _)| level);
        let fall = mathf::hypot(self.tilt.0, self.tilt.1) * self.radius;
        mathf::fmin(
            mathf::fmin(
                self.form.lowest() - self.datum - fall,
                self.rim.map_or(f64::INFINITY, |rim| rim - fall),
            ),
            clearing,
        )
    }
}

/// A level clearing in a land, where a scene's pieces stand: its middle, its
/// level and its radius, its edge wandering under its seed.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Clearing {
    /// Its middle.
    pub centre: (f64, f64),
    /// The level it lies at.
    pub level: f64,
    /// How far from its middle it reaches, before its edge wanders.
    pub radius: f64,
    /// The seed its edge wanders under.
    pub seed: u32,
}

impl Clearing {
    /// How much of the land's own relief shows at `(x, z)`: none within the
    /// clearing, where the land lies level, all of it well beyond. Its edge
    /// wanders in and out round it, as ground levelled by hand or filled by
    /// water does, rather than running a true circle.
    #[must_use]
    pub fn keep(&self, x: f64, z: f64) -> f64 {
        let (dx, dz) = (x - self.centre.0, z - self.centre.1);
        let distance = mathf::hypot(dx, dz);
        let (cos, sin) = if distance > 0.0 {
            (dx / distance, dz / distance)
        } else {
            (1.0, 0.0)
        };
        let reach = self.radius * (1.0 + 0.22 * noise2(2.5 * cos, 2.5 * sin, self.seed));
        smoothstep(reach, 2.5 * reach, distance)
    }

    /// `height` at `(x, z)`, pinned to the clearing's level within it, where
    /// nothing may stand it off level.
    #[must_use]
    pub fn pin(&self, x: f64, z: f64, height: f64) -> f64 {
        if self.keep(x, z) <= 0.0 {
            self.level
        } else {
            height
        }
    }
}

/// The share of the disc's radius within which the regional slope falls
/// evenly before it eases to a level.
const EASE: f64 = 0.7;

/// `(u, v)` pushed about by a slow field of noise, `strength` of a unit, so
/// no landform keeps the lattice's grain.
fn warp(u: f64, v: f64, seed: u32, strength: f64) -> (f64, f64) {
    (
        u + strength * noise2(u * 0.5, v * 0.5, seed ^ 0x51),
        v + strength * noise2(u * 0.5 + 5.2, v * 0.5 + 1.3, seed ^ 0x93),
    )
}

#[cfg(test)]
#[path = "relief_tests.rs"]
mod tests;
