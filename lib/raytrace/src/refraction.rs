//! How the air bends light: its index of refraction through the standard
//! atmosphere, and the path a ray bends along on its way out of the air.
//!
//! The index is standard dry air's at sea level (Ciddor, "Refractive index
//! of air: new equations for the visible and near infrared", Applied Optics
//! 35, 1996), scaled by the density of the U.S. Standard Atmosphere (1976),
//! whose temperature lapse sets the gradient near the ground that the bending
//! depends on. Where the index varies with height alone a ray keeps
//! `n r sin z` (Bouguer), so its path out of the air is a pair of integrals
//! over the radius: the angle it sweeps about the Earth's centre, and the
//! optical depth it crosses. Their integrand has a square-root singularity
//! where the ray runs level, and a ray rising a hair above level sets out
//! within a hair of one; measuring the radius from that level, `r = r₀ + u²`,
//! takes it out for every ray.

use tairix_util::mathf;

use crate::vector::Vec3;

/// Standard dry air's refractivity, `n − 1`, at sea level and `wavelength`
/// micrometres: 15 °C, 101 325 Pa and 450 ppm of carbon dioxide, the
/// standard atmosphere's own sea level.
pub(crate) fn refractivity(wavelength: f64) -> f64 {
    let wavenumber2 = 1.0 / (wavelength * wavelength);
    (5_792_105.0 / (238.0185 - wavenumber2) + 167_917.0 / (57.362 - wavenumber2)) * 1e-8
}

/// The standard atmosphere's layers: the geopotential height each begins at,
/// in kilometres, and how its temperature changes upward, in kelvin a
/// kilometre.
const LAYERS: [(f64, f64); 8] = [
    (0.0, -6.5),
    (11.0, 0.0),
    (20.0, 1.0),
    (32.0, 2.8),
    (47.0, 0.0),
    (51.0, -2.8),
    (71.0, -2.0),
    (84.852, 0.0),
];
/// Its temperature at sea level, in kelvin.
const SEA_LEVEL: f64 = 288.15;
/// The radius its geopotential heights are reckoned from, in kilometres.
const GEOPOTENTIAL_RADIUS: f64 = 6356.766;
/// `g₀ M / R*`, by which a layer's pressure falls, in kelvin a kilometre.
const HYDROSTATIC: f64 = 34.163_195;

/// Segments of the radius, after substitution, each leg is integrated over.
const SEGMENTS: u32 = 24;
/// The three-point Gauss–Legendre rule on `-1..1`: its nodes and weights.
const GAUSS: [(f64, f64); 3] = [
    (-0.774_596_669_241_483_4, 5.0 / 9.0),
    (0.0, 8.0 / 9.0),
    (0.774_596_669_241_483_4, 5.0 / 9.0),
];
/// Halvings a ray heading down is searched for the height it runs level at.
const LEVELLING: u32 = 52;
/// How near the ground, in kilometres, a ray may level off and still be
/// taken to graze it: well past the rounding of `n r` at the Earth's radius.
const GRAZE: f64 = 1e-9;

/// Where a layer of the standard atmosphere begins, and its air there.
#[derive(Copy, Clone, Debug)]
struct Layer {
    base: f64,
    lapse: f64,
    temperature: f64,
    /// As a share of the pressure at sea level.
    pressure: f64,
}

/// Air whose index of refraction changes with height alone.
#[derive(Clone, Debug)]
pub(crate) struct Refraction {
    /// `n − 1` at sea level.
    refractivity: f64,
    layers: [Layer; LAYERS.len()],
}

/// A ray traced out of the air.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Path {
    /// The angle between the vertical where the ray starts and the way it
    /// leaves the air: for the light it brings, the true zenith angle of its
    /// source.
    pub(crate) zenith: f64,
    /// The optical depth it crosses.
    pub(crate) depth: Vec3,
}

impl Refraction {
    /// The standard atmosphere's air, `refractivity` at sea level.
    pub(crate) fn new(refractivity: f64) -> Self {
        let mut layers = [Layer {
            base: 0.0,
            lapse: 0.0,
            temperature: SEA_LEVEL,
            pressure: 1.0,
        }; LAYERS.len()];
        let mut below = layers[0];
        for (slot, &(base, lapse)) in layers.iter_mut().zip(&LAYERS) {
            let (temperature, pressure) = below.at(base);
            *slot = Layer {
                base,
                lapse,
                temperature,
                pressure,
            };
            below = *slot;
        }
        Self {
            refractivity,
            layers,
        }
    }

    /// The air's density `height` kilometres above the sea, as a share of
    /// its density there.
    pub(crate) fn density(&self, height: f64) -> f64 {
        self.air(height).0
    }

