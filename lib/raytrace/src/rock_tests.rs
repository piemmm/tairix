//! Host tests of rocks: they are closed, face outward, keep to their
//! proportions, a seed shapes the same rock every time, wear takes stone away
//! and never adds it, rounding a broken stone the more it is worn, and slate
//! splits into flat plates.

use alloc::vec::Vec;

use super::*;
use crate::vector::Ray;

const HABIT: Habit = Habit {
    squash: 0.7,
    elongation: 0.85,
    fractures: 4,
    cleaved: false,
};

const SLATE: Habit = Habit {
    squash: 0.2,
    elongation: 0.7,
    fractures: 4,
    cleaved: true,
};

#[test]
fn a_rock_is_met_from_outside_whichever_way_it_is_looked_at() {
    for wear in [0.0, 1.5] {
        let stone = rock(HABIT, wear, 17).expect("a rock").whole();
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
                .intersect(&ray, (0.0, f64::INFINITY), None)
                .expect("a closed rock is met");
            assert!(
                hit.normal.dot(ray.dir) < 0.0,
                "met from outside at {toward:?}"
            );
            assert!(
                hit.shading.dot(ray.dir) < 0.0,
                "shaded from outside at {toward:?}"
            );
            assert_eq!(hit.material, None, "made in what it is placed in");
        }
    }
}

#[test]
fn a_seed_shapes_the_same_rock_and_another_seed_another() {
    let describe = |stone: &Prototype| alloc::format!("{:?}", stone.bounds());
    assert_eq!(
        describe(&rock(HABIT, 0.5, 5).expect("a rock").whole()),
        describe(&rock(HABIT, 0.5, 5).expect("a rock").whole())
    );
    assert_ne!(
        describe(&rock(HABIT, 0.5, 5).expect("a rock").whole()),
        describe(&rock(HABIT, 0.5, 6).expect("a rock").whole())
    );
}

/// The vertices of a rock of `habit` worn by `wear` under `seed`, and the
/// faces between them.
fn mesh(habit: Habit, wear: f64, seed: u64) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let mut wearing = Wearing::new(habit, wear, seed).expect("a rock");
    while !wearing.step().expect("worn") {}
    let faces = wearing.faces.clone();
    (
        proportioned(wearing.vertices, habit).expect("proportioned"),
        faces,
    )
}

/// The faces and vertices of a sphere of `levels` subdivisions, each vertex
/// `radius` along its direction.
fn sphere(levels: u32, radius: &dyn Fn(Vec3) -> f64) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let (mut directions, mut faces) = icosahedron().expect("an icosahedron");
    for _ in 0..levels {
        (directions, faces) = subdivide(&directions, &faces).expect("subdivided");
    }
    let vertices = directions
        .iter()
        .map(|&direction| direction * radius(direction))
        .collect();
    (vertices, faces)
}

fn face_normal(vertices: &[Vec3], [a, b, c]: [u32; 3]) -> Vec3 {
    let (pa, pb, pc) = (
        vertices[a as usize],
        vertices[b as usize],
        vertices[c as usize],
    );
    (pb - pa).cross(pc - pa).normalized()
}

/// The sharpest edge of a mesh: the greatest angle, in radians, between the
/// faces either side of any of its edges.
fn sharpest(vertices: &[Vec3], faces: &[[u32; 3]]) -> f64 {
    let mut sides: Vec<((u32, u32), usize)> = Vec::new();
    for (index, &[a, b, c]) in faces.iter().enumerate() {
        for (p, q) in [(a, b), (b, c), (c, a)] {
            sides.push(((p.min(q), p.max(q)), index));
        }
    }
    sides.sort_unstable();
    sides
        .windows(2)
        .filter(|pair| pair[0].0 == pair[1].0)
        .map(|pair| {
            let (one, other) = (
                face_normal(vertices, faces[pair[0].1]),
                face_normal(vertices, faces[pair[1].1]),
            );
            mathf::acos(one.dot(other).clamp(-1.0, 1.0))
        })
        .fold(0.0, f64::max)
}

#[test]
fn a_rock_keeps_to_its_proportions() {
    for (habit, seed) in [(HABIT, 1), (SLATE, 2), (HABIT, 9)] {
        for wear in [0.0, 0.7, 3.0] {
            let (vertices, _) = mesh(habit, wear, seed);
            let extent = |axis: fn(&Vec3) -> f64| {
                vertices
                    .iter()
                    .map(|vertex| axis(vertex).abs())
                    .fold(0.0, f64::max)
            };
            assert!((extent(|v| v.x) - 1.0).abs() < 1e-9);
            assert!((extent(|v| v.y) - habit.squash).abs() < 1e-9);
            assert!((extent(|v| v.z) - habit.elongation).abs() < 1e-9);
        }
    }
}

