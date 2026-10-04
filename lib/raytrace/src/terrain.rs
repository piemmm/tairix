//! Landforms and seas: the height functions a grid is filled from, and that
//! a scene's pieces are set on before any grid exists.

use core::f64::consts::TAU;

use tairix_util::mathf;

use crate::noise::{fbm2, noise2, ridged2, smoothstep};
use crate::sample::{mix32, unit};

/// A shape of land.
#[derive(Clone, Debug)]
pub(crate) enum Landform {
    /// Rolling hills `height` high, their swells `scale` apart.
    Hills { scale: f64, height: f64, seed: u32 },
    /// Ridged mountains `height` high, `scale` from ridge to ridge, rising
    /// from a valley floor `floor` wide about the origin.
    Mountains {
        scale: f64,
        height: f64,
        floor: f64,
        seed: u32,
    },
    /// Dunes `height` high and `scale` apart, their ridges across the wind
    /// blowing toward `heading`: a long windward slope, a steep slip face.
    Dunes {
        scale: f64,
        height: f64,
        heading: f64,
        seed: u32,
    },
    /// Land rising `height` from the sea within `radius` of `centre`, and
    /// falling away beneath it beyond.
    Island {
        centre: (f64, f64),
        radius: f64,
        height: f64,
        seed: u32,
    },
    /// Flat-topped mesas `height` high, in `steps` terraces, with canyons
    /// cut between them.
    Mesas {
        scale: f64,
        height: f64,
        steps: f64,
        seed: u32,
    },
    /// A river valley running toward `heading`, its floor `floor` wide,
    /// between hills `height` high.
    Valley {
        heading: f64,
        floor: f64,
        height: f64,
        seed: u32,
    },
}