    /// The air's density `height` kilometres above the sea, as a share of
    /// its density there, and how fast that changes a kilometre up.
    fn air(&self, height: f64) -> (f64, f64) {
        let height = height.max(0.0);
        let shrink = GEOPOTENTIAL_RADIUS / (GEOPOTENTIAL_RADIUS + height);
        let geopotential = height * shrink;
        let layer = self
            .layers
            .iter()
            .rev()
            .find(|layer| geopotential >= layer.base)
            .unwrap_or(&self.layers[0]);
        let (temperature, pressure) = layer.at(geopotential);
        let density = pressure * SEA_LEVEL / temperature;
        // Hydrostatic balance and the gas law: the density's logarithm falls
        // by `(g₀M/R* + lapse)/T` a geopotential kilometre.
        let falling = (HYDROSTATIC + layer.lapse) / temperature * shrink * shrink;
        (density, -density * falling)
    }

    /// The index of refraction `height` kilometres above the sea.
    pub(crate) fn index(&self, height: f64) -> f64 {
        1.0 + self.refractivity * self.density(height)
    }

    /// The ray leaving radius `r` kilometres from the Earth's centre at
    /// zenith cosine `mu`, traced out of the air between radii `ground` and
    /// `top` through `extinction`, per kilometre at a height above the
    /// ground; `None` where it meets the ground.
    pub(crate) fn trace(
        &self,
        (r, mu): (f64, f64),
        (ground, top): (f64, f64),
        extinction: &dyn Fn(f64) -> Vec3,
    ) -> Option<Path> {
        let r = r.clamp(ground, top);
        let sin = mathf::sqrt((1.0 - mu * mu).max(0.0));
        let (density, change) = self.air(r - ground);
        let index = 1.0 + self.refractivity * density;
        let invariant = index * r * sin;
        let level = if mu >= 0.0 {
            // Where the ray would run level were `n r` to rise on as it rises
            // at the start: measured from there, the integrand stays smooth
            // however near level the ray sets out.
            let rising = index + r * self.refractivity * change;
            r - index * r * mu * mu / (2.0 * rising)
        } else {
            self.levelling(invariant, (ground, r))?
        };
        let leg = |from: f64, to: f64| self.leg(invariant, level, (from, to), ground, extinction);
        let (start, mut swept, mut depth) = if mu >= 0.0 {
            (r, 0.0, Vec3::ZERO)
        } else {
            let (angle, crossed) = leg(level, r);
            (level, angle, crossed)
        };
        let (angle, crossed) = leg(start, top);
        swept += angle;
        depth += crossed;
        Some(Path {
            zenith: swept + mathf::asin((invariant / top).min(1.0)),
            depth,
        })
    }

    /// The angle about the Earth's centre a ray of `invariant`, running level
    /// at radius `level`, sweeps between radii `from` and `to`, and the
    /// optical depth it crosses there.
    fn leg(
        &self,
        invariant: f64,
        level: f64,
        (from, to): (f64, f64),
        ground: f64,
        extinction: &dyn Fn(f64) -> Vec3,
    ) -> (f64, Vec3) {
        let (low, high) = (
            mathf::sqrt((from - level).max(0.0)),
            mathf::sqrt((to - level).max(0.0)),
        );
        let width = (high - low) / f64::from(SEGMENTS);
        let (mut angle, mut depth) = (0.0, Vec3::ZERO);
        for segment in 0..SEGMENTS {
            for (node, weight) in GAUSS {
                let u = low + width * (f64::from(segment) + 0.5 + 0.5 * node);
                let radius = level + u * u;
                let reach = self.index(radius - ground) * radius;
                // `n r cos z`, which runs to nought as the ray levels off.
                let rise = mathf::sqrt((reach * reach - invariant * invariant).max(1e-300));
                let scale = weight * width * u / rise;
                angle += scale * invariant / radius;
                depth += extinction(radius - ground) * (scale * reach);
            }
        }
        (angle, depth)
    }

    /// Where a ray of `invariant` heading down from radius `r` runs level,
    /// `n r` falling to the invariant; `None` when that lies beneath the
    /// ground, which the ray meets first. A ray levelling within a
    /// micrometre of the ground grazes it.
    fn levelling(&self, invariant: f64, (ground, r): (f64, f64)) -> Option<f64> {
        let excess = |radius: f64| self.index(radius - ground) * radius - invariant;
        if excess(ground) > GRAZE {
            return None;
        }
        let (mut low, mut high) = (ground, r);
        for _ in 0..LEVELLING {
            let middle = f64::midpoint(low, high);
            if excess(middle) > 0.0 {
                high = middle;
            } else {
                low = middle;
            }
        }
        Some(high)
    }
}

impl Layer {
    /// The temperature and the share of sea-level pressure at `geopotential`
    /// kilometres within this layer.
    fn at(&self, geopotential: f64) -> (f64, f64) {
        let rise = geopotential - self.base;
        let temperature = self.temperature + self.lapse * rise;
        let pressure = if self.lapse == 0.0 {
            self.pressure * mathf::exp(-HYDROSTATIC * rise / self.temperature)
        } else {
            self.pressure
                * mathf::exp(HYDROSTATIC / self.lapse * mathf::ln(self.temperature / temperature))
        };
        (temperature, pressure)
    }
}

#[cfg(test)]
#[path = "refraction_tests.rs"]
mod tests;