/// Each vertex of a lumpy stone moves in along its own ray from the stone's
/// middle or not at all, carrying the surface in, so a step of wear only
/// ever takes stone away; and a sphere, curved alike all over, wears down
/// evenly and stays a sphere.
#[test]
fn wear_takes_stone_away_and_never_adds_it() {
    let (rough, faces) = sphere(3, &|direction| 1.0 + 0.3 * noise3(direction * 4.0, 7));
    let normals = normals_of(&rough, &faces).expect("normals");
    let mut worn = rough.clone();
    let mut surface = Surface::new(rough.len(), &faces).expect("a surface");
    surface.refresh(&rough, &faces).expect("refreshed");
    let shortest = shortest_edge(&rough, &faces).expect("an edge");
    let step = STABLE_STEP * shortest * shortest;
    surface.flow(&mut worn, step).expect("worn");
    let mut moved = 0;
    for ((before, after), normal) in rough.iter().zip(&worn).zip(&normals) {
        let normal = Vec3::new(
            f64::from(normal[0]),
            f64::from(normal[1]),
            f64::from(normal[2]),
        );
        let shift = *after - *before;
        assert!(shift.dot(normal) <= 1e-12, "moved out by {shift:?}");
        if shift.length() > 0.0 {
            moved += 1;
            let ray = before.normalized();
            assert!(shift.dot(ray) < 0.0, "moved off its ray outward");
            assert!(shift.cross(ray).length() < 1e-9 * (1.0 + shift.length()));
        }
    }
    assert!(
        moved > 0 && moved < rough.len(),
        "{moved} of {} moved",
        rough.len()
    );
    let (mut round, faces) = sphere(3, &|_| 1.0);
    let mut surface = Surface::new(round.len(), &faces).expect("a surface");
    for _ in 0..50 {
        surface.refresh(&round, &faces).expect("refreshed");
        surface.flow(&mut round, 1e-3).expect("worn");
    }
    let radii: Vec<f64> = round.iter().map(|vertex| vertex.length()).collect();
    let (least, most) = radii
        .iter()
        .fold((f64::INFINITY, 0.0f64), |(least, most), &r| {
            (least.min(r), most.max(r))
        });
    assert!(most < 1.0 && most - least < 0.02 * most, "{least}..{most}");
}

#[test]
fn the_further_a_broken_stone_is_carried_the_rounder_it_wears() {
    for seed in [3, 11, 23] {
        let edges: Vec<f64> = [0.0, 0.3, 1.0, 3.0]
            .iter()
            .map(|&wear| {
                let (vertices, faces) = mesh(HABIT, wear, seed);
                sharpest(&vertices, &faces)
            })
            .collect();
        assert!(
            edges.windows(2).all(|pair| pair[1] < pair[0]),
            "seed {seed}: {edges:?}"
        );
        // A fresh break leaves an edge sharper than 40°; a stone worn round
        // keeps none past 20°, and one carried three times as far none past
        // 10°.
        assert!(edges[0] > 40f64.to_radians(), "seed {seed}: {edges:?}");
        assert!(edges[2] < 20f64.to_radians(), "seed {seed}: {edges:?}");
        assert!(edges[3] < 10f64.to_radians(), "seed {seed}: {edges:?}");
    }
}

#[test]
fn slate_splits_into_a_flat_plate_with_angular_edges() {
    for seed in [4, 8, 15] {
        let wear = Lithology::Slate.most_wear();
        let (vertices, faces) = mesh(SLATE, wear, seed);
        let level = faces
            .iter()
            .filter(|&&face| face_normal(&vertices, face).y.abs() > 0.995)
            .count();
        assert!(
            level * 3 > faces.len(),
            "seed {seed}: {level} of {} faces on its cleavage",
            faces.len()
        );
        assert!(
            sharpest(&vertices, &faces) > 45f64.to_radians(),
            "seed {seed}: its edges stay sharp"
        );
    }
    let mut dice = NonCryptoRng::seed_from_u64(1);
    for _ in 0..50 {
        let habit = Lithology::Slate.habit(&mut dice);
        assert!(habit.cleaved && habit.squash <= 0.28 && habit.squash < habit.elongation);
        let granite = Lithology::Granite.habit(&mut dice);
        assert!(!granite.cleaved && granite.squash <= 0.85 && granite.elongation <= 0.95);
    }
}

/// However much wear a stone is asked to take it takes at most the most
/// any does, none for none, and its work comes in units of a bounded number
/// of steps.
#[test]
fn wear_is_bounded_and_done_a_unit_at_a_time() {
    let left = |wear: f64| Wearing::new(HABIT, wear, 1).expect("a rock").left;
    assert!(left(0.0) == 0.0 && left(-1.0) == 0.0 && left(f64::NAN) == 0.0);
    assert!((left(1.0) - TIME_PER_WEAR).abs() < 1e-12);
    assert!((left(1e9) - TIME_PER_WEAR * MOST_WEAR).abs() < 1e-12);
    assert!((left(f64::INFINITY) - TIME_PER_WEAR * MOST_WEAR).abs() < 1e-12);
    let mut wearing = Wearing::new(HABIT, 2.0, 1).expect("a rock");
    let before = wearing.left;
    assert!(!wearing.step().expect("a unit"));
    let taken = before - wearing.left;
    assert!((taken - f64::from(WEAR_UNIT) * wearing.step).abs() < 1e-12);
    let mut fresh = Wearing::new(HABIT, 0.0, 1).expect("a rock");
    assert!(fresh.step().expect("a unit"));
}
