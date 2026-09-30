//! The integrator: what light arrives along a ray, and what a pixel shows.
//!
//! Distributed ray tracing (Cook, Porter and Carpenter, 1984): every sample of
//! a pixel draws its own point in the pixel, on the lens, on each lamp and
//! through each glossy reflection, so soft shadows, blurred reflections and
//! depth of field all come of averaging. A surface gathers light from every
//! lamp directly, from the sky through a ray toward it, and from what its
//! reflection or refraction shows through a ray followed on. A glossy
//! reflection that finds a lamp is weighed against sampling that lamp by the
//! power heuristic (Veach and Guibas, 1995), so neither way's noise wins.
//!
//! A pixel is sampled in rounds of 8, 16, 32 and 64 — each a whole
//! stratification of every pair — and stops after any round whose samples
//! agree: flat sky settles at once, while an edge, a penumbra or a glass
//! interior takes what it needs. The rounds and the tolerance were chosen
//! against 256-sample references: fewer than eight samples can agree by
//! chance on a pixel that has not settled.

use core::f64::consts::PI;

use tairix_raster::Pixel;
use tairix_util::mathf;

use crate::light::Light;
use crate::material::{
    fresnel, refract, schlick, schlick_scalar, thin_film, Finish, Foam, Material, Microfacet,
    COAT_F0, SPREAD,
};
use crate::noise::{cells3, noise3, smoothstep};
use crate::pigment::{Pigment, Spot};
use crate::sample::{cosine_hemisphere, disc, mix32, tent, unit, Sampler};
use crate::scene::{Object, Scene, Sight};
use crate::shape::Hit;
use crate::tone::{display, Encoder};
use crate::vector::{Frame, Ray, Vec3};

/// The deepest a path is followed.
const MAX_DEPTH: u32 = 9;
/// To this depth glass is followed both ways, reflected and refracted; past
/// it, one way, drawn by the reflectance.
const SPLIT_DEPTH: u32 = 2;
/// A ray whose light can add less than this to its pixel is not followed.
const CUTOFF: f64 = 0.004;
/// To this depth a surface looks for the sky with a ray of its own; deeper,
/// it takes the scene's scattered light as its sky.
const AMBIENT_DEPTH: u32 = 1;
/// The roughness a point or spot lamp's highlight is drawn at however smooth
/// the surface: a real lamp has a size, and so a highlight.
const HIGHLIGHT_ROUGHNESS: f64 = 0.14;
/// How far the pixel filter reaches, in pixels.
const FILTER_RADIUS: f64 = 1.0;
/// After the first round, how far apart its samples may lie and still be
/// taken as settled, on the square-root scale the eye reads light on.
const SETTLED_SPREAD: f64 = 0.024;
/// After later rounds, the standard error of the mean they may leave, on
/// that scale: a little over three of the display's 255 levels.
const SETTLED_ERROR: f64 = 0.012;
/// How far a ray still in a medium when it leaves the scene is taken to have
/// travelled through it.
const FATHOMLESS: f64 = 1e3;
/// The colour breaking water whitens to.
const FOAM: Vec3 = Vec3::new(0.86, 0.9, 0.92);

/// How hard a tracer works at a pixel: the most samples it may take.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Quality {
    /// At most 8 samples.
    Draft,
    /// At most 16.
    Fair,
    /// At most 32.
    Good,
    /// At most 64.
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
            Self::Good => &[8, 16, 32],
            Self::Fine => &[8, 16, 32, 64],
        }
    }

    /// The most samples a pixel takes at this quality.
    #[must_use]
    pub const fn most(self) -> u32 {
        match self {
            Self::Draft => 8,
            Self::Fair => 16,
            Self::Good => 32,
            Self::Fine => 64,
        }
    }
}

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
    /// footprint a pattern is averaged over.
    travelled: f64,
    /// The one primary a dispersive surface split the path's light down to,
    /// once one has.
    channel: Option<usize>,
}

impl Path {
    const EYE: Self = Self {
        depth: 0,
        weight: 1.0,
        arrival: Arrival::Seen,
        medium: None,
        travelled: 0.0,
        channel: None,
    };

