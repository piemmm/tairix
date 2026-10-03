//! The sky every ray that escapes the scene sees: the atmosphere and the
//! stars beyond it, or about a studio the walls of its room; and the cloud.
//! The sun's own disc is a light, sampled as one, and not part of the sky.

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use crate::atmosphere::{Arriving, Atmosphere, Bend, Lit};
use crate::cloud::{sunlight_levels, Cloudbank, Lighting, SUNLIGHT_LEVELS};
use crate::noise::smoothstep;
use crate::stars::Starfield;
use crate::vector::Vec3;

/// A gradient from the horizon to the zenith: the walls of the room a
/// studio's pieces stand in.
#[derive(Clone, Debug)]
pub(crate) struct Gradient {
    pub(crate) zenith: Vec3,
    pub(crate) horizon: Vec3,
    /// Below the horizon, where no ground reaches.
    pub(crate) ground: Vec3,
}

impl Gradient {
    /// The walls' light toward `dir`.
    fn at(&self, dir: Vec3) -> Vec3 {
        let up = dir.y;
        if up >= 0.0 {
            let low = 1.0 - up;
            let low2 = low * low;
            self.zenith.lerp(self.horizon, low2 * low2)
        } else {
            self.horizon.lerp(self.ground, smoothstep(0.0, 0.15, -up))
        }
    }
}

/// What lights the sky itself.
#[allow(
    clippy::large_enum_variant,
    reason = "one sky a scene, and a box could not fail gracefully"
)]
#[derive(Clone, Debug)]
pub(crate) enum Dome {
    Gradient(Gradient),
    /// The physical atmosphere, lit by the scene's sun.
    Air(Atmosphere),
}

/// A sky.
#[derive(Clone, Debug)]
pub(crate) struct Sky {
    pub(crate) dome: Dome,
    /// The stars beyond the air of a sky out of doors.
    pub(crate) stars: Option<Starfield>,
    /// The cloud a ray marches through: the low and middle decks, and above
    /// them a high deck of cirrus.
    pub(crate) low: Option<Cloudbank>,
    pub(crate) high: Option<Cloudbank>,
}

/// How a ray escaping the scene sees the sky.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Seeing {
    /// Whether it is seen directly or reflected, which takes the clouds'
    /// finest edges.
    pub(crate) fine: bool,
    /// The angle the ray's footprint spans, which a star is spread over;
    /// `None` for a ray scattered off a diffuse surface, which takes the
    /// stars' mean.
    pub(crate) spread: Option<f64>,
    /// In `0.0..1.0`: how a march through cloud staggers where along its
    /// steps it reads the cloud, so no two samples of a pixel band alike.
    pub(crate) jitter: f64,
}

impl Sky {
    /// Build the next unit of the sky across `runner` — the atmosphere's
    /// tables, then the high bank's and the low bank's, each aimed along the
    /// sunlight the air bends to it — and once all are built, light the
    /// banks by the air; whether the sky is done, or `None` when the heap
    /// will not hold it.
    pub(crate) fn build(&mut self, runner: &dyn JobRunner) -> Option<bool> {
        if let Dome::Air(atmosphere) = &mut self.dome {
            if !atmosphere.step(runner) {
                return Some(false);
            }
        }
        if let Dome::Air(atmosphere) = &self.dome {
            for bank in [self.high.as_mut(), self.low.as_mut()]
                .into_iter()
                .flatten()
            {
                let (floor, ceiling) = bank.span();
                let height = f64::midpoint(floor, ceiling);
                let toward = atmosphere.air().sun;
                bank.aim(
                    atmosphere
                        .arriving(height, toward)
                        .map_or(toward, |arriving| arriving.dir),
                );
            }
        }
        if let Some(high) = self.high.as_mut() {
            if !high.step(runner, None)? {
                return Some(false);
            }
        }
        if let Some(low) = self.low.as_mut() {
            if !low.step(runner, self.high.as_ref())? {
                return Some(false);
            }
        }
        if let Dome::Air(atmosphere) = &self.dome {
            let air = atmosphere.air();
            // The sunlight on the level ground, which it gives back to the
            // clouds' undersides: arriving as the air bends and squashes it.
            let ground = atmosphere
                .arriving(0.0, air.sun)
                .map_or(Vec3::ZERO, |arriving| {
                    arriving.kept * air.solar * (arriving.dir.y.max(0.0) * arriving.stretch)
                });
            let below =
                air.albedo * (ground * (1.0 / core::f64::consts::PI) + atmosphere.ambient());
            for bank in [self.high.as_mut(), self.low.as_mut()]
                .into_iter()
                .flatten()
            {
                let levels = sunlight_levels(bank.span())
                    .map(|height| atmosphere.sunlight(height, air.sun) * air.solar);
                bank.light_by(Lighting {
                    sunlight: fallible::collected(SUNLIGHT_LEVELS, levels)?,
                    above: atmosphere.ambient(),
                    below,
                });
            }
        }
        Some(true)
    }

