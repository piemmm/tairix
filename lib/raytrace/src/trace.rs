//! The integrator: what light arrives along a ray, and what a pixel shows.
//!
//! Distributed ray tracing (Cook, Porter and Carpenter, 1984): every sample of
//! a pixel draws its own point in the pixel, on the lens, on each lamp and
//! through each glossy reflection, so soft shadows, blurred reflections and
//! depth of field all come of averaging. A surface gathers light from every
//! lamp directly, from everything else about it through one ray drawn by the
//! cosine and followed on to the sky or to whatever surface it meets, whose
//! own light is found the same way — so light passes from surface to
//! surface, as radiosity has it, and no part of it is guessed — and from
//! what its reflection or refraction shows through a ray followed on. A glossy
//! reflection that finds a lamp is weighed against sampling that lamp by the
//! power heuristic (Veach and Guibas, 1995), so neither way's noise wins.
//!
//! At its best a pixel is sampled in rounds of 16, 32, 64 and 128 — each a
//! whole stratification of every pair — and stops after any round whose
//! samples agree: flat sky settles at once, while an edge, a penumbra or a
//! glass interior takes what it needs. The samples are drawn from a Gaussian
//! reconstruction filter, so detail finer than a pixel is averaged rather
//! than aliased. The tolerance was chosen against 256-sample references; the
//! first round is sixteen samples, four by four over the filter, because
//! eight let thin edges slip between them.

use alloc::vec::Vec;
use core::f64::consts::PI;

use tairix_parallel::JobRunner;
use tairix_raster::Pixel;
use tairix_util::{fallible, mathf};

use crate::adapt::{self, Adaptation, Sample};
use crate::atmosphere::Lit;
use crate::band;
use crate::detail::Records;
use crate::grass::Canopy;
use crate::ground::Moisture;
use crate::heightfield::PLAIN;
use crate::light::Light;
use crate::material::{
    fresnel, refract, schlick, schlick_scalar, thin_film, widened, Bump, Finish, Foam, Material,
    Microfacet, Tilt, CARRIED_FOAM, COAT_F0, SPREAD,
};
use crate::noise::{cells3, noise3, smoothstep};
use crate::pigment::{Pigment, Spot};
use crate::radiosity::{self, Cell, Radiosity, Site};
use crate::sample::{cosine_hemisphere, disc, filter_offset, mix32, unit, Sampler};
use crate::scene::{Glare, Object, Scene, Sight};
use crate::shape::{Hit, Shape};
use crate::sky::Seeing;
use crate::tone::{display, Encoder};
use crate::vector::{real, share, Frame, Ray, Vec3, PACKET};

/// The deepest a path is followed.
const MAX_DEPTH: u32 = 9;
/// To this depth glass is followed both ways, reflected and refracted; past
/// it, one way, drawn by the reflectance.
const SPLIT_DEPTH: u32 = 2;
/// A ray whose light can add less than this to its pixel is not followed.
const CUTOFF: f64 = 0.004;
/// The least a diffuse bounce's light may still add to the pixel before it
/// goes on only by the chance its weight bears to this, its light scaled up
/// by that chance to make up for the paths ended (Russian roulette): what a
/// dim or long path carries is never simply dropped.
const ROULETTE: f64 = 0.1;
/// The share of a pixel's light at which a surface's own counts as much of
/// it: seen directly, or through clear water or a mirror, but not in a
/// coat's faint reflection. Such a surface's bounce always goes on, so a
/// dark surface's samples are not each all its light or none of it, and
/// its random gather keeps the pixel from settling on its first samples.
const GATHERED_SHARE: f64 = 0.25;
/// The roughness a spot's highlight is drawn at however smooth the surface:
/// a real lamp has a size, and so a highlight.
const HIGHLIGHT_ROUGHNESS: f64 = 0.14;
/// After the first round, how far apart its samples may lie and still be
/// taken as settled, on the square-root scale the eye reads light on.
const SETTLED_SPREAD: f64 = 0.024;
/// The fewest samples a pixel takes whose samples lean on light gathered by
/// random rays.
const GATHERED_LEAST: u32 = 64;
/// After later rounds, the standard error of the mean they may leave, and how
/// far their brightest may move it, on that scale: a little over three of
/// the display's 255 levels.
const SETTLED_ERROR: f64 = 0.012;
/// How far a ray still in a medium when it leaves the scene is taken to have
/// travelled through it.
const FATHOMLESS: f64 = 1e3;
/// The colour breaking water whitens to.
const FOAM: Vec3 = Vec3::new(0.86, 0.9, 0.92);
/// Water's reflectance at normal incidence.
const WATER_F0: f64 = 0.02;
/// Below this share of its surface wet, ground shows no sheen.
const DAMP: f64 = 0.08;
/// The most a slanting view draws a pixel's footprint out: past it, the
/// surface is all but edge on, and the footprint as long as the view.
pub(crate) const SLANTEST: f64 = 0.08;

/// How hard a tracer works at a pixel: the rounds of samples it may stop
/// after.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Quality {
    /// 8 samples.
    Draft,
    /// 8, or at most 16.
    Fair,
    /// 16, or at most 32.
    Good,
    /// 16, or at most 128: the best, which the screensaver traces at.
    Fine,
}

impl Quality {
    /// Every quality, the least first.
    pub const ALL: [Self; 4] = [Self::Draft, Self::Fair, Self::Good, Self::Fine];

    /// The sample counts a pixel may stop at.
    const fn rounds(self) -> &'static [u32] {
        match self {
            Self::Draft => &[8],
            Self::Fair => &[8, 16],
            Self::Good => &[16, 32],
            Self::Fine => &[16, 32, 64, 128],
        }
    }

    /// The most samples a pixel takes at this quality.
    #[must_use]
    pub const fn most(self) -> u32 {
        match self {
            Self::Draft => 8,
            Self::Fair => 16,
            Self::Good => 32,
            Self::Fine => 128,
        }
    }
}

/// The samples a packet takes.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a packet is a handful of rays, checked below to fit"
)]
const PACKET_SAMPLES: u32 = PACKET as u32;

// Every round a pixel may stop at is a whole number of packets, so a packet
// never takes samples past the round it falls in.
const _: () = {
    assert!(PACKET_SAMPLES as usize == PACKET);
    let mut quality = 0;
    while quality < Quality::ALL.len() {
        let rounds = Quality::ALL[quality].rounds();
        let mut round = 0;
        while round < rounds.len() {
            assert!(rounds[round].is_multiple_of(PACKET_SAMPLES));
            round += 1;
        }
        quality += 1;
    }
};

/// How a ray came to be followed, which decides what a lamp it finds is
/// worth.
#[derive(Copy, Clone, Debug)]
enum Arrival {
    /// From the eye, a mirror or glass: nothing else finds what it finds,
    /// so a lamp counts in full.
    Seen,
    /// Off a glossy surface at `from`, drawn with `density` per unit solid
    /// angle: a lamp it finds is weighed against sampling that lamp.
    Glossy { from: Vec3, density: f64 },
}

/// What a clear medium does to light crossing it.
#[derive(Copy, Clone, Debug)]
struct Medium {
    absorb: Vec3,
    /// The light it scatters back toward the eye, where it is deep.
    glow: Vec3,
    /// Its refractive index, which bends the sun's light into it.
    ior: f64,
}

/// How wide a path's view of what it meets grows with the distance it has
/// come: the eye's own spread, widened by every rough clear surface it has
/// passed through or glanced off (Amanatides, "Ray Tracing with Cones", 1984).
#[derive(Copy, Clone, Debug)]
struct Cone {
    /// The spread the rough surfaces added, in radians.
    spread: f64,
    /// The spread times the distance each was added at, which the width they
    /// add grows from.
    lead: f64,
}

impl Cone {
    /// The eye's own, a pixel's angle across.
    const PINHOLE: Self = Self {
        spread: 0.0,
        lead: 0.0,
    };

    /// The width of the view a pixel `pixel` radians across has `travelled`
    /// from the eye.
    fn width(self, pixel: f64, travelled: f64) -> f64 {
        travelled * pixel + (self.spread * travelled - self.lead).max(0.0)
    }

    /// This cone spread `more` radians further at `travelled` from the eye.
    fn widened(self, more: f64, travelled: f64) -> Self {
        Self {
            spread: self.spread + more,
            lead: self.lead + more * travelled,
        }
    }
}

/// A ray's place in its path.
#[derive(Copy, Clone, Debug)]
struct Path {
    depth: u32,
    /// The most any channel of this ray's light can add to the pixel.
    weight: f64,
    arrival: Arrival,
    /// The medium the ray is in; `None` for air.
    medium: Option<Medium>,
    /// How far the path had come by this ray's origin: what widens the
    /// footprint a pattern is averaged over, as its cone does.
    travelled: f64,
    cone: Cone,
    /// The one primary a dispersive surface split the path's light down to,
    /// once one has.
    channel: Option<usize>,
    /// Whether the path has scattered off a diffuse surface, which gathered
    /// the lamps and the sun directly: from there on their images and
    /// highlights count for nothing, so a caustic — sunlight glinting off
    /// water onto a wall — is not found by a path that could only stumble on
    /// it.
    scattered: bool,
}

impl Path {
    const EYE: Self = Self {
        depth: 0,
        weight: 1.0,
        arrival: Arrival::Seen,
        medium: None,
        travelled: 0.0,
        cone: Cone::PINHOLE,
        channel: None,
        scattered: false,
    };

    /// The path one bounce on, carrying `weight` of its light.
    fn next(self, weight: f64, arrival: Arrival, medium: Option<Medium>, travelled: f64) -> Self {
        Self {
            depth: self.depth + 1,
            weight,
            arrival,
            medium,
            travelled,
            cone: self.cone,
            channel: self.channel,
            scattered: self.scattered,
        }
    }
}