    /// The path one bounce on, carrying `weight` of its light.
    fn next(self, weight: f64, arrival: Arrival, medium: Option<Medium>, travelled: f64) -> Self {
        Self {
            depth: self.depth + 1,
            weight,
            arrival,
            medium,
            travelled,
            channel: self.channel,
        }
    }
}

/// A shading point.
struct Surface {
    point: Vec3,
    /// The geometric normal on the side the ray came from.
    facing: Vec3,
    /// The shading normal, tilted by relief, on that side.
    normal: Vec3,
    toward_eye: Vec3,
    travelled: f64,
    /// The point in the object's texture frame, and that frame's first axis
    /// in the world, which a brushed surface is brushed along.
    texture: Vec3,
    grain: Vec3,
}

/// How a specular lobe reflects: Schlick's approximation from a reflectance,
/// or the interference of a thin film.
#[derive(Copy, Clone, Debug)]
enum Reflectance {
    Schlick(Vec3),
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
    /// Whether only point and spot lamps are gathered: glass and bubbles take
    /// the rest through their own rays.
    points_only: bool,
}

/// A scene's tracer for a picture of `size`, each pixel's samples hashed
/// under the scene's `key`.
pub struct Tracer<'a> {
    scene: &'a Scene,
    encoder: &'a Encoder,
    size: (u32, u32),
    pixel_angle: f64,
    key: u32,
}

impl<'a> Tracer<'a> {
    /// A tracer of `scene` for a picture of `size`, toned through `encoder`,
    /// its samples hashed under `key`.
    #[must_use]
    pub fn new(scene: &'a Scene, encoder: &'a Encoder, size: (u32, u32), key: u32) -> Self {
        Self {
            scene,
            encoder,
            size,
            pixel_angle: scene.camera.pixel_angle(size.1),
            key,
        }
    }

    /// The pixel at `at` traced at `quality`, and how many samples it took.
    #[must_use]
    pub fn pixel(&self, at: (u32, u32), quality: Quality) -> (Pixel, u32) {
        let (light, taken) = self.light(at, quality);
        (self.encoder.pixel(light, at), taken)
    }

    /// What pixel `at` shows as display-linear light, and how many samples it
    /// took.
    fn light(&self, (x, y): (u32, u32), quality: Quality) -> (Vec3, u32) {
        let (width, height) = (f64::from(self.size.0.max(1)), f64::from(self.size.1.max(1)));
        let seed = mix32(self.key ^ mix32(y.wrapping_mul(self.size.0).wrapping_add(x)));
        let mut tally = Tally::new();
        let mut taken = 0;
        for &round in quality.rounds() {
            while taken < round {
                let mut sampler = Sampler::new(seed, taken);
                let (u, v) = sampler.next_2d();
                let film = (
                    2.0 * (f64::from(x) + 0.5 + tent(u) * FILTER_RADIUS) / width - 1.0,
                    1.0 - 2.0 * (f64::from(y) + 0.5 + tent(v) * FILTER_RADIUS) / height,
                );
                let lens = disc(sampler.next_2d());
                let ray = self.scene.camera.ray(film, lens);
                let light = self.radiance(&ray, Path::EYE, &mut sampler);
                tally.add(display(light * self.scene.exposure));
                taken += 1;
            }
            if tally.settled() {
                break;
            }
        }
        (tally.mean(), taken)
    }

    /// The light arriving back along `ray`.
    fn radiance(&self, ray: &Ray, path: Path, sampler: &mut Sampler) -> Vec3 {
        let sight = if path.depth == 0 {
            Sight::Eye
        } else {
            Sight::Bounce
        };
        let Some((index, hit)) = self.scene.closest(ray, f64::INFINITY, sight) else {
            // A medium with no far side is as good as fathomless.
            if let Some(medium) = path.medium {
                let kept = (medium.absorb * -FATHOMLESS).exp();
                return self.escaped(ray, path.arrival) * kept + medium.glow * (Vec3::ONE - kept);
            }
            // Below the horizon with nothing met, the ray has passed over the
            // land or the sea beyond where either is traced, into haze that
            // takes it whole.
            if ray.dir.y < 0.0 && self.scene.fog.is_some() {
                return self
                    .scene
                    .sky
                    .haze(Vec3::new(ray.dir.x, 0.0, ray.dir.z).normalized());
            }
            return self.escaped(ray, path.arrival);
        };
        let Some((object, material)) = self.scene.objects.get(index).and_then(|object| {
            self.scene
                .materials
                .get(object.material)
                .map(|material| (object, material))
        }) else {
            return Vec3::ZERO;
        };
        let light = match material.finish {
            Finish::Glow { radiance } => self.lamp(object, radiance, ray, hit.t, path.arrival),
            _ => self.shade(ray, hit, object, material, path, sampler),
        };
        self.attenuate(light, ray, hit.t, path.medium)
    }