    /// What a ray escaping from `origin` along the unit `dir` sees.
    ///
    /// Before each bank's cloud lies the air between it and the ray's origin,
    /// which dims the cloud and adds its own glow; behind the last, whatever
    /// of the clear sky's light the cloud lets through.
    pub(crate) fn radiance(&self, origin: Vec3, dir: Vec3, seeing: Seeing) -> Vec3 {
        let met = |bank: Option<&Cloudbank>| {
            bank.and_then(|bank| bank.seen(origin, dir, seeing.fine, seeing.jitter))
        };
        let (mut near, mut far) = (met(self.low.as_ref()), met(self.high.as_ref()));
        if let (Some(low), Some(high)) = (near, far) {
            if high.2 < low.2 {
                (near, far) = (far, near);
            }
        }
        let (mut light, mut through, mut before) = (Vec3::ZERO, 1.0, Vec3::ZERO);
        for (cloud, kept, depth) in [near, far].into_iter().flatten() {
            let (scattered, crossed) = match &self.dome {
                Dome::Air(atmosphere) => {
                    let between = atmosphere.between(dir, depth);
                    (between.light(), between.kept)
                }
                Dome::Gradient(_) => (Vec3::ZERO, Vec3::ONE),
            };
            light += ((scattered - before).max(Vec3::ZERO) + cloud * crossed) * through;
            through *= kept;
            before = scattered;
        }
        if through <= 0.0 {
            return light;
        }
        let clear = self.clear(origin, dir, seeing.spread);
        light + (clear - before).max(Vec3::ZERO) * through
    }

    /// How much of the light arriving at `point` along the unit `dir`, the
    /// way it comes in, crossed the air and the clouds.
    pub(crate) fn transmitted(&self, point: Vec3, dir: Vec3) -> Vec3 {
        let air = match &self.dome {
            Dome::Air(atmosphere) => atmosphere
                .leaving(point.y, dir)
                .map_or(Vec3::ZERO, |leaving| leaving.kept),
            Dome::Gradient(_) => Vec3::ONE,
        };
        air * self.clouded(point, dir)
    }

    /// How much of the sunlight arriving at `point` along `dir` the clouds
    /// let reach it.
    pub(crate) fn clouded(&self, point: Vec3, dir: Vec3) -> f64 {
        [self.low.as_ref(), self.high.as_ref()]
            .into_iter()
            .flatten()
            .map(|bank| bank.shadow(point, dir))
            .product()
    }

    /// How the light from the true direction `toward` arrives at `point`:
    /// bent by the air, or as it is under a room's walls; `None` where the
    /// Earth stands between.
    pub(crate) fn arriving(&self, point: Vec3, toward: Vec3) -> Option<Arriving> {
        match &self.dome {
            Dome::Air(atmosphere) => atmosphere.arriving(point.y, toward),
            Dome::Gradient(_) => Some(Arriving {
                dir: toward,
                stretch: 1.0,
                kept: Vec3::ONE,
                bend: Bend::none(toward),
            }),
        }
    }

