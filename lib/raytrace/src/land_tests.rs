//! Host tests of building a land: it lies as its grids hold it, its rivers
//! run downhill, its road keeps out of their water but where it bridges
//! them, and a land with no water standing on it keeps no water grid.

use alloc::vec::Vec;

use tairix_parallel::Threaded;

use super::*;
use crate::terrain::{Landform, Terrain};

/// Rolling hills `reach` either way of the middle, drained by rivers if
/// `rivers`, a track crossing them: coarse enough to build in a moment.
fn plan(rivers: bool, seed: u32) -> Plan {
    let reach = 1500.0;
    Plan {
        relief: Terrain {
            form: Landform::Hills {
                scale: 400.0,
                height: 60.0,
                seed,
            },
            datum: 0.0,
            centre: (0.0, 0.0),
            radius: reach,
            rim: Some(0.0),
            tilt: (0.01, 0.004),
            clearing: None,
        },
        reach,
        sea: None,
        wear: Wear {
            passes: 8,
            incision: 4.0e-4,
            creep: 0.09,
            repose: 0.9,
            infill: 0.3,
            strata: None,
        },
        rivers: rivers.then_some(Rivers {
            catchment: 2.0e5,
            width: 3.0,
            meander: 1.2,
        }),
        road: Some(Roadway {
            surface: Surface::Track,
            width: 3.0,
            heading: 0.7,
        }),
        roughness: 1.0,
        ridges: 0.4,
        droplets: 0.02,
        cells: (128, 256),
        nests: [
            Some(Nest {
                reach: 300.0,
                cells: 256,
                droplets: 0.04,
            }),
            None,
        ],
        horizon: None,
        snow_line: None,
        pond: None,
        growth: 1.0,
        seed: seed ^ 0x55,
    }
}

/// `plan` built into grids of its own, sited at the land's middle: the land,
/// its grids, its rivers, and whether water stands anywhere on it.
fn built(plan: Plan) -> (Land, Vec<Heightfield>, Courses, bool) {
    let origin = (
        plan.relief.centre.0 - plan.reach,
        plan.relief.centre.1 - plan.reach,
    );
    let placeholder = || Heightfield::new(1, origin, 1.0, false).expect("a grid");
    let mut fields = alloc::vec![placeholder(), placeholder(), placeholder()];
    let grids = Fields {
        far: 0,
        nests: [Some(1), None],
        water: Some(2),
        horizon: None,
    };
    let mut build = Build::new(plan, grids).expect("a build");
    let runner = Threaded::new(8);
    loop {
        let done = build.step(&mut fields, &runner).expect("builds");
        if done && build.waiting() {
            build.site((0.0, 0.0), (0.0, 0.0), None).expect("sited");
        } else if done {
            break;
        }
    }
    let watered = build.watered();
    let rivers = core::mem::replace(&mut build.rivers, Courses::none());
    (build.finish().expect("a land"), fields, rivers, watered)
}

#[test]
fn a_built_land_lies_as_its_grids_hold_it() {
    let (land, fields, _, _) = built(plan(true, 3));
    let mut on_road = 0;
    for row in -12..=12 {
        for column in -12..=12 {
            let (x, z) = (f64::from(column) * 110.0, f64::from(row) * 110.0);
            let lie = land.lie(&fields, x, z);
            assert!(
                (lie.height - land.height(&fields, x, z)).abs() < 1e-9,
                "({x}, {z})"
            );
            assert!(lie.height.is_finite());
            assert!(
                lie.upright > 0.0 && lie.upright <= 1.0,
                "({x}, {z}): {}",
                lie.upright
            );
            for share in [lie.wet, lie.green, lie.road, lie.path] {
                assert!((0.0..=1.0).contains(&share), "({x}, {z}): {lie:?}");
            }
            assert!((-1.0..=1.0).contains(&lie.sediment));
        }
    }
    for course in 0..land.roads.len() {
        for mark in land.roads.course(course).iter().step_by(4) {
            if land.lie(&fields, mark.x, mark.z).road > 0.5 {
                on_road += 1;
            }
        }
    }
    assert!(on_road > 10, "the road's own line reads as road: {on_road}");
}

#[test]
fn rivers_run_downhill_and_the_road_keeps_out_of_their_water() {
    let (land, fields, rivers, _) = built(plan(true, 7));
    assert!(
        rivers.len() > 0,
        "a land with a catchment drains through rivers"
    );
    for course in 0..rivers.len() {
        let marks = rivers.course(course);
        for pair in marks.windows(2) {
            if let [upstream, downstream] = pair {
                assert!(
                    downstream.level <= upstream.level + 0.05,
                    "a river climbs: {upstream:?} {downstream:?}"
                );
            }
        }
    }
    let bridged = |x: f64, z: f64| {
        land.crossings.iter().any(|crossing| {
            let middle = (
                f64::midpoint(crossing.from.x, crossing.to.x),
                f64::midpoint(crossing.from.z, crossing.to.z),
            );
            let half = 0.5
                * mathf::hypot(
                    crossing.to.x - crossing.from.x,
                    crossing.to.z - crossing.from.z,
                );
            mathf::hypot(x - middle.0, z - middle.1) < half + 3.0 * crossing.width
        })
    };
    for course in 0..land.roads.len() {
        for mark in land.roads.course(course) {
            if !bridged(mark.x, mark.z) {
                assert!(
                    !land.wet_at(&fields, mark.x, mark.z),
                    "the road runs through water at ({}, {})",
                    mark.x,
                    mark.z
                );
            }
        }
    }
}

