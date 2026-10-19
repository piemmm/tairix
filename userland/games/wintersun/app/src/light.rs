//! The low sun, the air, and the sky, and the pass that lights the geometry
//! buffer with them.
//!
//! `WinterSun` is lit by one directional light at a shallow angle: long
//! shadows and a cold-to-warm turn across every slope. Each pixel is lit from
//! its own surface normal, so a slope is lit as it is seen, and veiled by the
//! air between it and the eye — the fog of distance and the mist that pools
//! in hollows — before the sky takes whatever no surface covers.
//!
//! # Set up once a frame, read per pixel
//!
//! Everything the light depends on but the pixel — the sun's direction, how
//! far the fog reaches, the sky's colour up the screen — is folded into
//! tables when a frame begins ([`Atmosphere::new`]). A pixel's light is then
//! a few lookups and a blend, in integers.
//!
//! # Silhouettes are resolved here
//!
//! The terrain is scan-converted a pixel at a time, so its silhouette against
//! the sky or a farther slope is aliased. Every fragment recorded how near
//! its triangle's edge lies; where that edge is within half a pixel and the
//! surface beyond it is a different one — its depth jumps, or there is no
//! surface at all — the pixel takes its neighbour's light for the share of it
//! the neighbour covers.

use alloc::vec::Vec;
use core::f64::consts::FRAC_PI_2;

use tairix_raster::color::{Color, Pixel};
use tairix_raster::mesh::{Depth, EdgeNear, Toward};
use tairix_raster::surface::RowBand;
use tairix_raster::view::Perspective;
use tairix_util::mathf;
use tairix_wintersun_art::palette;
use tairix_wintersun_figure::reference::{SUN_ELEVATION, SUN_TOWARD};
use tairix_wintersun_figure::shadow::Light;
use tairix_wintersun_rules::terrain::MAX_STEP_RISE_SUB_UNITS;
use tairix_wintersun_world::geom::{CELL_SUB_UNITS, ELEVATION_SUB_UNITS};

use crate::error::ClientError;
use crate::mesh::{NORMAL_ONE, WORLD_PER_ELEVATION};
use crate::raster::{GBuffer, Texel};

/// Elevation above which no ground mist collects, in elevation sub-units.
///
/// Mist pools in hollows, so the term is driven by how far *below* this a
/// surface sits. Sixty world units is the valley floor of the realm's own
/// relief rather than a number picked to look right at one seed.
const MIST_CEILING: i32 = 60 * ELEVATION_SUB_UNITS;

/// How far the fog reaches, in world sub-units: the distance over which it
/// closes all but a third of the view. [`crate::terrain::VIEW_RANGE`] is set
/// where it has closed almost all of it.
pub const FOG_DEPTH: f64 = 900.0 * CELL_SUB_UNITS as f64;

/// The gain that leaves a channel exactly as the palette authored it.
///
/// Shading is *relative*: a slope facing neither toward nor away from the sun
/// draws the material's own colour, and the sun brightens or cools it from
/// there. A tint applied as a plain multiply would instead darken every
/// surface in the world by whatever its mid-point happened to be, which is a
/// palette change disguised as lighting.
pub const UNSHADED: u8 = 128;

/// How far a pixel's depth may differ from its neighbour's, as a fraction of
/// the nearer, before the two are different surfaces: well past what a
/// continuous slope changes over one pixel, short of the step a ridge makes
/// against the ground behind it.
const DISCONTINUITY: u32 = 8;

/// A direction and two colours: everything the ground is lit by.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Sun {
    /// The in-plane direction it travels in, east and south.
    toward: (i32, i32),
    /// The colour a slope facing the sun takes.
    pub warm: Color,
    /// The colour a slope facing away takes.
    pub cool: Color,
    /// How far a slope moves the mix either way, out of 255.
    pub relief: u8,
    /// The tint level ground takes, which every gain is measured against.
    neutral: Color,
}