    /// `light` after crossing `t` of the ray's medium: absorbed and lit
    /// within glass or water, veiled by haze in the open air.
    fn attenuate(&self, light: Vec3, ray: &Ray, t: f64, medium: Option<Medium>) -> Vec3 {
        if let Some(medium) = medium {
            let kept = (medium.absorb * -t).exp();
            return light * kept + medium.glow * (Vec3::ONE - kept);
        }
        let Some(fog) = self.scene.fog else {
            return light;
        };
        let kept = mathf::exp(-fog.density * t);
        // The haze takes the colour of the horizon beneath the ray, so
        // toward a low sun it glows.
        let haze = self
            .scene
            .sky
            .haze(Vec3::new(ray.dir.x, 0.0, ray.dir.z).normalized());
        light * kept + haze * (1.0 - kept)
    }

    /// What a ray arriving `t` along `ray` sees on a lamp's own surface.
    fn lamp(&self, object: &Object, radiance: Vec3, ray: &Ray, t: f64, arrival: Arrival) -> Vec3 {
        let Some(light) = object.light.and_then(|index| self.scene.lights.get(index)) else {
            return radiance;
        };
        let seen = light.seen(ray.dir);
        match arrival {
            Arrival::Seen => seen,
            Arrival::Glossy { from, density } => {
                seen * power(density, light.density(from, ray.dir, t))
            }
        }
    }

