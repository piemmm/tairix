//! The stars: points of light beyond the air, as many of each brightness as
//! the sky holds and each its own colour, and those too faint to count a
//! glow.
//!
//! How many stars are brighter than each magnitude is Allen's census
//! (Astrophysical Quantities) as the Astronomy Answers compilation tabulates
//! it; their colours are the Tycho-2 catalogue's. The sky is mapped onto a
//! cube and each face cut into cells, finer for each fainter tier of the
//! census: a cell holds as many stars as a Poisson draw from its tier's share
//! of the census over its solid angle gives, each placed and made as bright
//! and as coloured as draws from its own key say, so the sky is the same in
//! every scene and holds nothing in memory.
//!
//! A star is a point. A ray sees it through the footprint it is traced
//! with, a pixel's for one from the eye, as a Gaussian of that width holding
//! the star's light: as sharp as the picture resolves, whatever its size.
//! A footprint far wider than a tier's cells, as a scattered ray's, sees that
//! tier's mean; and a tier whose brightest star could not tell against the
//! light a ray sees it over, as by day, is not looked for.

use core::f64::consts::PI;

use tairix_util::mathf;

use crate::atmosphere::WAVELENGTHS;
use crate::body::{fainter, sunlight, FADING, SUN_MAGNITUDE};
use crate::noise::smoothstep;
use crate::sample::{mix32, unit};
use crate::vector::{cell_of, Vec3};

/// How many stars over the whole sky are brighter than each visual
/// magnitude.
const CENSUS: [(f64, f64); 30] = [
    (-1.46, 1.0),
    (0.0, 4.0),
    (0.5, 10.0),
    (1.0, 15.0),
    (2.0, 48.0),
    (2.59, 100.0),
    (3.0, 171.0),
    (4.0, 513.0),
    (4.6, 1.0e3),
    (5.0, 1602.0),
    (6.0, 4.8e3),
    (6.67, 1.0e4),
    (7.0, 1.4e4),
    (8.0, 4.2e4),
    (8.82, 1.0e5),
    (9.0, 1.21e5),
    (10.0, 3.4e5),
    (11.0, 9.27e5),
    (11.1, 1.0e6),
    (12.0, 2.46e6),
    (13.0, 6.29e6),
    (13.5, 1.0e7),
    (14.0, 1.55e7),
    (15.0, 3.69e7),
    (16.0, 8.37e7),
    (16.2, 1.0e8),
    (17.0, 1.82e8),
    (18.0, 3.74e8),
    (19.0, 7.33e8),
    (19.5, 1.0e9),
];
/// The census rows drawn star by star, to magnitude 12: a star fainter adds
/// to its pixel too little to tell from the moonlit sky at any size the
/// picture is drawn. Those past it, to the census's end, are a glow.
const COUNTED: usize = 20;
/// The census's tiers, each its faintest row and its cells along a face's
/// edge: the few bright stars in broad cells and the many faint in fine
/// ones, so a cell holds a star or two whatever its tier.
const TIERS: [(usize, u32); 3] = [(10, 32), (15, 128), (COUNTED - 1, 512)];

const _: () = assert!(TIERS[0].0 < TIERS[1].0 && TIERS[1].0 < TIERS[2].0);

/// Stars by colour, `B − V`, a tenth of a magnitude apart from −0.3 to 2.4:
/// the Tycho-2 catalogue's 2.4 million to magnitude 15 (Clark, "The Color of
/// Stars"), without those bluer than any star or redder than its reddened
/// giants, the scatter of its faintest photometry.
const COLOURS: [u32; 28] = [
    13_159, 23_521, 45_616, 76_357, 105_827, 134_717, 168_902, 205_831, 217_000, 193_206, 156_765,
    124_677, 108_664, 109_187, 115_567, 111_691, 97_171, 80_262, 65_384, 54_335, 45_887, 38_363,
    31_775, 22_922, 16_157, 11_864, 8_599, 6_488,
];
const BLUEST: f64 = -0.3;
const COLOUR_STEP: f64 = 0.1;
/// The sun's own `B − V`.
const SUN_INDEX: f64 = 0.653;
/// The second radiation constant, `hc/k`, in micrometre kelvins.
const RADIATION: f64 = 14_387.77;

