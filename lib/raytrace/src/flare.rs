//! A limb's foot swelling out of round where it meets the ground: a swell
//! all the way round, and a lobe out toward each of its roots, the trunk
//! deforming toward each as a buttress does. Each fades up the limb, and the
//! whole flare to nothing before the limb's far end, so the limb above meets
//! it in round.

use tairix_util::mathf;

use crate::noise::smoothstep;
use crate::vector::{single, wrapped};

/// The most lobes a flare holds.
pub(crate) const MOST_LOBES: usize = 12;

/// How far up a flare's reach its fading begins.
const FADES_FROM: f64 = 0.55;

/// A lobe of a flare: the angle round the limb it swells toward, as the
/// limb's bark reckons angles; how far it swells out at the ground, as a
/// share of the limb's radius; how wide it is either side, in radians; and
/// how far up the limb its swell falls to a share of `1/e`, in metres.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(crate) struct Lobe {
    pub(crate) angle: f32,
    pub(crate) out: f32,
    pub(crate) width: f32,
    pub(crate) climb: f32,
}

/// A flare: how far above the limb's first end the ground lies; how far up
/// the limb from that end it has faded out entirely; its swell all round,
/// how far out at the ground and how far up it falls to `1/e`; and its lobes.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Flare {
    ground: f32,
    reach: f32,
    swell: (f32, f32),
    lobes: [Lobe; MOST_LOBES],
    count: u8,
    /// Its bounds, read round it where its lobes swell most: the most it
    /// swells, how fast it falls up the limb a metre, and how fast its
    /// lobes rise round it a radian.
    most: f32,
    climbing: f32,
    rounding: f32,
}

/// How finely round a flare its bounds are read.
const BOUND_SPOKES: u32 = 720;

impl Flare {
    /// A flare of `lobes` and `swell` over a limb whose first end lies
    /// `ground` below the ground, fading out by `reach` up it; `None` when it
    /// holds more lobes than a flare does, or any measure is not a finite
    /// positive length or a share that is not.
    pub(crate) fn new(ground: f64, reach: f64, swell: (f64, f64), lobes: &[Lobe]) -> Option<Self> {
        let positive = |value: f64| value.is_finite() && value > 0.0;
        let share = |value: f64| value.is_finite() && value >= 0.0;
        let sound = share(ground)
            && positive(reach)
            && share(swell.0)
            && positive(swell.1)
            && lobes.iter().all(|lobe| {
                f64::from(lobe.angle).is_finite()
                    && share(f64::from(lobe.out))
                    && positive(f64::from(lobe.width))
                    && positive(f64::from(lobe.climb))
            });
        if !sound || lobes.len() > MOST_LOBES {
            return None;
        }
        let mut held = [Lobe::default(); MOST_LOBES];
        held.get_mut(..lobes.len())?.copy_from_slice(lobes);
        let mut flare = Self {
            ground: single(ground),
            reach: single(reach),
            swell: (single(swell.0), single(swell.1)),
            lobes: held,
            count: u8::try_from(lobes.len()).ok()?,
            most: 0.0,
            climbing: 0.0,
            rounding: 0.0,
        };
        // Read round it at the ground, where every lobe is at its fullest,
        // and widened by as much as a lobe could rise between two readings.
        let (mut most, mut climbing, mut rounding) = (0.0f64, 0.0f64, 0.0f64);
        for spoke in 0..BOUND_SPOKES {
            let angle = core::f64::consts::TAU * f64::from(spoke) / f64::from(BOUND_SPOKES);
            let (out, rise, slope) = flare.lobed(angle);
            most = most.max(out);
            climbing = climbing.max(rise);
            rounding = rounding.max(slope);
        }
        // How fast, at most, each reading could change between two spokes:
        // a lobe's profile `(1 − x²)²` turns no faster than `12/w²` and rises
        // no faster than `1.54/w` a radian.
        let between = core::f64::consts::PI / f64::from(BOUND_SPOKES);
        let (turning, climb_rising) =
            flare
                .lobes()
                .iter()
                .fold((0.0, 0.0), |(turning, rising), lobe| {
                    let (out, width) = (f64::from(lobe.out), f64::from(lobe.width));
                    (
                        turning + out * 12.0 / (width * width),
                        rising + out * 1.54 / (width * f64::from(lobe.climb)),
                    )
                });
        let rounding = rounding + turning * between;
        flare.most = single(1.0 + swell.0 + most + rounding * between);
        flare.climbing = single(swell.0 / swell.1 + climbing + climb_rising * between);
        flare.rounding = single(rounding);
        Some(flare)
    }

    /// Round the flare at `angle`, where its lobes are fullest: how far they
    /// swell it, as a share of its radius, how fast that falls a metre up
    /// the limb, and how fast it changes a radian round it.
    fn lobed(&self, angle: f64) -> (f64, f64, f64) {
        self.lobes()
            .iter()
            .fold((0.0, 0.0, 0.0), |(out, rise, slope), lobe| {
                let across = wrapped(angle - f64::from(lobe.angle)) / f64::from(lobe.width);
                if across.abs() >= 1.0 {
                    return (out, rise, slope);
                }
                let fall = 1.0 - across * across;
                let swollen = f64::from(lobe.out) * fall * fall;
                (
                    out + swollen,
                    rise + swollen / f64::from(lobe.climb),
                    slope + f64::from(lobe.out) * 4.0 * across.abs() * fall / f64::from(lobe.width),
                )
            })
    }

    fn lobes(&self) -> &[Lobe] {
        self.lobes.get(..usize::from(self.count)).unwrap_or(&[])
    }

    /// The limb's radius `up` metres along it from its first end and `angle`
    /// round it, as a share of its round radius there.
    pub(crate) fn factor(&self, up: f64, angle: f64) -> f64 {
        let reach = f64::from(self.reach);
        if up >= reach {
            return 1.0;
        }
        let above = (up - f64::from(self.ground)).max(0.0);
        let fading = 1.0 - smoothstep(FADES_FROM * reach, reach, up);
        let all_round = f64::from(self.swell.0) * mathf::exp(-above / f64::from(self.swell.1));
        let lobed: f64 = self
            .lobes()
            .iter()
            .map(|lobe| {
                let across = wrapped(angle - f64::from(lobe.angle)) / f64::from(lobe.width);
                if across.abs() >= 1.0 {
                    return 0.0;
                }
                let profile = (1.0 - across * across) * (1.0 - across * across);
                f64::from(lobe.out) * profile * mathf::exp(-above / f64::from(lobe.climb))
            })
            .sum();
        1.0 + fading * (all_round + lobed)
    }

    /// The most `factor` is anywhere.
    pub(crate) fn most(&self) -> f64 {
        f64::from(self.most)
    }

    /// How fast the flared surface of a limb no more than `radius` thick can
    /// rise, a metre along or round it, beyond the limb's own taper: a bound
    /// for a march toward it.
    pub(crate) fn steepest(&self, radius: f64) -> f64 {
        let fading = 1.5 / ((1.0 - FADES_FROM) * f64::from(self.reach));
        radius * (fading * (self.most() - 1.0) + f64::from(self.climbing))
            + f64::from(self.rounding)
    }
}

#[cfg(test)]
#[path = "flare_tests.rs"]
mod tests;