    /// What a ray leaving the scene sees: the sky and its clouds, and any sun
    /// whose disc it points into.
    fn escaped(&self, ray: &Ray, arrival: Arrival) -> Vec3 {
        let fine = matches!(arrival, Arrival::Seen);
        let mut light = self.scene.sky.radiance(ray.origin, ray.dir, fine);
        for sun in &self.scene.lights {
            let Light::Sun {
                toward,
                cos_radius,
                radiance,
            } = *sun
            else {
                continue;
            };
            if ray.dir.dot(toward) < cos_radius {
                continue;
            }
            let veiled = radiance * self.scene.sky.cloud_shadow(ray.origin, toward);
            light += match arrival {
                Arrival::Seen => veiled,
                Arrival::Glossy { from, density } => {
                    veiled * power(density, sun.density(from, ray.dir, f64::INFINITY))
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
    ) -> Vec3 {
        let (surface, outside) = Self::surface(ray, &hit, object, material, path.travelled);
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
                if let Some(foam) = foam {
                    if sampler.next_1d() < foam_cover(&foam, &surface) {
                        return self.coated(
                            &surface,
                            FOAM,
                            Microfacet::isotropic(0.9),
                            path,
                            sampler,
                        );
                    }
                }
                let medium = Medium { absorb, glow };
                self.glass(
                    &surface,
                    outside,
                    (ior, dispersion, roughness),
                    medium,
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
            Finish::Glow { radiance } => radiance,
        }
    }

    /// Where `ray` met `object` at `hit`, having come `travelled` before it:
    /// the shading point on the side it came from, its normal tilted by the
    /// material's relief, and whether it came from outside.
    fn surface(
        ray: &Ray,
        hit: &Hit,
        object: &Object,
        material: &Material,
        travelled: f64,
    ) -> (Surface, bool) {
        let point = ray.at(hit.t);
        let toward_eye = -ray.dir;
        let outside = hit.normal.dot(toward_eye) >= 0.0;
        let texture = object.texture.point_to_local(point);
        let tilted = material
            .relief
            .as_ref()
            .map_or(hit.shading, |relief| relief.tilt(hit.shading, texture));
        let side = if outside { 1.0 } else { -1.0 };
        let surface = Surface {
            point,
            facing: hit.normal * side,
            normal: facing_eye(tilted * side, toward_eye),
            toward_eye,
            travelled: travelled + hit.t,
            texture,
            grain: object.texture.frame.x,
        };
        (surface, outside)
    }

    /// Where `object`'s pigment is looked up for `surface`: in the object's
    /// own texture frame, normal and all, so a pattern turns with it.
    fn spot(&self, surface: &Surface, object: &Object, hit: &Hit) -> Spot {
        Spot {
            p: surface.texture,
            normal: object.texture.frame.to_local(surface.normal),
            height: surface.point.y,
            width: self.footprint(surface),
            mark: hit.mark,
            along: hit.along,
        }
    }

    /// How wide a patch of `surface` one pixel's view of it covers.
    fn footprint(&self, surface: &Surface) -> f64 {
        let slant = surface.normal.dot(surface.toward_eye).abs().max(0.08);
        surface.travelled * self.pixel_angle / slant
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
    ) -> Vec3 {
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
            points_only: false,
        };
        self.direct(surface, &lobes, path.medium, sampler)
            + pigment * under * self.ambient(surface, surface.normal, path, sampler)
            + self.reflection(surface, &coat, 1.0, path, sampler)
    }

    /// A matte pigment: the lamps and the sky on it, and no reflection.
    fn matte(&self, surface: &Surface, pigment: Vec3, path: Path, sampler: &mut Sampler) -> Vec3 {
        let lobes = Lobes {
            diffuse: pigment * (1.0 / PI),
            specular: None,
            translucent: Vec3::ZERO,
            points_only: false,
        };
        self.direct(surface, &lobes, path.medium, sampler)
            + pigment * self.ambient(surface, surface.normal, path, sampler)
    }

    /// A conductor: its reflection and its highlights, nothing beneath.
    fn metal(
        &self,
        surface: &Surface,
        pigment: Vec3,
        micro: Microfacet,
        path: Path,
        sampler: &mut Sampler,
    ) -> Vec3 {
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
            points_only: false,
        };
        self.direct(surface, &lobes, path.medium, sampler)
            + self.reflection(surface, &lobe, 1.0, path, sampler)
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
    ) -> Vec3 {
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
            points_only: false,
        };
        let mut light = self.direct(surface, &lobes, path.medium, sampler) * (1.0 - coat);
        let glints = Lobes {
            specular: Some(mirror),
            points_only: true,
            ..lobes
        };
        light += self.direct(surface, &glints, path.medium, sampler);
        // The coat's image, followed with the chance its own reflectance
        // gives it, counts as the whole of what it adds; the base's, followed
        // otherwise, likewise.
        light
            + if sampler.next_1d() < coat {
                self.reflection(surface, &mirror, 1.0 / coat.max(1e-6), path, sampler)
            } else {
                self.reflection(surface, &base, 1.0, path, sampler)
            }
    }