impl Sun {
    /// A sun travelling toward `(dx, dy)` at the stage's elevation.
    ///
    /// A zero direction is a sun directly overhead, which shades nothing
    /// sideways — the honest answer rather than a refusal.
    #[must_use]
    pub fn new(dx: i32, dy: i32, warm: Color, cool: Color, relief: u8) -> Self {
        Self {
            toward: (dx, dy),
            warm,
            cool,
            relief,
            neutral: palette::lerp(cool, warm, UNSHADED),
        }
    }

    /// Daylight from `WinterSun`'s low sun in the north-west — the sun the
    /// art harness measures every figure under — at a neutral colour
    /// temperature, so it lights an ice field and a savanna alike until the
    /// sky gives it the hour and the weather.
    #[must_use]
    pub fn daylight() -> Self {
        let (dx, dy) = SUN_TOWARD;
        Self::new(
            dx,
            dy,
            Color::rgb(255, 240, 216),
            Color::rgb(140, 152, 178),
            200,
        )
    }

    /// The light the figures under this sun are shaded by: its direction, at
    /// the height the art harness measures them under, so a figure and the
    /// slope it stands on are lit from one side. An overhead sun lights them
    /// from overhead.
    ///
    /// # Errors
    ///
    /// [`ClientError::Figure`] never, for a sun this type can hold.
    pub fn light(&self) -> Result<Light, ClientError> {
        let (across, into) = self.toward;
        let light = if across == 0 && into == 0 {
            Light::new(1.0, 0.0, FRAC_PI_2)
        } else {
            Light::new(f64::from(across), f64::from(into), SUN_ELEVATION)
        };
        light.map_err(|_| ClientError::Figure)
    }

    /// The gain a surface at `level` applies to each channel.
    #[must_use]
    pub fn gain(&self, level: u8) -> Color {
        let tint = palette::lerp(self.cool, self.warm, level);
        let about = |lit: u8, neutral: u8| {
            let scaled = u32::from(lit) * u32::from(UNSHADED) / u32::from(neutral).max(1);
            u8::try_from(scaled.min(u32::from(u8::MAX))).unwrap_or(u8::MAX)
        };
        Color::rgb(
            about(tint.r, self.neutral.r),
            about(tint.g, self.neutral.g),
            about(tint.b, self.neutral.b),
        )
    }
}

/// The sky the ground sits under.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Sky {
    /// What ground mist is coloured.
    pub mist: Color,
    /// How opaque the mist becomes in the deepest hollow, out of 255.
    pub mist_depth: u8,
    /// The sky at the horizon, which the fog of distance is coloured too, so
    /// far ground fades into the sky beyond it.
    pub horizon: Color,
    /// The sky straight up.
    pub zenith: Color,
}

impl Sky {
    /// A neutral daylight haze pooling in the valleys, under a pale sky,
    /// until the sky gives it the hour and the weather.
    #[must_use]
    pub const fn daylight() -> Self {
        Self {
            mist: Color::rgb(170, 176, 186),
            mist_depth: 88,
            horizon: Color::rgb(188, 196, 208),
            zenith: Color::rgb(92, 128, 176),
        }
    }
}

/// Steps per axis the normal table is indexed in.
const NORMAL_STEPS: usize = 64;

/// Octaves of reciprocal depth the fog table spans, and the steps of it per
/// octave.
const FOG_OCTAVES: usize = 32;
const FOG_STEPS: usize = 16;

/// The light and the air for one frame, folded into what each pixel reads.
#[derive(Clone, Debug)]
pub struct Atmosphere {
    /// The gain a surface takes from the sun, by where its normal falls in
    /// the normal table: unshaded throughout where the ground takes no sun.
    tints: Vec<Color>,
    /// How far toward the fog a surface is, by its reciprocal depth's octave
    /// and step, out of 255.
    fog: Vec<u8>,
    /// The sky at each render row.
    rows: Vec<Color>,
    sky: Sky,
}

