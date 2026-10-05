//! The lights a scene is lit by, and how a shading point samples each.
//!
//! A spot and the sun are sampled outright; an orb and a panel are
//! also objects a reflected ray can find by chance, so each answers the
//! density it would have drawn that direction with, and the two ways of
//! finding it are weighed against each other. The sun's disc is drawn where
//! it truly stands and seen where the air bends its light to, its density
//! taken over the solid angle the light arrives in.

use core::f64::consts::TAU;

use tairix_util::mathf;

use crate::noise::smoothstep;
use crate::sample::{cone, mix32, unit};
use crate::sky::Sky;
use crate::vector::{Frame, Vec3};

/// One light.
#[derive(Clone, Debug)]
pub(crate) enum Light {
    /// A point shining `intensity` down the unit `axis`: full strength within
    /// `cos_inner` of it, fading to nothing at `cos_outer`.
    Spot {
        at: Vec3,
        axis: Vec3,
        cos_inner: f64,
        cos_outer: f64,
        intensity: Vec3,
    },
    /// A distant disc whose true direction is the unit `toward`, `cos_radius`
    /// across, of mean `radiance` spread over it as `limb` has it: the sun,
    /// or the moon.
    Sun {
        toward: Vec3,
        cos_radius: f64,
        radiance: Vec3,
        limb: Limb,
    },
    /// A sphere glowing `radiance`, a scene object of its own.
    Orb {
        centre: Vec3,
        radius: f64,
        radiance: Vec3,
    },
    /// A rectangle glowing `radiance` from its face only, a scene object of
    /// its own.
    Panel {
        corner: Vec3,
        edge_u: Vec3,
        edge_v: Vec3,
        radiance: Vec3,
    },
}

/// How a disc's brightness falls across it: a disc with no darkening is
/// evenly bright.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Limb {
    /// As `μ^α` of the cosine `μ` of the angle the line of sight meets its
    /// surface at, each channel with its own `α`: the sun.
    Darkening(Vec3),
    /// A moon's, lit from the unit `sun`, the way toward the sun from it: each
    /// channel `sunlit` times the Lommel–Seeliger law, `μ₀/(μ₀ + μ)`, and
    /// `earthlit` more all over for the Earth's light, as shares of the
    /// disc's mean.
    Phase {
        sun: Vec3,
        sunlit: Vec3,
        earthlit: Vec3,
    },
}

impl Limb {
    /// The radiance of each channel of a disc toward the unit `toward`,
    /// `radius` the sine of its angular radius, where that channel's light
    /// truly comes from `sources`, as a share of the disc's mean.
    pub(crate) fn profile(self, (toward, radius): (Vec3, f64), sources: [Vec3; 3]) -> Vec3 {
        match self {
            Self::Darkening(power) => {
                let channel = |alpha: f64, source: Vec3| {
                    let fraction = (sine(source.dot(toward)) / radius).min(1.0);
                    let cosine = mathf::sqrt((1.0 - fraction * fraction).max(0.0));
                    // `μ^α` over its mean across the disc, `2/(α + 2)`.
                    if cosine > 0.0 {
                        mathf::exp(alpha * mathf::ln(cosine)) * (0.5 * alpha + 1.0)
                    } else {
                        0.0
                    }
                };
                Vec3::new(
                    channel(power.x, sources[0]),
                    channel(power.y, sources[1]),
                    channel(power.z, sources[2]),
                )
            }
            Self::Phase {
                sun,
                sunlit,
                earthlit,
            } => {
                let frame = Frame::around(toward);
                let channel = |index: usize| {
                    let source = sources[index];
                    let place = (source.dot(frame.x) / radius, source.dot(frame.y) / radius);
                    sunlit.along(index) * phase_lit(place, (frame, toward), sun)
                        + earthlit.along(index)
                };
                Vec3::new(channel(0), channel(1), channel(2))
            }
        }
    }
}

/// How brightly the sun lights the moon's face seen at `(u, v)` across its
/// disc, in its radii along `frame`'s x and y, the moon toward `toward` and
/// the sun toward the unit `sun`: as the Lommel–Seeliger law scatters it off
/// regolith, `μ₀/(μ₀ + μ)`, none beyond its terminator or its edge.
pub(crate) fn phase_lit((u, v): (f64, f64), (frame, toward): (Frame, Vec3), sun: Vec3) -> f64 {
    let outward = 1.0 - u * u - v * v;
    if outward < 0.0 {
        return 0.0;
    }
    let seen = mathf::sqrt(outward);
    let normal = frame.x * u + frame.y * v - toward * seen;
    let lit = normal.dot(sun);
    if lit > 0.0 {
        lit / (lit + seen)
    } else {
        0.0
    }
}

