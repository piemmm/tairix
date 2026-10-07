use super::*;

/// A plant reaches as far as its limbs do, their thickness and all, and a
/// bent limb is built piece by piece along its stem.
#[test]
fn a_plant_reaches_as_far_as_its_limbs() {
    let mut growing = Growing::with_room(4, 0).expect("room");
    let bent = [
        Vec3::ZERO,
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::new(0.5, 1.5, 0.0),
    ];
    growing.limb(&bent, (0.04, 0.02), (1, 7)).expect("a limb");
    growing.reaching(Vec3::new(0.0, 0.2, -0.8), 0.0);
    let (building, top, reach) = growing.finish().expect("built");
    assert_eq!(building.parts().len(), 2);
    assert!((top - 1.52).abs() < 1e-9, "it stands {top} tall");
    assert!((reach - 0.8).abs() < 1e-9, "it reaches {reach}");
    let Part::Tube(upper) = building.parts()[1] else {
        panic!("a limb");
    };
    assert!(
        (f64::from(upper.stem[0]) - 1.0).abs() < 1e-6,
        "its second piece starts {} along its stem",
        upper.stem[0]
    );
}
