//! Host tests of the buildings a scene sets out: a row of arches keeps the
//! ground clear beneath its bays as well as under its piers.

use super::*;
use crate::detail::Detail;

#[test]
fn a_row_of_arches_keeps_its_bays_clear_as_well_as_its_piers() {
    let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
    let stone = stage
        .material(Material::new(
            Pigment::Solid(Vec3::splat(0.6)),
            Finish::Matte,
        ))
        .expect("a stone");
    let (bays, span, pier, heading) = (4, 9.0, 1.2, 0.4);
    arches(
        &mut stage,
        (Vec3::ZERO, heading),
        (bays, span),
        (pier, 8.0),
        stone,
    )
    .expect("arches");
    let along = direction(heading, FRAC_PI_2, 0.0);
    let across = Vec3::UP.cross(along);
    let row = span * f64::from(bays);
    for step in 0..=144u32 {
        let at = along * (f64::from(step) / 144.0 * row);
        // Across the masonry: its piers a pier either side, its arches less.
        for side in [-0.9, 0.0, 0.9] {
            let place = at + across * (side * pier);
            assert!(
                !stage.clear((place.x, place.z), 0.0),
                "a tree could stand beneath the arches at {place:?}"
            );
        }
    }
    let beside = along * (0.5 * span) + across * (4.0 * pier);
    assert!(
        stage.clear((beside.x, beside.z), 0.0),
        "the ground beside the row is free"
    );
}