/// Light arriving at a shading point from one sample of one light.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Incidence {
    /// The unit direction toward the light.
    pub(crate) dir: Vec3,
    /// How far the light is along it; infinite for the sun.
    pub(crate) distance: f64,
    /// The light arriving, already divided by the density it was drawn with.
    pub(crate) light: Vec3,
    /// That density per unit solid angle; `0.0` for a spot, which nothing
    /// but sampling can find.
    pub(crate) density: f64,
    /// What of its light the air and the clouds let through: all of it but
    /// the sun's.
    pub(crate) kept: Vec3,
}

impl Light {
    /// The light a disc sends square to its way above the air: its mean
    /// radiance over its solid angle; nothing for a light with no disc.
    pub(crate) fn irradiance(&self) -> Vec3 {
        match *self {
            Self::Sun {
                cos_radius,
                radiance,
                ..
            } => radiance / disc_density(cos_radius),
            Self::Spot { .. } | Self::Orb { .. } | Self::Panel { .. } => Vec3::ZERO,
        }
    }

    /// `count` samples of the light reaching `p` under `sky`, spread evenly
    /// over it.
    pub(crate) fn samples<'a>(
        &'a self,
        p: Vec3,
        count: u32,
        sky: &'a Sky,
    ) -> impl Iterator<Item = Incidence> + 'a {
        (0..count).filter_map(move |index| {
            let pair = (
                (f64::from(index) + 0.5) / f64::from(count),
                unit(mix32(index ^ 0x9e37)),
            );
            self.sample(p, pair, sky)
        })
    }

    /// One sample of the light reaching `p` under `sky`, drawn from `pair`;
    /// `None` when none can.
    pub(crate) fn sample(&self, p: Vec3, pair: (f64, f64), sky: &Sky) -> Option<Incidence> {
        match *self {
            Self::Spot {
                at,
                axis,
                cos_inner,
                cos_outer,
                intensity,
            } => {
                let (dir, distance) = toward(p, at)?;
                let spread = smoothstep(cos_outer, cos_inner, -dir.dot(axis));
                if spread <= 0.0 {
                    return None;
                }
                Some(Incidence {
                    dir,
                    distance,
                    light: intensity * (spread * spread / (distance * distance)),
                    density: 0.0,
                    kept: Vec3::ONE,
                })
            }
            Self::Sun {
                toward, cos_radius, ..
            } => {
                let (x, y, z) = cone(cos_radius, pair);
                let from = Frame::around(toward).to_world(Vec3::new(x, y, z));
                let arriving = sky.arriving(p, from)?;
                let density = disc_density(cos_radius) / arriving.stretch;
                Some(Incidence {
                    dir: arriving.dir,
                    distance: f64::INFINITY,
                    light: self.shine(sky.channels(&arriving, from)) / density,
                    density,
                    kept: arriving.kept * sky.clouded(p, arriving.dir),
                })
            }
            Self::Orb {
                centre,
                radius,
                radiance,
                ..
            } => {
                let (axis, distance) = toward(p, centre)?;
                let (sin2, cos_max, density) = orb_cone(distance, radius)?;
                let (x, y, z) = cone(cos_max, pair);
                let dir = Frame::around(axis).to_world(Vec3::new(x, y, z));
                // The near side of the sphere along `dir`; the tangent length
                // where rounding leaves the ray a hair outside it.
                let along = dir.dot(centre - p);
                let chord = radius * radius - (distance * distance - along * along);
                let reach = if chord > 0.0 {
                    along - mathf::sqrt(chord)
                } else {
                    distance * mathf::sqrt(1.0 - sin2)
                };
                Some(Incidence {
                    dir,
                    distance: reach,
                    light: radiance / density,
                    density,
                    kept: Vec3::ONE,
                })
            }
            Self::Panel {
                corner,
                edge_u,
                edge_v,
                radiance,
                ..
            } => {
                let point = corner + edge_u * pair.0 + edge_v * pair.1;
                let (dir, distance) = toward(p, point)?;
                let density = panel_density(edge_u.cross(edge_v), dir, distance)?;
                Some(Incidence {
                    dir,
                    distance,
                    light: radiance / density,
                    density,
                    kept: Vec3::ONE,
                })
            }
        }
    }

    /// The density [`sample`](Self::sample) draws the unit `dir` from `p`
    /// under `sky` with, for a ray that met this light `distance` along it.
    pub(crate) fn density(&self, p: Vec3, dir: Vec3, distance: f64, sky: &Sky) -> f64 {
        match *self {
            Self::Spot { .. } => 0.0,
            Self::Sun {
                toward, cos_radius, ..
            } => sky
                .leaving(p, dir)
                .filter(|&(from, _)| from.dot(toward) >= cos_radius)
                .map_or(0.0, |(_, stretch)| disc_density(cos_radius) / stretch),
            Self::Orb { centre, radius, .. } => {
                orb_cone((centre - p).length(), radius).map_or(0.0, |(_, _, density)| density)
            }
            Self::Panel { edge_u, edge_v, .. } => {
                panel_density(edge_u.cross(edge_v), dir, distance).unwrap_or(0.0)
            }
        }
    }

    /// The radiance a ray from `origin` along the unit `dir` meets on this
    /// light's disc, each channel's where its own bent ray truly points, as
    /// the air and the clouds let it through; nothing for a light with no
    /// disc.
    pub(crate) fn disc(&self, origin: Vec3, dir: Vec3, sky: &Sky) -> Vec3 {
        let Self::Sun {
            toward, cos_radius, ..
        } = *self
        else {
            return Vec3::ZERO;
        };
        if dir.dot(toward) < sky.reach(cos_radius) {
            return Vec3::ZERO;
        }
        sky.beyond(origin, dir)
            .map_or(Vec3::ZERO, |(sources, kept)| self.shine(sources) * kept)
    }

    /// Whether a ray along the unit `dir` meets this light's disc, which hides
    /// whatever lies beyond it.
    pub(crate) fn covers(&self, dir: Vec3, sky: &Sky) -> bool {
        match *self {
            Self::Sun {
                toward, cos_radius, ..
            } => dir.dot(toward) >= sky.reach(cos_radius),
            Self::Spot { .. } | Self::Orb { .. } | Self::Panel { .. } => false,
        }
    }

    /// The radiance this disc shows of light whose channels truly come from
    /// `sources`: as its limb has it, and none past its edge.
    fn shine(&self, sources: [Vec3; 3]) -> Vec3 {
        let Self::Sun {
            toward,
            cos_radius,
            radiance,
            limb,
        } = *self
        else {
            return Vec3::ZERO;
        };
        let radius = sine(cos_radius);
        let cosines = sources.map(|source| source.dot(toward));
        let profile = limb.profile((toward, radius), sources);
        let shown = |channel: usize| {
            if cosines[channel] >= cos_radius {
                radiance.along(channel) * profile.along(channel)
            } else {
                0.0
            }
        };
        Vec3::new(shown(0), shown(1), shown(2))
    }

    /// The radiance a ray arriving along `dir` sees on this light's own
    /// surface: the panel shines from its face alone.
    pub(crate) fn seen(&self, dir: Vec3) -> Vec3 {
        match *self {
            Self::Orb { radiance, .. } => radiance,
            Self::Panel {
                edge_u,
                edge_v,
                radiance,
                ..
            } => {
                if dir.dot(edge_u.cross(edge_v)) < 0.0 {
                    radiance
                } else {
                    Vec3::ZERO
                }
            }
            Self::Spot { .. } | Self::Sun { .. } => Vec3::ZERO,
        }
    }
}