    /// Light every lamp sends `surface` directly, one sample of each: onto
    /// its face, and through it for a thin surface lit from behind.
    fn direct(
        &self,
        surface: &Surface,
        lobes: &Lobes,
        medium: Option<Medium>,
        sampler: &mut Sampler,
    ) -> Vec3 {
        let highlight = Microfacet::isotropic(HIGHLIGHT_ROUGHNESS);
        let lit_behind = lobes.translucent.max_element() > 0.0;
        let mut total = Vec3::ZERO;
        for light in &self.scene.lights {
            let point = light.object().is_none() && !matches!(light, Light::Sun { .. });
            if lobes.points_only && !point {
                continue;
            }
            let Some(incidence) = light.sample(surface.point, sampler.next_2d()) else {
                continue;
            };
            let cos_light = surface.normal.dot(incidence.dir);
            let front = surface.facing.dot(incidence.dir) > 0.0 && cos_light > 0.0;
            let reflectance = if front {
                let mut reflectance = lobes.diffuse;
                if let Some(lobe) = &lobes.specular {
                    reflectance += glossy(lobe, surface, incidence, point, highlight);
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
            );
            if matches!(light, Light::Sun { .. }) {
                passed = passed * self.scene.sky.cloud_shadow(origin, incidence.dir);
            }
            total += arriving * passed;
        }
        total
    }

    /// The sky's light reaching `surface` about `around`, through one ray
    /// drawn by the cosine; where the scene hides the sky, the light
    /// scattered about it.
    fn ambient(&self, surface: &Surface, around: Vec3, path: Path, sampler: &mut Sampler) -> Vec3 {
        let bounce = self.scene.bounce;
        if path.depth > AMBIENT_DEPTH {
            return (self.scene.sky.ambient() + bounce) * 0.5;
        }
        let (x, y, z) = cosine_hemisphere(sampler.next_2d());
        let dir = Frame::around(around).to_world(Vec3::new(x, y, z));
        let side = if surface.facing.dot(around) >= 0.0 {
            surface.facing
        } else {
            -surface.facing
        };
        if side.dot(dir) <= 0.0 {
            return bounce;
        }
        let origin = lift(surface.point, side);
        let open = self.scene.transmittance(
            &Ray::new(origin, dir),
            f64::INFINITY,
            path.medium.map(|m| m.absorb),
        );
        self.scene.sky.radiance(origin, dir, false) * open + bounce * (Vec3::ONE - open)
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
    ) -> Vec3 {
        if path.depth + 1 >= MAX_DEPTH {
            return Vec3::ZERO;
        }
        let v = lobe.frame.to_local(surface.toward_eye);
        if v.z <= 0.0 {
            return Vec3::ZERO;
        }
        let (dir, weight, arrival) = if lobe.micro.is_mirror() {
            let dir = (-surface.toward_eye).reflect(lobe.frame.z);
            (dir, lobe.reflectance.at(v.z), Arrival::Seen)
        } else {
            let h = lobe.micro.sample(v, sampler.next_2d());
            let l = (-v).reflect(h);
            if l.z <= 0.0 {
                return Vec3::ZERO;
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
            return Vec3::ZERO;
        }
        let weight = weight * boost;
        let carried = path.weight * weight.max_element();
        if carried < CUTOFF {
            return Vec3::ZERO;
        }
        let next = path.next(carried, arrival, path.medium, surface.travelled);
        weight
            * self.radiance(
                &Ray::new(lift(surface.point, surface.facing), dir),
                next,
                sampler,
            )
    }

    /// A clear surface: what it reflects and what it lets through, both
    /// followed near the eye and one drawn by its reflectance deeper, with
    /// the glints of point lamps on it. A dispersive one bends each primary
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
    ) -> Vec3 {
        let glints = Lobes {
            diffuse: Vec3::ZERO,
            specular: Some(Specular {
                frame: Frame::around(surface.normal),
                micro: Microfacet::isotropic(roughness),
                reflectance: Reflectance::Schlick(Vec3::splat(COAT_F0)),
                share: 1.0,
            }),
            translucent: Vec3::ZERO,
            points_only: true,
        };
        let mut light = if outside {
            self.direct(surface, &glints, path.medium, sampler)
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
        let reflected = Some(incident.reflect(micro)).filter(|dir| surface.facing.dot(*dir) > 0.0);
        // The medium each way leads into: the glass's own on the way in, and
        // air on the way out.
        let inner = if outside { Some(medium) } else { None };
        let reflect = (reflected, surface.facing, path.medium);
        let transmit = (refracted, -surface.facing, inner);
        let mut through = if path.depth < SPLIT_DEPTH {
            self.follow(surface, reflect, reflectance, path, sampler) * reflectance
                + self.follow(surface, transmit, 1.0 - reflectance, path, sampler)
                    * (1.0 - reflectance)
        } else if sampler.next_1d() < reflectance {
            self.follow(surface, reflect, 1.0, path, sampler)
        } else {
            self.follow(surface, transmit, 1.0, path, sampler)
        };
        if let (true, Some(channel)) = (split_here, channel) {
            through = only(through, channel);
        }
        light += through;
        light
    }

    /// A soap bubble's skin: it reflects the colours its film interferes to,
    /// and lets the rest straight through, unbent.
    fn bubble(
        &self,
        surface: &Surface,
        (thickness, index): (f64, f64),
        path: Path,
        sampler: &mut Sampler,
    ) -> Vec3 {
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
            points_only: true,
        };
        let mut light = self.direct(surface, &glints, path.medium, sampler);
        let reflected = Some((-surface.toward_eye).reflect(surface.normal));
        let reflect = (reflected, surface.facing, path.medium);
        let pass = (Some(-surface.toward_eye), -surface.facing, path.medium);
        let share = reflectance.max_element().clamp(0.0, 1.0);
        light += if path.depth < SPLIT_DEPTH {
            self.follow(surface, reflect, share, path, sampler) * reflectance
                + self.follow(surface, pass, 1.0 - share, path, sampler) * (Vec3::ONE - reflectance)
        } else if sampler.next_1d() < share {
            self.follow(surface, reflect, 1.0, path, sampler) * (reflectance / share.max(1e-6))
        } else {
            self.follow(surface, pass, 1.0, path, sampler)
                * ((Vec3::ONE - reflectance) / (1.0 - share).max(1e-6))
        };
        light
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
    ) -> Vec3 {
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
            points_only: false,
        };
        self.direct(surface, &lobes, path.medium, sampler)
            + colour * under * self.ambient(surface, surface.normal, path, sampler)
            + self.reflection(surface, &coat, 1.0, path, sampler)
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
    ) -> Vec3 {
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
            points_only: false,
        };
        let sky_behind = (self.scene.sky.ambient() + self.scene.bounce) * 0.5;
        self.direct(surface, &lobes, path.medium, sampler)
            + pigment * (1.0 - translucency) * self.ambient(surface, surface.normal, path, sampler)
            + through * PI * sky_behind * 0.5
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
    ) -> Vec3 {
        let Some(dir) = dir else {
            return Vec3::ZERO;
        };
        let carried = path.weight * weight;
        if path.depth + 1 >= MAX_DEPTH || carried < CUTOFF {
            return Vec3::ZERO;
        }
        let next = path.next(carried, Arrival::Seen, medium, surface.travelled);
        self.radiance(&Ray::new(lift(surface.point, side), dir), next, sampler)
    }
}