impl Atmosphere {
    /// Fold `sun` and `sky`, seen through `camera` onto a render target
    /// `height` rows tall, into a frame's tables; an unlit frame draws every
    /// surface in its materials' own colours.
    ///
    /// # Errors
    ///
    /// [`ClientError::OutOfMemory`] where the tables do not fit.
    #[allow(
        clippy::float_arithmetic,
        reason = "the sun's direction, the fog's falloff and the sky's colour \
                  up the screen are trigonometry and exponentials, folded once \
                  a frame into the tables every pixel reads in integers"
    )]
    pub fn new(
        sun: &Sun,
        sky: Sky,
        camera: &Perspective,
        height: u32,
        lit: bool,
    ) -> Result<Self, ClientError> {
        let (dx, dy) = sun.toward;
        let length = mathf::hypot(f64::from(dx), f64::from(dy));
        let (east, south) = if length > 0.0 {
            (f64::from(dx) / length, f64::from(dy) / length)
        } else {
            (0.0, 0.0)
        };
        let elevation = if length > 0.0 {
            SUN_ELEVATION
        } else {
            FRAC_PI_2
        };
        let (flat, high) = (mathf::cos(elevation), mathf::sin(elevation));
        // Toward the sun: against the way it travels, and up.
        let toward = [-east * flat, -south * flat, high];
        // The slope at which the shading saturates is the steepest a body may
        // step up, the line the terrain itself draws between slope and cliff.
        let steepest = mathf::atan(
            f64::from(MAX_STEP_RISE_SUB_UNITS * WORLD_PER_ELEVATION) / f64::from(CELL_SUB_UNITS),
        );
        let full = mathf::sin(elevation + steepest) - high;
        let swing = f64::from(sun.relief) * 0.5;

        let mut tints = Vec::new();
        tints
            .try_reserve(NORMAL_STEPS * NORMAL_STEPS)
            .map_err(|_| ClientError::OutOfMemory)?;
        for iy in 0..NORMAL_STEPS {
            for ix in 0..NORMAL_STEPS {
                let centre =
                    |step: usize| (mathf::real(step * 4) + 2.0 - 128.0) / f64::from(NORMAL_ONE);
                let (mut nx, mut ny) = (centre(ix), centre(iy));
                let rim = nx * nx + ny * ny;
                if rim > 1.0 {
                    let shrink = 1.0 / mathf::sqrt(rim);
                    (nx, ny) = (nx * shrink, ny * shrink);
                }
                let nz = mathf::sqrt(mathf::fmax(0.0, 1.0 - nx * nx - ny * ny));
                let facing = mathf::fmax(0.0, nx * toward[0] + ny * toward[1] + nz * toward[2]);
                let lean = if full > 0.0 {
                    mathf::clamp((facing - high) / full, -1.0, 1.0)
                } else {
                    0.0
                };
                let level = mathf::byte((f64::from(UNSHADED) + lean * swing) / 255.0);
                tints.push(if lit {
                    sun.gain(level)
                } else {
                    Color::rgb(UNSHADED, UNSHADED, UNSHADED)
                });
            }
        }

        let mut fog = Vec::new();
        fog.try_reserve(FOG_OCTAVES * FOG_STEPS + 1)
            .map_err(|_| ClientError::OutOfMemory)?;
        for slot in 0..=FOG_OCTAVES * FOG_STEPS {
            let octave = mathf::real(slot / FOG_STEPS);
            let step = mathf::real(slot % FOG_STEPS) / mathf::real(FOG_STEPS);
            let reciprocal = mathf::power(2.0, octave) * (1.0 + step);
            // A reciprocal depth's own reciprocal is the distance it stands at.
            let distance = camera.depth(reciprocal);
            fog.push(mathf::byte(1.0 - mathf::exp(-distance / FOG_DEPTH)));
        }

        let mut rows = Vec::new();
        rows.try_reserve(height as usize)
            .map_err(|_| ClientError::OutOfMemory)?;
        let (middle, _) = camera.centre();
        for row in 0..height {
            let ray = camera.ray(middle, f64::from(row) + 0.5);
            let up = mathf::sqrt(mathf::fmax(0.0, ray[2]));
            rows.push(palette::lerp(sky.horizon, sky.zenith, mathf::byte(up)));
        }

        Ok(Self {
            tints,
            fog,
            rows,
            sky,
        })
    }

    /// Light `band`'s rows of the frame from `buffer`.
    pub fn composite(&self, buffer: &GBuffer, band: &mut RowBand<'_>) {
        let width = buffer.width();
        for row in band.rows() {
            let (Some((_, pixels)), Some(here)) =
                (band.row_span_mut(row, 0, width), buffer.row(row))
            else {
                continue;
            };
            let sky = self.sky_at(row);
            let columns = pixels
                .iter_mut()
                .zip(here.depth)
                .zip(here.albedo.iter().zip(here.texels))
                .zip(here.edges);
            for (x, (((pixel, &depth), (&albedo, &texel)), edge)) in (0u32..).zip(columns) {
                let own = if depth == Depth::CLEAR {
                    sky
                } else {
                    self.surface(depth, albedo, texel)
                };
                *pixel = match edge.get() {
                    Some(near) if near.0 < EdgeNear::STEPS / 2 => {
                        self.resolve(buffer, (x, row), depth, own, near)
                    }
                    _ => own,
                };
            }
        }
    }

    /// The light at pixel `(x, y)`, `depth` deep and lit `own`, whose
    /// triangle's edge lies `distance` toward `toward`: its silhouette
    /// resolved against the surface beyond the edge.
    fn resolve(
        &self,
        buffer: &GBuffer,
        (x, y): (u32, u32),
        depth: Depth,
        own: Pixel,
        (distance, toward): (u8, Toward),
    ) -> Pixel {
        let half = EdgeNear::STEPS / 2;
        let (nx, ny) = match toward {
            Toward::Left => (x.checked_sub(1), Some(y)),
            Toward::Right => (Some(x + 1), Some(y)),
            Toward::Up => (Some(x), y.checked_sub(1)),
            Toward::Down => (Some(x), Some(y + 1)),
        };
        let Some((beyond, far_albedo, far_texel, _)) =
            nx.zip(ny).and_then(|(nx, ny)| buffer.at(nx, ny))
        else {
            return own;
        };
        if !apart(depth, beyond) {
            return own;
        }
        let other = self.shade(beyond, far_albedo, far_texel, ny.unwrap_or(y));
        // The neighbour covers what lies past the edge: half a pixel less the
        // edge's distance from this one's centre.
        let share = u32::from(half - distance) * 255 / u32::from(EdgeNear::STEPS);
        mix(own, other, u8::try_from(share).unwrap_or(0))
    }

    /// A surface's light: its albedo under the sun, veiled by mist and fog;
    /// or the sky at row `y` where no surface was drawn.
    fn shade(&self, depth: Depth, albedo: Pixel, texel: Texel, y: u32) -> Pixel {
        if depth == Depth::CLEAR {
            self.sky_at(y)
        } else {
            self.surface(depth, albedo, texel)
        }
    }

    /// The sky at render row `y`.
    fn sky_at(&self, y: u32) -> Pixel {
        opaque(
            self.rows
                .get(y as usize)
                .copied()
                .unwrap_or(self.sky.horizon),
        )
    }

    /// A drawn surface's light: its albedo under the sun, veiled by mist and
    /// fog.
    fn surface(&self, depth: Depth, albedo: Pixel, texel: Texel) -> Pixel {
        let gain = self
            .tints
            .get(normal_index(texel.normal))
            .copied()
            .unwrap_or(Color::rgb(UNSHADED, UNSHADED, UNSHADED));
        let modulate = |channel: u8, gain: u8| {
            let scaled = u32::from(channel) * u32::from(gain) / u32::from(UNSHADED);
            u8::try_from(scaled.min(u32::from(u8::MAX))).unwrap_or(u8::MAX)
        };
        let lit = Color::rgb(
            modulate(albedo.r, gain.r),
            modulate(albedo.g, gain.g),
            modulate(albedo.b, gain.b),
        );
        let misted = palette::lerp(lit, self.sky.mist, self.mist(texel.elevation));
        opaque(palette::lerp(misted, self.sky.horizon, self.fog(depth)))
    }

    /// How far toward the mist a surface at `elevation` is mixed, out of
    /// 255.
    #[must_use]
    pub fn mist(&self, elevation: i16) -> u8 {
        let below = (MIST_CEILING - i32::from(elevation)).clamp(0, MIST_CEILING);
        let pooled = i64::from(below) * i64::from(self.sky.mist_depth) / i64::from(MIST_CEILING);
        u8::try_from(pooled).unwrap_or(u8::MAX)
    }

    /// How far toward the fog a surface at `depth` is mixed, out of 255.
    #[must_use]
    pub fn fog(&self, depth: Depth) -> u8 {
        let q = depth.0;
        if q == 0 {
            return u8::MAX;
        }
        // The octave and a fraction of it in eight bits, read between the
        // table's two nearest steps.
        let octave = q.ilog2();
        let fraction = if octave >= 12 {
            (q >> (octave - 12)) & 0xFFF
        } else {
            (q << (12 - octave)) & 0xFFF
        };
        let slot = (octave as usize) * FOG_STEPS + (fraction >> 8) as usize;
        let (near, far) = (
            self.fog.get(slot).copied().unwrap_or(0),
            self.fog.get(slot + 1).copied().unwrap_or(0),
        );
        let between = i32::from(far) - i32::from(near);
        let value = i32::from(near) + between * i32::try_from(fraction & 0xFF).unwrap_or(0) / 256;
        u8::try_from(value.clamp(0, 255)).unwrap_or(0)
    }

    /// What a figure standing on ground at `elevation`, `depth` from the eye,
    /// is veiled by: the mist and the fog it stands in, composed.
    #[must_use]
    pub fn veil(&self, depth: Depth, elevation: i16) -> u8 {
        let (mist, fog) = (u32::from(self.mist(elevation)), u32::from(self.fog(depth)));
        let clear = (255 - mist) * (255 - fog) / 255;
        u8::try_from(255 - clear).unwrap_or(u8::MAX)
    }

    /// The tone a veil mixes toward.
    #[must_use]
    pub const fn veil_tone(&self) -> Color {
        self.sky.horizon
    }
}