/// A shading point.
struct Surface {
    point: Vec3,
    /// The geometric normal on the side the ray came from.
    facing: Vec3,
    /// The shading normal, tilted by relief, on that side, and the smooth
    /// normal it was tilted from.
    normal: Vec3,
    smooth: Vec3,
    toward_eye: Vec3,
    travelled: f64,
    /// How wide a patch across the view one pixel's view of the point covers.
    width: f64,
    /// The point in the texture frame of what was met, that frame's first
    /// axis in the world, which a brushed surface is brushed along, the frame
    /// itself, and the key of the placing met.
    texture: Vec3,
    grain: Vec3,
    frame: Frame,
    instance: u32,
    /// The surface's own coordinates there, and whether its front was met.
    uv: (f64, f64),
    front: bool,
    /// The blades of the sward the point lies within, which thin the light
    /// reaching it.
    canopy: Option<Canopy>,
    /// The slope variance of relief too fine for the pixel to resolve, which
    /// roughens the surface instead.
    unresolved: f64,
}

/// How a specular lobe reflects: Schlick's approximation from a reflectance,
/// a film of water over some share of a surface, or the interference of a
/// thin film.
#[derive(Copy, Clone, Debug)]
enum Reflectance {
    Schlick(Vec3),
    /// Water's own reflectance, over the share of the surface it wets.
    Damp(f64),
    Film {
        thickness: f64,
        index: f64,
        below: f64,
    },
}

impl Reflectance {
    fn at(&self, cos: f64) -> Vec3 {
        match *self {
            Self::Schlick(f0) => schlick(f0, cos),
            Self::Damp(wet) => Vec3::splat(schlick_scalar(WATER_F0, cos) * wet),
            Self::Film {
                thickness,
                index,
                below,
            } => thin_film(cos, thickness, index, below),
        }
    }
}

/// One specular lobe of a surface, in its own shading frame.
#[derive(Copy, Clone, Debug)]
struct Specular {
    frame: Frame,
    micro: Microfacet,
    reflectance: Reflectance,
    /// The chance this lobe's reflection is the one a sample follows, which
    /// its density is weighed by.
    share: f64,
}

/// How a surface reflects a lamp.
struct Lobes {
    /// The diffuse reflectance, divided by π.
    diffuse: Vec3,
    specular: Option<Specular>,
    /// What light reaching the back of a thin surface comes through, divided
    /// by π.
    translucent: Vec3,
    /// Whether only spots are gathered: glass and bubbles take the rest
    /// through their own rays.
    spots_only: bool,
}

/// Light a ray brings back, and whether it leans on a random ray gathered by
/// a surface whose own light is much of it: a pixel's first samples doing so
/// can all miss the light its later ones find.
#[derive(Copy, Clone, Debug)]
struct Shaded {
    light: Vec3,
    gathered: bool,
}

impl Shaded {
    /// `light` no random gather brought.
    const fn ungathered(light: Vec3) -> Self {
        Self {
            light,
            gathered: false,
        }
    }
}

impl core::ops::Add for Shaded {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            light: self.light + other.light,
            gathered: self.gathered || other.gathered,
        }
    }
}

impl core::ops::Add<Vec3> for Shaded {
    type Output = Self;

    fn add(self, light: Vec3) -> Self {
        Self {
            light: self.light + light,
            ..self
        }
    }
}

impl core::ops::Mul<Vec3> for Shaded {
    type Output = Self;

    fn mul(self, by: Vec3) -> Self {
        Self {
            light: self.light * by,
            ..self
        }
    }
}

impl core::ops::Mul<f64> for Shaded {
    type Output = Self;

    fn mul(self, by: f64) -> Self {
        Self {
            light: self.light * by,
            ..self
        }
    }
}

/// A scene's tracer for a picture of `size`, each pixel's samples hashed
/// under the scene's `key`.
pub struct Tracer<'a> {
    scene: &'a Scene,
    radiosity: Option<&'a Radiosity>,
    encoder: &'a Encoder,
    size: (u32, u32),
    pixel_angle: f64,
    key: u32,
    /// Which way the sun's light comes in at the eye, if one lights the
    /// scene.
    sun: Option<Vec3>,
}

impl<'a> Tracer<'a> {
    /// A tracer of `scene` for a picture of `size`, toned through `encoder`,
    /// its samples hashed under `key`.
    #[must_use]
    pub fn new(scene: &'a Scene, encoder: &'a Encoder, size: (u32, u32), key: u32) -> Self {
        let eye = scene.camera.eye();
        Self {
            scene,
            radiosity: scene.radiosity.as_ref(),
            encoder,
            size,
            pixel_angle: scene.camera.pixel_angle(size.1),
            key,
            sun: scene.sun_at(eye).map(|(toward, _)| toward),
        }
    }

    /// The pixel at `at` traced at `quality`, and how many samples it took.
    #[must_use]
    pub fn pixel(&self, at: (u32, u32), quality: Quality) -> (Pixel, u32) {
        let (light, taken) = self.light(at, quality);
        (self.encoder.pixel(light, at), taken)
    }

    /// The log luminance of metering point `index`: one sample of the
    /// scene's own light, unexposed, at a jittered place in its cell of the
    /// meter's grid.
    fn metered(&self, index: u32) -> f64 {
        let (column, row) = (index % METER_COLUMNS, index / METER_COLUMNS);
        let mut sampler = Sampler::new(mix32(index ^ 0x6d2b_79f5), 0);
        let (u, v) = sampler.next_2d();
        let film = (
            2.0 * (f64::from(column) + u) / f64::from(METER_COLUMNS) - 1.0,
            1.0 - 2.0 * (f64::from(row) + v) / f64::from(METER_ROWS),
        );
        let ray = self.scene.camera.ray(film, (0.0, 0.0));
        let light = self.radiance(&ray, Path::EYE, &mut sampler).light;
        let luminance = light.luminance();
        if luminance.is_finite() {
            mathf::ln(luminance.max(1e-9))
        } else {
            0.0
        }
    }

    /// Adaptation sample `index` of a film measured over `points` across and
    /// down: one of four drawn two by two within its point, its light as the
    /// lens exposes it, glare and all, in stops from the exposed `key`.
    fn adapting(&self, index: usize, points: (usize, usize), key: f64) -> Sample {
        let (point, stratum) = (index / ADAPT_SAMPLES, index % ADAPT_SAMPLES);
        let (column, row) = (point % points.0.max(1), point / points.0.max(1));
        let draw = u32::try_from(index).unwrap_or(u32::MAX);
        let mut sampler = Sampler::new(mix32(draw ^ 0x2c9b_7f01), 0);
        let (u, v) = sampler.next_2d();
        let across = (real(column) + f64::midpoint(real(stratum % 2), u)) / real(points.0.max(1));
        let down = (real(row) + f64::midpoint(real(stratum / 2), v)) / real(points.1.max(1));
        let ray = self
            .scene
            .camera
            .ray((2.0 * across - 1.0, 1.0 - 2.0 * down), (0.0, 0.0));
        let light = (self.radiance(&ray, Path::EYE, &mut sampler).light + self.glare(ray.dir))
            * self.scene.exposure;
        let luminance = light.luminance();
        Sample {
            across,
            down,
            stops: if luminance.is_finite() {
                adapt::stops(luminance, key)
            } else {
                0.0
            },
        }
    }

    /// What pixel `at` shows as display-linear light, and how many samples it
    /// took.
    fn light(&self, (x, y): (u32, u32), quality: Quality) -> (Vec3, u32) {
        let seed = mix32(self.key ^ mix32(y.wrapping_mul(self.size.0).wrapping_add(x)));
        let mut tally = Tally::new();
        let mut taken = 0;
        for (index, &round) in quality.rounds().iter().enumerate() {
            while taken < round {
                // A packet of samples: their eye rays all pass through this
                // pixel, near enough parallel to cross a lawn's cells
                // together, and each is then lit as it would be alone.
                let mut samplers: [Sampler; PACKET] = core::array::from_fn(|lane| {
                    Sampler::new(seed, taken + u32::try_from(lane).unwrap_or(u32::MAX))
                });
                let mut rays = [Ray::new(Vec3::ZERO, Vec3::ZERO); PACKET];
                let mut films = [(0.0, 0.0); PACKET];
                for ((ray, film), sampler) in rays.iter_mut().zip(&mut films).zip(&mut samplers) {
                    let (u, v) = sampler.next_2d();
                    let offset = (filter_offset(u), filter_offset(v));
                    let lens = disc(sampler.next_2d());
                    *film = self.film((x, y), offset);
                    *ray = self.scene.camera.ray(*film, lens);
                }
                let mut found = [None; PACKET];
                self.scene
                    .closest_of(&rays, f64::INFINITY, Sight::Eye, &mut found);
                for (((ray, film), sampler), found) in
                    rays.iter().zip(films).zip(&mut samplers).zip(found)
                {
                    let (arrived, _) = self.arrived(ray, Path::EYE, sampler, found);
                    let exposed = (arrived.light + self.glare(ray.dir)) * self.scene.exposure;
                    tally.add(display(self.adapted(exposed, film)), arrived.gathered);
                }
                taken += PACKET_SAMPLES;
            }
            if tally.settled(index == 0) {
                break;
            }
        }
        (tally.mean(), taken)
    }

    /// `exposed` light, a sample at `film` shows, corrected by the scene's
    /// local adaptation where it has one.
    fn adapted(&self, exposed: Vec3, (x, y): (f64, f64)) -> Vec3 {
        match &self.scene.adaptation {
            Some(adaptation) => {
                let at = (f64::midpoint(x, 1.0), f64::midpoint(1.0, -y));
                exposed * adaptation.factor(at, exposed.luminance())
            }
            None => exposed,
        }
    }

    /// Where on the film the point `offset` from the centre of pixel `(x, y)`
    /// lies.
    fn film(&self, (x, y): (u32, u32), (dx, dy): (f64, f64)) -> (f64, f64) {
        let (width, height) = (f64::from(self.size.0.max(1)), f64::from(self.size.1.max(1)));
        (
            2.0 * (f64::from(x) + 0.5 + dx) / width - 1.0,
            1.0 - 2.0 * (f64::from(y) + 0.5 + dy) / height,
        )
    }

    /// The eye's ray through the centre of pixel `at`.
    pub(crate) fn eye_ray(&self, at: (u32, u32)) -> Ray {
        self.scene.camera.ray(self.film(at, (0.0, 0.0)), (0.0, 0.0))
    }