/// The most stars a cell is drawn with, far past what its mean reaches.
const MOST: u32 = 24;
/// How many deviations of its footprint a star's light reaches.
const REACH: f64 = 3.0;
/// The least share of the light a star is seen against it must add to be
/// looked for: about a hundredth of the least step an eight-bit pixel takes.
const VISIBLE: f64 = 1e-4;

/// One star: where it is, and the key its brightness and colour are drawn
/// from.
#[derive(Copy, Clone, Debug)]
struct Star {
    at: Vec3,
    key: u32,
}

/// One tier of the census's stars.
#[derive(Copy, Clone, Debug)]
struct Tier {
    /// Its cells along each edge of a face, and what their keys are salted
    /// with.
    cells: u32,
    salt: u32,
    /// Stars a steradian.
    density: f64,
    /// The census's counts at its brightest and at its faintest, between
    /// which its stars' brightnesses are drawn.
    counts: (f64, f64),
    /// The most light its brightest star sends, a channel at a time.
    brightest: Vec3,
    /// Its stars' radiance spread evenly.
    mean: Vec3,
}

/// The stars cell `(column, row)` of `face` holds of `tier`.
fn stars(tier: &Tier, face: u32, (column, row): (u32, u32)) -> impl Iterator<Item = Star> {
    let (x0, x1) = (edge(column, tier.cells), edge(column + 1, tier.cells));
    let (y0, y1) = (edge(row, tier.cells), edge(row + 1, tier.cells));
    let key = mix32(
        face.wrapping_mul(0x9e37_79b9) ^ tier.salt ^ mix32(column ^ mix32(row ^ 0x632b_e5ab)),
    );
    let count = poisson(tier.density * solid((x0, x1), (y0, y1)), unit(key));
    (0..count).map(move |star| {
        let key = mix32(key ^ (star + 1).wrapping_mul(0x85eb_ca6b));
        Star {
            at: point(
                face,
                x0 + (x1 - x0) * unit(mix32(key ^ 1)),
                y0 + (y1 - y0) * unit(mix32(key ^ 2)),
            ),
            key,
        }
    })
}

/// How a ray sees the stars: through a footprint of `deviation`, or as
/// their mean where `None`, a scattered ray's; and which tiers of them could
/// show against the light it sees them over.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Glimpse {
    deviation: Option<f64>,
    shown: [bool; TIERS.len()],
}

/// The stars.
#[derive(Clone, Debug)]
pub(crate) struct Starfield {
    /// For each counted census row, its magnitude and the logarithm of its
    /// count.
    census: [(f64, f64); COUNTED],
    tiers: [Tier; TIERS.len()],
    /// For each colour, the share of the stars that colour or bluer, and the
    /// tint against sunlight it gives.
    colours: [(f64, Vec3); COLOURS.len()],
    /// The radiance of the counted stars spread evenly, and of the glow of
    /// those past counting.
    counted: Vec3,
    glow: Vec3,
    /// A footprint's Gaussian at its reach, which it is lowered by so it
    /// meets nought there; and its integral, so lowered, over a deviation's
    /// square.
    floor: f64,
    volume: f64,
}

