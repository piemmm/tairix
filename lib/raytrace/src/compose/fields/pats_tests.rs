use super::*;
use crate::compose::testland;
use crate::detail::Detail;
use crate::heightfield::{Attributes, CHANNELS, GROWN, SNOW};
use crate::snow;

/// A pat is a low mound a unit across, highest at its middle and sunk at
/// its ragged edge, the younger the higher.
#[test]
fn a_pat_is_a_low_mound_sunk_at_its_edge() {
    let mut rises = [0.0; 3];
    for (index, aged) in AGES.iter().enumerate() {
        let building = pat(aged, 0, &mut Dice::keyed(5, index)).expect("a pat");
        let bounds = building.bounds();
        assert!(
            bounds.min.y < 0.0,
            "{index}: its edge stands at {}",
            bounds.min.y
        );
        assert!(
            bounds.max.y <= aged.top() + 1e-9,
            "{index}: it rises to {}",
            bounds.max.y
        );
        let across = bounds.max.x - bounds.min.x;
        assert!(
            across > 1.6 && across < 2.0 * (1.0 + aged.ragged),
            "{index}: {across} across"
        );
        rises[index] = bounds.max.y;
    }
    assert!(rises[0] > rises[1] && rises[1] > rises[2], "{rises:?}");
}

/// However ragged and sunken its edge, a pat stands over all the ground it
/// smothers, so no bare ring shows between it and the grass.
#[test]
fn a_pat_covers_the_ground_it_smothers() {
    // Each spoke's edge may come no nearer than this, so the facets between
    // two spokes still reach the ground smothered.
    let least = crate::grazing::SMOTHERED / mathf::cos(PI / crate::vector::real(PIECES));
    for (index, aged) in AGES.iter().enumerate() {
        for seed in 0..8 {
            let building = pat(aged, 0, &mut Dice::keyed(seed, index)).expect("a pat");
            let at = |ring: usize, piece: usize| {
                let index = if ring == 0 {
                    0
                } else {
                    1 + (ring - 1) * PIECES + piece
                };
                building
                    .vertex(u32::try_from(index).expect("an index"))
                    .expect("a vertex")
            };
            for piece in 0..PIECES {
                let out = |point: Vec3| mathf::hypot(point.x, point.z);
                let edge = (1..=RINGS)
                    .find_map(|ring| {
                        let (inner, outer) = (at(ring - 1, piece), at(ring, piece));
                        (outer.y < 0.0).then(|| {
                            out(inner) + (out(outer) - out(inner)) * inner.y / (inner.y - outer.y)
                        })
                    })
                    .unwrap_or_else(|| out(at(RINGS, piece)));
                assert!(
                    edge >= least,
                    "age {index}, seed {seed}: its edge comes in to {edge} along spoke {piece}"
                );
            }
        }
    }
}

/// Pats are laid on grazed ground about the eye, out to their reach, and
/// none on ground grazed by nothing.
#[test]
fn pats_lie_only_on_grazed_ground() {
    let laid = |grown: Grown| {
        let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
        let mut lie: Attributes = [0; CHANNELS];
        lie[GROWN] = grown.code();
        let land = testland::land(&mut stage, &|_, _| 0.0, lie);
        let before = stage.objects.len();
        lay(&mut stage, &land, (Point::new(0.0, 0.0), 40.0), 9).expect("laid");
        stage.objects[before..]
            .iter()
            .filter_map(|object| match object.shape {
                Shape::Instance { pose, .. } => Some(mathf::hypot(pose.at.x, pose.at.z)),
                _ => None,
            })
            .collect::<alloc::vec::Vec<f64>>()
    };
    let grazed = laid(Grown::Grazed);
    assert!(grazed.len() > 100, "{} pats", grazed.len());
    assert!(
        grazed.iter().all(|&off| off <= 40.0),
        "a pat laid past its reach"
    );
    assert!(laid(Grown::Mown).is_empty(), "pats on a meadow");
}

/// A pat lies on the ground beneath the snow, and none is laid where the
/// snow lies deeper than it stands.
#[test]
fn pats_lie_beneath_the_snow_and_none_it_buries() {
    let laid = |depth: f64| {
        let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
        let mut lie: Attributes = [0; CHANNELS];
        lie[GROWN] = Grown::Grazed.code();
        lie[SNOW] = snow::kept(depth);
        let land = testland::land(&mut stage, &|_, _| 0.0, lie);
        let before = stage.objects.len();
        lay(&mut stage, &land, (Point::new(0.0, 0.0), 40.0), 9).expect("laid");
        let feet: alloc::vec::Vec<f64> = stage.objects[before..]
            .iter()
            .filter_map(|object| match object.shape {
                Shape::Instance { pose, .. } => Some(pose.at.y),
                _ => None,
            })
            .collect();
        (feet, land.grids.beneath_snow(&stage.fields, 0.0, 0.0))
    };
    let (dusted, ground) = laid(0.005);
    assert!(ground < -0.004, "the ground lies at {ground}");
    assert!(
        dusted.len() > 100,
        "{} pats through a dusting",
        dusted.len()
    );
    assert!(
        dusted.iter().all(|&foot| (foot - ground).abs() < 1e-9),
        "a pat lies on the snow"
    );
    assert!(laid(0.2).0.is_empty(), "a pat shows through deep snow");
}

/// Where more pats lie about the eye than the stage has room for, the
/// nearest are the ones kept, whichever side of the eye they lie.
#[test]
fn the_nearest_pats_are_kept_when_room_is_short() {
    let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
    let mut lie: Attributes = [0; CHANNELS];
    lie[GROWN] = Grown::Grazed.code();
    let land = testland::land(&mut stage, &|_, _| 0.0, lie);
    let about = (Point::new(0.0, 0.0), 40.0);
    let off = |pat: &Pat| mathf::hypot(pat.at.0, pat.at.1);
    let all = nearest(&stage, &land, about, (9, usize::MAX)).expect("pats");
    let kept = nearest(&stage, &land, about, (9, 60)).expect("pats");
    assert!(all.len() > 120, "{} pats", all.len());
    assert_eq!(kept.len(), 60);
    let farthest = kept.iter().map(off).fold(0.0, f64::max);
    assert_eq!(
        all.iter().filter(|pat| off(pat) <= farthest).count(),
        60,
        "a nearer pat was passed over"
    );
}