    /// Where the eye's ray through the centre of pixel `at` first meets a
    /// diffuse surface a radiosity record may be gathered on; `None` where it
    /// meets glass, metal, a leaf or nothing.
    pub(crate) fn site(&self, at: (u32, u32)) -> Option<Site> {
        let ray = self.eye_ray(at);
        let (index, hit) = self.scene.closest(&ray, f64::INFINITY, Sight::Recorded)?;
        let object = self.scene.objects.get(index)?;
        let material = self
            .scene
            .materials
            .get(hit.material.map_or(object.material, |part| part as usize))?;
        let diffuse = matches!(
            material.finish,
            Finish::Matte
                | Finish::Coated { .. }
                | Finish::Ground
                | Finish::Film { shell: false, .. }
        );
        if !diffuse {
            return None;
        }
        let (surface, _) = self.surface(&ray, &hit, (object, material), (0.0, Cone::PINHOLE));
        Some(Site {
            point: surface.point,
            facing: surface.facing,
            normal: surface.smooth,
            travelled: surface.travelled,
            span: surface.travelled * self.pixel_angle * f64::from(self.size.1),
            seed: radiosity::seed(at),
        })
    }

    /// Fill `cells`, row `row` of a radiosity record's hemisphere cut as
    /// `records` has it, with what its rays bring back to `site`, each
    /// followed as a path of its own.
    pub(crate) fn gather(
        &self,
        site: &Site,
        (records, row): (&Records, usize),
        cells: &mut [Cell],
    ) {
        let frame = Frame::around(site.normal);
        let origin = lift(site.point, site.facing);
        let path = Path {
            depth: 1,
            weight: 1.0,
            arrival: Arrival::Seen,
            medium: None,
            travelled: site.travelled,
            cone: Cone::PINHOLE,
            channel: None,
            scattered: true,
        };
        let first = u32::try_from(row * records.columns).unwrap_or(u32::MAX);
        for (index, cell) in (first..).zip(cells.iter_mut().take(records.columns)) {
            let mut sampler = Sampler::new(site.seed, index);
            let dir = frame.to_world(radiosity::direction(
                records,
                index as usize,
                sampler.next_2d(),
            ));
            *cell = if dir.dot(site.facing) > 0.0 {
                let (arrived, distance) = self.arriving(&Ray::new(origin, dir), path, &mut sampler);
                let light = arrived.light;
                if light.is_finite() {
                    Cell { light, distance }
                } else {
                    Cell::DARK
                }
            } else {
                Cell::DARK
            };
        }
    }

    /// The glare a lens spreads about the sun's image toward `dir`: a bloom
    /// close about it and a faint veil further out, each a share of the sun's
    /// light the eye receives.
    fn glare(&self, dir: Vec3) -> Vec3 {
        let Some(Glare { toward, irradiance }) = self.scene.glare else {
            return Vec3::ZERO;
        };
        let cos = dir.dot(toward);
        if cos <= 0.0 {
            return Vec3::ZERO;
        }
        let angle2 = 2.0 * (1.0 - cos);
        let bloom = GLARE_BLOOM * mathf::exp(-angle2 / (2.0 * GLARE_BLOOM_SPREAD2))
            / (2.0 * PI * GLARE_BLOOM_SPREAD2);
        let veil_spread = 1.0 + angle2 / GLARE_VEIL_SPREAD2;
        let veil = GLARE_VEIL / (PI * GLARE_VEIL_SPREAD2 * veil_spread * veil_spread);
        irradiance * (bloom + veil)
    }

    /// The light arriving back along `ray`, and whether it leans on a random
    /// gather.
    fn radiance(&self, ray: &Ray, path: Path, sampler: &mut Sampler) -> Shaded {
        self.arriving(ray, path, sampler).0
    }

    /// The light arriving back along `ray`, and how far along it lies the
    /// surface it left: infinitely far for the sky.
    fn arriving(&self, ray: &Ray, path: Path, sampler: &mut Sampler) -> (Shaded, f64) {
        let sight = if path.scattered {
            Sight::Scattered
        } else if path.depth == 0 {
            Sight::Eye
        } else {
            Sight::Bounce
        };
        let found = self.scene.closest(ray, f64::INFINITY, sight);
        self.arrived(ray, path, sampler, found)
    }

    /// [`Tracer::arriving`] for a ray whose nearest object, and where it
    /// meets it, is `found`.
    fn arrived(
        &self,
        ray: &Ray,
        path: Path,
        sampler: &mut Sampler,
        found: Option<(usize, Hit)>,
    ) -> (Shaded, f64) {
        let Some((index, hit)) = found else {
            // A medium with no far side is as good as fathomless.
            if let Some(medium) = path.medium {
                let kept = (medium.absorb * -FATHOMLESS).exp();
                let light =
                    self.escaped(ray, path, sampler) * kept + medium.glow * (Vec3::ONE - kept);
                return (Shaded::ungathered(light), f64::INFINITY);
            }
            return (
                Shaded::ungathered(self.escaped(ray, path, sampler)),
                f64::INFINITY,
            );
        };
        let Some((object, material)) = self.scene.objects.get(index).and_then(|object| {
            let made = hit.material.map_or(object.material, |part| part as usize);
            self.scene
                .materials
                .get(made)
                .map(|material| (object, material))
        }) else {
            return (Shaded::ungathered(Vec3::ZERO), hit.t);
        };
        let shaded = match material.finish {
            Finish::Glow { radiance } => {
                Shaded::ungathered(self.lamp(object, radiance, ray, hit.t, path))
            }
            _ => self.shade(ray, hit, object, material, path, sampler),
        };
        let light = self.attenuate(shaded.light, ray, hit.t, path, sampler);
        (Shaded { light, ..shaded }, hit.t)
    }

    /// `light` after crossing `t` of the ray's medium: absorbed and lit
    /// within glass or water, dimmed and brightened by the air or veiled by
    /// haze in the open.
    fn attenuate(&self, light: Vec3, ray: &Ray, t: f64, path: Path, sampler: &mut Sampler) -> Vec3 {
        if let Some(medium) = path.medium {
            let kept = (medium.absorb * -t).exp();
            return light * kept + medium.glow * (Vec3::ONE - kept);
        }
        let sky = self.lit_air(ray, t);
        // What the eye sees of the air is shadowed by what stands in the
        // sun's way, judged at one point drawn along it as that air gathers
        // its sunlight; a bounce's shorter, dimmer stretch takes the sky's
        // roofing for the sun's too, and its middle for the sky overhead.
        let lit = || {
            let (sun, at) = if path.depth == 0 && !path.scattered {
                let at = ray.at(self.scene.sky.drawn(ray.dir, (0.0, t), sampler.next_1d()));
                (self.sunlit_air(at), at)
            } else {
                (Vec3::splat(sky), ray.at(0.5 * t))
            };
            let (open, glow) = self.scene.sky.beneath(at);
            Lit {
                sun,
                sky,
                open,
                glow,
            }
        };
        self.scene
            .sky
            .aerial(ray.dir, t, light, lit)
            .unwrap_or(light)
    }

    /// How much of the sun's light reaches the air at `at`: what stands
    /// between it and the sun, and the clouds overhead. Drawn afresh along
    /// the air for each of a pixel's samples, the points average to the
    /// share of the air lit, so shafts through a wood's gaps glow and its
    /// shadow does not.
    fn sunlit_air(&self, at: Vec3) -> Vec3 {
        let Some(toward) = self.sun else {
            return Vec3::ONE;
        };
        self.scene
            .transmittance(&Ray::new(at, toward), f64::INFINITY, None)
            * self.scene.sky.clouded(at, toward)
    }

    /// How much of the air along `ray` out to `t` the sky lights: all of it
    /// in the open, and beneath a wood's crowns only what they let through.
    fn lit_air(&self, ray: &Ray, t: f64) -> f64 {
        let Some(shades) = &self.scene.shades else {
            return 1.0;
        };
        if t < ROOFED_AIR.0 {
            return 1.0;
        }
        let (above, through) = (ray.origin.y + ROOFED_AIR.1, ROOFED_AIR.2);
        let lit: f64 = (0..AIR_SAMPLES)
            .map(|sample| {
                let at = ray.at(t * (f64::from(sample) + 0.5) / f64::from(AIR_SAMPLES));
                if at.y > above {
                    1.0
                } else {
                    1.0 - (1.0 - through) * shades.at(at.x, at.z).1
                }
            })
            .sum();
        lit / f64::from(AIR_SAMPLES)
    }

    /// What a ray arriving `t` along `ray` sees on a lamp's own surface: all
    /// of a glowing thing no lamp stands for, which nothing gathers directly.
    fn lamp(&self, object: &Object, radiance: Vec3, ray: &Ray, t: f64, path: Path) -> Vec3 {
        let Some(light) = object.light.and_then(|index| self.scene.lights.get(index)) else {
            return radiance;
        };
        if path.scattered {
            return Vec3::ZERO;
        }
        let seen = light.seen(ray.dir);
        match path.arrival {
            Arrival::Seen => seen,
            Arrival::Glossy { from, density } => {
                seen * power(density, light.density(from, ray.dir, t, &self.scene.sky))
            }
        }
    }

    /// What a ray leaving the scene sees: the sky and its clouds, the stars
    /// spread over the ray's footprint, and any sun whose disc the air bends
    /// it into.
    fn escaped(&self, ray: &Ray, path: Path, sampler: &mut Sampler) -> Vec3 {
        let sky = &self.scene.sky;
        let discs = || self.scene.lights.iter().chain(sky.moon.as_ref());
        let seeing = Seeing {
            fine: matches!(path.arrival, Arrival::Seen) && !path.scattered,
            spread: (!path.scattered).then_some(self.pixel_angle + path.cone.spread),
            jitter: sampler.next_1d(),
            air: sampler.next_1d(),
            occulted: discs().any(|disc| disc.covers(ray.dir, sky)),
        };
        let mut light = sky.radiance(ray.origin, ray.dir, seeing);
        if path.scattered {
            return light;
        }
        for sun in discs() {
            let disc = sun.disc(ray.origin, ray.dir, sky);
            if disc.max_element() <= 0.0 {
                continue;
            }
            light += match path.arrival {
                Arrival::Seen => disc,
                Arrival::Glossy { from, density } => {
                    disc * power(density, sun.density(from, ray.dir, f64::INFINITY, sky))
                }
            };
        }
        light
    }