    /// The true direction the green light a ray from `point` along `dir`
    /// meets comes from, and the solid angle that light is seen over for
    /// each it truly fills; `None` where the ray meets the ground.
    pub(crate) fn leaving(&self, point: Vec3, dir: Vec3) -> Option<(Vec3, f64)> {
        match &self.dome {
            Dome::Air(atmosphere) => atmosphere
                .leaving(point.y, dir)
                .map(|leaving| (leaving.toward, leaving.stretch)),
            Dome::Gradient(_) => Some((dir, 1.0)),
        }
    }

    /// The true directions each channel of the light a ray from `point`
    /// along `dir` meets comes from, and what of it the air and the clouds
    /// let reach `point`; `None` where the ray meets the ground.
    pub(crate) fn beyond(&self, point: Vec3, dir: Vec3) -> Option<([Vec3; 3], Vec3)> {
        let (sources, kept) = match &self.dome {
            Dome::Air(atmosphere) => {
                let leaving = atmosphere.leaving(point.y, dir)?;
                (
                    atmosphere.sources(dir, leaving.bend, leaving.toward),
                    leaving.kept,
                )
            }
            Dome::Gradient(_) => ([dir; 3], Vec3::ONE),
        };
        Some((sources, kept * self.clouded(point, dir)))
    }

    /// The true directions each channel of the light `arriving` comes from,
    /// the green's being `toward`.
    pub(crate) fn channels(&self, arriving: &Arriving, toward: Vec3) -> [Vec3; 3] {
        match &self.dome {
            Dome::Air(atmosphere) => atmosphere.sources(arriving.dir, arriving.bend, toward),
            Dome::Gradient(_) => [toward; 3],
        }
    }

    /// The cosine to the centre of a disc of angular radius whose cosine is
    /// `cos_radius` below which a ray meets no channel of its light,
    /// however the air bends it.
    pub(crate) fn reach(&self, cos_radius: f64) -> f64 {
        match &self.dome {
            Dome::Air(atmosphere) => {
                let (cos, sin) = atmosphere.bending();
                let sin_radius = mathf::sqrt(((1.0 - cos_radius) * (1.0 + cos_radius)).max(0.0));
                cos_radius * cos - sin_radius * sin
            }
            Dome::Gradient(_) => cos_radius,
        }
    }

    /// What a ray from the eye along `dir` shows of `light` met `distance`
    /// away, the air between having dimmed it and added its own glow as far
    /// as `lit` says the sun and the sky reach that air; `None` under a
    /// gradient, which has no air, and so never asks how lit it is.
    pub(crate) fn aerial(
        &self,
        dir: Vec3,
        distance: f64,
        light: Vec3,
        lit: impl FnOnce() -> Lit,
    ) -> Option<Vec3> {
        match &self.dome {
            Dome::Air(atmosphere) => Some(atmosphere.aerial(dir, distance, light, lit())),
            Dome::Gradient(_) => None,
        }
    }

    /// The sky without its clouds: the air's light and, past it, the stars
    /// bright enough to show against it that the air lets through, each where
    /// the air bends its light from.
    fn clear(&self, origin: Vec3, dir: Vec3, spread: Option<f64>) -> Vec3 {
        match &self.dome {
            Dome::Air(atmosphere) => {
                let air = atmosphere.sky(dir);
                let Some((stars, glimpse)) = self
                    .stars
                    .as_ref()
                    .and_then(|stars| Some((stars, stars.glimpse(spread, air)?)))
                else {
                    return air;
                };
                atmosphere.leaving(origin.y, dir).map_or(air, |leaving| {
                    air + stars.radiance(leaving.toward, glimpse) * leaving.kept
                })
            }
            Dome::Gradient(gradient) => gradient.at(dir),
        }
    }
}

#[cfg(test)]
#[path = "sky_tests.rs"]
pub(crate) mod tests;
