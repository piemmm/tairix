use super::*;
use crate::prototype::point;
use crate::vector::Ray;

/// Where a ray straight down from high over `(x, z)` meets `prototype`, and
/// the height it meets it at.
fn top_at(prototype: &Prototype, (x, z): (f64, f64)) -> Option<f64> {
    let down = Ray::new(Vec3::new(x, 50.0, z), -Vec3::UP);
    prototype
        .intersect(&down, 1e-9, 100.0)
        .map(|hit| 50.0 - hit.t)
}

#[test]
fn a_fallen_trunk_lies_along_the_ground_narrowing_toward_its_crown() {
    for seed in 0..6u64 {
        let (length, radius) = (9.0, 0.35);
        let log = log(length, radius, (0, 1, seed % 2 == 0), seed)
            .expect("a log")
            .whole();
        let bounds = log.bounds();
        assert!(
            (0.9 * length..1.3 * length).contains(&bounds.max.z),
            "{seed}: {:?}",
            bounds.max
        );
        // Lying, not standing: its top about its thickness above the ground,
        // and lower toward where its crown was.
        let foot = top_at(&log, (0.0, 0.1 * length)).expect("met near its foot");
        let far = top_at(&log, (0.0, 0.7 * length));
        assert!(
            (1.2 * radius..2.2 * radius).contains(&foot),
            "{seed}: {foot}"
        );
        if let Some(far) = far {
            assert!(far < foot, "{seed}: narrows, {far} beyond {foot}");
        }
        assert!(
            bounds.max.y < 2.0 * length * 0.3 + 3.0 * radius,
            "{seed}: {:?}",
            bounds.max
        );
    }
}

#[test]
fn a_windthrown_trunk_carries_its_roots_and_a_snapped_one_ends_in_splinters() {
    let radius = 0.4;
    let thrown = log(8.0, radius, (0, 1, true), 3)
        .expect("a log")
        .whole()
        .bounds();
    let snapped = log(8.0, radius, (0, 1, false), 3)
        .expect("a log")
        .whole()
        .bounds();
    // The plate of roots spreads far wider than the trunk's own girth.
    assert!(thrown.max.x - thrown.min.x > 3.0 * radius, "{thrown:?}");
    assert!(
        thrown.max.y > 2.5 * radius,
        "the plate stands on edge: {thrown:?}"
    );
    // Splinters point back past its foot, and no further out than the trunk.
    assert!(snapped.min.z < -0.1 * radius, "{snapped:?}");
    assert!(
        snapped.max.y < 2.2 * radius + 8.0 * 0.4 * 2.0,
        "{snapped:?}"
    );
}

/// A trunk breaks torn, not rounded: looking back along it from beyond
/// either broken end a ray meets the torn wood of the break or the fibres
/// bristling from it, never a rounded cap of bark, and the fibres stand out
/// past the end the trunk itself reaches.
#[test]
fn a_broken_end_is_torn_wood_bristling_with_fibres() {
    let (length, radius) = (6.0, 0.25);
    for seed in 0..4u64 {
        let log = log(length, radius, (0, 1, false), seed)
            .expect("a log")
            .whole();
        let woods = log
            .parts()
            .iter()
            .filter(|part| match part {
                Part::Facet(facet) => facet.material == Some(1),
                Part::Tube(tube) => tube.material == 1,
                Part::Leaf(_) => false,
            })
            .count();
        assert!(
            woods > 2 * (3 * BREAK_SIDES as usize + FIBRES.0 as usize),
            "{seed}: {woods} parts of torn wood"
        );
        // The trunk's segments come first, foot to crown.
        let segment = |index: usize| match log.parts().get(index) {
            Some(Part::Tube(tube)) => (point(tube.a), point(tube.b)),
            _ => panic!("{seed}: segment {index} is no tube"),
        };
        let ((foot, second), (before, tip)) = (segment(0), segment(LOG_SEGMENTS as usize - 1));
        for (end, out) in [
            (foot, (foot - second).normalized()),
            (tip, (tip - before).normalized()),
        ] {
            let hit = log
                .intersect(&Ray::new(end + out * 3.0, -out), 1e-9, 10.0)
                .expect("the break is met");
            assert_eq!(hit.material, Some(1), "{seed}: torn wood, not bark");
        }
        assert!(
            log.bounds().min.z < foot.z - 0.2 * radius,
            "{seed}: fibres past its foot"
        );
    }
}

#[test]
fn a_stump_stands_on_its_roots_sawn_flat_or_snapped_in_splinters() {
    let (height, radius) = (0.8, 0.3);
    let sawn = stump(height, radius, (Top::Sawn, 0, 1), 5)
        .expect("a stump")
        .whole();
    let face = top_at(&sawn, (0.0, 0.0)).expect("met at its middle");
    assert!((face - height).abs() < 0.08, "the sawn face: {face}");
    let snapped = stump(height, radius, (Top::Snapped, 0, 1), 5)
        .expect("a stump")
        .whole();
    assert!(
        snapped.bounds().max.y > height + 0.2 * radius,
        "splinters above the break"
    );
    for stump in [&sawn, &snapped] {
        let bounds = stump.bounds();
        assert!(bounds.min.y < 0.0, "its roots reach into the ground");
        assert!(
            bounds.max.x - bounds.min.x > 3.0 * radius,
            "its roots flare"
        );
    }
}