    fn shade(
        &self,
        ray: &Ray,
        hit: Hit,
        object: &Object,
        material: &Material,
        path: Path,
        sampler: &mut Sampler,
    ) -> Shaded {
        let (mut surface, outside) =
            self.surface(ray, &hit, (object, material), (path.travelled, path.cone));
        surface.canopy = self.scene.canopy(surface.point);
        let spot = self.spot(&surface, object, &hit);
        let pigment = || material.pigment.colour(&spot);
        match material.finish {
            Finish::Coated { roughness } => self.coated(
                &surface,
                pigment(),
                Microfacet::isotropic(roughness),
                path,
                sampler,
            ),
            Finish::Matte => self.matte(&surface, pigment(), path, sampler),
            Finish::Metal { roughness } => self.metal(
                &surface,
                pigment(),
                Microfacet::isotropic(roughness),
                path,
                sampler,
            ),
            Finish::Brushed { along, across } => {
                let micro = Microfacet::anisotropic(along, across);
                self.metal(&surface, pigment(), micro, path, sampler)
            }
            Finish::Lacquer { roughness, flakes } => {
                self.lacquer(&surface, pigment(), (roughness, flakes), path, sampler)
            }
            Finish::Glass {
                ior,
                absorb,
                glow,
                roughness,
                dispersion,
                foam,
            } => {
                let carried = spot.ground.get(CARRIED_FOAM).copied().unwrap_or(0.0);
                let foamed = foam
                    .map_or(0.0, |foam| foam_cover(&foam, &surface))
                    .max(carried_foam(carried, &surface));
                self.clear(
                    (&surface, outside),
                    (ior, dispersion, roughness),
                    (absorb, glow, foamed),
                    path,
                    sampler,
                )
            }
            Finish::Film {
                thickness,
                index,
                shell,
                seed,
            } => {
                let thick = film_thickness(&surface, thickness, seed);
                if shell {
                    self.bubble(&surface, (thick, index), path, sampler)
                } else {
                    self.iridescent(
                        &surface,
                        &material.pigment,
                        &spot,
                        (thick, index),
                        path,
                        sampler,
                    )
                }
            }
            Finish::Leaf { translucency } => {
                self.leaf(&surface, pigment(), translucency, path, sampler)
            }
            Finish::Ground => {
                let Moisture { standing, soaked } = match &material.pigment {
                    Pigment::Ground(ground) => ground.moisture(&spot),
                    _ => Moisture {
                        standing: 0.0,
                        soaked: 0.0,
                    },
                };
                // Wet soil darkens, and where water stands on it, shines.
                let albedo = pigment() * (1.0 - 0.4 * soaked);
                if standing < DAMP {
                    self.matte(&surface, albedo, path, sampler)
                } else {
                    self.damp(&surface, albedo, standing, path, sampler)
                }
            }
            Finish::Glow { radiance } => Shaded::ungathered(radiance),
        }
    }

    /// A clear surface bending light by its index, dispersion and roughness,
    /// filled with a medium that absorbs and glows as its finish has it, or
    /// the foam over it where `foamed` of it is foam.
    fn clear(
        &self,
        (surface, outside): (&Surface, bool),
        bending: (f64, f64, f64),
        (absorb, glow, foamed): (Vec3, Vec3, f64),
        path: Path,
        sampler: &mut Sampler,
    ) -> Shaded {
        if foamed > 0.0 && sampler.next_1d() < foamed {
            return self.coated(surface, FOAM, Microfacet::isotropic(0.9), path, sampler);
        }
        let medium = Medium {
            absorb,
            glow: glow * self.scene.daylight,
            ior: bending.0,
        };
        self.glass(surface, outside, bending, medium, path, sampler)
    }

    /// Where `ray` met `object` at `hit`, having come `travelled` before it
    /// within `cone`: the shading point on the side it came from, its normal
    /// tilted by the material's relief, and whether it came from outside.
    fn surface(
        &self,
        ray: &Ray,
        hit: &Hit,
        (object, material): (&Object, &Material),
        (travelled, cone): (f64, Cone),
    ) -> (Surface, bool) {
        let point = ray.at(hit.t);
        let toward_eye = -ray.dir;
        let outside = hit.normal.dot(toward_eye) >= 0.0;
        let (placing, instance) = object.placing(hit, self.scene.geometry());
        let texture = placing.point_to_local(point);
        let width = cone.width(self.pixel_angle, travelled + hit.t);
        let bump = Bump {
            p: texture,
            uv: hit.uv,
            tangent: hit.tangent,
            girth: hit.girth,
            instance,
            width,
            stretch: along_view(width, hit.shading, toward_eye),
        };
        let untilted = Tilt {
            normal: hit.shading,
            unresolved: 0.0,
        };
        let tilt = match material.relief.as_ref() {
            Some(relief) if !hit.relieved => relief.tilt(hit.shading, &bump),
            _ => untilted,
        };
        let side = if outside { 1.0 } else { -1.0 };
        let surface = Surface {
            point,
            facing: hit.normal * side,
            normal: facing_eye(tilt.normal * side, toward_eye),
            smooth: facing_eye(hit.shading * side, toward_eye),
            toward_eye,
            travelled: travelled + hit.t,
            width,
            texture,
            grain: placing.frame.x,
            frame: placing.frame,
            instance,
            uv: hit.uv,
            front: outside,
            canopy: None,
            unresolved: tilt.unresolved,
        };
        (surface, outside)
    }

    /// Where `object`'s pigment is looked up for `surface`: in the object's
    /// own texture frame, normal and all, so a pattern turns with it; on a
    /// land, with what the land is like there.
    fn spot(&self, surface: &Surface, object: &Object, hit: &Hit) -> Spot {
        let ground = match object.shape {
            Shape::Land { field } => self.scene.fields.get(field as usize).map_or(PLAIN, |grid| {
                grid.attributes_at(surface.point.x, surface.point.z)
            }),
            _ => PLAIN,
        };
        Spot {
            p: surface.texture,
            normal: surface.frame.to_local(surface.normal),
            height: surface.point.y,
            width: Self::footprint(surface),
            mark: hit.mark,
            along: hit.along,
            uv: surface.uv,
            girth: hit.girth,
            instance: surface.instance,
            front: surface.front,
            ground,
            thatch: surface.canopy.map_or(0.0, |canopy| 1.0 - canopy.diffuse()),
            cover: hit.cover.map(f64::from),
        }
    }

    /// How finely one pixel's view of `surface` resolves it.
    fn resolution(surface: &Surface) -> f64 {
        resolved(surface.width, surface.normal, surface.toward_eye)
    }

    /// How wide a patch of `surface` one pixel's view of it covers.
    fn footprint(surface: &Surface) -> f64 {
        along_view(surface.width, surface.normal, surface.toward_eye)
    }

    /// A shading frame about `surface`'s normal, its first axis along the
    /// texture frame's, so a brushed surface is brushed that way.
    fn frame(surface: &Surface) -> Frame {
        let along = surface.grain - surface.normal * surface.grain.dot(surface.normal);
        if along.length() < 1e-6 {
            return Frame::around(surface.normal);
        }
        let x = along.normalized();
        Frame {
            x,
            y: surface.normal.cross(x),
            z: surface.normal,
        }
    }

    /// A pigment under a clear coat: the lamps and the sky on the pigment,
    /// less what the coat reflects, and the coat's reflection over it.
    fn coated(
        &self,
        surface: &Surface,
        pigment: Vec3,
        micro: Microfacet,
        path: Path,
        sampler: &mut Sampler,
    ) -> Shaded {
        let cos_eye = surface.normal.dot(surface.toward_eye).max(1e-4);
        let under = 1.0 - schlick_scalar(COAT_F0, cos_eye);
        let coat = Specular {
            frame: Frame::around(surface.normal),
            micro,
            reflectance: Reflectance::Schlick(Vec3::splat(COAT_F0)),
            share: 1.0,
        };
        let lobes = Lobes {
            diffuse: pigment * (under / PI),
            specular: Some(coat),
            translucent: Vec3::ZERO,
            spots_only: false,
        };
        let direct = self.direct(surface, &lobes, path, sampler);
        let diffused = self.diffused(surface, pigment * under, path, sampler);
        Shaded::ungathered(direct) + diffused + self.coat(surface, &coat, path, sampler)
    }

    /// Ground wet over `wet` of its surface: matte beneath, and the film of
    /// water over it reflecting as smoothly as it lies.
    fn damp(
        &self,
        surface: &Surface,
        pigment: Vec3,
        wet: f64,
        path: Path,
        sampler: &mut Sampler,
    ) -> Shaded {
        let film = Specular {
            frame: Frame::around(surface.normal),
            micro: Microfacet::isotropic(0.55 - 0.45 * wet),
            reflectance: Reflectance::Damp(wet * wet),
            share: 1.0,
        };
        let cos_eye = surface.normal.dot(surface.toward_eye).max(1e-4);
        let under = 1.0 - schlick_scalar(WATER_F0, cos_eye) * wet * wet;
        let lobes = Lobes {
            diffuse: pigment * (under / PI),
            specular: Some(film),
            translucent: Vec3::ZERO,
            spots_only: false,
        };
        let direct = self.direct(surface, &lobes, path, sampler);
        let diffused = self.diffused(surface, pigment * under, path, sampler);
        Shaded::ungathered(direct) + diffused + self.coat(surface, &film, path, sampler)
    }

    /// A matte pigment: the lamps and the sky on it, and no reflection.
    fn matte(&self, surface: &Surface, pigment: Vec3, path: Path, sampler: &mut Sampler) -> Shaded {
        let lobes = Lobes {
            diffuse: pigment * (1.0 / PI),
            specular: None,
            translucent: Vec3::ZERO,
            spots_only: false,
        };
        let direct = self.direct(surface, &lobes, path, sampler);
        Shaded::ungathered(direct) + self.diffused(surface, pigment, path, sampler)
    }

    /// A conductor: its reflection and its highlights, nothing beneath.
    fn metal(
        &self,
        surface: &Surface,
        pigment: Vec3,
        micro: Microfacet,
        path: Path,
        sampler: &mut Sampler,
    ) -> Shaded {
        let lobe = Specular {
            frame: Self::frame(surface),
            micro,
            reflectance: Reflectance::Schlick(pigment),
            share: 1.0,
        };
        let lobes = Lobes {
            diffuse: Vec3::ZERO,
            specular: Some(lobe),
            translucent: Vec3::ZERO,
            spots_only: false,
        };
        let direct = self.direct(surface, &lobes, path, sampler);
        Shaded::ungathered(direct) + self.reflection(surface, &lobe, 1.0, path, sampler)
    }

