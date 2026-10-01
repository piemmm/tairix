//! Host tests of rocks: they are closed, face outward, keep to their size,
//! and a seed shapes the same rock every time.

use super::*;
use crate::vector::Ray;

const HABIT: Habit = Habit {
    squash: 0.7,
    fractures: 4,
};

#[test]
fn a_rock_is_met_from_outside_whichever_way_it_is_looked_at() {
    let stone = rock(HABIT, 3, 17).expect("a rock");
    let bounds = stone.bounds();
    assert!(
        bounds.max.x - bounds.min.x > 1.0 && bounds.max.x - bounds.min.x < 3.0,
        "{bounds:?}"
    );
    assert!(
        bounds.max.y - bounds.min.y < bounds.max.x - bounds.min.x,
        "squashed: {bounds:?}"
    );
    for step in 0..200u32 {
        let turn = f64::from(step) * 2.399_963;
        let rise = 1.0 - 2.0 * (f64::from(step) + 0.5) / 200.0;
        let level = mathf::sqrt(1.0 - rise * rise);
        let toward = Vec3::new(level * mathf::cos(turn), rise, level * mathf::sin(turn));
        let ray = Ray::new(toward * 5.0, -toward);
        let hit = stone
            .intersect(&ray, 0.0, f64::INFINITY)
            .expect("a closed rock is met");
        assert!(
            hit.normal.dot(ray.dir) < 0.0,
            "met from outside at {toward:?}"
        );
        assert!(
            hit.shading.dot(ray.dir) < 0.0,
            "shaded from outside at {toward:?}"
        );
        assert_eq!(hit.material, Some(3));
    }
}

#[test]
fn a_seed_shapes_the_same_rock_and_another_seed_another() {
    let describe = |stone: &Prototype| alloc::format!("{:?}", stone.bounds());
    assert_eq!(
        describe(&rock(HABIT, 0, 5).expect("a rock")),
        describe(&rock(HABIT, 0, 5).expect("a rock"))
    );
    assert_ne!(
        describe(&rock(HABIT, 0, 5).expect("a rock")),
        describe(&rock(HABIT, 0, 6).expect("a rock"))
    );
}
