//! What a surface is made of — its pigment, its finish, and the relief that
//! tilts its normal — and the microfacet, Fresnel and thin-film terms light
//! meets it by.

use alloc::vec::Vec;
use core::f64::consts::{FRAC_PI_2, PI, TAU};

use tairix_util::mathf::{self, Phasor};

use crate::bark::{Bark, OnLimb};
use crate::noise::{fbm2, noise3, smoothstep};
use crate::pigment::Pigment;
use crate::sample::{mix32, unit};
use crate::vector::Vec3;

/// How many waves a wind-ruffled surface sums: enough, spread over their band
/// of lengths and about the wind, that the slope at one place says little of
/// the slope anywhere else — measured, a correlation below a quarter at any
/// lag, where six swells correlate past a half.
const WAVES: usize = 96;

/// One travelling wave of a ruffled surface: its wave vector, where it
/// starts, how high it rises, how long it is, and the slope variance it
/// holds.
#[derive(Copy, Clone, Debug)]
struct Wave {
    kx: f64,
    kz: f64,
    phase: f64,
    height: f64,
    length: f64,
    variance: f64,
}

/// What raises a surface's waves, and how.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Wind {
    /// The variance of the surface's slope, every wave together, where the
    /// wind blows fully: the open sea under a wind of `w` m/s holds about
    /// `0.003 + 0.00512 w` (Cox and Munk, 1954), sheltered water far less.
    pub(crate) slope_variance: f64,
    /// The longest and the shortest waves' lengths, in metres.
    pub(crate) lengths: (f64, f64),
    /// How far either side of the wind the longest waves run, in radians:
    /// the shortest stray twice as far.
    pub(crate) spread: f64,
    /// How much of the waves' height a lull leaves, and how wide the patches
    /// gusts and lulls take, in metres.
    pub(crate) gusts: (f64, f64),
}

/// The waves a wind raises on a level surface, under the patches its gusts
/// draw: a spectrum of travelling waves, their lengths spread over the band
/// a log at a time, no two in the same direction or a whole number of each
/// other's lengths, so their sum never repeats.
#[derive(Clone, Debug)]
pub(crate) struct Waves {
    /// The waves, longest first.
    spectrum: Vec<Wave>,
    /// The slope variance of every wave from each on: what is left to the
    /// roughness when a footprint resolves none of them.
    beyond: Vec<f64>,
    calm: f64,
    patch: f64,
    seed: u32,
}

impl Waves {
    /// The waves `wind` raises under `seed`, the wind's heading drawn from
    /// it; `None` when the heap will not hold them.
    fn new(wind: Wind, seed: u32) -> Option<Self> {
        let (longest, shortest) = (wind.lengths.0.max(1e-6), wind.lengths.1.max(1e-6));
        let falls = mathf::ln((shortest / longest).min(1.0));
        let heading = TAU * unit(mix32(seed));
        let mut waves = Vec::new();
        waves.try_reserve_exact(WAVES).ok()?;
        let mut key = seed;
        for index in 0..WAVES {
            key = mix32(key ^ 0x9e37_79b9);
            // Wave `index` in the `index`th of equal bands of log length,
            // somewhere within it, so the lengths fill the band unevenly.
            let band = (crate::vector::real(index) + unit(key)) / crate::vector::real(WAVES);
            let length = longest * mathf::exp(band * falls);
            let stray = wind.spread * (1.0 + band);
            let angle = heading + (2.0 * unit(mix32(key ^ 1)) - 1.0) * stray;
            let number = TAU / length;
            // Each wave about as steep as the next, as a wind sea's slope
            // spectrum holds each octave alike; scaled below to the variance.
            let steepness = 0.6 + 0.8 * unit(mix32(key ^ 2));
            waves.push(Wave {
                kx: number * mathf::cos(angle),
                kz: number * mathf::sin(angle),
                phase: TAU * unit(mix32(key ^ 3)),
                height: steepness / number,
                length,
                variance: 0.5 * steepness * steepness,
            });
        }
        let total: f64 = waves.iter().map(|wave| wave.variance).sum();
        let scale = wind.slope_variance.max(0.0) / total.max(f64::MIN_POSITIVE);
        for wave in &mut waves {
            wave.height *= mathf::sqrt(scale);
            wave.variance *= scale;
        }
        let mut beyond = Vec::new();
        beyond.try_reserve_exact(WAVES + 1).ok()?;
        beyond.resize(WAVES + 1, 0.0);
        for index in (0..WAVES).rev() {
            beyond[index] = beyond[index + 1] + waves[index].variance;
        }
        Some(Self {
            spectrum: waves,
            beyond,
            calm: wind.gusts.0.clamp(0.0, 1.0),
            patch: wind.gusts.1.max(1e-3),
            seed: mix32(seed ^ 0x5bd1_e995),
        })
    }