    /// Car paint: a mirror-smooth clear coat over a metallic base whose flakes
    /// each tilt the normal their own way. A sample follows the coat's
    /// reflection by the coat's reflectance, the base's otherwise.
    fn lacquer(
        &self,
        surface: &Surface,
        pigment: Vec3,
        (roughness, flakes): (f64, f64),
        path: Path,
        sampler: &mut Sampler,
    ) -> Shaded {
        let cos_eye = surface.normal.dot(surface.toward_eye).max(1e-4);
        let coat = schlick_scalar(COAT_F0, cos_eye);
        let flake = cells3(surface.texture * (1.0 / flakes.max(1e-4)), 0x51ab, 1.0).id;
        let tilt = Vec3::new(
            unit(flake) - 0.5,
            unit(mix32(flake)) - 0.5,
            unit(mix32(flake ^ 1)) - 0.5,
        );
        let across = tilt - surface.normal * tilt.dot(surface.normal);
        let tilted = facing_eye(
            (surface.normal + across * 0.5).normalized(),
            surface.toward_eye,
        );
        let base = Specular {
            frame: Frame::around(tilted),
            micro: Microfacet::isotropic(roughness),
            reflectance: Reflectance::Schlick(pigment),
            share: 1.0 - coat,
        };
        let mirror = Specular {
            frame: Frame::around(surface.normal),
            micro: Microfacet::isotropic(0.0),
            reflectance: Reflectance::Schlick(Vec3::splat(COAT_F0)),
            share: coat,
        };
        let lobes = Lobes {
            diffuse: Vec3::ZERO,
            specular: Some(base),
            translucent: Vec3::ZERO,
            spots_only: false,
        };
        let mut light = self.direct(surface, &lobes, path, sampler) * (1.0 - coat);
        let glints = Lobes {
            specular: Some(mirror),
            spots_only: true,
            ..lobes
        };
        light += self.direct(surface, &glints, path, sampler);
        // The coat's image, followed with the chance its own reflectance
        // gives it, counts as the whole of what it adds; the base's, followed
        // otherwise, likewise.
        Shaded::ungathered(light)
            + if sampler.next_1d() < coat {
                self.reflection(surface, &mirror, 1.0 / coat.max(1e-6), path, sampler)
            } else {
                self.reflection(surface, &base, 1.0, path, sampler)
            }
    }

    /// Light every lamp sends `surface` directly, one sample of each: onto
    /// its face, and through it for a thin surface lit from behind. A path
    /// that has scattered takes no highlight.
    fn direct(&self, surface: &Surface, lobes: &Lobes, path: Path, sampler: &mut Sampler) -> Vec3 {
        if path.scattered && lobes.spots_only {
            return Vec3::ZERO;
        }
        let specular = lobes.specular.as_ref().filter(|_| !path.scattered);
        let medium = path.medium;
        let highlight = Microfacet::isotropic(HIGHLIGHT_ROUGHNESS);
        let lit_behind = lobes.translucent.max_element() > 0.0;
        let mut total = Vec3::ZERO;
        let mut suns = 0;
        for light in &self.scene.lights {
            let spot = matches!(light, Light::Spot { .. });
            if lobes.spots_only && !spot {
                continue;
            }
            let Some(incidence) = light.sample(surface.point, sampler.next_2d(), &self.scene.sky)
            else {
                continue;
            };
            if let Light::Sun { .. } = light {
                // The caustics are laid for the first sun, as the scene's.
                let focused = suns == 0 && !path.scattered;
                suns += 1;
                if let Some(medium) = medium {
                    if let Some(light) =
                        self.sun_beneath(surface, lobes, (incidence, medium), focused)
                    {
                        total += light;
                        continue;
                    }
                } else if !path.scattered {
                    total += self.sun_glanced(surface, lobes, incidence, focused);
                }
            }
            let cos_light = surface.normal.dot(incidence.dir);
            let front = surface.facing.dot(incidence.dir) > 0.0 && cos_light > 0.0;
            let reflectance = if front {
                let mut reflectance = lobes.diffuse;
                if let Some(lobe) = specular {
                    reflectance += glossy(lobe, surface, incidence, spot, highlight);
                }
                reflectance
            } else if lit_behind && surface.facing.dot(incidence.dir) < 0.0 {
                lobes.translucent
            } else {
                continue;
            };
            let arriving = reflectance * incidence.light * cos_light.abs();
            if arriving.max_element() <= 1e-9 {
                continue;
            }
            let reach = if incidence.distance.is_finite() {
                incidence.distance * (1.0 - 1e-6)
            } else {
                f64::INFINITY
            };
            let side = if front {
                surface.facing
            } else {
                -surface.facing
            };
            let origin = lift(surface.point, side);
            let mut passed = self.scene.transmittance(
                &Ray::new(origin, incidence.dir),
                reach,
                medium.map(|m| m.absorb),
            ) * incidence.kept;
            if let Some(canopy) = surface.canopy {
                passed = passed * canopy.through(incidence.dir);
            }
            total += arriving * passed;
        }
        total
    }

    /// The light of the sun, sampled as `incidence`, that `lobes` diffuse at
    /// `surface` under water of `medium`: bent through the level surface
    /// above, less what it reflects and what the water absorbs, and focused
    /// by the waves where `focused`; `None` where no water's surface lies
    /// above for it to come through.
    fn sun_beneath(
        &self,
        surface: &Surface,
        lobes: &Lobes,
        (incidence, medium): (crate::light::Incidence, Medium),
        focused: bool,
    ) -> Option<Vec3> {
        let toward = incidence.dir;
        if toward.y <= 0.0 {
            return Some(Vec3::ZERO);
        }
        let up = -refract(-toward, Vec3::UP, medium.ior)?;
        let (water, rise) = self
            .scene
            .water_toward(&Ray::new(surface.point, up), true)?;
        let Some((weight, side)) = diffusely(surface, lobes, up) else {
            return Some(Vec3::ZERO);
        };
        let origin = lift(surface.point, side);
        let crossed = self.scene.transmittance(
            &Ray::new(origin, up),
            rise * (1.0 - 1e-6),
            Some(medium.absorb),
        );
        if crossed.max_element() <= 0.0 {
            return Some(Vec3::ZERO);
        }
        let met = surface.point + up * rise;
        let above = self.sunlight_at(met, toward);
        // The beam narrows or widens as the surface bends it.
        let passed = (1.0 - fresnel(toward.y, medium.ior)) * toward.y / up.y.max(1e-6);
        let focus = if focused {
            let footprint = Self::resolution(surface);
            self.scene
                .caustics
                .beneath(water, surface.point, met.y - surface.point.y, footprint)
        } else {
            1.0
        };
        Some(weight * incidence.light * (passed * focus) * crossed * above)
    }

    /// The light of the sun, sampled as `incidence`, that `lobes` diffuse at
    /// `surface` off water below it: as much as a level surface reflects,
    /// focused by the waves where `focused`.
    fn sun_glanced(
        &self,
        surface: &Surface,
        lobes: &Lobes,
        incidence: crate::light::Incidence,
        focused: bool,
    ) -> Vec3 {
        let toward = incidence.dir;
        if self.scene.waters.is_empty() || toward.y <= 0.0 {
            return Vec3::ZERO;
        }
        let down = Vec3::new(toward.x, -toward.y, toward.z);
        let Some((weight, side)) = diffusely(surface, lobes, down) else {
            return Vec3::ZERO;
        };
        let ray = Ray::new(lift(surface.point, side), down);
        let Some((water, reach)) = self.scene.water_toward(&ray, false) else {
            return Vec3::ZERO;
        };
        let Some(ior) = self.scene.water(water).map(|water| water.ior) else {
            return Vec3::ZERO;
        };
        let between = self.scene.transmittance(&ray, reach * (1.0 - 1e-6), None);
        if between.max_element() <= 0.0 {
            return Vec3::ZERO;
        }
        let met = ray.at(reach);
        let reflected = fresnel(toward.y, ior);
        let focus = if focused {
            let footprint = Self::resolution(surface);
            self.scene
                .caustics
                .over(water, surface.point, surface.point.y - met.y, footprint)
        } else {
            1.0
        };
        let canopy = surface.canopy.map_or(1.0, |canopy| canopy.through(down));
        weight
            * incidence.light
            * (reflected * focus * canopy)
            * between
            * self.sunlight_at(met, toward)
    }

    /// How much of the sun's light toward `toward` reaches `point` on a water
    /// surface, past what stands in its way and through the air and cloud.
    fn sunlight_at(&self, point: Vec3, toward: Vec3) -> Vec3 {
        let origin = lift(point, Vec3::UP);
        self.scene
            .transmittance(&Ray::new(origin, toward), f64::INFINITY, None)
            * self.scene.sky.transmitted(origin, toward)
    }

    /// What a diffuse lobe of `albedo` reflects of the light reaching
    /// `surface` from all but the lamps: the radiosity records' light where
    /// they hold, in the open air, and a traced ray's elsewhere.
    fn diffused(
        &self,
        surface: &Surface,
        albedo: Vec3,
        path: Path,
        sampler: &mut Sampler,
    ) -> Shaded {
        let recorded = self
            .radiosity
            .filter(|_| path.medium.is_none())
            .and_then(|records| records.light(surface.point, surface.smooth, surface.normal));
        match recorded {
            Some(light) => Shaded::ungathered(
                albedo * light * surface.canopy.map_or(1.0, |canopy| canopy.diffuse()),
            ),
            None => self.ambient(surface, surface.normal, albedo, path, sampler),
        }
    }