impl Starfield {
    /// The sky's stars, as the census counts them and the catalogue colours
    /// them.
    pub(crate) fn new() -> Self {
        let mut census = [(0.0, 0.0); COUNTED];
        for (row, &(magnitude, count)) in census.iter_mut().zip(&CENSUS) {
            *row = (magnitude, mathf::ln(count));
        }
        let all: u32 = COLOURS.iter().sum();
        let reference = temperature(SUN_INDEX);
        let mut colours = [(0.0, Vec3::ZERO); COLOURS.len()];
        let (mut bluer, mut mean, mut tinted, mut index) = (0u32, Vec3::ZERO, Vec3::ZERO, BLUEST);
        for (slot, &count) in colours.iter_mut().zip(&COLOURS) {
            bluer += count;
            let tint = tint(index, reference);
            mean += tint * f64::from(count);
            tinted = tinted.max(tint);
            *slot = (f64::from(bluer) / f64::from(all), tint);
            index += COLOUR_STEP;
        }
        let spread = sunlight() * (mean / f64::from(all)) * (1.0 / (4.0 * PI));
        let mut tiers = [Tier {
            cells: 1,
            salt: 0,
            density: 0.0,
            counts: (0.0, 0.0),
            brightest: Vec3::ZERO,
            mean: Vec3::ZERO,
        }; TIERS.len()];
        let mut bright = 0;
        for ((slot, &(faint, cells)), salt) in tiers.iter_mut().zip(&TIERS).zip(0x2545_f491u32..) {
            let first = bright == 0;
            let from = if first { 0.0 } else { CENSUS[bright].1 };
            let to = CENSUS[faint].1;
            *slot = Tier {
                cells,
                salt: mix32(salt),
                density: (to - from) / (4.0 * PI),
                counts: (from, to),
                brightest: sunlight() * tinted * fainter(CENSUS[bright].0 - SUN_MAGNITUDE),
                mean: spread * light(&CENSUS[bright..=faint], first),
            };
            bright = faint;
        }
        let floor = mathf::exp(-0.5 * REACH * REACH);
        Self {
            census,
            tiers,
            colours,
            counted: tiers.iter().fold(Vec3::ZERO, |sum, tier| sum + tier.mean),
            glow: spread * light(&CENSUS[COUNTED - 1..], false),
            floor,
            volume: PI * (2.0 * (1.0 - floor) - REACH * REACH * floor),
        }
    }

    /// How a ray seeing `spread` radians across, or scattered where `None`,
    /// sees the stars over `against`, the light it sees them against; `None`
    /// where neither any one star nor the stars' mean could add a share of
    /// it worth looking for.
    pub(crate) fn glimpse(&self, spread: Option<f64>, against: Vec3) -> Option<Glimpse> {
        let least = against * VISIBLE;
        let stands_out = |light: Vec3| (light - least).max_element() > 0.0;
        let Some(spread) = spread else {
            return stands_out(self.counted + self.glow).then_some(Glimpse {
                deviation: None,
                shown: [true; TIERS.len()],
            });
        };
        let deviation = 0.5 * spread.max(1e-12);
        let peak = (1.0 - self.floor) / (self.volume * deviation * deviation);
        let shown = self
            .tiers
            .map(|tier| stands_out((tier.brightest * peak).max(tier.mean)));
        (shown.contains(&true) || stands_out(self.glow)).then_some(Glimpse {
            deviation: Some(deviation),
            shown,
        })
    }

    /// The stars' light along the unit `dir`, the true way out to them, as
    /// `glimpse` sees them.
    pub(crate) fn radiance(&self, dir: Vec3, glimpse: Glimpse) -> Vec3 {
        let Some(deviation) = glimpse.deviation else {
            return self.counted + self.glow;
        };
        let mut light = self.glow;
        let mut sharp = [0.0; TIERS.len()];
        for ((tier, shown), sharp) in self.tiers.iter().zip(glimpse.shown).zip(&mut sharp) {
            if shown {
                // A footprint up to a cell wide searches the cells it reaches;
                // a wider one holds so many stars that their mean is what it
                // sees, blended in from half a cell. The cell is as broad as
                // at a face's middle wherever the footprint falls, so a star
                // blurs alike wherever it stands.
                let cell = 2.0 / f64::from(tier.cells);
                let broad = smoothstep(0.5 * cell, cell, deviation);
                light += tier.mean * broad;
                *sharp = 1.0 - broad;
            }
        }
        if sharp.iter().any(|&share| share > 0.0) {
            light += self.points(dir, deviation, sharp);
        }
        light
    }

    /// The light of the stars within reach of `dir` through a footprint of
    /// `deviation`, each tier's in its share `sharp`.
    fn points(&self, dir: Vec3, deviation: f64, sharp: [f64; TIERS.len()]) -> Vec3 {
        let sine = mathf::sin(REACH * deviation);
        let mut light = Vec3::ZERO;
        for face in 0..6 {
            let Some((across, up)) = reached(face, dir, sine) else {
                continue;
            };
            for (tier, &share) in self
                .tiers
                .iter()
                .zip(&sharp)
                .filter(|&(_, &share)| share > 0.0)
            {
                let (columns, rows) = (span(across, tier.cells), span(up, tier.cells));
                let mut held = Vec3::ZERO;
                for column in columns.0..=columns.1 {
                    for row in rows.0..=rows.1 {
                        held += self.cell(tier, face, (column, row), dir, deviation);
                    }
                }
                light += held * share;
            }
        }
        light * (1.0 / (self.volume * deviation * deviation))
    }