    /// How much of the waves' height the gusts raise at `(x, z)`: the calm's
    /// share in a lull, all of it in a gust.
    fn gust(&self, x: f64, z: f64) -> f64 {
        let drift = fbm2(x / self.patch, z / self.patch, self.seed, (2, 0.5, 2.0));
        self.calm + (1.0 - self.calm) * smoothstep(-0.5, 0.6, drift)
    }

    /// `normal` tilted by the waves at `p`, those a stretch `footprint` long
    /// resolves; the rest's slope variance is the roughness they lend.
    pub(crate) fn tilt(&self, normal: Vec3, p: Vec3, footprint: f64) -> Tilt {
        let (mut slope_x, mut slope_z, mut unresolved) = (0.0, 0.0, 0.0);
        for (index, wave) in self.spectrum.iter().enumerate() {
            let kept = kept(wave, footprint);
            if kept <= 0.0 {
                // Every wave from here on is shorter still.
                unresolved += self.beyond.get(index).copied().unwrap_or(0.0);
                break;
            }
            let crest = mathf::cos(wave.kx * p.x + wave.kz * p.z + wave.phase) * wave.height * kept;
            slope_x += crest * wave.kx;
            slope_z += crest * wave.kz;
            unresolved += (1.0 - kept * kept) * wave.variance;
        }
        let gust = self.gust(p.x, p.z);
        Tilt {
            normal: raised(normal, (slope_x, slope_z), gust),
            unresolved: unresolved * gust * gust,
        }
    }

    /// Begin `sweep` at `start`, stepping `step` a point, over the waves a
    /// stretch `footprint` long resolves; both in the texture's frame.
    pub(crate) fn begin(&self, sweep: &mut Sweep, start: Vec3, step: Vec3, footprint: f64) {
        sweep.count = 0;
        for (wave, (crest, number)) in self
            .spectrum
            .iter()
            .zip(sweep.crests.iter_mut().zip(&mut sweep.numbers))
        {
            let kept = kept(wave, footprint);
            if kept <= 0.0 {
                break;
            }
            // Its cosine, as the sine a quarter turn on.
            *crest = Phasor::new(
                wave.height * kept,
                wave.kx * start.x + wave.kz * start.z + wave.phase + FRAC_PI_2,
                wave.kx * step.x + wave.kz * step.z,
            );
            *number = (wave.kx, wave.kz);
            sweep.count += 1;
        }
    }

    /// `normal` tilted at `p` by the slope `sweep` stands at.
    pub(crate) fn tilted(&self, normal: Vec3, p: Vec3, sweep: &Sweep) -> Vec3 {
        raised(normal, sweep.slope(), self.gust(p.x, p.z))
    }

    /// The variance of the surface's slope where a gust raises every wave.
    pub(crate) fn slope_variance(&self) -> f64 {
        self.beyond.first().copied().unwrap_or(0.0)
    }

    /// The standard deviation of the surface's curvature, in reciprocal
    /// metres, of the waves a stretch `footprint` long resolves, where a gust
    /// raises them all: what focuses the light they bend.
    pub(crate) fn curvature(&self, footprint: f64) -> f64 {
        let variance: f64 = self
            .spectrum
            .iter()
            .map(|wave| {
                let kept = kept(wave, footprint);
                let number2 = wave.kx * wave.kx + wave.kz * wave.kz;
                kept * kept * wave.variance * number2
            })
            .sum();
        mathf::sqrt(variance)
    }

