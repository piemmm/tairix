//! A trunk's foot: the flare it swells in, all round and out toward each of
//! its roots as a buttress, and the roots themselves. Each root runs on out
//! of its lobe where the lobe comes down to the root's back, just under half
//! buried, wandering to one side as it narrows and diving into the soil, a
//! rootlet carrying it on down, now and then forking. A living tree's foot
//! and a stump's are laid out alike.

use core::f64::consts::TAU;

use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::mathf;

use crate::flare::{Flare, Lobe, MOST_LOBES};
use crate::prototype::{point, Part, Tube};
use crate::sample::mix32;
use crate::vector::{power, real, single, Vec3};

/// How far up a trunk, in its radii, its swell all round falls to `1/e` of
/// what it is at the ground, and how much of its flare that swell takes; and
/// by how far up it the flare has faded out.
pub(crate) const SWELL_REACH: f64 = 1.1;
pub(crate) const SWELL: f64 = 0.45;
pub(crate) const FLARE_TOP: f64 = 3.5;

/// How many roots a trunk spreads, at the fewest and the most.
pub(crate) const ROOTS: (u32, u32) = (5, 8);

/// The segments a root runs out in; how steeply its run dives into the soil
/// by its end, so it lies buried on all but the steepest ground a wood grows
/// on, where its downhill roots stand out of the soil the slope has shed; and
/// how high above the ground its axis leaves the trunk, in its radii.
const ROOT_SEGMENTS: u32 = 4;
const DIVE: f64 = 0.55;
const ROOT_RISE: f64 = 0.3;

/// A root as it is laid out: the level way it runs, how far off the trunk's
/// axis it leaves, how thick it is there and how far on it runs, how far it
/// wanders to one side as a share of that, and where it forks, if it does,
/// as a share of its run, and the way it turns there.
#[derive(Copy, Clone, Debug, Default)]
struct Root {
    out: Vec3,
    from: f64,
    thick: f64,
    length: f64,
    wander: f64,
    fork: Option<(f64, f64)>,
}

/// A trunk's foot: the flare its limb swells in, and its roots.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Foot {
    flare: Flare,
    roots: [Root; MOST_LOBES],
    count: usize,
}

fn range(dice: &mut NonCryptoRng, (low, high): (f64, f64)) -> f64 {
    low + (high - low) * dice.next_f64()
}

impl Foot {
    /// The foot of `tube`, the upright limb a trunk `radius` thick stands on,
    /// its first end `ground` below the ground: flaring by `flare` and faded
    /// out by `reach` up it, and spreading `roots` roots, drawn from `dice`;
    /// `None` when it would spread more roots than a flare holds lobes, or its
    /// measures make no flare.
    pub(crate) fn new(
        tube: &Tube,
        (radius, ground, reach): (f64, f64, f64),
        (flare, roots): (f64, u32),
        dice: &mut NonCryptoRng,
    ) -> Option<Self> {
        let count = usize::try_from(roots).ok()?;
        let mut lobes = [Lobe::default(); MOST_LOBES];
        let mut laid = [Root::default(); MOST_LOBES];
        let turn = range(dice, (0.0, TAU));
        let swell = SWELL * flare;
        for (index, (lobe, root)) in lobes.iter_mut().zip(&mut laid).enumerate().take(count) {
            let around = turn + TAU * real(index) / f64::from(roots) + range(dice, (-0.35, 0.35));
            let out = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around));
            let thick = radius * range(dice, (0.22, 0.34));
            let swollen = flare * range(dice, (0.8, 1.3));
            *lobe = Lobe {
                angle: single(tube.angle_of(out)),
                out: single(swollen),
                width: single((1.5 * thick / (radius * (1.0 + swell + swollen))).clamp(0.3, 0.6)),
                climb: single(radius * range(dice, (0.35, 0.6))),
            };
            let fork = (dice.next_f64() < 0.5).then(|| {
                let side = if dice.next_f64() < 0.5 { -1.0 } else { 1.0 };
                (range(dice, (0.35, 0.55)), side * range(dice, (0.6, 1.0)))
            });
            *root = Root {
                out,
                from: 0.0,
                thick,
                length: radius * range(dice, (0.8, 2.0)),
                wander: range(dice, (-0.3, 0.3)),
                fork,
            };
        }
        let length = (point(tube.b) - point(tube.a)).length();
        let made = Flare::new(
            ground,
            reach,
            (swell, SWELL_REACH * radius),
            lobes.get(..count)?,
        )?;
        // Where its lobe comes down to the height of the root's back, the root
        // runs on out of it.
        for (lobe, root) in lobes.iter().zip(&mut laid).take(count) {
            let up = ground + (1.0 + ROOT_RISE) * root.thick;
            let angle = f64::from(lobe.angle);
            let axis = (point(tube.b) - point(tube.a)) * (1.0 / length.max(1e-12));
            let ridge = point(tube.a)
                + axis * up
                + tube.way(angle) * (tube.round_radius(up) * made.factor(up, angle));
            root.from =
                (Vec3::new(ridge.x, 0.0, ridge.z).length() - 1.2 * root.thick).max(0.3 * radius);
        }
        Some(Self {
            flare: made,
            roots: laid,
            count,
        })
    }

    /// The flare the foot's limb swells in.
    pub(crate) const fn flare(&self) -> Flare {
        self.flare
    }

    /// Lay the foot's roots out in `bark`, keyed from `key`, `travelled`
    /// metres along the tree's path where they leave, through `push`.
    pub(crate) fn roots(
        &self,
        (bark, key): (u16, u32),
        travelled: f64,
        push: &mut dyn FnMut(Part) -> Option<()>,
    ) -> Option<()> {
        for (index, root) in self.roots.iter().take(self.count).enumerate() {
            let key = mix32(key ^ u32::try_from(index).ok()?.wrapping_mul(0x9e37_79b9));
            root.lay((bark, key), travelled, push)?;
        }
        Some(())
    }
}

