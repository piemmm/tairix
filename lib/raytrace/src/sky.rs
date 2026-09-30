//! The sky every ray that escapes the scene sees: a gradient from the
//! horizon to the zenith, the glow the sun spreads through the air around
//! itself, a layer of cloud, and at night the stars. The sun's own disc is a
//! light, sampled as one, and not part of the sky.

use alloc::vec::Vec;
use core::f64::consts::PI;
use core::ops::Range;

use tairix_util::{fallible, mathf};

use crate::heightfield::{banded, bilinear};
use crate::noise::{noise2, smoothstep};
use crate::sample::{mix32, unit};
use crate::scene::{Grid, Layout};
use crate::vector::{real, Vec3};

/// A layer of cloud `altitude` up.
///
/// Its cover comes from a grid, filled a band of rows at a time as a height
/// grid is, `span` across and centred over the scene; finer noise at trace
/// time sharpens its edges past the grid's own resolution.
#[derive(Clone, Debug)]
pub(crate) struct Clouds {
    /// Vertices along each side of the cover grid.
    side: usize,
    span: f64,
    cover: Vec<f32>,
    pub(crate) altitude: f64,
    /// The density at which cloud begins, and over how much more it
    /// thickens to full.
    pub(crate) threshold: f64,
    pub(crate) softness: f64,
    /// How opaque a full cloud is, as an optical depth.
    pub(crate) depth: f64,
    /// The light a cloud's shaded underside gives, and the sunlight on it.
    pub(crate) shade: Vec3,
    pub(crate) sunlight: Vec3,
    /// The unit direction toward the sun that lights it.
    pub(crate) toward: Vec3,
    /// The scale of the finer noise that sharpens the cover.
    pub(crate) detail: f64,
    pub(crate) seed: u32,
    /// How much of the sky the layer covers, once its grid is sealed.
    gloom: f64,
}

impl Clouds {
    /// A layer whose cover grid has `cells` cells a side over `span`; `None`
    /// when the heap will not hold it.
    pub(crate) fn new(cells: usize, span: f64, template: CloudLook) -> Option<Self> {
        let side = cells.checked_add(1)?;
        Some(Self {
            side,
            span,
            cover: fallible::filled(side.checked_mul(side)?, 0.0f32)?,
            altitude: template.altitude,
            threshold: template.threshold,
            softness: template.softness,
            depth: template.depth,
            shade: template.shade,
            sunlight: template.sunlight,
            toward: template.toward,
            detail: template.detail,
            seed: template.seed,
            gloom: 0.0,
        })
    }

    /// Settle the layer once every row is filled: how much of the sky it
    /// covers, which dims the light scattered about the scene.
    pub(crate) fn seal(&mut self) {
        let mut total = 0.0;
        let mut count = 0.0;
        let stride = (self.side / 32).max(1);
        for row in (0..self.side).step_by(stride) {
            for column in (0..self.side).step_by(stride) {
                let at = f64::from(
                    self.cover
                        .get(row * self.side + column)
                        .copied()
                        .unwrap_or(-1.0),
                );
                total += smoothstep(self.threshold, self.threshold + self.softness, at);
                count += 1.0;
            }
        }
        self.gloom = if count > 0.0 { total / count } else { 0.0 };
    }

    /// The cloud's density at world `(x, z)`: the grid's, and when `fine`
    /// the finer noise's on top of it.
    fn density(&self, x: f64, z: f64, fine: bool) -> f64 {
        let cells = self.side.saturating_sub(1);
        let step = self.span / real(cells);
        let (u, v) = ((x + 0.5 * self.span) / step, (z + 0.5 * self.span) / step);
        let limit = real(cells) - 1e-9;
        if !(0.0..limit).contains(&u) || !(0.0..limit).contains(&v) {
            return -1.0;
        }
        let (column, row) = (mathf::floor(u), mathf::floor(v));
        let (across, down) = (u - column, v - row);
        let (left, near) = (
            usize::try_from(mathf::round_i32(column)).unwrap_or(0),
            usize::try_from(mathf::round_i32(row)).unwrap_or(0),
        );
        let at = |column: usize, row: usize| {
            f64::from(
                self.cover
                    .get(row * self.side + column)
                    .copied()
                    .unwrap_or(-1.0),
            )
        };
        let corners = [
            at(left, near),
            at(left + 1, near),
            at(left, near + 1),
            at(left + 1, near + 1),
        ];
        let coarse = bilinear(corners, (across, down));
        if !fine {
            return coarse;
        }
        coarse
            + 0.22 * noise2(x / self.detail, z / self.detail, self.seed)
            + 0.1
                * noise2(
                    x / (0.4 * self.detail),
                    z / (0.4 * self.detail),
                    self.seed ^ 5,
                )
    }

    /// How much cloud covers world `(x, z)`, from `0.0` clear to `1.0`.
    fn cover_at(&self, x: f64, z: f64, fine: bool) -> f64 {
        smoothstep(
            self.threshold,
            self.threshold + self.softness,
            self.density(x, z, fine),
        )
    }