    /// The slope variance of the waves a stretch `footprint` long does not
    /// resolve, where a gust raises them all.
    pub(crate) fn unresolved(&self, footprint: f64) -> f64 {
        self.spectrum
            .iter()
            .map(|wave| {
                let kept = kept(wave, footprint);
                (1.0 - kept * kept) * wave.variance
            })
            .sum()
    }
}

/// How much of `wave` a stretch `footprint` long resolves: none of a wave
/// shorter than one and a half of it, all of one longer than three.
fn kept(wave: &Wave, footprint: f64) -> f64 {
    smoothstep(1.5 * footprint, 3.0 * footprint, wave.length)
}

/// `normal` tilted by a slope of the waves, `gust` of their height raised.
fn raised(normal: Vec3, (x, z): (f64, f64), gust: f64) -> Vec3 {
    (normal + Vec3::new(-x, 0.0, -z) * gust).normalized()
}

/// Waves read along a row of evenly spaced points: each wave's crest turned a
/// step at a time, where reading it afresh at every point takes a series.
#[derive(Clone, Debug)]
pub(crate) struct Sweep {
    crests: [Phasor; WAVES],
    numbers: [(f64, f64); WAVES],
    /// How many of the waves the footprint resolves, longest first.
    count: usize,
}

impl Sweep {
    /// A sweep over no waves, to be begun.
    pub(crate) fn new() -> Self {
        Self {
            crests: [Phasor::new(0.0, 0.0, 0.0); WAVES],
            numbers: [(0.0, 0.0); WAVES],
            count: 0,
        }
    }

    /// The waves' slope along x and z where the sweep stands.
    fn slope(&self) -> (f64, f64) {
        self.crests.iter().zip(&self.numbers).take(self.count).fold(
            (0.0, 0.0),
            |(x, z), (crest, &(kx, kz))| {
                let crest = crest.value();
                (x + crest * kx, z + crest * kz)
            },
        )
    }

    /// Step on to the next point.
    pub(crate) fn advance(&mut self) {
        for crest in self.crests.iter_mut().take(self.count) {
            crest.advance();
        }
    }
}

/// A normal tilted by relief, and the slope variance of the relief too fine
/// for the footprint it was read over: the roughness it lends the surface.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Tilt {
    pub(crate) normal: Vec3,
    pub(crate) unresolved: f64,
}

/// The fine shape of a surface, which tilts its normal from point to point.
#[derive(Clone, Debug)]
pub(crate) enum Relief {
    /// Waves a wind raises on a level surface: water, or a dune's ripples.
    Waves(Waves),
    /// An even, fine unevenness in every direction: hammered metal, honed
    /// stone, snow, ground. `depth` is the most it tilts the normal, all its
    /// octaves together, and `scale` how fine the coarsest of them is.
    Grain { depth: f64, scale: f64, seed: u32 },
    /// The ridges and furrows of a bark, `depth` metres deep, over its limb.
    Bark { bark: Bark, depth: f64 },
}

/// Where a relief is read: the point in its object's texture frame; the
/// surface's own coordinates, grain and girth there; the key its instance was
/// placed under; how wide a patch of it one pixel covers, and how long a
/// stretch along the view, which a slanting view draws out.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Bump {
    pub(crate) p: Vec3,
    pub(crate) uv: (f64, f64),
    pub(crate) tangent: Vec3,
    pub(crate) girth: f64,
    pub(crate) instance: u32,
    pub(crate) width: f64,
    pub(crate) stretch: f64,
}

impl Relief {
    /// A grain whose coarsest octave, `scale` waves to the metre, stands
    /// `coarse` steep, its finer octaves adding to it as they each do.
    #[must_use]
    pub(crate) fn grain(coarse: f64, scale: f64, seed: u32) -> Self {
        Self::Grain {
            depth: coarse * GRAIN_SUM,
            scale,
            seed,
        }
    }

    /// The waves `wind` raises under `seed`; `None` when the heap will not
    /// hold them.
    pub(crate) fn waves(wind: Wind, seed: u32) -> Option<Self> {
        Waves::new(wind, seed).map(Self::Waves)
    }