impl Root {
    /// Lay the root out in `bark` keyed `key`, `travelled` metres along the
    /// tree's path where it leaves, through `push`.
    fn lay(
        &self,
        (bark, key): (u16, u32),
        travelled: f64,
        push: &mut dyn FnMut(Part) -> Option<()>,
    ) -> Option<()> {
        let Self {
            out,
            from,
            thick,
            length,
            wander,
            fork,
        } = *self;
        let side = Vec3::new(-out.z, 0.0, out.x);
        let tip = 0.28 * thick;
        let path = |t: f64| {
            let reach = from + (1.2 * thick + length) * t;
            let rise =
                ROOT_RISE * thick - (ROOT_RISE * thick + tip + DIVE * length) * power(t, 1.6);
            out * reach + side * (wander * length * t * t) + Vec3::UP * rise
        };
        let girth = |t: f64| thick * (1.0 - 0.72 * t);
        let limb = |(a, b): (Vec3, Vec3), radii: (f64, f64), along: f64| {
            Part::Tube(Tube::new(
                (a, b),
                (radii, (along, along + (b - a).length())),
                (bark, key),
                side,
            ))
        };
        let mut along = travelled;
        for segment in 0..ROOT_SEGMENTS {
            let (t0, t1) = (
                f64::from(segment) / f64::from(ROOT_SEGMENTS),
                f64::from(segment + 1) / f64::from(ROOT_SEGMENTS),
            );
            let (a, b) = (path(t0), path(t1));
            push(limb((a, b), (girth(t0), girth(t1)), along))?;
            along += (b - a).length();
        }
        let end = path(1.0);
        let down = (out * 0.3 - Vec3::UP).normalized();
        push(limb(
            (end, end + down * (2.0 * tip)),
            (tip, 0.2 * tip),
            along,
        ))?;
        if let Some((at, turn)) = fork {
            let start = path(at);
            let branch = (out * mathf::cos(turn) + side * mathf::sin(turn)).normalized();
            let thin = 0.45 * girth(at);
            let reach = 0.5 * length;
            let dive = start.y + thin + DIVE * reach;
            let middle = start + branch * (0.5 * reach) - Vec3::UP * (0.3 * dive);
            let last = start + branch * reach - Vec3::UP * dive;
            push(limb((start, middle), (thin, 0.7 * thin), along))?;
            push(limb((middle, last), (0.7 * thin, 0.25 * thin), along))?;
        }
        Some(())
    }
}

#[cfg(test)]
#[path = "foot_tests.rs"]
mod tests;