    /// The light of `tier`'s stars in cell `(column, row)` of `face` along
    /// `dir`, through a footprint's Gaussian of `deviation` before it is
    /// scaled to hold the light it spreads.
    fn cell(&self, tier: &Tier, face: u32, cell: (u32, u32), dir: Vec3, deviation: f64) -> Vec3 {
        let mut light = Vec3::ZERO;
        for star in stars(tier, face, cell) {
            // The chord's square falls short of the angle's by a twelfth of
            // the angle's own square: parts in ten million across a pixel's
            // footprint, a few in a thousand at the broadest a search runs.
            let gap = dir - star.at;
            let rise = self.rise(gap.dot(gap), deviation);
            if rise > 0.0 {
                light += self.shine(tier, star.key).1 * rise;
            }
        }
        light
    }

    /// The magnitude of `tier`'s star drawn from `key`, and the light it
    /// sends.
    fn shine(&self, tier: &Tier, key: u32) -> (f64, Vec3) {
        let (from, to) = tier.counts;
        let magnitude = self.magnitude(from + (to - from) * unit(mix32(key ^ 3)));
        let light =
            sunlight() * self.tint(unit(mix32(key ^ 4))) * fainter(magnitude - SUN_MAGNITUDE);
        (magnitude, light)
    }

    /// How high a footprint's Gaussian of `deviation` stands `apart`, the
    /// square of the angle, from its middle: lowered so it meets nought at its
    /// reach, before it is scaled to hold the light it spreads.
    fn rise(&self, apart: f64, deviation: f64) -> f64 {
        let width = 2.0 * deviation * deviation;
        if apart >= REACH * REACH * deviation * deviation {
            return 0.0;
        }
        mathf::exp(-apart / width) - self.floor
    }

    /// The magnitude of a star the census counts `count` stars as bright as.
    fn magnitude(&self, count: f64) -> f64 {
        let target = mathf::ln(count.max(1e-300));
        let mut below = self.census[0];
        if target <= below.1 {
            return below.0;
        }
        for &above in &self.census[1..] {
            if target <= above.1 {
                return below.0 + (above.0 - below.0) * (target - below.1) / (above.1 - below.1);
            }
            below = above;
        }
        below.0
    }

    /// The tint of a star drawn at `u` among the colours.
    fn tint(&self, u: f64) -> Vec3 {
        self.colours
            .iter()
            .find(|(bluer, _)| u < *bluer)
            .or(self.colours.last())
            .map_or(Vec3::ONE, |&(_, tint)| tint)
    }
}

/// The light, as a share of the sun's, of the stars the census `rows`
/// count between their first and their last, each row's count growing
/// exponentially in magnitude toward the next; with the first row's own
/// stars where `first`.
fn light(rows: &[(f64, f64)], first: bool) -> f64 {
    let mut total = match rows.first() {
        Some(&(magnitude, count)) if first => count * fainter(magnitude - SUN_MAGNITUDE),
        _ => 0.0,
    };
    for pair in rows.windows(2) {
        let [(from, below), (to, above)] = [pair[0], pair[1]];
        let width = to - from;
        let growth = mathf::ln(above / below) / width;
        // The light of the stars the row adds, integrated exactly over its
        // exponential count.
        let rate = growth - FADING;
        let spread = if rate.abs() < 1e-9 {
            width
        } else {
            (mathf::exp(rate * width) - 1.0) / rate
        };
        total += growth * below * fainter(from - SUN_MAGNITUDE) * spread;
    }
    total
}

/// A star's tint, against sunlight, for colour index `index`, beside the
/// sun's temperature `reference`: blue against green as the index itself
/// says, red against green as a body of its temperature shines.
fn tint(index: f64, reference: f64) -> Vec3 {
    let warmth = |kelvin: f64| planck(WAVELENGTHS[0], kelvin) / planck(WAVELENGTHS[1], kelvin);
    Vec3::new(
        warmth(temperature(index)) / warmth(reference),
        1.0,
        fainter(index - SUN_INDEX),
    )
}