    /// The waves this relief is, if it is waves.
    pub(crate) const fn as_waves(&self) -> Option<&Waves> {
        match self {
            Self::Waves(waves) => Some(waves),
            Self::Grain { .. } | Self::Bark { .. } => None,
        }
    }

    /// `normal` where `bump` says, tilted by this relief, and the slope
    /// variance of what the footprint there cannot resolve.
    pub(crate) fn tilt(&self, normal: Vec3, bump: &Bump) -> Tilt {
        let p = bump.p;
        let tilted = |normal: Vec3| Tilt {
            normal,
            unresolved: 0.0,
        };
        match self {
            Self::Waves(waves) => waves.tilt(normal, p, bump.stretch),
            &Self::Grain { depth, scale, seed } => grained(normal, bump, (depth, scale, seed)),
            Self::Bark { bark, depth } => {
                // The height's slope along the limb and round it is read a
                // millimetre apart.
                const STEP: f64 = 1e-3;
                let along_limb = bump.tangent - normal * bump.tangent.dot(normal);
                if along_limb.length() < 1e-9 {
                    return tilted(normal);
                }
                let along_limb = along_limb.normalized();
                // The way a limb's angle grows, which its bark is laid round.
                let round = along_limb.cross(normal);
                let at = OnLimb::new(
                    bump.uv.0,
                    bump.uv.1,
                    bump.girth,
                    (bump.instance, bump.width),
                );
                let here = bark.height(&at);
                let slope_along = (bark.height(&at.moved(STEP, 0.0)) - here) / STEP;
                let slope_round = (bark.height(&at.moved(0.0, STEP)) - here) / STEP;
                let slope = (along_limb * slope_along + round * slope_round) * *depth;
                let steep = slope.length();
                let slope = if steep > STEEPEST {
                    slope * (STEEPEST / steep)
                } else {
                    slope
                };
                tilted((normal - slope).normalized())
            }
        }
    }
}

/// How many octaves a grain's relief runs to, each this many times finer than
/// the last and this share as steep: the coarsest `scale` waves to the metre,
/// the finest about a millimetre on ground or stone.
const GRAIN_OCTAVES: u32 = 5;
const GRAIN_LACUNARITY: f64 = 2.7;
const GRAIN_GAIN: f64 = 0.62;
/// How steep all a grain's octaves stand together against its first.
const GRAIN_SUM: f64 = {
    let (mut sum, mut steep, mut octave) = (0.0, 1.0, 0);
    while octave < GRAIN_OCTAVES {
        sum += steep;
        steep *= GRAIN_GAIN;
        octave += 1;
    }
    sum
};

/// `normal` tilted by grain at most `depth` steep, from `scale` through its
/// finer octaves, each kept as far as `bump`'s footprint resolves it: what
/// it cannot resolve lends its slope variance to the surface's roughness
/// instead, so the grain settles to its mean in relief as in colour.
fn grained(normal: Vec3, bump: &Bump, (depth, scale, seed): (f64, f64, u32)) -> Tilt {
    let (mut across, mut unresolved) = (Vec3::ZERO, 0.0);
    let (mut frequency, mut steep) = (scale, depth / GRAIN_SUM);
    for octave in 0..GRAIN_OCTAVES {
        // A noise's slope spreads about a third of its steepness squared.
        let variance = steep * steep / 3.0;
        let kept = 1.0 - smoothstep(0.25, 1.0, bump.stretch * frequency);
        if kept > 0.0 {
            let (q, salt) = (bump.p * frequency, seed ^ octave.wrapping_mul(0x9e37_79b9));
            let jolt = Vec3::new(
                noise3(q, salt),
                noise3(q, salt ^ 0x2c1b_3c6d),
                noise3(q, salt ^ 0x297a_2d39),
            );
            across += (jolt - normal * jolt.dot(normal)) * (steep * kept);
        }
        unresolved += variance * (1.0 - kept * kept);
        frequency *= GRAIN_LACUNARITY;
        steep *= GRAIN_GAIN;
    }
    Tilt {
        normal: (normal + across).normalized(),
        unresolved,
    }
}