/// The sine of the angle whose cosine is `cos`, kept precise for a small
/// angle.
fn sine(cos: f64) -> f64 {
    mathf::sqrt(((1.0 - cos) * (1.0 + cos)).max(0.0))
}

/// The unit direction and distance from `from` to `to`; `None` when they
/// meet.
fn toward(from: Vec3, to: Vec3) -> Option<(Vec3, f64)> {
    let offset = to - from;
    let distance = offset.length();
    (distance > 1e-9).then(|| (offset / distance, distance))
}

/// How much of the sky a sphere of `radius` fills seen from `distance` away,
/// as the squared sine and the cosine of its half-angle, and the uniform
/// density over that cone; `None` from inside it.
fn orb_cone(distance: f64, radius: f64) -> Option<(f64, f64, f64)> {
    if distance <= radius {
        return None;
    }
    let sin2 = (radius * radius) / (distance * distance);
    let cos_max = mathf::sqrt((1.0 - sin2).max(0.0));
    Some((sin2, cos_max, cone_density(sin2 / (1.0 + cos_max))))
}

/// The uniform density over the disc whose angular radius has cosine
/// `cos_radius`.
pub(crate) fn disc_density(cos_radius: f64) -> f64 {
    cone_density(1.0 - cos_radius)
}

/// The density per unit solid angle of a point drawn evenly over a panel
/// facing `face` (its edges' cross product), seen along `dir` from
/// `distance` away; `None` from behind it or edge on.
fn panel_density(face: Vec3, dir: Vec3, distance: f64) -> Option<f64> {
    let area = face.length();
    let cos_light = -dir.dot(face) / area;
    (cos_light > 1e-6).then(|| distance * distance / (area * cos_light))
}

/// The uniform density over a cone whose `1 - cos` is `gap`, taken from the
/// gap itself so a small, distant light keeps its precision.
fn cone_density(gap: f64) -> f64 {
    1.0 / (TAU * gap.max(1e-12))
}

#[cfg(test)]
#[path = "light_tests.rs"]
mod tests;
