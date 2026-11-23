use alloc::vec::Vec;

use super::*;

#[path = "recorded.rs"]
mod recorded;

fn lines(curve: &[(i32, i32); 4], flatness: u32) -> Result<Vec<(i32, i32)>, TooDeep> {
    let [p0, p1, p2, p3] = curve.map(|(x, y)| Point::new(x, y));
    let mut ends = Vec::from([(p0.x, p0.y)]);
    flatten(p0, p1, p2, p3, Threshold::of(flatness), |p| {
        ends.push((p.x, p.y));
        Ok::<(), TooDeep>(())
    })?;
    Ok(ends)
}

#[test]
fn every_recorded_curve_flattens_to_the_points_genuine_risc_os_gave() {
    for (curve, flatness, points) in recorded::CASES {
        assert_eq!(
            lines(curve, *flatness).expect("flattens").as_slice(),
            *points,
            "{curve:?} at flatness {flatness}"
        );
    }
}

#[test]
fn the_threshold_is_four_times_the_flatness_held_in_a_word() {
    assert_eq!(
        Threshold::of(0),
        Threshold::of(u32::try_from(DEFAULT).expect("a word"))
    );
    assert_eq!(
        Threshold::of(0x4000_0000),
        Threshold::of(0),
        "a word's carry leaves nought"
    );
    assert_eq!(Threshold::of(0x8000_0000), Threshold::of(0));
    let curve = [(0, 0), (0, 25600), (25600, 25600), (25600, 0)];
    assert_eq!(
        lines(&curve, 0x1FFF_FFFF).expect("flat").len(),
        2,
        "the largest positive"
    );
    for wraps_negative in [0x7FFF_FFFF, 0x3FFF_FFFF, 0x2000_0000, u32::MAX] {
        assert_eq!(
            lines(&curve, wraps_negative),
            Err(TooDeep),
            "{wraps_negative:#X}"
        );
    }
}

#[test]
fn a_curve_straddling_the_default_flatness_is_split_past_it_alone() {
    // The first control point sits 3y + m units off its third of the chord.
    let at = |y1: i32, m: i32| lines(&[(0, 0), (10000, y1), (20000, 0), (30000 + m, 0)], 0);
    assert_eq!(at(682, 2).expect("flat").len(), 2, "2048 is flat");
    assert_eq!(at(683, 0).expect("split").len(), 3, "2049 is not");
}

#[test]
fn halving_reaches_any_curve_within_a_word() {
    let widest = [
        (i32::MIN, i32::MIN),
        (i32::MIN, i32::MAX),
        (i32::MAX, i32::MAX),
        (i32::MAX, i32::MIN),
    ];
    let ends = lines(&widest, 1).expect("ends");
    assert_eq!(*ends.last().expect("an end"), (i32::MAX, i32::MIN));
}