/// A microfacet roughness widened by the slope variance of relief too fine to
/// tilt the normal itself: GGX's width squared is near enough the slope
/// variance a Beckmann surface of that width holds, so the two add there.
pub(crate) fn widened(roughness: f64, unresolved: f64) -> f64 {
    let width = roughness * roughness;
    mathf::sqrt(mathf::sqrt(width * width + unresolved.max(0.0)))
}

/// The most bark's relief tilts a normal, as the tangent of the angle: a bump
/// any steeper would light the walls of a furrow its own sides hide.
const STEEPEST: f64 = 0.7;

/// Which of a water grid's attributes holds the foam its flow carries,
/// where it carries any: what a stream sheds behind its stones and where its
/// waves break.
pub(crate) const CARRIED_FOAM: usize = 0;

/// Where breaking water turns to foam.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Foam {
    /// The height above which crests begin to whiten.
    pub(crate) crest: f64,
    /// Over how much height the foam thickens from none to full.
    pub(crate) spread: f64,
    pub(crate) seed: u32,
}

/// How a surface meets light.
#[derive(Clone, Debug)]
pub(crate) enum Finish {
    /// The pigment under a clear coat: paint, plastic, stone, lacquered wood.
    /// `roughness` is the coat's, from mirror-smooth `0.0` to matte `1.0`.
    Coated { roughness: f64 },
    /// The pigment alone, scattering light every way and reflecting none:
    /// soil, sand, snow.
    Matte,
    /// A conductor, the pigment its reflectance at normal incidence.
    Metal { roughness: f64 },
    /// A conductor brushed along its texture frame's x axis: smoother along
    /// the brushing than across it, so its highlights draw out into streaks.
    Brushed { along: f64, across: f64 },
    /// Car paint: a clear coat over a metallic base of the pigment's colour,
    /// whose flakes, `flakes` in size, each catch the light their own way.
    Lacquer { roughness: f64, flakes: f64 },
    /// A clear dielectric of refractive index `ior`: glass, crystal, water.
    /// Within it light is absorbed at `absorb` per unit length and, where
    /// `glow` is not black, scattered back toward the eye as deep water is;
    /// `dispersion` spreads its index over the spectrum, as a diamond's does.
    Glass {
        ior: f64,
        absorb: Vec3,
        glow: Vec3,
        roughness: f64,
        dispersion: f64,
        foam: Option<Foam>,
    },
    /// A film between `thickness.0` and `thickness.1` nanometres thick, of
    /// index `index`, coloured by the light its two faces reflect
    /// interfering: a soap bubble when `shell`, else a film laid over the
    /// pigment, as on oil or a beetle's back.
    Film {
        thickness: (f64, f64),
        index: f64,
        shell: bool,
        seed: u32,
    },
    /// A thin surface lit from either side: a leaf, a blade of grass, a
    /// petal. `translucency` of the light reaching its back comes through.
    Leaf { translucency: f64 },
    /// A lamp's own surface, shining `radiance`.
    Glow { radiance: Vec3 },
    /// The land: matte where it is dry, and glossed by the water standing in
    /// it where it is wet, as its pigment's ground says.
    Ground,
}

/// A surface's whole make-up.
#[derive(Clone, Debug)]
pub(crate) struct Material {
    pub(crate) pigment: Pigment,
    pub(crate) finish: Finish,
    pub(crate) relief: Option<Relief>,
}

impl Material {
    pub(crate) const fn new(pigment: Pigment, finish: Finish) -> Self {
        Self {
            pigment,
            finish,
            relief: None,
        }
    }

    pub(crate) fn with_relief(self, relief: Relief) -> Self {
        Self {
            relief: Some(relief),
            ..self
        }
    }

    /// What this material is as water, if it is water: clear, and ruffled by
    /// waves.
    pub(crate) fn water(&self) -> Option<Water<'_>> {
        let Finish::Glass { ior, absorb, .. } = self.finish else {
            return None;
        };
        Some(Water {
            ior,
            absorb,
            waves: self.relief.as_ref()?.as_waves()?,
        })
    }
}