    /// What a diffuse lobe of `albedo` about `around` reflects of the light
    /// reaching `surface` from all but the lamps, which `direct` gathers:
    /// one ray drawn by the cosine, followed to the sky or to the surface it
    /// meets, whose own light is found alike.
    fn ambient(
        &self,
        surface: &Surface,
        around: Vec3,
        albedo: Vec3,
        path: Path,
        sampler: &mut Sampler,
    ) -> Shaded {
        let (x, y, z) = cosine_hemisphere(sampler.next_2d());
        let dir = Frame::around(around).to_world(Vec3::new(x, y, z));
        let through = surface.canopy.map_or(1.0, |canopy| canopy.through(dir));
        let carried = path.weight * albedo.max_element() * through;
        if path.depth + 1 >= MAX_DEPTH || carried <= 0.0 {
            return Shaded::ungathered(Vec3::ZERO);
        }
        let much = path.weight >= GATHERED_SHARE;
        let nothing = Shaded {
            light: Vec3::ZERO,
            gathered: much,
        };
        let survives = if much {
            1.0
        } else {
            (carried / ROULETTE).min(1.0)
        };
        if survives < 1.0 && sampler.next_1d() >= survives {
            return nothing;
        }
        let side = if surface.facing.dot(around) >= 0.0 {
            surface.facing
        } else {
            -surface.facing
        };
        // A direction the shading normal allows but the surface itself hides.
        if side.dot(dir) <= 0.0 {
            return nothing;
        }
        let next = Path {
            scattered: true,
            ..path.next(
                carried / survives,
                Arrival::Seen,
                path.medium,
                surface.travelled,
            )
        };
        let arriving = self.radiance(&Ray::new(lift(surface.point, side), dir), next, sampler);
        Shaded {
            light: albedo * arriving.light * (through / survives),
            gathered: much || arriving.gathered,
        }
    }

    /// What a clear coat or film `lobe` over a diffuse base reflects of the
    /// scene: nothing on a path that has scattered, whose light the coat's
    /// few percent would add to for the cost of a ray.
    fn coat(
        &self,
        surface: &Surface,
        lobe: &Specular,
        path: Path,
        sampler: &mut Sampler,
    ) -> Shaded {
        if path.scattered {
            return Shaded::ungathered(Vec3::ZERO);
        }
        self.reflection(surface, lobe, 1.0, path, sampler)
    }

    /// What `lobe` reflects of the scene — its mirror image, or one glossy
    /// sample of it — scaled by `boost`, the reciprocal of the chance it was
    /// chosen when it was drawn among others.
    fn reflection(
        &self,
        surface: &Surface,
        lobe: &Specular,
        boost: f64,
        path: Path,
        sampler: &mut Sampler,
    ) -> Shaded {
        let nothing = Shaded::ungathered(Vec3::ZERO);
        if path.depth + 1 >= MAX_DEPTH {
            return nothing;
        }
        let v = lobe.frame.to_local(surface.toward_eye);
        if v.z <= 0.0 {
            return nothing;
        }
        let (dir, weight, arrival) = if lobe.micro.is_mirror() {
            let dir = (-surface.toward_eye).reflect(lobe.frame.z);
            (dir, lobe.reflectance.at(v.z), Arrival::Seen)
        } else {
            let h = lobe.micro.sample(v, sampler.next_2d());
            let l = (-v).reflect(h);
            if l.z <= 0.0 {
                return nothing;
            }
            let density = lobe.share * lobe.micro.reflection_density(v, h);
            (
                lobe.frame.to_world(l),
                lobe.reflectance.at(v.dot(h).max(0.0)) * lobe.micro.masking(l),
                Arrival::Glossy {
                    from: surface.point,
                    density,
                },
            )
        };
        if surface.facing.dot(dir) <= 0.0 {
            return nothing;
        }
        let weight = weight * boost;
        let carried = path.weight * weight.max_element();
        if carried < CUTOFF {
            return nothing;
        }
        let next = path.next(carried, arrival, path.medium, surface.travelled);
        self.radiance(
            &Ray::new(lift(surface.point, surface.facing), dir),
            next,
            sampler,
        ) * weight
    }

    /// A clear surface: what it reflects and what it lets through, both
    /// followed near the eye and one drawn by its reflectance deeper, with
    /// the glints of spots on it. A dispersive one bends each primary
    /// its own way: its path is followed for one, drawn at random, and counts
    /// three times over in that one alone.
    fn glass(
        &self,
        surface: &Surface,
        outside: bool,
        (ior, dispersion, roughness): (f64, f64, f64),
        medium: Medium,
        path: Path,
        sampler: &mut Sampler,
    ) -> Shaded {
        let roughness = widened(roughness, surface.unresolved);
        let glints = Lobes {
            diffuse: Vec3::ZERO,
            specular: Some(Specular {
                frame: Frame::around(surface.normal),
                micro: Microfacet::isotropic(roughness),
                reflectance: Reflectance::Schlick(Vec3::splat(COAT_F0)),
                share: 1.0,
            }),
            translucent: Vec3::ZERO,
            spots_only: true,
        };
        let glinted = if outside {
            self.direct(surface, &glints, path, sampler)
        } else {
            Vec3::ZERO
        };
        let (channel, split_here) = if dispersion > 0.0 {
            match path.channel {
                Some(channel) => (Some(channel), false),
                None => (Some(pick_channel(sampler.next_1d())), true),
            }
        } else {
            (path.channel, false)
        };
        let ior = channel.map_or(ior, |channel| ior + dispersion * SPREAD[channel]);
        let path = Path { channel, ..path };
        let eta = if outside { ior } else { 1.0 / ior };
        let micro = if roughness < crate::material::MIRROR {
            surface.normal
        } else {
            let frame = Frame::around(surface.normal);
            frame.to_world(
                Microfacet::isotropic(roughness)
                    .sample(frame.to_local(surface.toward_eye), sampler.next_2d()),
            )
        };
        let incident = -surface.toward_eye;
        let refracted = refract(incident, micro, eta).filter(|dir| surface.facing.dot(*dir) < 0.0);
        let reflectance = match refracted {
            Some(_) => fresnel(surface.toward_eye.dot(micro), eta),
            None => 1.0,
        };
        // A facet tilted toward the eye by more than half the view's slant
        // sends its reflection under the surface, where it meets the surface
        // again and goes on up: mirrored back above it rather than lost, so
        // water seen low keeps the brightness its reflectance owes.
        let bounced = incident.reflect(micro);
        let below = surface.facing.dot(bounced);
        let reflected = Some(if below > 0.0 {
            bounced
        } else {
            (bounced - surface.facing * (2.0 * below)).normalized()
        })
        .filter(|dir| surface.facing.dot(*dir) > 0.0);
        // The medium each way leads into: the glass's own on the way in, and
        // air on the way out.
        let inner = if outside { Some(medium) } else { None };
        let reflect = (reflected, surface.facing, path.medium);
        let transmit = (refracted, -surface.facing, inner);
        // The surface's facets spread what lies beyond it: a reflection twice
        // as far as they tilt, and what is seen through it as far as tilting
        // the facet turns the refracted ray.
        let width = roughness * roughness;
        let cos_i = surface.toward_eye.dot(surface.normal).clamp(0.0, 1.0);
        let cos_t = mathf::sqrt((1.0 - (1.0 - cos_i * cos_i) / (eta * eta)).max(1e-6));
        let spread = |more: f64| Path {
            cone: path.cone.widened(more, surface.travelled),
            ..path
        };
        let (glancing, passing) = (
            spread(2.0 * width),
            spread(width * (1.0 - cos_i / (eta * cos_t)).abs()),
        );
        let mut through = if path.depth < SPLIT_DEPTH {
            self.follow(surface, reflect, reflectance, glancing, sampler) * reflectance
                + self.follow(surface, transmit, 1.0 - reflectance, passing, sampler)
                    * (1.0 - reflectance)
        } else if sampler.next_1d() < reflectance {
            self.follow(surface, reflect, 1.0, glancing, sampler)
        } else {
            self.follow(surface, transmit, 1.0, passing, sampler)
        };
        if let (true, Some(channel)) = (split_here, channel) {
            through.light = only(through.light, channel);
        }
        Shaded::ungathered(glinted) + through
    }

    /// A soap bubble's skin: it reflects the colours its film interferes to,
    /// and lets the rest straight through, unbent.
    fn bubble(
        &self,
        surface: &Surface,
        (thickness, index): (f64, f64),
        path: Path,
        sampler: &mut Sampler,
    ) -> Shaded {
        let cos_eye = surface.normal.dot(surface.toward_eye).max(1e-4);
        let film = Reflectance::Film {
            thickness,
            index,
            below: 1.0,
        };
        let reflectance = film.at(cos_eye);
        let glints = Lobes {
            diffuse: Vec3::ZERO,
            specular: Some(Specular {
                frame: Frame::around(surface.normal),
                micro: Microfacet::isotropic(0.0),
                reflectance: film,
                share: 1.0,
            }),
            translucent: Vec3::ZERO,
            spots_only: true,
        };
        let glinted = self.direct(surface, &glints, path, sampler);
        let reflected = Some((-surface.toward_eye).reflect(surface.normal));
        let reflect = (reflected, surface.facing, path.medium);
        let pass = (Some(-surface.toward_eye), -surface.facing, path.medium);
        let share = reflectance.max_element().clamp(0.0, 1.0);
        let through = if path.depth < SPLIT_DEPTH {
            self.follow(surface, reflect, share, path, sampler) * reflectance
                + self.follow(surface, pass, 1.0 - share, path, sampler) * (Vec3::ONE - reflectance)
        } else if sampler.next_1d() < share {
            self.follow(surface, reflect, 1.0, path, sampler) * (reflectance / share.max(1e-6))
        } else {
            self.follow(surface, pass, 1.0, path, sampler)
                * ((Vec3::ONE - reflectance) / (1.0 - share).max(1e-6))
        };
        Shaded::ungathered(glinted) + through
    }

    /// A film laid over the pigment, as oil lies on dark water or colour on
    /// a beetle's back: the film's interference is the coat's reflectance.
    fn iridescent(
        &self,
        surface: &Surface,
        pigment: &Pigment,
        spot: &Spot,
        (thickness, index): (f64, f64),
        path: Path,
        sampler: &mut Sampler,
    ) -> Shaded {
        let colour = pigment.colour(spot);
        let cos_eye = surface.normal.dot(surface.toward_eye).max(1e-4);
        let film = Reflectance::Film {
            thickness,
            index,
            below: 1.5,
        };
        let under = Vec3::ONE - film.at(cos_eye);
        let coat = Specular {
            frame: Frame::around(surface.normal),
            micro: Microfacet::isotropic(0.05),
            reflectance: film,
            share: 1.0,
        };
        let lobes = Lobes {
            diffuse: colour * under * (1.0 / PI),
            specular: Some(coat),
            translucent: Vec3::ZERO,
            spots_only: false,
        };
        let direct = self.direct(surface, &lobes, path, sampler);
        let diffused = self.diffused(surface, colour * under, path, sampler);
        Shaded::ungathered(direct) + diffused + self.coat(surface, &coat, path, sampler)
    }

