//! Host tests of a trunk's foot: it swells further toward each root than
//! between them, each root leaves from within its lobe with its back above
//! the ground and ends buried, and no foot spreads more roots than a flare
//! holds lobes.

use alloc::vec::Vec;

use super::*;
use crate::flare::MOST_LOBES;

/// A trunk 0.3 thick, its first end 6 cm below the ground, its flared foot
/// a metre tall, and its foot spreading `roots` roots from `seed`.
fn foot(roots: u32, seed: u64) -> (Tube, Option<Foot>) {
    let tube = Tube::new(
        (Vec3::new(0.0, -0.06, 0.0), Vec3::new(0.0, 1.0, 0.0)),
        ((0.3, 0.29), (0.0, 1.06)),
        (0, 1),
        Vec3::new(1.0, 0.0, 0.0),
    );
    let mut dice = NonCryptoRng::seed_from_u64(seed);
    (
        tube,
        Foot::new(&tube, (0.3, 0.06, 1.06), (0.6, roots), &mut dice),
    )
}

#[test]
fn each_root_runs_out_of_its_lobe_and_dives_into_the_soil() {
    for seed in 0..12 {
        let (tube, foot) = foot(6, seed);
        let foot = foot.expect("a foot");
        let flare = foot.flare();
        let mut parts: Vec<Part> = Vec::new();
        foot.roots((0, 7), 0.0, &mut |part| {
            parts.push(part);
            Some(())
        })
        .expect("laid");
        let mut roots: Vec<Vec<Tube>> = Vec::new();
        for part in parts {
            let Part::Tube(limb) = part else { continue };
            match roots
                .iter_mut()
                .find(|root| root.first().is_some_and(|first| first.key == limb.key))
            {
                Some(root) => root.push(limb),
                None => roots.push(alloc::vec![limb]),
            }
        }
        assert_eq!(roots.len(), 6, "{seed}");
        let between = (0..72u32)
            .map(|step| flare.factor(0.06, core::f64::consts::TAU * f64::from(step) / 72.0))
            .fold(f64::INFINITY, f64::min);
        for root in &roots {
            let first = root.first().expect("a root");
            let start = point(first.a);
            let out = Vec3::new(start.x, 0.0, start.z);
            let angle = tube.angle_of(out.normalized());
            assert!(
                flare.factor(0.06, angle) > between + 0.15,
                "{seed}: its lobe swells"
            );
            let up = start.y + 0.06;
            let lobe = tube.round_radius(up) * flare.factor(up, angle);
            assert!(
                out.length() < lobe,
                "{seed}: leaves {} out, inside its lobe {lobe}",
                out.length()
            );
            assert!(
                start.y + f64::from(first.radii[0]) > 0.0,
                "{seed}: its back shows"
            );
            for limb in root.iter().filter(|limb| point(limb.b).length() > 0.9) {
                assert!(
                    f64::from(limb.b[1]) + f64::from(limb.radii[1]) < 0.0,
                    "{seed}: a far end shows: {limb:?}"
                );
            }
        }
    }
}

#[test]
fn a_foot_spreads_no_more_roots_than_a_flare_holds_lobes() {
    let cap = u32::try_from(MOST_LOBES).expect("a count");
    assert!(foot(cap, 1).1.is_some());
    assert!(foot(cap + 1, 1).1.is_none());
    assert!(ROOTS.1 <= cap);
}