/// Water's refractive index, the share of each primary it absorbs per
/// metre, and the waves on it.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Water<'a> {
    pub(crate) ior: f64,
    pub(crate) absorb: Vec3,
    pub(crate) waves: &'a Waves,
}

/// The reflectance at normal incidence of a coat of index 1.5.
pub(crate) const COAT_F0: f64 = 0.04;

/// Below this roughness a surface is a mirror.
pub(crate) const MIRROR: f64 = 0.02;

/// A GGX (Trowbridge–Reitz) distribution of microfacet normals, `ax` wide
/// along the shading frame's x and `ay` along its y, all its vectors given in
/// that frame, the surface normal its z.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Microfacet {
    pub(crate) ax: f64,
    pub(crate) ay: f64,
}

impl Microfacet {
    /// The same roughness every way.
    pub(crate) fn isotropic(roughness: f64) -> Self {
        Self::anisotropic(roughness, roughness)
    }

    /// Perceptual roughness `along` the frame's x and `across` it, squared
    /// into the distribution's widths.
    pub(crate) fn anisotropic(along: f64, across: f64) -> Self {
        Self {
            ax: (along * along).max(1e-4),
            ay: (across * across).max(1e-4),
        }
    }

    /// Whether every facet faces as the surface does.
    pub(crate) fn is_mirror(&self) -> bool {
        self.ax.max(self.ay) < MIRROR * MIRROR
    }

    /// The density of facet normal `h`.
    pub(crate) fn density(&self, h: Vec3) -> f64 {
        let (x, y) = (h.x / self.ax, h.y / self.ay);
        let d = x * x + y * y + h.z * h.z;
        1.0 / (PI * self.ax * self.ay * d * d)
    }

    /// Smith's masking of direction `w`.
    pub(crate) fn masking(&self, w: Vec3) -> f64 {
        let cos = w.z.max(1e-7);
        let (x, y) = (self.ax * w.x, self.ay * w.y);
        2.0 * cos / (cos + mathf::sqrt(x * x + y * y + cos * cos))
    }

    /// The density per unit solid angle that [`sample`](Self::sample) draws
    /// the reflection `l` of `v` about `h` with.
    pub(crate) fn reflection_density(&self, v: Vec3, h: Vec3) -> f64 {
        self.masking(v) * self.density(h) / (4.0 * v.z.max(1e-7))
    }

    /// A facet normal drawn from those `view` can see (Heitz, "Sampling the
    /// GGX Distribution of Visible Normals", JCGT 2018).
    pub(crate) fn sample(&self, view: Vec3, (u, v): (f64, f64)) -> Vec3 {
        let stretched = Vec3::new(self.ax * view.x, self.ay * view.y, view.z).normalized();
        let across = stretched.x * stretched.x + stretched.y * stretched.y;
        let first = if across > 0.0 {
            Vec3::new(-stretched.y, stretched.x, 0.0) * (1.0 / mathf::sqrt(across))
        } else {
            Vec3::new(1.0, 0.0, 0.0)
        };
        let second = stretched.cross(first);
        let radius = mathf::sqrt(u);
        let angle = TAU * v;
        let p1 = radius * mathf::cos(angle);
        let blend = stretched.z.midpoint(1.0);
        let p2 = (1.0 - blend) * mathf::sqrt((1.0 - p1 * p1).max(0.0))
            + blend * radius * mathf::sin(angle);
        let lift = mathf::sqrt((1.0 - p1 * p1 - p2 * p2).max(0.0));
        let normal = first * p1 + second * p2 + stretched * lift;
        Vec3::new(self.ax * normal.x, self.ay * normal.y, normal.z.max(0.0)).normalized()
    }
}

/// Schlick's approximation of a conductor's or a coat's reflectance.
pub(crate) fn schlick(f0: Vec3, cos: f64) -> Vec3 {
    let m = (1.0 - cos).clamp(0.0, 1.0);
    let m2 = m * m;
    f0 + (Vec3::ONE - f0) * (m2 * m2 * m)
}