    /// A thin, waxy surface lit from either side: diffuse on its face, some
    /// of what reaches its back coming through, and a faint sheen.
    fn leaf(
        &self,
        surface: &Surface,
        pigment: Vec3,
        translucency: f64,
        path: Path,
        sampler: &mut Sampler,
    ) -> Shaded {
        let sheen = Specular {
            frame: Frame::around(surface.normal),
            micro: Microfacet::isotropic(0.35),
            reflectance: Reflectance::Schlick(Vec3::splat(COAT_F0)),
            share: 1.0,
        };
        // Light through a leaf crosses its pigment twice, so it deepens.
        let through = pigment * pigment * (translucency * 1.6 / PI);
        let lobes = Lobes {
            diffuse: pigment * ((1.0 - translucency) / PI),
            specular: Some(sheen),
            translucent: through,
            spots_only: false,
        };
        // One ray gathers for either face, drawn by what each passes on.
        let (face, back) = (pigment * (1.0 - translucency), through * PI);
        let facing = face.max_element() / (face.max_element() + back.max_element()).max(1e-9);
        let gathered = if sampler.next_1d() < facing {
            self.ambient(
                surface,
                surface.normal,
                face * (1.0 / facing),
                path,
                sampler,
            )
        } else {
            let behind = back * (1.0 / (1.0 - facing).max(1e-9));
            self.ambient(surface, -surface.normal, behind, path, sampler)
        };
        Shaded::ungathered(self.direct(surface, &lobes, path, sampler)) + gathered
    }

    /// The light found by following `dir` from `surface`, leaving on the
    /// side of `side` into `medium`, as a share `weight` of the path.
    fn follow(
        &self,
        surface: &Surface,
        (dir, side, medium): (Option<Vec3>, Vec3, Option<Medium>),
        weight: f64,
        path: Path,
        sampler: &mut Sampler,
    ) -> Shaded {
        let nothing = Shaded::ungathered(Vec3::ZERO);
        let Some(dir) = dir else {
            return nothing;
        };
        let carried = path.weight * weight;
        if path.depth + 1 >= MAX_DEPTH || carried < CUTOFF {
            return nothing;
        }
        let next = path.next(carried, Arrival::Seen, medium, surface.travelled);
        self.radiance(&Ray::new(lift(surface.point, side), dir), next, sampler)
    }
}

/// How far along a ray the air must reach before the crowns roofing it
/// matter; how far above the ray's start they roof it; and how much of the
/// light they stop reaches the air beneath them anyway.
const ROOFED_AIR: (f64, f64, f64) = (8.0, 22.0, 0.12);

/// The places along a ray its air's roofing is read at.
const AIR_SAMPLES: u32 = 4;

/// What `lobes` diffuse at `surface` of light arriving from `dir`, by its
/// cosine, and the side light from there leaves the surface by: its face, or
/// a thin surface's back. A lobe's highlight of light that reached it through
/// water is the light's own image, which its reflection finds.
fn diffusely(surface: &Surface, lobes: &Lobes, dir: Vec3) -> Option<(Vec3, Vec3)> {
    let cos = surface.normal.dot(dir);
    let facing = surface.facing.dot(dir);
    if facing > 0.0 && cos > 0.0 {
        Some((lobes.diffuse * cos, surface.facing))
    } else if facing < 0.0 && lobes.translucent.max_element() > 0.0 {
        Some((lobes.translucent * cos.abs(), -surface.facing))
    } else {
        None
    }
}

/// What `lobe` reflects of a lamp sampled at `incidence`, weighed against
/// finding that lamp by the lobe's own reflection.
fn glossy(
    lobe: &Specular,
    surface: &Surface,
    incidence: crate::light::Incidence,
    spot: bool,
    highlight: Microfacet,
) -> Vec3 {
    // A mirror shows an area lamp only in its reflected ray; a spot, which
    // no ray can find, only as a highlight.
    if !spot && lobe.micro.is_mirror() {
        return Vec3::ZERO;
    }
    let micro = if spot && lobe.micro.is_mirror() {
        highlight
    } else {
        lobe.micro
    };
    let v = lobe.frame.to_local(surface.toward_eye);
    let l = lobe.frame.to_local(incidence.dir);
    if v.z <= 0.0 || l.z <= 0.0 {
        return Vec3::ZERO;
    }
    let h = (v + l).normalized();
    let spread = micro.density(h);
    let masking = micro.masking(v) * micro.masking(l);
    let specular = lobe.reflectance.at(v.dot(h).max(0.0)) * (spread * masking / (4.0 * v.z * l.z));
    let weight = if spot {
        1.0
    } else {
        power(
            incidence.density,
            lobe.share * micro.reflection_density(v, h),
        )
    };
    specular * weight
}

/// How much foam covers breaking water at `surface`: none below the crests,
/// thickening with height and broken up by noise.
fn foam_cover(foam: &Foam, surface: &Surface) -> f64 {
    let broken = foam.spread * noise3(surface.texture * 1.3, foam.seed);
    smoothstep(
        foam.crest - foam.spread,
        foam.crest + foam.spread,
        surface.point.y + broken,
    )
}

/// How much of a stream's surface the foam its flow carries covers at
/// `surface`, where its grid holds `held` of it: gathered into the bubbles
/// and the streaks a running stream draws it out in, never an even film.
fn carried_foam(held: f64, surface: &Surface) -> f64 {
    if held <= 0.0 {
        return 0.0;
    }
    let bubbles = noise3(surface.texture * 30.0, CARRIED_SEED);
    smoothstep(0.35, 0.95, held + 0.3 * bubbles)
}

/// The key the foam a stream carries is broken up under.
const CARRIED_SEED: u32 = 0x0f0a_3b17;

/// How thick a film is at `surface`, in nanometres: thinner toward the top,
/// as a bubble's drains, and swirled by the currents in it.
fn film_thickness(surface: &Surface, (thin, thick): (f64, f64), seed: u32) -> f64 {
    let swirl = 0.5 + 0.5 * noise3(surface.texture * 3.1, seed);
    let drain = 0.5 + 0.5 * (-surface.normal.y);
    thin + (thick - thin) * (0.55 * swirl + 0.45 * drain).clamp(0.0, 1.0)
}

/// The primary a draw `u` picks, each a third of the time.
fn pick_channel(u: f64) -> usize {
    if u < 1.0 / 3.0 {
        0
    } else if u < 2.0 / 3.0 {
        1
    } else {
        2
    }
}

/// `light` kept in `channel` alone, three times over.
fn only(light: Vec3, channel: usize) -> Vec3 {
    match channel {
        0 => Vec3::new(3.0 * light.x, 0.0, 0.0),
        1 => Vec3::new(0.0, 3.0 * light.y, 0.0),
        _ => Vec3::new(0.0, 0.0, 3.0 * light.z),
    }
}

/// The power heuristic's weight for a sample drawn at density `drawn` that
/// another strategy would have drawn at `other`.
fn power(drawn: f64, other: f64) -> f64 {
    let (a, b) = (drawn * drawn, other * other);
    if a + b > 0.0 {
        a / (a + b)
    } else {
        1.0
    }
}

/// `normal`, turned just far enough to face the eye: a relief tilted past the
/// silhouette would otherwise shade the surface's back.
fn facing_eye(normal: Vec3, toward_eye: Vec3) -> Vec3 {
    const LEAST: f64 = 0.02;
    let cos = normal.dot(toward_eye);
    if cos >= LEAST {
        normal
    } else {
        (normal + toward_eye * (LEAST - cos)).normalized()
    }
}

/// How long a stretch of a surface facing `normal` a view `width` across
/// covers along the view `toward_eye`: drawn out the more the view slants,
/// but no further than `SLANTEST` lets it.
fn along_view(width: f64, normal: Vec3, toward_eye: Vec3) -> f64 {
    width / normal.dot(toward_eye).abs().max(SLANTEST)
}

/// How finely a view `width` across resolves a surface facing `normal` seen
/// along `toward_eye`: drawn out by a slanting view only by the square root of
/// the slant, so it is resolved across the view as well as along it.
pub(crate) fn resolved(width: f64, normal: Vec3, toward_eye: Vec3) -> f64 {
    width / mathf::sqrt(normal.dot(toward_eye).abs().max(SLANTEST))
}

/// `point` moved off its surface along `normal`, by an amount that grows
/// with its distance from the origin so rounding cannot put it back.
pub(crate) fn lift(point: Vec3, normal: Vec3) -> Vec3 {
    let scale = point.x.abs().max(point.y.abs()).max(point.z.abs());
    point + normal * (2e-7 * (1.0 + scale))
}

/// The share of the sun's light a lens spreads close about its image, and the
/// square of that bloom's angular spread; and the fainter veil further out.
const GLARE_BLOOM: f64 = 0.012;
const GLARE_BLOOM_SPREAD2: f64 = 0.012 * 0.012;
const GLARE_VEIL: f64 = 0.004;
const GLARE_VEIL_SPREAD2: f64 = 0.06 * 0.06;

/// Film points a scene's exposure is measured over: enough, spread evenly,
/// that no one part of the picture swings it.
const METER_COLUMNS: u32 = 48;
const METER_ROWS: u32 = 32;
/// Of the measured points, the share at each end set aside, as a divisor: a
/// fiftieth, for a sun's disc, or the black inside a hollow, says nothing of
/// how bright the scene is.
const METER_TRIM: usize = 50;
/// Film points one core measures in a unit of work.
const METER_UNIT: u32 = 8;
/// Rays toward the sun its glare is judged by.
const GLARE_RAYS: u32 = 16;

/// The most of a picture, in hundredths, its brightest part may cover and
/// still blow out; the exposed light the rest is held below, where the
/// filmic curve still shows a white bark's marks and a sky's gradations
/// before its shoulder flattens them; and the most the exposure is pulled
/// down to hold it there — two stops — lest a bright sky blacken the land.
const BLOWN_SHARE: usize = 5;
const NEAR_WHITE: f64 = 2.5;
const MOST_PULL: f64 = 4.0;