    /// The colour of the cloud a ray from `origin` along `dir` meets, and
    /// how much of what is behind it it hides.
    fn seen(&self, origin: Vec3, dir: Vec3, fine: bool) -> Option<(Vec3, f64)> {
        if dir.y < 0.015 || origin.y >= self.altitude {
            return None;
        }
        let t = (self.altitude - origin.y) / dir.y;
        // Far off, the layer thins into the haze toward the horizon.
        let fade = mathf::exp(-t / (0.35 * self.span));
        if fade < 0.01 {
            return None;
        }
        let point = origin + dir * t;
        let cover = self.cover_at(point.x, point.z, fine);
        if cover <= 0.0 {
            return None;
        }
        let opacity = (1.0 - mathf::exp(-cover * self.depth)) * fade;
        // Toward the sun the cloud above shadows the part seen; its edges,
        // lit through thin cloud, shine.
        let reach = 700.0 / self.toward.y.max(0.12);
        let shadowing = self.cover_at(
            point.x + self.toward.x * reach,
            point.z + self.toward.z * reach,
            false,
        );
        let lit = mathf::exp(-shadowing * self.depth * 0.45);
        let forward = scattering(dir.dot(self.toward), 0.6) * 4.0 * PI;
        let direct = self.sunlight * (lit * (0.35 + 0.65 * forward.min(6.0)));
        let colour = self.shade * (1.0 - 0.4 * cover) + direct * (1.0 - 0.5 * cover);
        Some((colour, opacity))
    }

    /// How much of the sunlight toward `toward` reaches `point` through the
    /// layer.
    pub(crate) fn shadow(&self, point: Vec3, toward: Vec3) -> f64 {
        if toward.y < 0.015 || point.y >= self.altitude {
            return 1.0;
        }
        let t = (self.altitude - point.y) / toward.y;
        let spot = point + toward * t;
        mathf::exp(-self.cover_at(spot.x, spot.z, false) * self.depth)
    }
}

impl Grid for Clouds {
    fn rows(&self) -> usize {
        self.side
    }

    fn layout(&self) -> Layout {
        Layout {
            origin: (-0.5 * self.span, -0.5 * self.span),
            step: self.span / real(self.side.saturating_sub(1)),
        }
    }

    fn bands(
        &mut self,
        range: Range<usize>,
        rows: usize,
    ) -> impl Iterator<Item = (usize, &mut [f32])> {
        banded(&mut self.cover, self.side, range, rows)
    }
}

/// What a cloud layer looks like, before its cover is drawn.
#[derive(Copy, Clone, Debug)]
pub(crate) struct CloudLook {
    pub(crate) altitude: f64,
    pub(crate) threshold: f64,
    pub(crate) softness: f64,
    pub(crate) depth: f64,
    pub(crate) shade: Vec3,
    pub(crate) sunlight: Vec3,
    pub(crate) toward: Vec3,
    pub(crate) detail: f64,
    pub(crate) seed: u32,
}

/// The Henyey–Greenstein phase function: how much of the light a cloud
/// scatters goes out at `cos` to the way it came, for asymmetry `g`.
fn scattering(cos: f64, g: f64) -> f64 {
    let base = 1.0 + g * g - 2.0 * g * cos;
    (1.0 - g * g) / (4.0 * PI * base * mathf::sqrt(base.max(1e-9)))
}

/// The haze the sun lights about itself.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Glow {
    /// The unit direction toward the sun.
    pub(crate) toward: Vec3,
    /// The halo's colour at its brightest.
    pub(crate) colour: Vec3,
    /// How much of the halo hugs the horizon beneath the sun, as at sunset.
    pub(crate) horizon: f64,
}

/// A sky.
#[derive(Clone, Debug)]
pub(crate) struct Sky {
    pub(crate) zenith: Vec3,
    pub(crate) horizon: Vec3,
    /// Below the horizon, where no ground reaches.
    pub(crate) ground: Vec3,
    pub(crate) glow: Option<Glow>,
    /// How bright the stars are; `0.0` for none.
    pub(crate) stars: f64,
    pub(crate) clouds: Option<Clouds>,
}

/// The share of the sun's glow the haze before the horizon takes.
const HAZE_GLOW: f64 = 0.4;

/// Star cells along each edge of each face of the cube the sky is mapped
/// onto: a cell holds one star at most.
const STAR_CELLS: f64 = 120.0;
/// Out of 256, how many cells hold a star.
const STAR_SHARE: u32 = 40;
/// A star's angular radius, in radians.
const STAR_RADIUS: f64 = 0.0011;

