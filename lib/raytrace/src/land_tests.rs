//! Host tests of building a land: it lies as its grids hold it, its rivers
//! run downhill, its road keeps out of their water but where it bridges
//! them, and a land with no water standing on it keeps no water grid.

use alloc::vec::Vec;

use tairix_parallel::Threaded;

use super::*;
use crate::terrain::{Landform, Terrain};
use crate::vector::Ray;

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
            flowing: 1.0,
            ledges: 0.0,
            outcrops: 0.1,
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
        near_water: None,
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
    let mut fields = alloc::vec![placeholder(), placeholder(), placeholder(), placeholder()];
    let grids = Fields {
        far: 0,
        nests: [Some(1), None],
        water: Some(2),
        near_water: plan.near_water.map(|_| 3),
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
    let land = build.finish().expect("a land");
    let rivers = land.rivers.clone();
    (land, fields, rivers, watered)
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
        flowing: 1.0,
        ledges: 0.0,
        outcrops: 0.0,
    };
    let course: Vec<Mark> = (0..64)
        .map(|index| Mark {
            x: 4.0 * real(index),
            z: 10.0,
            level: 2.0,
            width: 4.0,
            depth: 1.0,
            ..Mark::default()
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

/// A bed's hard cap wears its hardness times as slowly as the softer rock
/// beneath it, so a canyon's caps stand as cliffs over its benches; ground
/// with no strata wears as ordinary ground does.
#[test]
fn a_hard_cap_wears_its_hardness_times_as_slowly() {
    let (spacing, hardness) = (6.0, 4.0);
    let strata = Some((spacing, hardness));
    for bed in 0..5 {
        let floor = spacing * f64::from(bed);
        let cap = erodibility(strata, floor + 0.95 * spacing);
        let soft = erodibility(strata, floor + 0.5 * spacing);
        assert!((cap * hardness - 1.0).abs() < 1e-12, "bed {bed}: cap {cap}");
        assert!((soft - 1.0).abs() < 1e-12, "bed {bed}: soft {soft}");
        assert!(cap < soft);
    }
    assert!((erodibility(None, 17.0) - 1.0).abs() < 1e-12);
}

/// A low river's bed is soaked where it is bared, never under standing
/// water above the river's level, and nothing roots in it; the plants on its
/// banks thin toward it over a fringe, not at one grid step; and what its
/// channel laid there has the say within its banks' faces, and none beyond.
#[test]
fn a_low_rivers_bare_margin_is_soaked_and_its_banks_thin_toward_it() {
    let station = Station {
        along: 0.0,
        brim: 10.0,
        width: 6.0,
        depth: 1.0,
        phase: 0.6,
        turn: 0.0,
        fall: 0.01,
    };
    let form = Form {
        flowing: 0.4,
        ledges: 0.0,
        outcrops: 0.0,
        seed: 3,
    };
    let banked = Banked::new(&station, &form);
    let section = &banked.section;
    // On the bar, across from where the deep water swings.
    let side = if section.thalweg >= 0.0 { -1.0 } else { 1.0 };
    let near = |distance: f64| Nearest {
        course: 0,
        along: 0.0,
        distance,
        side,
        level: 10.0,
        width: 6.0,
        depth: 1.0,
        run: 0.0,
        phase: 0.6,
        turn: 0.0,
        fall: 0.01,
        toward: (0.0, 1.0),
    };
    let step = 0.05;
    let at = |distance: f64| river_bed(&banked, &near(distance), 12.0, (step, 0.0));
    let deepest = river_bed(
        &banked,
        &Nearest {
            side: section.thalweg.signum(),
            ..near(section.thalweg.abs())
        },
        12.0,
        (step, 0.0),
    );
    assert!(deepest.wet > 0.99 && deepest.scoured > 0.99);
    assert!((deepest.height - section.deepest).abs() < 1e-9);
    // Bared: the bed stands above the water up the bar.
    let edge = side * section.edge(side);
    let bared = at(edge + 0.6 * (section.half - edge));
    assert!(bared.height > section.water + BARE, "{bared:?}");
    assert!(bared.wet < 0.82 && bared.wet > 0.6, "{bared:?}");
    // Pioneer plants take at most their share of a bar's top.
    assert!(bared.scoured >= 1.0 - PIONEERS - 1e-9, "{bared:?}");
    let fringe = (FRINGE * 6.0).clamp(0.4, 2.0);
    let from = section.half - 0.25 * fringe;
    let scours: Vec<f64> = (0..200)
        .map(|index| at(from + 0.01 * f64::from(index)).scoured)
        .collect();
    assert!(scours[0] >= 1.0 - PIONEERS - 1e-9 && scours[199] < 1e-12);
    let thinning = scours
        .iter()
        .filter(|&&share| share > 0.02 && share < 0.98)
        .count();
    assert!(
        0.01 * f64::from(u32::try_from(thinning).expect("a count")) > 0.5 * fringe,
        "the fringe thins over {thinning} hundredths"
    );
    assert!(at(section.half + 1.5 * fringe).scoured < 1e-12);
    assert!((at(1.0).say - 1.0).abs() < 1e-12);
    let faces = section.half + banked.bank_reach(side, step);
    assert!(at(faces + 3.0 * step).say < 1e-12);
    // The bank rises from the brim to the land beyond.
    assert!((at(section.half).height - section.brim).abs() < 1e-9);
    assert!(at(faces + 20.0).height > 11.9);
}

/// The droplets run over a land after its rivers are carved rill the land
/// beyond them but not their channels: within a river's brim a finer grid
/// stands as the channel's bed and holds what the channel laid there, to the
/// grid's own precision.
#[test]
fn a_channel_stands_as_carved_against_the_droplets() {
    let (land, fields, rivers, _) = built(plan(true, 7));
    let form = land.form.expect("a river's form");
    let nest = land.nests[0].expect("a finer grid");
    let grid = &fields[nest.field as usize];
    let ((origin_x, origin_z), step) = grid.placing();
    let side = grid.side();
    // Clear of the border, where the grid gives way to the far one's.
    let inner = Laid {
        reach: 0.9 * nest.reach,
        ..nest
    };
    let mut checked = 0;
    for row in 0..side {
        for column in 0..side {
            let (x, z) = (
                origin_x + step * f64::from(u32::try_from(column).expect("a column")),
                origin_z + step * f64::from(u32::try_from(row).expect("a row")),
            );
            if !inner.holds(x, z) {
                continue;
            }
            let Some(near) = rivers.nearest(x, z) else {
                continue;
            };
            let banked = Banked::new(&Station::of(&near), &form);
            let section = &banked.section;
            if near.distance > 0.8 * section.half {
                continue;
            }
            let across = near.side * near.distance;
            let height = f64::from(grid.heights()[row * side + column]);
            assert!(
                (height - section.bed(across)).abs() < 2e-3,
                "({x}, {z}): {height} against the bed's {}",
                section.bed(across)
            );
            let [_, sediment, _, _] = grid.attributes_of(column, row);
            let sediment = 2.0 * f64::from(sediment) / 255.0 - 1.0;
            assert!(
                (sediment - banked.laid(across)).abs() < 0.01,
                "({x}, {z}): {sediment} laid against the channel's {}",
                banked.laid(across)
            );
            checked += 1;
        }
    }
    assert!(checked > 50, "only {checked} places in a channel");
}

/// The fresh water's finer grid meets the far water grid along its border
/// rather than sharing a ring of cells with it, so a ray meets one water's
/// surface there, not two; the two stand alike along the border; and the
/// land's water within the finer grid is the finer grid's.
#[test]
fn a_finer_water_grid_meets_the_far_one_at_its_seam() {
    let mut plan = plan(true, 7);
    plan.near_water = Some(NearWater {
        reach: 300.0,
        cells: 512,
    });
    let (land, fields, _, _) = built(plan);
    let near = land.near_water.expect("a finer water grid");
    let water = land.water.expect("a water grid");
    let (finer, far) = (&fields[near.field as usize], &fields[water as usize]);
    let (cx, cz, reach) = (near.centre.0, near.centre.1, near.reach);
    let mut met = 0;
    for step in 0..=2000 {
        let t = -reach + 2.0 * reach * f64::from(step) / 2000.0;
        for (x, z) in [
            (cx + t, cz - reach),
            (cx + t, cz + reach),
            (cx - reach, cz + t),
            (cx + reach, cz + t),
        ] {
            let (own, other) = (finer.height_at(x, z), far.height_at(x, z));
            if own.is_finite() && other.is_finite() {
                assert!(
                    (own - other).abs() < 1e-3,
                    "({x}, {z}): {own} against {other}"
                );
                met += 1;
            }
        }
    }
    assert!(met > 0, "the water crosses the seam");
    let ((origin_x, origin_z), cell) = far.placing();
    let mut inner = 0;
    for row in 0..far.side() {
        for column in 0..far.side() {
            let (x, z) = (
                origin_x + cell * (f64::from(u32::try_from(column).expect("a column")) + 0.5),
                origin_z + cell * (f64::from(u32::try_from(row).expect("a row")) + 0.5),
            );
            if (x - cx).abs() >= reach || (z - cz).abs() >= reach {
                continue;
            }
            let level = finer.height_at(x, z);
            if !level.is_finite() {
                continue;
            }
            let down = Ray::new(Vec3::new(x, level + 50.0, z), Vec3::new(0.0, -1.0, 0.0));
            assert!(
                far.intersect(&down, 0.0, 100.0).is_none(),
                "({x}, {z}) twice"
            );
            assert!(
                finer.intersect(&down, 0.0, 100.0).is_some(),
                "({x}, {z}) never"
            );
            assert_eq!(land.water_level(&fields, x, z), Some(level));
            inner += 1;
        }
    }
    assert!(inner > 0, "water stands within the finer grid");
}

/// The water grid's units reach only the rows they fill, never zeroing the
/// whole grid on the first.
#[test]
fn a_water_grids_units_reach_only_their_own_rows() {
    let origin = (-1500.0, -1500.0);
    let placeholder = || Heightfield::new(1, origin, 1.0, false).expect("a grid");
    let mut fields = alloc::vec![placeholder(), placeholder(), placeholder()];
    let grids = Fields {
        far: 0,
        nests: [Some(1), None],
        water: Some(2),
        near_water: None,
        horizon: None,
    };
    let mut build = Build::new(plan(true, 6), grids).expect("a build");
    let runner = tairix_parallel::SERIAL;
    loop {
        let done = build.step(&mut fields, &runner).expect("builds");
        if done && build.waiting() {
            build.site((0.0, 0.0), (0.0, 0.0), None).expect("sited");
        }
        if let Step::Water(row) = build.stage {
            let water = &fields[2];
            assert!(row < water.side());
            assert_eq!(water.heights().len(), row * water.side());
            return;
        }
        assert!(
            !matches!(build.stage, Step::Done),
            "the land never filled its water"
        );
    }
}