/// Whether two depths belong to different surfaces: one is the sky, or they
/// differ by more than [`DISCONTINUITY`]'s share of the nearer.
fn apart(a: Depth, b: Depth) -> bool {
    if a == Depth::CLEAR || b == Depth::CLEAR {
        return a != b;
    }
    let (near, far) = (a.0.max(b.0), a.0.min(b.0));
    near - far > near / DISCONTINUITY
}

/// Where `normal` falls in the normal table.
fn normal_index(normal: [i8; 2]) -> usize {
    let step = |component: i8| {
        usize::try_from((i32::from(component) + 128) >> 2)
            .unwrap_or(0)
            .min(NORMAL_STEPS - 1)
    };
    step(normal[1]) * NORMAL_STEPS + step(normal[0])
}

/// `color` as an opaque pixel.
fn opaque(color: Color) -> Pixel {
    Pixel {
        r: color.r,
        g: color.g,
        b: color.b,
        a: u8::MAX,
    }
}

/// `a` moved `t` out of 255 of the way to `b`, both opaque.
fn mix(a: Pixel, b: Pixel, t: u8) -> Pixel {
    let channel = |from: u8, to: u8| {
        let moved = i32::from(from) + (i32::from(to) - i32::from(from)) * i32::from(t) / 255;
        u8::try_from(moved.clamp(0, 255)).unwrap_or(from)
    };
    Pixel {
        r: channel(a.r, b.r),
        g: channel(a.g, b.g),
        b: channel(a.b, b.b),
        a: u8::MAX,
    }
}

#[cfg(test)]
#[path = "light_tests.rs"]
mod tests;