/// The temperature of a blackbody whose `B − V` is `index`, in kelvins
/// (Ballesteros, "New insights into black bodies", 2012).
fn temperature(index: f64) -> f64 {
    4600.0 * (1.0 / (0.92 * index + 1.7) + 1.0 / (0.92 * index + 0.62))
}

/// A blackbody's radiance at `wavelength` micrometres and `kelvin`, to a
/// constant factor.
fn planck(wavelength: f64, kelvin: f64) -> f64 {
    let fifth = wavelength * wavelength * wavelength * wavelength * wavelength;
    1.0 / (fifth * (mathf::exp(RADIATION / (wavelength * kelvin)) - 1.0))
}

/// How many stars a cell whose mean is `mean` holds, drawn at `u`.
fn poisson(mean: f64, u: f64) -> u32 {
    let mut chance = mathf::exp(-mean);
    let mut below = chance;
    let mut count = 0;
    while u >= below && count < MOST {
        count += 1;
        chance *= mean / f64::from(count);
        below += chance;
    }
    count
}

/// The axis `face` looks along and which way, and the two its coordinates
/// run along.
const fn axes(face: u32) -> (usize, f64, usize, usize) {
    let sign = if face.is_multiple_of(2) { 1.0 } else { -1.0 };
    match face / 2 {
        0 => (0, sign, 1, 2),
        1 => (1, sign, 0, 2),
        _ => (2, sign, 0, 1),
    }
}

/// The unit direction through `(x, y)` on `face`.
fn point(face: u32, x: f64, y: f64) -> Vec3 {
    let (axis, sign, u, v) = axes(face);
    let mut parts = [0.0; 3];
    parts[axis] = sign;
    parts[u] = x;
    parts[v] = y;
    Vec3::new(parts[0], parts[1], parts[2]).normalized()
}

/// Where edge `index` of a face cut into `cells` lies, from −1 to 1.
fn edge(index: u32, cells: u32) -> f64 {
    2.0 * f64::from(index) / f64::from(cells) - 1.0
}

/// Where on `face` the cone about `dir` whose half-angle's sine is `sine`, a
/// footprint's, meets it: the least and greatest of each of its coordinates
/// there; `None` where it misses the face.
fn reached(face: u32, dir: Vec3, sine: f64) -> Option<((f64, f64), (f64, f64))> {
    let (axis, sign, u, v) = axes(face);
    let major = sign * dir.along(axis);
    let level = major * major - sine * sine;
    // A coordinate's bounds are where a plane through the eye on which it is
    // constant touches the cone.
    let bounds = |along: f64| {
        let middle = along * major;
        let half = sine * mathf::sqrt(level + along * along);
        let (low, high) = ((middle - half) / level, (middle + half) / level);
        (low <= 1.0 && high >= -1.0).then(|| (low.max(-1.0), high.min(1.0)))
    };
    // A footprint reaching behind the face's plane lies far from the face.
    if major > sine {
        Some((bounds(dir.along(u))?, bounds(dir.along(v))?))
    } else {
        None
    }
}

/// The first and last of a face's `cells` its coordinates from `low` to
/// `high` cross.
fn span((low, high): (f64, f64), cells: u32) -> (u32, u32) {
    let cell = |coordinate: f64| {
        let (index, _) = cell_of(f64::midpoint(coordinate, 1.0) * f64::from(cells));
        u32::try_from(index).unwrap_or(u32::MAX).min(cells - 1)
    };
    (cell(low), cell(high))
}

/// The solid angle of the cell between `x0..x1` and `y0..y1` of a face: its
/// area over the cube of its distance, within a part in a thousand of the
/// exact for the broadest tier's cells and closer for the finer.
fn solid((x0, x1): (f64, f64), (y0, y1): (f64, f64)) -> f64 {
    let (x, y) = (f64::midpoint(x0, x1), f64::midpoint(y0, y1));
    let distance = mathf::sqrt(1.0 + x * x + y * y);
    (x1 - x0) * (y1 - y0) / (distance * distance * distance)
}

#[cfg(test)]
#[path = "stars_tests.rs"]
mod tests;