/// Schlick's approximation for a scalar reflectance.
pub(crate) fn schlick_scalar(f0: f64, cos: f64) -> f64 {
    let m = (1.0 - cos).clamp(0.0, 1.0);
    let m2 = m * m;
    f0 + (1.0 - f0) * (m2 * m2 * m)
}

/// The exact Fresnel reflectance of unpolarised light arriving at `cos_i` to
/// the normal of a boundary into a medium `eta` times as dense optically: `1`
/// at total internal reflection.
pub(crate) fn fresnel(cos_i: f64, eta: f64) -> f64 {
    let cos_i = cos_i.clamp(0.0, 1.0);
    let sin2_t = (1.0 - cos_i * cos_i) / (eta * eta);
    if sin2_t >= 1.0 {
        return 1.0;
    }
    let cos_t = mathf::sqrt(1.0 - sin2_t);
    let s = (cos_i - eta * cos_t) / (cos_i + eta * cos_t);
    let p = (eta * cos_i - cos_t) / (eta * cos_i + cos_t);
    (s * s).midpoint(p * p)
}

/// The direction unit `incident` takes through a boundary whose unit
/// `normal` faces it, into a medium `eta` times as dense optically; `None` at
/// total internal reflection.
pub(crate) fn refract(incident: Vec3, normal: Vec3, eta: f64) -> Option<Vec3> {
    let ratio = 1.0 / eta;
    let cos_i = -incident.dot(normal);
    let sin2_t = ratio * ratio * (1.0 - cos_i * cos_i);
    if sin2_t > 1.0 {
        return None;
    }
    let cos_t = mathf::sqrt(1.0 - sin2_t);
    Some((incident * ratio + normal * (ratio * cos_i - cos_t)).normalized())
}

/// The wavelengths, in nanometres, each of the display's primaries is taken
/// to span: three apiece, so a film's colours blend as white light's do
/// rather than banding.
const BANDS: [[f64; 3]; 3] = [
    [610.0, 645.0, 690.0],
    [505.0, 545.0, 580.0],
    [430.0, 460.0, 490.0],
];

/// The reflectance, per primary, of a film `thickness` nanometres thick and
/// of index `film`, between air and a medium of index `below`, of light
/// arriving at `cos` to its normal: the Airy sum of the light its two faces
/// reflect, for each polarisation, averaged.
pub(crate) fn thin_film(cos: f64, thickness: f64, film: f64, below: f64) -> Vec3 {
    let cos1 = cos.clamp(1e-4, 1.0);
    let sin2_1 = 1.0 - cos1 * cos1;
    let cos2 = mathf::sqrt((1.0 - sin2_1 / (film * film)).max(0.0));
    let sin2_3 = sin2_1 / (below * below);
    if sin2_3 >= 1.0 {
        return Vec3::ONE;
    }
    let cos3 = mathf::sqrt(1.0 - sin2_3);
    let amplitudes = [
        (
            (cos1 - film * cos2) / (cos1 + film * cos2),
            (film * cos2 - below * cos3) / (film * cos2 + below * cos3),
        ),
        (
            (film * cos1 - cos2) / (film * cos1 + cos2),
            (below * cos2 - film * cos3) / (below * cos2 + film * cos3),
        ),
    ];
    let path = 4.0 * PI * film * thickness * cos2;
    let primary = |band: &[f64; 3]| {
        let mut total = 0.0;
        for wavelength in band {
            let phase = mathf::cos(path / wavelength);
            for &(top, bottom) in &amplitudes {
                let cross = 2.0 * top * bottom * phase;
                let reflected = (top * top + bottom * bottom + cross)
                    / (1.0 + top * top * bottom * bottom + cross);
                total += reflected;
            }
        }
        (total / 6.0).clamp(0.0, 1.0)
    };
    Vec3::new(primary(&BANDS[0]), primary(&BANDS[1]), primary(&BANDS[2]))
}

/// How much more or less than its nominal index a dispersive medium bends
/// each primary, per unit of its dispersion: blue most, red least.
pub(crate) const SPREAD: [f64; 3] = [-0.8, 0.0, 1.1];

#[cfg(test)]
#[path = "material_tests.rs"]
mod tests;
