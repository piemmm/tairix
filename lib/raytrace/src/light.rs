//! The lights a scene is lit by, and how a shading point samples each.
//!
//! A point, a spot and the sun are sampled outright; an orb and a panel are
//! also objects a reflected ray can find by chance, so each answers the
//! density it would have drawn that direction with, and the two ways of
//! finding it are weighed against each other.

use core::f64::consts::TAU;

use tairix_util::mathf;

use crate::noise::smoothstep;
use crate::sample::cone;
use crate::vector::{Frame, Vec3};

/// One light.
#[derive(Clone, Debug)]
pub(crate) enum Light {
    /// A point shining `intensity` every way.
    Point { at: Vec3, intensity: Vec3 },
    /// A point shining `intensity` down the unit `axis`: full strength within
    /// `cos_inner` of it, fading to nothing at `cos_outer`.
    Spot {
        at: Vec3,
        axis: Vec3,
        cos_inner: f64,
        cos_outer: f64,
        intensity: Vec3,
    },
    /// A distant disc of `radiance` toward the unit `toward`, `cos_radius`
    /// across: the sun, or the moon.
    Sun {
        toward: Vec3,
        cos_radius: f64,
        radiance: Vec3,
    },
    /// Scene object `object`, a sphere glowing `radiance`.
    Orb {
        object: u32,
        centre: Vec3,
        radius: f64,
        radiance: Vec3,
    },
    /// Scene object `object`, a rectangle glowing `radiance` from its face
    /// only.
    Panel {
        object: u32,
        corner: Vec3,
        edge_u: Vec3,
        edge_v: Vec3,
        radiance: Vec3,
    },
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
    /// That density per unit solid angle; `0.0` for a point or a spot, which
    /// nothing but sampling can find.
    pub(crate) density: f64,
}

impl Light {
    /// The scene object that is this light's own surface, if it has one.
    pub(crate) const fn object(&self) -> Option<u32> {
        match *self {
            Self::Orb { object, .. } | Self::Panel { object, .. } => Some(object),
            Self::Point { .. } | Self::Spot { .. } | Self::Sun { .. } => None,
        }
    }

    /// One sample of the light reaching `p`, drawn from `pair`; `None` when
    /// none can.
    pub(crate) fn sample(&self, p: Vec3, pair: (f64, f64)) -> Option<Incidence> {
        match *self {
            Self::Point { at, intensity } => {
                let (dir, distance) = toward(p, at)?;
                Some(Incidence {
                    dir,
                    distance,
                    light: intensity / (distance * distance),
                    density: 0.0,
                })
            }
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
                })
            }
            Self::Sun {
                toward,
                cos_radius,
                radiance,
            } => {
                let (x, y, z) = cone(cos_radius, pair);
                let density = disc_density(cos_radius);
                Some(Incidence {
                    dir: Frame::around(toward).to_world(Vec3::new(x, y, z)),
                    distance: f64::INFINITY,
                    light: radiance / density,
                    density,
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
                })
            }
        }
    }

    /// The density [`sample`](Self::sample) draws the unit `dir` from `p`
    /// with, for a ray that met this light `distance` along it.
    pub(crate) fn density(&self, p: Vec3, dir: Vec3, distance: f64) -> f64 {
        match *self {
            Self::Point { .. } | Self::Spot { .. } => 0.0,
            Self::Sun {
                toward, cos_radius, ..
            } => {
                if dir.dot(toward) >= cos_radius {
                    disc_density(cos_radius)
                } else {
                    0.0
                }
            }
            Self::Orb { centre, radius, .. } => {
                orb_cone((centre - p).length(), radius).map_or(0.0, |(_, _, density)| density)
            }
            Self::Panel { edge_u, edge_v, .. } => {
                panel_density(edge_u.cross(edge_v), dir, distance).unwrap_or(0.0)
            }
        }
    }

    /// The radiance a ray arriving along `dir` sees on this light's own
    /// surface: the panel shines from its face alone.
    pub(crate) fn seen(&self, dir: Vec3) -> Vec3 {
        match *self {
            Self::Orb { radiance, .. } | Self::Sun { radiance, .. } => radiance,
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
            Self::Point { .. } | Self::Spot { .. } => Vec3::ZERO,
        }
    }
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
fn disc_density(cos_radius: f64) -> f64 {
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