impl Landform {
    /// The seed the landform is drawn under.
    pub(crate) const fn seed(&self) -> u32 {
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
    pub(crate) fn lowest(&self) -> f64 {
        match *self {
            Self::Hills { .. } | Self::Mountains { .. } => 0.0,
            Self::Dunes { height, .. } => -0.1 * height,
            Self::Island { height, .. } => -0.25 * height,
            Self::Mesas { height, .. } => -0.02 * height,
            Self::Valley { height, .. } => -0.01 * height,
        }
    }

    /// The land's height at `(x, z)`.
    pub(crate) fn height(&self, x: f64, z: f64) -> f64 {
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
    // The river's course meanders down the valley.
    let course = across - 0.6 * floor * noise2(along / (4.0 * floor), 0.5, seed);
    let sides = smoothstep(0.5 * floor, 2.5 * floor, course.abs());
    let broad = 3.0 * floor;
    let hills = 0.5 + 0.5 * fbm2(x / broad, z / broad, seed ^ 9, (7, 0.5, 2.0));
    height * (0.03 + sides * (0.35 + 0.65 * hills)) - 0.04 * height * (1.0 - sides)
}

/// Land drawn over a disc: a landform on a regional slope, levelled in a
/// clearing at the disc's middle where a scene's pieces stand, and either
/// running on past the disc or settling toward its edge to a level `rim` —
/// itself on the slope — where whatever lies beyond takes over.
#[derive(Clone, Debug)]
pub(crate) struct Terrain {
    pub(crate) form: Landform,
    /// The landform's height taken as nought: its own height where a scene's
    /// pieces stand, so the land about them lies as it would.
    pub(crate) datum: f64,
    pub(crate) centre: (f64, f64),
    pub(crate) radius: f64,
    pub(crate) rim: Option<f64>,
    /// How the land falls across the disc, in metres per metre along x and
    /// z: the slope its rivers run down to leave it, levelling off beyond.
    pub(crate) tilt: (f64, f64),
    /// The clearing's level and radius, if it has one.
    pub(crate) clearing: Option<(f64, f64)>,
}

impl Terrain {
    /// The land's height at `(x, z)`.
    pub(crate) fn height(&self, x: f64, z: f64) -> f64 {
        let (dx, dz) = (x - self.centre.0, z - self.centre.1);
        let distance = mathf::hypot(dx, dz);
        let fall = self.fall(dx, dz);
        let height = self.level((dx, dz), self.form.height(x, z) - self.datum + fall);
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

    /// How much of the land's own relief shows at `(x, z)`: none within
    /// the clearing, where the land lies level, all of it well beyond.
    pub(crate) fn keep(&self, x: f64, z: f64) -> f64 {
        self.kept((x - self.centre.0, z - self.centre.1))
    }

    /// `height` at `(x, z)`, pinned to the clearing's level within the
    /// clearing, where nothing may stand it off level.
    pub(crate) fn pin(&self, x: f64, z: f64, height: f64) -> f64 {
        match self.clearing {
            Some((level, _)) if self.keep(x, z) <= 0.0 => level,
            _ => height,
        }
    }

    /// How much of the land's own relief shows `(dx, dz)` from the middle.
    /// The clearing's edge wanders in and out round it, as ground levelled
    /// by hand or filled by water does, rather than running a true circle.
    fn kept(&self, (dx, dz): (f64, f64)) -> f64 {
        let Some((_, radius)) = self.clearing else {
            return 1.0;
        };
        let distance = mathf::hypot(dx, dz);
        let (cos, sin) = if distance > 0.0 {
            (dx / distance, dz / distance)
        } else {
            (1.0, 0.0)
        };
        let reach = radius * (1.0 + 0.22 * noise2(2.5 * cos, 2.5 * sin, self.form.seed() ^ 0xc1ea));
        smoothstep(reach, 2.5 * reach, distance)
    }

    fn level(&self, offset: (f64, f64), height: f64) -> f64 {
        match self.clearing {
            Some((level, _)) => level + (height - level) * self.kept(offset),
            None => height,
        }
    }

    /// A height the land never falls below.
    pub(crate) fn lowest(&self) -> f64 {
        let clearing = self.clearing.map_or(f64::INFINITY, |(level, _)| level);
        let fall = mathf::hypot(self.tilt.0, self.tilt.1) * self.radius;
        (self.form.lowest() - self.datum - fall)
            .min(self.rim.map_or(f64::INFINITY, |rim| rim - fall))
            .min(clearing)
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

/// How many swells a sea sums.
const WAVES: usize = 18;

/// One swell of a sea.
#[derive(Copy, Clone, Debug)]
struct Wave {
    kx: f64,
    kz: f64,
    amplitude: f64,
    phase: f64,
    /// How peaked its crests are: `1` a sine, `2` narrower crests over
    /// broader troughs.
    peak: u32,
}

/// A sea: swells running before a wind, repeating every `period` each way
/// so its grid can tile the open water.
#[derive(Clone, Debug)]
pub(crate) struct Sea {
    waves: [Wave; WAVES],
}

impl Sea {
    /// A sea under `seed` whose largest swells are `swell` long and shortest
    /// `shortest`, `height` its significant wave height — four times the
    /// swells' standard deviation, about the height of the highest third
    /// from trough to crest — running toward `heading`, `spread` radians
    /// either side of it, and repeating every `period`.
    pub(crate) fn new(
        period: f64,
        (swell, shortest, height): (f64, f64, f64),
        (heading, spread): (f64, f64),
        seed: u32,
    ) -> Self {
        let mut waves = [Wave {
            kx: 0.0,
            kz: 0.0,
            amplitude: 0.0,
            phase: 0.0,
            peak: 1,
        }; WAVES];
        let mut key = seed;
        let falls = mathf::ln((shortest / swell).clamp(1e-3, 1.0)) / crate::vector::real(WAVES - 1);
        for (index, wave) in waves.iter_mut().enumerate() {
            key = mix32(key ^ 0x9e37_79b9);
            let order = f64::from(u32::try_from(index).unwrap_or(0));
            // Wavelengths fall away geometrically to the shortest, and
            // shorter swells scatter wider about the wind.
            let length = swell * mathf::exp(order * falls);
            let angle = heading + (unit(key) - 0.5) * 2.0 * spread * (0.6 + 0.08 * order);
            // Snapped to whole wavelengths of the period, so the grid tiles.
            let whole = |part: f64| mathf::round(period * part / length);
            let (m, n) = (whole(mathf::cos(angle)), whole(mathf::sin(angle)));
            let (m, n) = if m == 0.0 && n == 0.0 {
                (1.0, 0.0)
            } else {
                (m, n)
            };
            let steep = if index < 3 { 0.11 } else { 0.07 };
            *wave = Wave {
                kx: TAU * m / period,
                kz: TAU * n / period,
                amplitude: (steep * length)
                    * (0.55 + 0.45 * unit(mix32(key)))
                    * mathf::exp(-order * 0.12),
                phase: TAU * unit(mix32(key ^ 1)),
                peak: if index < 6 { 2 } else { 1 },
            };
        }
        // Summed, the swells stand far higher than any one; scaled so their
        // spread is the sea's, each keeps its share of it.
        let variance: f64 = waves
            .iter()
            .map(|wave| 0.5 * wave.amplitude * wave.amplitude)
            .sum();
        let scale = 0.25 * height / mathf::sqrt(variance).max(1e-12);
        for wave in &mut waves {
            wave.amplitude *= scale;
        }
        Self { waves }
    }

    /// The water's height at `(x, z)`.
    pub(crate) fn height(&self, x: f64, z: f64) -> f64 {
        let mut total = 0.0;
        for wave in &self.waves {
            let rise = 0.5 + 0.5 * mathf::sin(wave.kx * x + wave.kz * z + wave.phase);
            let crest = if wave.peak == 1 { rise } else { rise * rise };
            total += wave.amplitude * (2.0 * crest - 1.0);
        }
        total
    }
}

#[cfg(test)]
#[path = "terrain_tests.rs"]
mod tests;
