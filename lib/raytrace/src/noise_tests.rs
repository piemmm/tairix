use super::*;

/// Points spread over a few lattice cells either side of the origin.
fn spread(count: u32) -> impl Iterator<Item = Vec3> {
    (0..count).map(|index| {
        let key = mix32(index);
        let at = |salt: u32| 12.0 * (unit(mix32(key ^ salt)) - 0.5);
        Vec3::new(at(1), at(2), at(3))
    })
}

#[test]
fn a_cell_is_the_floor_and_the_fraction_what_is_left() {
    assert_eq!(cell(2.25), (2, 0.25));
    assert_eq!(cell(0.0), (0, 0.0));
    assert_eq!(cell(-0.25), ((-1i32).cast_unsigned(), 0.75));
    assert_eq!(cell(-3.0), ((-3i32).cast_unsigned(), 0.0));
    for p in spread(500) {
        let (_, fraction) = cell(p.x);
        assert!((0.0..1.0).contains(&fraction), "{fraction} at {}", p.x);
    }
}

#[test]
fn gradient_noise_is_nought_on_the_lattice_and_bounded_between() {
    for i in -3..3 {
        for j in -3..3 {
            let lattice = Vec3::new(f64::from(i), f64::from(j), f64::from(i + j));
            assert!(noise3(lattice, 7).abs() < 1e-12);
            assert!(noise2(f64::from(i), f64::from(j), 7).abs() < 1e-12);
        }
    }
    for p in spread(4000) {
        assert!(noise3(p, 3).abs() <= 1.1, "{p:?}");
        assert!(noise2(p.x, p.z, 3).abs() <= 1.1, "{p:?}");
    }
}

#[test]
fn gradient_noise_is_smooth_and_differs_with_its_seed() {
    let mut differ = 0;
    for p in spread(500) {
        let step = Vec3::splat(1e-4);
        assert!((noise3(p, 5) - noise3(p + step, 5)).abs() < 1e-3, "{p:?}");
        assert!((noise2(p.x, p.z, 5) - noise2(p.x + 1e-4, p.z, 5)).abs() < 1e-3);
        if (noise3(p, 5) - noise3(p, 6)).abs() > 1e-6 {
            differ += 1;
        }
    }
    assert!(differ > 450, "{differ} of 500 differ under another seed");
}

#[test]
fn sums_of_octaves_stay_within_their_ranges() {
    for p in spread(2000) {
        let sum = fbm2(p.x, p.z, 9, (6, 0.5, 2.0));
        assert!(sum.abs() <= 1.1, "{sum}");
        let ridges = ridged2(p.x, p.z, 9, 6);
        assert!((0.0..=1.0).contains(&ridges), "{ridges}");
        let veins = turbulence3(p, 9, 4);
        assert!((0.0..=1.1).contains(&veins), "{veins}");
    }
}

#[test]
fn a_pattern_sums_only_the_octaves_coarser_than_its_footprint() {
    assert_eq!(octaves_within(1.0), 1);
    assert_eq!(octaves_within(0.2), 2);
    assert_eq!(octaves_within(0.0), MAX_OCTAVES);
    let mut last = MAX_OCTAVES;
    for step in 0..40 {
        let octaves = octaves_within(f64::from(step) * 0.01);
        assert!(octaves <= last, "fewer octaves the wider the footprint");
        last = octaves;
    }
}

#[test]
fn cellular_noise_finds_the_nearest_features_and_their_walls() {
    for p in spread(1000) {
        for found in [cells2(p.x, p.z, 11, 0.85), cells3(p, 11, 1.0)] {
            assert!(found.nearest <= found.second, "{found:?}");
            assert!(found.wall() >= 0.0);
        }
    }
    // Unjittered, each feature is its cell's middle.
    let middle = cells2(3.5, -1.5, 11, 0.0);
    assert!(middle.nearest < 1e-12);
    assert!((middle.second - 1.0).abs() < 1e-12);
    // The whole of one cell answers to the same feature.
    let id = cells2(3.45, -1.55, 11, 0.0).id;
    assert_eq!(cells2(3.3, -1.7, 11, 0.0).id, id);
    assert_ne!(cells2(4.5, -1.5, 11, 0.0).id, id);
}

#[test]
fn smoothstep_runs_from_nought_to_one_between_its_edges() {
    for (edges, at, expected) in [
        ((0.0, 1.0), -1.0, 0.0),
        ((0.0, 1.0), 2.0, 1.0),
        ((0.0, 1.0), 0.5, 0.5),
        // Falling edges reverse it.
        ((1.0, 0.0), 0.25, 0.84375),
        // Where the edges meet, a step.
        ((1.0, 1.0), 1.0, 1.0),
        ((1.0, 1.0), 0.9, 0.0),
    ] {
        let value = smoothstep(edges.0, edges.1, at);
        assert!(
            (value - expected).abs() < 1e-12,
            "{edges:?} at {at}: {value}"
        );
    }
}
