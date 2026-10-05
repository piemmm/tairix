//! Stones strewn over a land as the land would have them, never an even
//! sprinkle: gathered in boulder fields where a broad noise has them, each
//! boulder among a few smaller ones; fanned in scree below the crags they
//! fell from, the larger rolling the further; and washed into drifts of
//! pebbles in the hollows near the eye. Eight rocks of the land's own stone
//! a scene, each stone turned and sized its own way.

use tairix_util::mathf;

use super::landscape::{rock_of, Vantage};
use super::stones::Stones;
use super::{Dice, Stage};
use crate::land::{Land, Lie};
use crate::noise::{fbm2, noise2, smoothstep};
use crate::vector::Vec3;

/// What a setting strews: the most boulders and how large they come, how far
/// off and how far either side of the view.
#[derive(Copy, Clone, Debug)]
pub(super) struct Strewing {
    pub(super) boulders: (u32, (f64, f64)),
    pub(super) reach: (f64, f64),
    pub(super) spread: f64,
}

/// Tries at a place for every boulder strewn: most fall where the land holds
/// none.
const TRIES: u32 = 10;

/// How broad a land's boulder fields run, in metres; the share of a field's
/// worth of stones that lie out of any; and how many smaller stones lie about
/// a boulder in a field, at most.
const FIELD: f64 = 70.0;
const STRAYS: f64 = 0.04;
const COMPANIONS: u32 = 3;

/// How steep ground must stand to be a crag stones fall from, as its normal's
/// upward part; the slopes scree lies on below one; and how far uphill a crag
/// is looked for, nearest first.
const CRAG: f64 = 0.6;
const TALUS: (f64, f64) = (0.62, 0.92);
const UPHILL: [f64; 5] = [3.0, 6.0, 10.0, 15.0, 22.0];

/// How broad the drifts of pebbles are and how many a drift holds; how large
/// a pebble comes; and the least wetness or laid sediment a hollow water
/// washed them to shows.
const DRIFT: f64 = 0.6;
const DRIFTED: (u32, u32) = (4, 12);
const PEBBLES: (f64, f64) = (0.04, 0.14);
const WASHED: f64 = 0.25;

/// Strew `strewing`'s stones of `land`'s own rock about `vantage`, wherever
/// `lies` says a stone can lie, from draws of their own so a detail strewing
/// more shifts nothing else. `None` when the stage will not hold them.
pub(super) fn strew(
    stage: &mut Stage,
    dice: &mut Dice,
    (land, vantage): (&Land, &Vantage),
    strewing: Strewing,
    lies: &dyn Fn(&Lie) -> bool,
) -> Option<()> {
    let mut dice = Dice::keyed(dice.wide(), 0);
    let stones = Stones::new(stage, &mut dice, rock_of(stage, land)?)?;
    let density = stage.densities.strewn;
    let (count, sizes) = strewing.boulders;
    let wanted = f64::from(count) * density.boulders;
    let wanted = u32::try_from(mathf::round_i32(wanted).max(0)).ok()?;
    let seed = dice.seed();
    let Vantage { eye, heading } = *vantage;
    let mut strewn = 0;
    for _ in 0..wanted.saturating_mul(TRIES) {
        if strewn >= wanted {
            break;
        }
        let angle = heading + dice.range(-strewing.spread, strewing.spread);
        // The nearer ground holds more of them, as the eye would see them.
        let near = dice.unit();
        let distance =
            strewing.reach.0 + (strewing.reach.1 - strewing.reach.0) * near * mathf::sqrt(near);
        let at = (
            eye.x + mathf::sin(angle) * distance,
            eye.z + mathf::cos(angle) * distance,
        );
        let lie = land.lie(&stage.fields, at.0, at.1);
        if !lies(&lie) || land.wet_at(&stage.fields, at.0, at.1) {
            continue;
        }
        let field = smoothstep(
            0.1,
            0.45,
            fbm2(at.0 / FIELD, at.1 / FIELD, seed, (3, 0.5, 2.0)),
        );
        let fallen = scree(land, &stage.fields, (at, &lie));
        let chance = STRAYS + (1.0 - STRAYS) * field.max(fallen.map_or(0.0, |(chance, _)| chance));
        if dice.unit() >= chance {
            continue;
        }
        // Below a crag the larger stones rolled the further; elsewhere the
        // smaller are the commoner.
        let size = match fallen {
            Some((chance, rolled)) if chance > field => {
                sizes.0 + (sizes.1 - sizes.0) * rolled * dice.range(0.5, 1.0)
            }
            _ => sizes.0 + (sizes.1 - sizes.0) * dice.unit() * dice.unit(),
        };
        if !lay(stage, &mut dice, (land, &stones), (at, size))? {
            continue;
        }
        strewn += 1;
        if field > 0.25 {
            for _ in 0..dice.count(0, COMPANIONS) {
                let (around, off) = (
                    dice.range(0.0, core::f64::consts::TAU),
                    size * dice.range(0.8, 2.5),
                );
                let beside = (
                    at.0 + mathf::sin(around) * off,
                    at.1 + mathf::cos(around) * off,
                );
                if lies(&land.lie(&stage.fields, beside.0, beside.1)) {
                    let smaller = size * dice.range(0.25, 0.65);
                    lay(stage, &mut dice, (land, &stones), (beside, smaller))?;
                }
            }
        }
    }
    drifts(
        stage,
        &mut dice,
        (land, &stones, vantage),
        (strewing.spread, seed),
        lies,
    )
}