/// About how many film points the local adaptation is measured over: some
/// 128 by 72 in a widescreen picture, at the picture's own shape; fewer in a
/// picture of fewer than sixteen pixels a point. Each point takes four
/// samples, two by two within it.
const ADAPT_POINTS: usize = 9216;
const ADAPT_PIXELS: u64 = 16;
const ADAPT_SAMPLES: usize = 4;
/// Adaptation samples one core measures in a unit of work.
const ADAPT_UNIT: usize = 16;

/// The measurement a scene's exposure is set from — its luminance at points
/// spread over the picture — and then the local adaptation it is corrected
/// by, measured once the exposure and the glare are known.
#[derive(Debug)]
pub(crate) struct Meter {
    key: f64,
    logs: Vec<f64>,
    next: u32,
    encoder: Encoder,
    /// The picture the scene is traced for.
    size: (u32, u32),
    /// The film points the adaptation is measured over, across and down.
    points: (usize, usize),
    /// The adaptation's samples, once the exposure is set; and how many of
    /// them are measured.
    samples: Vec<Sample>,
    measured: usize,
}

impl Meter {
    /// A measurement toward `key` for a picture of `size`; `None` when the
    /// heap will not hold it.
    pub(crate) fn new(key: f64, size: (u32, u32)) -> Option<Self> {
        let count = METER_COLUMNS * METER_ROWS;
        Some(Self {
            key,
            logs: fallible::filled(count as usize, 0.0)?,
            next: 0,
            encoder: Encoder::new()?,
            size,
            points: adapt_points(size),
            samples: Vec::new(),
            measured: 0,
        })
    }

    /// The share of the measurement made so far, by the samples traced.
    pub(crate) fn done(&self) -> f64 {
        let adapting = self.points.0 * self.points.1 * ADAPT_SAMPLES;
        share(
            self.next as usize + self.measured,
            self.logs.len() + adapting,
        )
    }

    /// Measure the next unit of points of `scene` across `runner`, setting
    /// its exposure and glare once they are measured, and then its local
    /// adaptation; whether all is measured, or `None` when the heap will not
    /// hold the adaptation.
    pub(crate) fn step(&mut self, scene: &mut Scene, runner: &dyn JobRunner) -> Option<bool> {
        let total = METER_COLUMNS * METER_ROWS;
        if self.next < total {
            self.expose(scene, runner);
            if self.next >= total {
                self.logs.sort_unstable_by(f64::total_cmp);
                scene.exposure = exposure_of(&self.logs, self.key);
                scene.glare = sun_in_view(scene);
                let count = self.points.0 * self.points.1 * ADAPT_SAMPLES;
                let blank = crate::adapt::Sample {
                    across: 0.0,
                    down: 0.0,
                    stops: 0.0,
                };
                self.samples = fallible::filled(count, blank)?;
            }
            return Some(false);
        }
        let width = runner.width().max(1);
        let (first, points, key) = (self.measured, self.points, self.key);
        let end = first
            .saturating_add(ADAPT_UNIT * width)
            .min(self.samples.len());
        let tracer = Tracer::new(scene, &self.encoder, self.size, 0);
        if let Some(slots) = self.samples.get_mut(first..end) {
            band::for_each(runner, slots, (0, ADAPT_UNIT), &|piece, out| {
                let start = first.saturating_add(piece.saturating_mul(ADAPT_UNIT));
                for (index, slot) in (start..).zip(out.iter_mut()) {
                    *slot = tracer.adapting(index, points, key);
                }
            });
        }
        self.measured = end;
        if end < self.samples.len() {
            return Some(false);
        }
        let adaptation = Adaptation::measured(&self.samples, key, points)?;
        scene.adaptation = adaptation.corrects().then_some(adaptation);
        self.samples = Vec::new();
        Some(true)
    }

    /// Measure the next unit of the exposure's points of `scene` across
    /// `runner`.
    fn expose(&mut self, scene: &Scene, runner: &dyn JobRunner) {
        let total = METER_COLUMNS * METER_ROWS;
        let first = self.next;
        let end = first
            .saturating_add(METER_UNIT * u32::try_from(runner.width().max(1)).unwrap_or(1))
            .min(total);
        let tracer = Tracer::new(scene, &self.encoder, (METER_COLUMNS * 8, METER_ROWS * 8), 0);
        let Some(slots) = self.logs.get_mut(first as usize..end as usize) else {
            self.next = total;
            return;
        };
        band::for_each(runner, slots, (0, METER_UNIT as usize), &|piece, out| {
            let start = u32::try_from(piece).map_or(u32::MAX, |piece| {
                first.saturating_add(piece.saturating_mul(METER_UNIT))
            });
            for (offset, slot) in (0u32..).zip(out.iter_mut()) {
                *slot = tracer.metered(start.saturating_add(offset));
            }
        });
        self.next = end;
    }
}

/// The film points a picture of `size` measures its adaptation over, across
/// and down: about `ADAPT_POINTS` at the picture's own shape.
fn adapt_points((width, height): (u32, u32)) -> (usize, usize) {
    let pixels = u64::from(width) * u64::from(height);
    let wanted = usize::try_from(pixels / ADAPT_PIXELS)
        .unwrap_or(ADAPT_POINTS)
        .clamp(1, ADAPT_POINTS);
    let aspect = f64::from(width.max(1)) / f64::from(height.max(1));
    let count = |value: f64| usize::try_from(mathf::round_i32(value).max(1)).unwrap_or(1);
    let across = count(mathf::sqrt(crate::vector::real(wanted) * aspect));
    (
        across,
        count(crate::vector::real(wanted) / crate::vector::real(across)),
    )
}

/// The exposure a scene's metered luminances call for, `logs` their
/// logarithms in order: the one taking their trimmed mean to `key`, pulled
/// down by as much as two stops where it would blow out more of the picture
/// than its brightest sliver — a sky, say, rather than the sun and its
/// glints. A frame much of which lies in deep shade would otherwise be
/// exposed for the shade, and its sky burnt white.
fn exposure_of(logs: &[f64], key: f64) -> f64 {
    let count = logs.len();
    let trim = count / METER_TRIM;
    let kept = logs.get(trim..count.saturating_sub(trim)).unwrap_or(&[]);
    let mean = if kept.is_empty() {
        0.0
    } else {
        kept.iter().sum::<f64>() / crate::vector::real(kept.len())
    };
    let metered = key / mathf::exp(mean).max(1e-9);
    let brightest = logs.get(count.saturating_sub(count * BLOWN_SHARE / 100 + 1));
    let held = brightest.map_or(metered, |&log| NEAR_WHITE / mathf::exp(log).max(1e-9));
    metered.min(held).max(metered / MOST_PULL).clamp(1e-6, 1e6)
}

/// How much of the first sun's light reaches the eye past what stands before
/// it, through the air and the clouds, and the way it comes in: the glare a
/// lens spreads about its image. `None` with no sun's light reaching the eye.
fn sun_in_view(scene: &Scene) -> Option<Glare> {
    let eye = scene.camera.eye();
    let sun = scene.sun()?;
    let (mut irradiance, mut weighed) = (Vec3::ZERO, Vec3::ZERO);
    for incidence in sun.samples(eye, GLARE_RAYS, &scene.sky) {
        let reaching = incidence.light
            * incidence.kept
            * scene
                .transmittance(&Ray::new(eye, incidence.dir), f64::INFINITY, None)
                .max_element();
        irradiance += reaching * (1.0 / f64::from(GLARE_RAYS));
        weighed += incidence.dir * reaching.luminance();
    }
    (irradiance.max_element() > 0.0).then(|| Glare {
        toward: weighed.normalized(),
        irradiance,
    })
}

/// A pixel's samples as they accumulate: their mean, and on the square-root
/// scale the eye reads light on, their spread and variance; and whether any
/// gathered some of its light by a random ray.
struct Tally {
    sum: Vec3,
    roots: Vec3,
    squares: Vec3,
    low: Vec3,
    high: Vec3,
    count: u32,
    gathered: bool,
}

impl Tally {
    const fn new() -> Self {
        Self {
            sum: Vec3::ZERO,
            roots: Vec3::ZERO,
            squares: Vec3::ZERO,
            low: Vec3::splat(f64::INFINITY),
            high: Vec3::ZERO,
            count: 0,
            gathered: false,
        }
    }

    fn add(&mut self, light: Vec3, gathered: bool) {
        let root = roots(light);
        self.sum += light;
        self.roots += root;
        self.squares += root * root;
        self.low = self.low.min(root);
        self.high = self.high.max(root);
        self.count += 1;
        self.gathered |= gathered;
    }

    fn mean(&self) -> Vec3 {
        self.sum / f64::from(self.count.max(1))
    }

    /// Whether the samples agree: after the `first` round, all of them within
    /// a spread, unless some gathered their light by random rays, which can
    /// all have missed the light the next round finds; after later ones,
    /// their mean within a standard error, and what it shows not hanging on
    /// its brightest sample, as it does where a few rare paths bring most of
    /// the light.
    fn settled(&self, first: bool) -> bool {
        if self.gathered && self.count < GATHERED_LEAST {
            return false;
        }
        if first {
            return (self.high - self.low).max_element() <= SETTLED_SPREAD;
        }
        let n = f64::from(self.count);
        let mean = self.roots / n;
        let variance = (self.squares / n - mean * mean) * (n / (n - 1.0));
        let without = (self.sum - self.high * self.high) * (1.0 / (n - 1.0).max(1.0));
        let hanging = roots(self.mean()) - roots(without.max(Vec3::ZERO));
        (variance / n).max_element() <= SETTLED_ERROR * SETTLED_ERROR
            && hanging.max_element() <= SETTLED_ERROR
    }
}

/// Each channel of `light` on the square-root scale the eye reads it on.
fn roots(light: Vec3) -> Vec3 {
    Vec3::new(
        mathf::sqrt(light.x),
        mathf::sqrt(light.y),
        mathf::sqrt(light.z),
    )
}

#[cfg(test)]
#[path = "trace_tests.rs"]
mod tests;