impl Sky {
    /// What a ray escaping from `origin` along the unit `dir` sees; `fine`
    /// when it is seen directly or reflected, so clouds keep their finest
    /// edges.
    pub(crate) fn radiance(&self, origin: Vec3, dir: Vec3, fine: bool) -> Vec3 {
        let clear = self.clear(dir);
        match self
            .clouds
            .as_ref()
            .and_then(|clouds| clouds.seen(origin, dir, fine))
        {
            Some((cloud, opacity)) => clear.lerp(cloud, opacity),
            None => clear,
        }
    }

    /// How much of the sunlight toward `toward` reaches `point` past the
    /// clouds.
    pub(crate) fn cloud_shadow(&self, point: Vec3, toward: Vec3) -> f64 {
        self.clouds
            .as_ref()
            .map_or(1.0, |clouds| clouds.shadow(point, toward))
    }

    /// The sky without its clouds.
    fn clear(&self, dir: Vec3) -> Vec3 {
        let up = dir.y;
        let mut colour = if up >= 0.0 {
            let low = 1.0 - up;
            let low2 = low * low;
            self.zenith.lerp(self.horizon, low2 * low2)
        } else {
            self.horizon.lerp(self.ground, smoothstep(0.0, 0.15, -up))
        };
        colour += self.glow(dir);
        if self.stars > 0.0 && up > 0.0 {
            colour += Vec3::splat(Self::star(dir) * self.stars * smoothstep(0.0, 0.2, up));
        }
        colour
    }

    /// The light the sun's glow adds toward `dir`.
    fn glow(&self, dir: Vec3) -> Vec3 {
        let Some(glow) = self.glow else {
            return Vec3::ZERO;
        };
        let near = dir.dot(glow.toward).max(0.0);
        let n2 = near * near;
        let n4 = n2 * n2;
        let n8 = n4 * n4;
        let n32 = n8 * n8 * n8 * n8;
        let low = (1.0 - dir.y.abs()).max(0.0);
        let low4 = low * low * low * low;
        glow.colour * (0.35 * n8 + 0.9 * n32 * n32 + glow.horizon * low4 * low4 * n2)
    }

    /// The colour of the haze toward the level `dir`: the horizon's, and a
    /// share of the sun's glow, which the long path to the horizon gathers
    /// in full and the shorter one to a hill does not.
    pub(crate) fn haze(&self, dir: Vec3) -> Vec3 {
        self.horizon + self.glow(dir) * HAZE_GLOW
    }

    /// The mean radiance of the upper half of the sky, roughly: what light
    /// scattered about a scene is taken to average.
    pub(crate) fn ambient(&self) -> Vec3 {
        let clear = self.zenith.lerp(self.horizon, 0.45);
        match &self.clouds {
            Some(clouds) => clear.lerp(clouds.shade, clouds.gloom),
            None => clear,
        }
    }

    /// How bright the star of the cell `dir` falls in is along it.
    ///
    /// The sky is mapped onto a cube, and each face cut into a grid: a
    /// direction and its cell's star lie on the same face in the same cell,
    /// and the star is kept off the cell's walls, so none is ever cut.
    fn star(dir: Vec3) -> f64 {
        let size = Vec3::new(dir.x.abs(), dir.y.abs(), dir.z.abs());
        let (face, major, u, v) = if size.x >= size.y && size.x >= size.z {
            (u32::from(dir.x < 0.0), size.x, dir.y, dir.z)
        } else if size.y >= size.z {
            (2 + u32::from(dir.y < 0.0), size.y, dir.x, dir.z)
        } else {
            (4 + u32::from(dir.z < 0.0), size.z, dir.x, dir.y)
        };
        let grid = |along: f64| (along / major + 1.0) * (0.5 * STAR_CELLS);
        let (column, row) = (mathf::floor(grid(u)), mathf::floor(grid(v)));
        let key = mix32(
            face.wrapping_mul(0x9e37_79b9)
                ^ mix32(
                    mathf::round_i32(column).cast_unsigned()
                        ^ mix32(mathf::round_i32(row).cast_unsigned()),
                ),
        );
        if key & 0xff >= STAR_SHARE {
            return 0.0;
        }
        // Two star radii clear of the walls even at a face's corner, where a
        // cell spans the least angle.
        let jitter = |cell: f64, salt: u32| {
            let at = cell + 0.3 + 0.4 * unit(mix32(key ^ salt));
            at / STAR_CELLS * 2.0 - 1.0
        };
        let (first, second) = (jitter(column, 1), jitter(row, 2));
        let sign = if face % 2 == 0 { 1.0 } else { -1.0 };
        let star = match face / 2 {
            0 => Vec3::new(sign, first, second),
            1 => Vec3::new(first, sign, second),
            _ => Vec3::new(first, second, sign),
        }
        .normalized();
        let falloff = (dir - star).length() / STAR_RADIUS;
        if falloff > 3.0 {
            return 0.0;
        }
        let brightness = unit(mix32(key ^ 3));
        brightness * brightness * brightness * mathf::exp(-falloff * falloff)
    }
}

#[cfg(test)]
#[path = "sky_tests.rs"]
mod tests;
