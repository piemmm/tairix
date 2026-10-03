//! The sun and the full moon as they are seen from the Earth: how large
//! their discs are, how bright and what colour their light is, and how the
//! sun's disc darkens toward its edge.

use core::f64::consts::LN_10;

use tairix_util::mathf;

use crate::atmosphere::{GROUND, WAVELENGTHS};
use crate::light::{disc_density, Light, Limb};
use crate::vector::Vec3;

/// The sun's irradiance above the air, in the scene's units of light.
const SOLAR: f64 = 20.0;
/// Sunlight's colour above the air: the white the camera is balanced to,
/// near enough.
const SUN_COLOUR: Vec3 = Vec3::new(1.0, 0.985, 0.955);
/// How much of the light's logarithm a magnitude takes off it.
pub(crate) const FADING: f64 = 0.4 * LN_10;
/// The sun's apparent visual magnitude, which a star's is reckoned against.
pub(crate) const SUN_MAGNITUDE: f64 = -26.74;
/// The full moon's apparent visual magnitude at its mean distance.
const FULL_MOON_MAGNITUDE: f64 = -12.74;

/// The sun's radius (IAU 2015 B3) and the astronomical unit, in kilometres.
const SUN_RADIUS: f64 = 695_700.0;
const ASTRONOMICAL_UNIT: f64 = 149_597_870.7;
/// The moon's radius and its mean distance from the Earth's centre, in
/// kilometres.
const MOON_RADIUS: f64 = 1737.4;
const MOON_DISTANCE: f64 = 384_400.0;

/// The full moon's reflectance at the red and blue channels' wavelengths
/// against the green's: the ROLO lunar model (Kieffer and Stone, "The
/// Spectral Irradiance of the Moon", 2005, version 311g) at 3° of phase,
/// fitted by a line over its bands from 405 to 745 nm.
const LUNAR_TINT: Vec3 = Vec3::new(1.187, 1.0, 0.842);

/// Sunlight's illuminance above the air at its mean distance, in lux
/// (Darula, Kittler and Gueymard, "Reference luminous solar constant and
/// solar luminance for illuminance calculations", 2005).
const SOLAR_ILLUMINANCE: f64 = 133_334.0;

/// The exponent α of the sun's limb darkening, `I(μ)/I(1) = μ^α`, by
/// wavelength in nanometres: Neckel and Labs' (1994) profiles as Hestroffer
/// and Magnan ("Wavelength dependency of the Solar limb darkening", 1998,
/// table 2) fit them, within a percent of the profile.
const LIMB_EXPONENTS: [(f64, f64); 16] = [
    (416.319, 0.724),
    (427.930, 0.682),
    (443.885, 0.649),
    (445.125, 0.646),
    (457.345, 0.628),
    (477.427, 0.594),
    (492.905, 0.570),
    (519.930, 0.538),
    (541.760, 0.514),
    (559.950, 0.496),
    (579.880, 0.477),
    (610.975, 0.447),
    (640.970, 0.428),
    (669.400, 0.407),
    (700.875, 0.386),
    (748.710, 0.361),
];

/// Sunlight above the air.
pub(crate) fn sunlight() -> Vec3 {
    SUN_COLOUR * SOLAR
}

/// How much of the scene's light one lux of illuminance is.
pub(crate) fn per_lux() -> f64 {
    sunlight().luminance() / SOLAR_ILLUMINANCE
}

/// What a magnitude `delta` fainter is, as a share of the light.
pub(crate) fn fainter(delta: f64) -> f64 {
    mathf::exp(-FADING * delta)
}

/// The sun toward the unit `toward`.
pub(crate) fn sun(toward: Vec3) -> Light {
    disc(
        toward,
        mathf::asin(SUN_RADIUS / ASTRONOMICAL_UNIT),
        sunlight(),
        Limb::Darkening(solar_limb()),
    )
}

/// The full moon toward the unit `toward`: sunlight the moon's grey
/// reflects, the brighter and wider the nearer the moon stands, which is
/// the higher in the sky. Lit square on, its disc is evenly bright.
pub(crate) fn full_moon(toward: Vec3) -> Light {
    let rise = toward.y;
    let distance = -GROUND * rise
        + mathf::sqrt(MOON_DISTANCE * MOON_DISTANCE - GROUND * GROUND * (1.0 - rise * rise));
    let nearer = MOON_DISTANCE / distance;
    let share = fainter(FULL_MOON_MAGNITUDE - SUN_MAGNITUDE) * nearer * nearer;
    disc(
        toward,
        mathf::asin(MOON_RADIUS / distance),
        sunlight() * LUNAR_TINT * share,
        Limb::Even,
    )
}

/// A disc `radius` radians across toward `toward` whose light is
/// `irradiance` square to it: its mean radiance spreads that over the solid
/// angle its samples are drawn over.
fn disc(toward: Vec3, radius: f64, irradiance: Vec3, limb: Limb) -> Light {
    let cos_radius = mathf::cos(radius);
    Light::Sun {
        toward,
        cos_radius,
        radiance: irradiance * disc_density(cos_radius),
        limb,
    }
}

/// The sun's limb-darkening exponent at each channel's wavelength.
fn solar_limb() -> Vec3 {
    let [red, green, blue] = WAVELENGTHS.map(|wavelength| limb_exponent(wavelength * 1000.0));
    Vec3::new(red, green, blue)
}

/// The limb-darkening exponent at `nanometres`, read along the measured
/// profiles.
fn limb_exponent(nanometres: f64) -> f64 {
    let mut below = LIMB_EXPONENTS[0];
    for &above in &LIMB_EXPONENTS[1..] {
        if nanometres <= above.0 {
            let t = ((nanometres - below.0) / (above.0 - below.0)).clamp(0.0, 1.0);
            return below.1 + (above.1 - below.1) * t;
        }
        below = above;
    }
    below.1
}

#[cfg(test)]
#[path = "body_tests.rs"]
mod tests;
