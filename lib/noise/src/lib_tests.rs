use tairix_mix::{mix32, unit};
use tairix_util::space::Vec3;

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
        let billows = billow2(p.x, p.z, 9, 6);
        assert!((0.0..=1.0).contains(&billows), "{billows}");
        let veins = turbulence3(p, 9, 4);
        assert!((0.0..=1.1).contains(&veins), "{veins}");
    }
}

/// Sampled finely enough, the resolved sum is the plain one bit for bit;
/// sampled coarsely, an octave too fine to hold settles to nought while its
/// weight still counts, so the pattern keeps its scale; and none survives a
/// footprint wider than every lattice.
#[test]
fn a_resolved_sum_keeps_only_the_octaves_its_footprint_holds_at_their_weight() {
    let shape = (4, 0.5, 2.0);
    for p in spread(500) {
        assert_eq!(
            fbm2_resolved(p.x, p.z, 7, shape, 0.0).to_bits(),
            fbm2(p.x, p.z, 7, shape).to_bits()
        );
        // Lattices of 1, ½, ¼ and ⅛: a footprint of ⅛ holds the first two
        // whole, the third not at all.
        let held = fbm2_resolved(p.x, p.z, 7, shape, 0.125);
        let coarse = fbm2(p.x, p.z, 7, (2, 0.5, 2.0)) * (1.5 / 1.875);
        assert!((held - coarse).abs() < 1e-12, "{held} against {coarse}");
        assert_eq!(
            fbm2_resolved(p.x, p.z, 7, shape, 0.6).to_bits(),
            0.0f64.to_bits()
        );
    }
    assert!(resolved(0.1, 0.25) > 0.0 && resolved(0.1, 0.25) < 1.0);
    assert_eq!(resolved(0.1, 0.2).to_bits(), 0.0f64.to_bits());
    assert_eq!(resolved(0.1, 0.4).to_bits(), 1.0f64.to_bits());
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

/// Sparing the cells whose features cannot be nearer finds just what a
/// search of every neighbouring cell finds.
#[test]
fn cellular_noise_finds_what_a_search_of_every_cell_finds() {
    let every = |p: Vec3, seed: u32, jitter: f64| {
        let ((cx, fx), (cy, fy), (cz, fz)) = (cell(p.x), cell(p.y), cell(p.z));
        let mut found = Cells::NONE;
        let offset = |d: u32| if d == u32::MAX { -1.0 } else { f64::from(d) };
        for dz in [u32::MAX, 0, 1] {
            for dy in [u32::MAX, 0, 1] {
                for dx in [u32::MAX, 0, 1] {
                    let id = hash3(
                        cx.wrapping_add(dx),
                        cy.wrapping_add(dy),
                        cz.wrapping_add(dz),
                        seed,
                    );
                    let jittered = |salt: u32, d: u32, f: f64| {
                        offset(d) + 0.5 + jitter * (unit(mix32(id ^ salt)) - 0.5) - f
                    };
                    record(
                        &mut found,
                        Vec3::new(
                            jittered(JITTER[0], dx, fx),
                            jittered(JITTER[1], dy, fy),
                            jittered(JITTER[2], dz, fz),
                        ),
                        id,
                    );
                }
            }
        }
        (
            mathf::sqrt(found.nearest),
            mathf::sqrt(found.second),
            found.id,
        )
    };
    for jitter in [0.0, 0.5, 0.85, 0.9, 1.0] {
        for p in spread(4000) {
            let found = cells3(p, 23, jitter);
            let (nearest, second, id) = every(p, 23, jitter);
            assert_eq!(
                (found.nearest, found.second, found.id),
                (nearest, second, id),
                "{p:?} jittered {jitter}"
            );
        }
    }
}

/// A sparse search keeps to the features its filter keeps, finds the
/// nearest of those, and finds none where it keeps none.
#[test]
fn a_sparse_search_finds_only_the_features_it_keeps() {
    let kept = |id: u32| id.is_multiple_of(3);
    for p in spread(2000) {
        let all = cells3(p, 41, 0.9);
        let some = cells3_among(p, 41, 0.9, kept);
        assert!(!some.nearest.is_finite() || kept(some.id), "{p:?}");
        assert!(some.nearest >= all.nearest, "{p:?}");
        assert!(cells3_among(p, 41, 0.9, |_| false).nearest.is_infinite());
    }
}

/// A plane cell's id says nothing of where its feature stands, so a field
/// shaded by its cells' ids does not follow their places.
#[test]
fn a_cells_id_is_drawn_apart_from_where_it_stands() {
    let (mut moments, count) = ([0.0f64; 3], 20_000u32);
    // Spread wide, so nearly every place finds a feature of its own.
    for p in spread(count).map(|p| p * 400.0) {
        let found = cells2(p.x, p.z, 7, 1.0);
        let feature = p.x + found.toward.x;
        let offset = feature - mathf::floor(feature) - 0.5;
        let shade = unit(found.id) - 0.5;
        moments[0] += offset * shade;
        moments[1] += offset * offset;
        moments[2] += shade * shade;
    }
    let correlation = moments[0] / mathf::sqrt(moments[1] * moments[2]);
    assert!(correlation.abs() < 0.05, "{correlation}");
}

/// Gradient noise rises no faster than its stated slope whatever gradients
/// its corners pick: at a place and along a way, each corner's part of the
/// rise is that of the gradient it picks, so the steepest any choice makes
/// is each corner's steepest pick, summed, searched over the places of half
/// a cell, which its symmetries make every cell, and over every way.
#[test]
fn gradient_noise_rises_no_faster_than_its_slope() {
    let slope = |t: f64| 30.0 * t * t * (t - 1.0) * (t - 1.0);
    let corner = |bit: u32, t: f64| {
        if bit == 1 {
            (fade(t), slope(t))
        } else {
            (1.0 - fade(t), -slope(t))
        }
    };
    let steps = 8u32;
    let at = |step: u32| 0.5 * f64::from(step) / f64::from(steps);
    let places = (0..=steps).flat_map(|east| {
        (0..=steps).flat_map(move |up| (0..=steps).map(move |north| [at(east), at(up), at(north)]))
    });
    let ways = (0..1500u32).map(|index| {
        let height = 1.0 - 2.0 * (f64::from(index) + 0.5) / 1500.0;
        let around = 2.399_963_229_728_653 * f64::from(index);
        let level = mathf::sqrt(1.0 - height * height);
        [
            level * mathf::cos(around),
            height,
            level * mathf::sin(around),
        ]
    });
    let mut space = 0.0f64;
    for place in places {
        for way in ways.clone() {
            let rise: f64 = (0..8u32)
                .map(|corner_bits| {
                    let picks =
                        [0, 1, 2].map(|axis| corner((corner_bits >> axis) & 1, place[axis]));
                    let weight = picks[0].0 * picks[1].0 * picks[2].0;
                    let along: f64 = (0..3)
                        .map(|axis| {
                            let others: f64 = (0..3)
                                .filter(|&other| other != axis)
                                .map(|other| picks[other].0)
                                .product();
                            way[axis] * picks[axis].1 * others
                        })
                        .sum();
                    let toward =
                        [0, 1, 2].map(|axis| place[axis] - f64::from((corner_bits >> axis) & 1));
                    let reach = [0, 1, 2].map(|axis| toward[axis] * along + weight * way[axis]);
                    GRADIENTS
                        .iter()
                        .map(|gradient| {
                            gradient[0] * reach[0] + gradient[1] * reach[1] + gradient[2] * reach[2]
                        })
                        .fold(f64::NEG_INFINITY, mathf::fmax)
                })
                .sum();
            space = mathf::fmax(space, rise);
        }
    }
    assert!((3.7..NOISE_SLOPE).contains(&space), "space rises {space}");
    for p in spread(4000) {
        let step = 1e-6;
        let rise = |a: f64, b: f64| (b - a).abs() / step;
        assert!(rise(noise2(p.x, p.z, 3), noise2(p.x + step, p.z, 3)) <= NOISE_SLOPE);
        assert!(rise(noise2(p.x, p.z, 3), noise2(p.x, p.z + step, 3)) <= NOISE_SLOPE);
    }
}

/// Each octave's turn is the last's turned once more by the same angle, a
/// rotation of unit length.
#[test]
fn each_octaves_turn_is_the_last_turned_once_more() {
    let (step, mut last) = (turn(1.0, 0.0, 1), turn(1.0, 0.0, 0));
    for octave in 1..8 {
        let (x, z) = turn(1.0, 0.0, octave);
        let expected = (
            last.0 * step.0 - last.1 * step.1,
            last.0 * step.1 + last.1 * step.0,
        );
        assert!(
            (x - expected.0).abs() < 1e-12 && (z - expected.1).abs() < 1e-12,
            "{octave}"
        );
        assert!((mathf::hypot(x, z) - 1.0).abs() < 1e-12, "{octave}");
        last = (x, z);
    }
    assert_eq!(turn(1.0, 0.0, 8), turn(1.0, 0.0, 0));
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

/// Noise is continuous across every lattice wall, whatever its seed: either
/// side of a wall reads the same gradient there, so no pattern drawn from it
/// shows the lattice as seams.
#[test]
fn noise_is_continuous_across_every_lattice_wall() {
    let mut worst: f64 = 0.0;
    for seed in [0u32, 1, 7, 0xdead_beef] {
        for wall in 1..200 {
            for along in 0..20 {
                let (a, w) = (f64::from(along) * 0.37 + 0.1, f64::from(wall));
                let across_z = |e: f64| noise2(a, w + e, seed);
                let across_x = |e: f64| noise2(w + e, a, seed);
                worst = mathf::fmax(
                    mathf::fmax(worst, (across_z(-1e-9) - across_z(1e-9)).abs()),
                    (across_x(-1e-9) - across_x(1e-9)).abs(),
                );
                for axis in [Vec3::new(1.0, 0.0, 0.0), Vec3::UP, Vec3::new(0.0, 0.0, 1.0)] {
                    let p = Vec3::new(a, 0.3 + a, 0.7 * a)
                        + axis * (w - (Vec3::new(a, 0.3 + a, 0.7 * a)).dot(axis));
                    let jump =
                        (noise3(p - axis * 1e-9, seed) - noise3(p + axis * 1e-9, seed)).abs();
                    worst = mathf::fmax(worst, jump);
                }
            }
        }
    }
    assert!(worst < 1e-6, "noise jumps {worst} across a lattice wall");
}

/// Billowed octaves lie about a quarter on average, troughs creased where each
/// octave crosses nought: so a land's dunes, read as twice them less a half,
/// neither raise nor lower it.
#[test]
fn billowed_octaves_lie_about_a_quarter() {
    let count = 4000;
    let mean = spread(count).map(|p| billow2(p.x, p.z, 5, 4)).sum::<f64>() / f64::from(count);
    assert!((mean - 0.25).abs() < 0.02, "{mean}");
}