/// With no river and no lake, the land holds no water, and its water grid
/// is one empty cell rather than a grid of nothing the size of the land; a
/// land with water holds a grid of it as fine as its far land's.
#[test]
fn a_dry_land_keeps_no_water_grid_and_a_wet_one_keeps_one() {
    let mut kinds = [false, false];
    for seed in 0..6 {
        let (land, fields, rivers, watered) = built(plan(false, 20 + seed));
        assert_eq!(rivers.len(), 0);
        let water = fields.get(2).expect("the water grid");
        if watered {
            assert!(water.side() > 2, "{seed}: a lake keeps its water grid");
        } else {
            assert!(
                water.side() <= 2,
                "{seed}: a dry land's water grid holds {} a side",
                water.side()
            );
            let dry = (0..400u32).all(|index| {
                let (x, z) = (
                    f64::from(index % 20) * 140.0 - 1400.0,
                    f64::from(index / 20) * 140.0 - 1400.0,
                );
                land.water(&fields, x, z).is_none()
            });
            assert!(dry, "{seed}: nothing stands on a dry land");
        }
        kinds[usize::from(watered)] = true;
    }
    assert!(kinds[0], "some of the lands are dry");
}

#[test]
fn a_lane_carries_its_road_or_its_path_and_the_road_wins() {
    for share in [0.0, 0.25, 0.5, 1.0] {
        let (road, path) = decode_lane(f64::from(encode_lane(share, 0.0)) / 255.0);
        assert!(
            (road - if share > 0.01 { share } else { 0.0 }).abs() < 0.01 && path == 0.0,
            "{share}"
        );
        let (road, path) = decode_lane(f64::from(encode_lane(0.0, share)) / 255.0);
        assert!(road == 0.0 && (path - share).abs() < 0.01, "{share}");
    }
    let (road, path) = decode_lane(f64::from(encode_lane(0.6, 0.9)) / 255.0);
    assert!(
        (road - 0.6).abs() < 0.01 && path == 0.0,
        "the road over a path"
    );
}

#[test]
fn a_straight_level_river_wanders_only_sideways_by_its_own_length_run() {
    let rivers = Rivers {
        catchment: 1.0e5,
        width: 4.0,
        meander: 1.0,
    };
    let course: Vec<Mark> = (0..64)
        .map(|index| Mark {
            x: 4.0 * real(index),
            z: 10.0,
            level: 2.0,
            width: 4.0,
            depth: 1.0,
        })
        .collect();
    let seed = 0x5eed;
    let wandering = meandering(&course, rivers, seed).expect("a course");
    assert_eq!(wandering.len(), course.len());
    let (first, last) = (wandering[0], wandering[course.len() - 1]);
    assert_eq!((first.x, first.z), (0.0, 10.0), "the ends stay put");
    assert_eq!((last.x, last.z), (252.0, 10.0), "the ends stay put");
    let wavelength = 11.0 * 4.0;
    let mut swayed = 0.0_f64;
    for (index, (moved, at)) in wandering.iter().zip(&course).enumerate().skip(1).take(62) {
        assert_eq!(
            moved.x.to_bits(),
            at.x.to_bits(),
            "a course along x moves across it alone"
        );
        // Measured along the course as it lay, not as earlier points left it.
        let travelled = 4.0 * real(index);
        let sway = 4.0
            * mathf::sin(
                TAU * travelled / wavelength + noise2(travelled / (4.0 * wavelength), 0.3, seed),
            );
        assert!(
            (moved.z - (10.0 + sway)).abs() < 1e-9,
            "{index}: {} against {sway}",
            moved.z - 10.0
        );
        swayed = swayed.max(sway.abs());
    }
    assert!(
        swayed > 3.0,
        "it wanders as far as its width either way: {swayed}"
    );
}

#[test]
fn a_narrow_slanting_river_wets_every_corner_of_every_cell_it_crosses() {
    // A river a quarter of a cell wide, slanting across the grid at many
    // angles: each cell its course passes through keeps all four corners wet.
    let (step, width) = (4.0, 1.0);
    for turn in 0..24 {
        let angle = f64::from(turn) * 0.13 + 0.05;
        let (dx, dz) = (mathf::cos(angle), mathf::sin(angle));
        let apart = |(x, z): (f64, f64)| (x * dz - z * dx).abs();
        for along in 0..400 {
            let t = f64::from(along) * 0.1 - 20.0;
            let (x, z) = (dx * t + 0.37, dz * t - 0.21);
            let (column, row) = (mathf::floor(x / step), mathf::floor(z / step));
            for corner in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                let at = (
                    (column + corner.0) * step - 0.37,
                    (row + corner.1) * step + 0.21,
                );
                assert!(
                    apart(at) < river_reach(width, step),
                    "{angle}: a corner {} off the river's middle",
                    apart(at)
                );
            }
        }
    }
}
