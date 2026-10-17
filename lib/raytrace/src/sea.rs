//! Seas: swells running before a wind, the height function the open sea's
//! grid is filled from.

use core::f64::consts::TAU;

use tairix_mix::{mix32, next_key, unit};
use tairix_util::mathf;

/// How many swells a sea sums.
const WAVES: usize = 18;

/// One swell of a sea.
#[derive(Copy, Clone, Debug)]
struct Wave {
    kx: f64,
    kz: f64,
    amplitude: f64,
    phase: f64,
    /// How peaked its crests are: `1` a sine, `2` narrower crests over
    /// broader troughs.
    peak: u32,
}

/// A sea: swells running before a wind, repeating every `period` each way
/// so its grid can tile the open water.
#[derive(Clone, Debug)]
pub(crate) struct Sea {
    waves: [Wave; WAVES],
}

impl Sea {
    /// A sea under `seed` whose largest swells are `swell` long and shortest
    /// `shortest`, `height` its significant wave height — four times the
    /// swells' standard deviation, about the height of the highest third
    /// from trough to crest — running toward `heading`, `spread` radians
    /// either side of it, and repeating every `period`.
    pub(crate) fn new(
        period: f64,
        (swell, shortest, height): (f64, f64, f64),
        (heading, spread): (f64, f64),
        seed: u32,
    ) -> Self {
        let mut waves = [Wave {
            kx: 0.0,
            kz: 0.0,
            amplitude: 0.0,
            phase: 0.0,
            peak: 1,
        }; WAVES];
        let mut key = seed;
        let falls = mathf::ln((shortest / swell).clamp(1e-3, 1.0)) / mathf::real(WAVES - 1);
        for (index, wave) in waves.iter_mut().enumerate() {
            key = next_key(key);
            let order = f64::from(u32::try_from(index).unwrap_or(0));
            // Wavelengths fall away geometrically to the shortest, and
            // shorter swells scatter wider about the wind.
            let length = swell * mathf::exp(order * falls);
            let angle = heading + (unit(key) - 0.5) * 2.0 * spread * (0.6 + 0.08 * order);
            // Snapped to whole wavelengths of the period, so the grid tiles.
            let whole = |part: f64| mathf::round(period * part / length);
            let (m, n) = (whole(mathf::cos(angle)), whole(mathf::sin(angle)));
            let (m, n) = if m == 0.0 && n == 0.0 {
                (1.0, 0.0)
            } else {
                (m, n)
            };
            let steep = if index < 3 { 0.11 } else { 0.07 };
            *wave = Wave {
                kx: TAU * m / period,
                kz: TAU * n / period,
                amplitude: (steep * length)
                    * (0.55 + 0.45 * unit(mix32(key)))
                    * mathf::exp(-order * 0.12),
                phase: TAU * unit(mix32(key ^ 1)),
                peak: if index < 6 { 2 } else { 1 },
            };
        }
        // Summed, the swells stand far higher than any one; scaled so their
        // spread is the sea's, each keeps its share of it.
        let variance: f64 = waves
            .iter()
            .map(|wave| 0.5 * wave.amplitude * wave.amplitude)
            .sum();
        let scale = 0.25 * height / mathf::sqrt(variance).max(1e-12);
        for wave in &mut waves {
            wave.amplitude *= scale;
        }
        Self { waves }
    }

    /// The water's height at `(x, z)`.
    pub(crate) fn height(&self, x: f64, z: f64) -> f64 {
        let mut total = 0.0;
        for wave in &self.waves {
            let rise = 0.5 + 0.5 * mathf::sin(wave.kx * x + wave.kz * z + wave.phase);
            let crest = if wave.peak == 1 { rise } else { rise * rise };
            total += wave.amplitude * (2.0 * crest - 1.0);
        }
        total
    }
}

#[cfg(test)]
#[path = "sea_tests.rs"]
mod tests;