/// What `lobe` reflects of a lamp sampled at `incidence`, weighed against
/// finding that lamp by the lobe's own reflection.
fn glossy(
    lobe: &Specular,
    surface: &Surface,
    incidence: crate::light::Incidence,
    point: bool,
    highlight: Microfacet,
) -> Vec3 {
    // A mirror shows an area lamp only in its reflected ray; a point lamp,
    // which no ray can find, only as a highlight.
    if !point && lobe.micro.is_mirror() {
        return Vec3::ZERO;
    }
    let micro = if point && lobe.micro.is_mirror() {
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
    let weight = if point {
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

/// `point` moved off its surface along `normal`, by an amount that grows
/// with its distance from the origin so rounding cannot put it back.
fn lift(point: Vec3, normal: Vec3) -> Vec3 {
    let scale = point.x.abs().max(point.y.abs()).max(point.z.abs());
    point + normal * (2e-7 * (1.0 + scale))
}

/// A pixel's samples as they accumulate: their mean, and on the square-root
/// scale the eye reads light on, their spread and variance.
struct Tally {
    sum: Vec3,
    roots: Vec3,
    squares: Vec3,
    low: Vec3,
    high: Vec3,
    count: u32,
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
        }
    }

    fn add(&mut self, light: Vec3) {
        let root = Vec3::new(
            mathf::sqrt(light.x),
            mathf::sqrt(light.y),
            mathf::sqrt(light.z),
        );
        self.sum += light;
        self.roots += root;
        self.squares += root * root;
        self.low = self.low.min(root);
        self.high = self.high.max(root);
        self.count += 1;
    }

    fn mean(&self) -> Vec3 {
        self.sum / f64::from(self.count.max(1))
    }

    fn settled(&self) -> bool {
        if self.count <= Quality::Fine.rounds()[0] {
            return (self.high - self.low).max_element() <= SETTLED_SPREAD;
        }
        let n = f64::from(self.count);
        let mean = self.roots / n;
        let variance = (self.squares / n - mean * mean) * (n / (n - 1.0));
        (variance / n).max_element() <= SETTLED_ERROR * SETTLED_ERROR
    }
}

#[cfg(test)]
#[path = "trace_tests.rs"]
mod tests;