/// Lay a stone `size` across of `stones` at `at` on `land` if it stays clear
/// of what stands there: whether it did, or `None` when the stage will not
/// hold it.
fn lay(
    stage: &mut Stage,
    dice: &mut Dice,
    (land, stones): (&Land, &Stones),
    (at, size): ((f64, f64), f64),
) -> Option<bool> {
    if !stage.clear(at, 0.5 * size) {
        return Some(false);
    }
    stage.claim(at, 0.5 * size)?;
    let base = Vec3::new(at.0, land.height(&stage.fields, at.0, at.1), at.1);
    let normal = land.normal(&stage.fields, at.0, at.1);
    stones.lay(stage, dice, (base, normal), size)?;
    Some(true)
}

/// How likely a stone fallen from a crag above lies at `at`, whose ground
/// `lie` describes, on a land whose grids are `fields`, and how far it rolled
/// to get there, as a share of the farthest: a crag the nearer uphill, the
/// likelier, on a slope scree rests on; `None` with no crag above.
fn scree(
    land: &Land,
    fields: &[crate::heightfield::Heightfield],
    (at, lie): ((f64, f64), &Lie),
) -> Option<(f64, f64)> {
    let on_talus = smoothstep(TALUS.0, TALUS.0 + 0.06, lie.upright)
        * (1.0 - smoothstep(TALUS.1 - 0.04, TALUS.1, lie.upright));
    if on_talus <= 0.0 {
        return None;
    }
    let normal = land.normal(fields, at.0, at.1);
    let fall = mathf::hypot(normal.x, normal.z);
    if fall < 1e-6 {
        return None;
    }
    let uphill = (-normal.x / fall, -normal.z / fall);
    let crag = UPHILL.iter().copied().find(|&distance| {
        land.normal(
            fields,
            at.0 + uphill.0 * distance,
            at.1 + uphill.1 * distance,
        )
        .y < CRAG
    })?;
    let farthest = UPHILL[UPHILL.len() - 1];
    let rolled = smoothstep(UPHILL[0], farthest, crag);
    Some((0.85 * on_talus * (1.0 - 0.6 * rolled), rolled))
}

/// Drifts of pebbles washed into the hollows ahead of `vantage`, within the
/// detail's reach of the eye, `spread` either side of the view: where water
/// wets the ground or laid what it carried, in patches under `seed`.
fn drifts(
    stage: &mut Stage,
    dice: &mut Dice,
    (land, stones, vantage): (&Land, &Stones, &Vantage),
    (spread, seed): (f64, u32),
    lies: &dyn Fn(&Lie) -> bool,
) -> Option<()> {
    let density = stage.densities.strewn;
    let Vantage { eye, heading } = *vantage;
    for _ in 0..density.drifts * TRIES {
        let angle = heading + dice.range(-spread, spread);
        let distance = 2.5 + (density.pebbles - 2.5) * dice.unit();
        let at = (
            eye.x + mathf::sin(angle) * distance,
            eye.z + mathf::cos(angle) * distance,
        );
        let lie = land.lie(&stage.fields, at.0, at.1);
        let washed = lie.wet.max(lie.sediment);
        let patch = smoothstep(0.0, 0.4, noise2(at.0 / 8.0, at.1 / 8.0, seed ^ 0xd1));
        if !lies(&lie) || washed < WASHED || dice.unit() >= patch {
            continue;
        }
        for _ in 0..dice.count(DRIFTED.0, DRIFTED.1) {
            let (around, off) = (
                dice.range(0.0, core::f64::consts::TAU),
                DRIFT * mathf::sqrt(dice.unit()),
            );
            let place = (
                at.0 + mathf::sin(around) * off,
                at.1 + mathf::cos(around) * off,
            );
            let size = PEBBLES.0 + (PEBBLES.1 - PEBBLES.0) * dice.unit() * dice.unit();
            lay(stage, dice, (land, stones), (place, size))?;
        }
    }
    Some(())
}

#[cfg(test)]
#[path = "strewn_tests.rs"]
mod tests;
