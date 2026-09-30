//! What a surface is made of — its pigment, its finish, and the relief that
//! tilts its normal — and the microfacet, Fresnel and thin-film terms light
//! meets it by.

use core::f64::consts::{PI, TAU};

use tairix_util::mathf;

use crate::noise::noise3;
use crate::pigment::Pigment;
use crate::sample::{mix32, unit};
use crate::vector::Vec3;

/// One travelling swell of a wavy surface: its wave vector, where it
/// starts, and how high it rises.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Swell {
    kx: f64,
    kz: f64,
    phase: f64,
    height: f64,
}

/// How many swells a rippled surface sums.
const SWELLS: usize = 6;

/// The fine shape of a surface, which tilts its normal from point to point.
#[derive(Clone, Debug)]
pub(crate) enum Relief {
    /// Ripples on a level surface: swells, each shorter and lower than the
    /// last, in a spread of directions about one wind.
    Ripples { swells: [Swell; SWELLS] },
    /// An even, fine unevenness in every direction: hammered metal, honed
    /// stone, snow. `depth` is how far it tilts the normal, `scale` how fine
    /// it is.
    Grain { depth: f64, scale: f64, seed: u32 },
}

impl Relief {
    /// Ripples under `seed`, the longest `height` high and `length` long,
    /// spread `spread` radians either side of one wind.
    pub(crate) fn ripples(height: f64, length: f64, spread: f64, seed: u32) -> Self {
        let wind = TAU * unit(mix32(seed));
        let mut swells = [Swell {
            kx: 0.0,
            kz: 0.0,
            phase: 0.0,
            height: 0.0,
        }; SWELLS];
        let (mut wavelength, mut amplitude, mut key) = (length, height, seed);
        for swell in &mut swells {
            key = mix32(key ^ 0x9e37_79b9);
            let heading = wind + (unit(key) - 0.5) * 2.0 * spread;
            let number = TAU / wavelength;
            *swell = Swell {
                kx: number * mathf::cos(heading),
                kz: number * mathf::sin(heading),
                phase: TAU * unit(mix32(key)),
                height: amplitude,
            };
            wavelength *= 0.63;
            amplitude *= 0.55;
        }
        Self::Ripples { swells }
    }

    /// `normal` at `p`, tilted by this relief.
    pub(crate) fn tilt(&self, normal: Vec3, p: Vec3) -> Vec3 {
        match self {
            Self::Ripples { swells } => {
                let (mut slope_x, mut slope_z) = (0.0, 0.0);
                for swell in swells {
                    let crest =
                        mathf::cos(swell.kx * p.x + swell.kz * p.z + swell.phase) * swell.height;
                    slope_x += crest * swell.kx;
                    slope_z += crest * swell.kz;
                }
                (normal + Vec3::new(-slope_x, 0.0, -slope_z)).normalized()
            }
            &Self::Grain { depth, scale, seed } => {
                let q = p * scale;
                let jolt = Vec3::new(
                    noise3(q, seed),
                    noise3(q, seed ^ 0x2c1b_3c6d),
                    noise3(q, seed ^ 0x297a_2d39),
                );
                let across = jolt - normal * jolt.dot(normal);
                (normal + across * depth).normalized()
            }
        }
    }
}

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
