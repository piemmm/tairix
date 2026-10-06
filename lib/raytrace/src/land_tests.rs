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
        farming: None,
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
        snowpack: None,
        pond: None,
        growth: 1.0,
        seed: seed ^ 0x55,
    }
}

/// The far grid `plan` is built into, laid as a scene lays it.
fn far_of(plan: &Plan) -> Heightfield {
    let origin = (
        plan.relief.centre.0 - plan.reach,
        plan.relief.centre.1 - plan.reach,
    );
    Heightfield::new(plan.cells.1, origin, plan.far_step(), false).expect("a grid")
}

/// `plan` built into grids of its own, sited at the land's middle: the land,
/// its grids, its rivers, and whether water stands anywhere on it.
fn built(plan: Plan) -> (Land, Vec<Heightfield>, Courses, bool) {
    let far = far_of(&plan);
    built_on(plan, far)
}

/// `plan` built as [`built`] builds it, its far land into `far`.
fn built_on(plan: Plan, far: Heightfield) -> (Land, Vec<Heightfield>, Courses, bool) {
    built_cut(plan, far, &[])
}

/// `plan` built as [`built_on`] builds it, `cuttings` dug into it.
fn built_cut(
    plan: Plan,
    far: Heightfield,
    cuttings: &[Vec<Mark>],
) -> (Land, Vec<Heightfield>, Courses, bool) {
    let origin = far.placing().0;
    let placeholder = || Heightfield::new(1, origin, 1.0, false).expect("a grid");
    let mut fields = alloc::vec![far, placeholder(), placeholder(), placeholder()];
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
            build
                .site((0.0, 0.0), (0.0, 0.0), (None, cuttings))
                .expect("sited");
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
            let lie = land.grids.lie(&fields, x, z);
            assert!(
                (lie.height - land.grids.height(&fields, x, z)).abs() < 1e-9,
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
    for course in 0..land.ways.of(Surface::Track).len() {
        for mark in land.ways.of(Surface::Track).course(course).iter().step_by(4) {
            if land.grids.lie(&fields, mark.x, mark.z).road > 0.5 {
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
    for course in 0..land.ways.of(Surface::Track).len() {
        for mark in land.ways.of(Surface::Track).course(course) {
            if !bridged(mark.x, mark.z) {
                assert!(
                    !land.grids.wet_at(&fields, mark.x, mark.z),
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
    for (rivers, seed) in (0..6).map(|seed| (false, 20 + seed)).chain([(true, 7)]) {
        let planned = plan(rivers, seed);
        let cells = planned.cells.1;
        let (land, fields, courses, watered) = built(planned);
        assert_eq!(
            courses.len() > 0,
            rivers,
            "{seed}: rivers where a land drains through them"
        );
        let water = fields.get(2).expect("the water grid");
        if watered {
            assert_eq!(
                water.side(),
                cells + 1,
                "{seed}: water keeps a grid as fine as the far land's"
            );
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
                land.grids
                    .water_level(&fields, x, z)
                    .is_none_or(|level| level <= land.grids.height(&fields, x, z))
            });
            assert!(dry, "{seed}: nothing stands on a dry land");
        }
        kinds[usize::from(watered)] = true;
    }
    assert!(kinds[0], "some of the lands are dry");
    assert!(kinds[1], "a land with rivers holds water");
}

/// Between a road's last vertex and the plain ground beside it the road
/// thins away, and no path appears: its road and its path are each a
/// quantity of their own, blended on their own.
#[test]
fn a_roads_edge_thins_to_plain_ground_and_is_never_read_as_a_path() {
    let mut grid = Heightfield::new(4, (0.0, 0.0), 1.0, false).expect("a grid");
    let side = grid.side();
    assert!(grid.carry_attributes());
    {
        let (_, attributes) = grid.rows_mut(0..side);
        for (index, slot) in attributes.iter_mut().enumerate() {
            let road = if index % side == 0 { 255 } else { 0 };
            *slot = [0, 128, road, 0, 255, 0];
        }
    }
    grid.seal();
    for step in 0..=20u32 {
        let x = f64::from(step) / 20.0;
        let lie = lie_on(&grid, x, 1.5);
        assert!((lie.road - (1.0 - x)).abs() < 1e-9, "at {x}: road {}", lie.road);
        assert!(lie.path == 0.0, "at {x}: read as a path {}", lie.path);
    }
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
        past: 0.0,
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
    let nest = land.grids.nests[0].expect("a finer grid");
    let grid = &fields[nest.field as usize];
    let ((origin_x, origin_z), step) = grid.placing();
    let side = grid.side();
    // Clear of the band within its border where the grid gives way to the
    // far one's.
    let (_, parent_step) = fields[land.grids.far as usize].placing();
    let inner = Laid {
        reach: nest.reach - NEST_BAND.1 * parent_step,
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
            let [_, sediment, _, _, _, _] = grid.attributes_of(column, row);
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

/// A cutting is dug to its level floor, stays dug for all the droplets that
/// would silt it, and ends square: past its end the land is as it would be
/// uncut.
#[test]
fn a_cutting_is_dug_level_and_square_and_stays_dug() {
    let far = || far_of(&plan(false, 3));
    let (land, fields, _, _) = built_cut(plan(false, 3), far(), &[]);
    let finest = |fields: &[Heightfield], x: f64| land.grids.height(fields, x, 0.0);
    let lowest = (0..=40)
        .map(|step| finest(&fields, -60.0 + 3.0 * f64::from(step)))
        .fold(f64::INFINITY, f64::min);
    let level = lowest - 2.0;
    let marks: Vec<Mark> = (0..=12)
        .map(|step| Mark {
            x: -60.0 + 10.0 * f64::from(step),
            z: 0.0,
            level,
            width: 6.0,
            ..Mark::default()
        })
        .collect();
    let (cut, dug, _, _) = built_cut(plan(false, 3), far(), &[marks]);
    for step in 0..=56 {
        let x = -56.0 + 2.0 * f64::from(step);
        let height = cut.grids.height(&dug, x, 0.0);
        assert!(
            (height - level).abs() < 0.05,
            "{x}: {height} on a floor dug to {level}"
        );
    }
    // Past its end, beyond the step the grid softens it across.
    for x in [72.0, 80.0] {
        let (height, uncut) = (cut.grids.height(&dug, x, 0.0), finest(&fields, x));
        assert!(
            height > level + 1.0 && (height - uncut).abs() < 0.5,
            "{x}: {height} past the end, {uncut} uncut"
        );
    }
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
    let near = land.grids.near_water.expect("a finer water grid");
    let water = land.grids.water.expect("a water grid");
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
            assert_eq!(land.grids.water_level(&fields, x, z), Some(level));
            inner += 1;
        }
    }
    assert!(inner > 0, "water stands within the finer grid");
}

/// A bank inside the finer grid's square but past its brim, where it holds no
/// water, reads the far grid's level, as the same bank outside the square
/// does, so what stands at the water's edge does not stop at the square.
#[test]
fn a_bank_past_the_finer_grids_brim_reads_the_far_level() {
    let mut plan = plan(true, 7);
    plan.near_water = Some(NearWater {
        reach: 300.0,
        cells: 512,
    });
    let (land, fields, _, _) = built(plan);
    let near = land.grids.near_water.expect("a finer water grid");
    let water = land.grids.water.expect("a water grid");
    let (finer, far) = (&fields[near.field as usize], &fields[water as usize]);
    let ((origin_x, origin_z), cell) = far.placing();
    let mut banks = 0;
    for row in 0..far.side() {
        for column in 0..far.side() {
            let (x, z) = (
                origin_x + cell * (f64::from(u32::try_from(column).expect("a column")) + 0.5),
                origin_z + cell * (f64::from(u32::try_from(row).expect("a row")) + 0.5),
            );
            if near.inside((x, z)) <= 0.0 {
                continue;
            }
            let (own, other) = (finer.height_at(x, z), far.height_at(x, z));
            if own.is_finite() || !other.is_finite() {
                continue;
            }
            assert_eq!(land.grids.water_level(&fields, x, z), Some(other));
            banks += 1;
        }
    }
    assert!(
        banks > 0,
        "the far grid holds water up banks past the finer brim"
    );
}

/// The water grid's units reach only the rows they fill, never zeroing the
/// whole grid on the first.
#[test]
fn a_water_grids_units_reach_only_their_own_rows() {
    let origin = (-1500.0, -1500.0);
    let placeholder = || Heightfield::new(1, origin, 1.0, false).expect("a grid");
    let mut fields = alloc::vec![far_of(&plan(true, 6)), placeholder(), placeholder()];
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
            build
                .site((0.0, 0.0), (0.0, 0.0), (None, &[]))
                .expect("sited");
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

/// A land under snow stands above the same land bare by the depth the wind
/// left lying there, which its grids carry: deep in the lee of its hills and
/// thin where it scoured them, so the land is drifted, not evenly mantled,
/// and grows only where the snow lies too thin to bury its grass.
#[test]
fn snow_lies_on_a_land_as_the_wind_drifted_it() {
    let reach = 1500.0;
    let bare = Plan {
        relief: Terrain {
            form: Landform::Hills {
                scale: 260.0,
                height: 50.0,
                seed: 7,
            },
            datum: 0.0,
            centre: (0.0, 0.0),
            radius: reach,
            rim: None,
            tilt: (0.0, 0.0),
            clearing: None,
        },
        reach,
        rivers: None,
        road: None,
        farming: None,
        ridges: 0.0,
        roughness: 0.6,
        droplets: 0.0,
        ..plan(false, 7)
    };
    let pack = Snowpack {
        heading: 0.6,
        fallen: 0.4,
        seed: 3,
    };
    let snowy = Plan {
        snowpack: Some(pack),
        ..bare.clone()
    };
    let (_, bare_fields, _, _) = built_on(bare.clone(), far_of(&bare));
    let (_, snowy_fields, _, _) = built_on(snowy.clone(), far_of(&snowy));
    let (bare, snowy) = (&bare_fields[0], &snowy_fields[0]);
    let side = snowy.side();
    let (mut depths, mut deepest, mut thinnest) = (Vec::new(), 0.0f64, f64::INFINITY);
    for row in (8..side - 8).step_by(7) {
        for column in (8..side - 8).step_by(7) {
            let [_, _, _, _, green, kept] = snowy.attributes_of(column, row);
            let depth = snow::depth_of(f64::from(kept) / 255.0);
            let at = row * side + column;
            let risen = f64::from(snowy.heights()[at] - bare.heights()[at]);
            assert!(
                (risen - depth).abs() < 0.01 + 0.03 * depth,
                "({column}, {row}): risen {risen} by snow {depth}"
            );
            // The least depth the byte kept stands for buries the least.
            let least = snow::depth_of((f64::from(kept) - 0.5).max(0.0) / 255.0);
            let showing = 1.0 - snow::buries(least, GRASS_BURIED);
            assert!(
                f64::from(green) / 255.0 <= showing + 0.01,
                "({column}, {row}): {green} grows through {depth} of snow"
            );
            deepest = deepest.max(depth);
            thinnest = thinnest.min(depth);
            depths.push(depth);
        }
    }
    assert!(deepest > 1.6 * pack.fallen, "drifts: {deepest}");
    assert!(thinnest < 0.5 * pack.fallen, "scoured: {thinnest}");
    let mean =
        depths.iter().sum::<f64>() / f64::from(u32::try_from(depths.len()).expect("a count"));
    assert!(
        (0.6 * pack.fallen..1.5 * pack.fallen).contains(&mean),
        "{mean}"
    );
}

/// A finer grid meets the far grid along its border, giving way to it there
/// to the far grid's own height, and the far grid leaves out the cells the
/// finer one covers, keeping one ring under its border, so a ray meets one
/// land there and not two.
#[test]
fn a_finer_grid_meets_the_far_one_along_its_border() {
    let (land, fields, _, _) = built(plan(true, 3));
    let far = &fields[land.grids.far as usize];
    let nest = land.grids.nests[0].expect("a finer grid");
    let grid = &fields[nest.field as usize];
    let ((origin_x, origin_z), step) = grid.placing();
    let side = grid.side();
    for index in 0..side {
        for (column, row) in [(index, 0), (index, side - 1), (0, index), (side - 1, index)] {
            let (x, z) = (origin_x + step * real(column), origin_z + step * real(row));
            let height = f64::from(grid.heights()[row * side + column]);
            assert!(
                (height - far.height_at(x, z)).abs() < 2e-3,
                "({x}, {z}): {height} against the far grid's {}",
                far.height_at(x, z)
            );
        }
    }
    let (_, far_step) = far.placing();
    for (x, z) in [
        (nest.centre.0, nest.centre.1),
        (
            nest.centre.0 + 0.5 * nest.reach,
            nest.centre.1 - 0.4 * nest.reach,
        ),
        (nest.centre.0 - nest.reach + 3.0 * far_step, nest.centre.1),
    ] {
        let down = Ray::new(Vec3::new(x, 1e4, z), -Vec3::UP);
        assert!(
            far.intersect(&down, 0.0, 2e4).is_none(),
            "({x}, {z}): the far grid stands under the finer one"
        );
        assert!(grid.intersect(&down, 0.0, 2e4).is_some(), "({x}, {z})");
    }
}

/// A road's bridge stands its clearance and more over the water it spans,
/// its road level along the deck from end to end.
#[test]
fn a_bridges_deck_clears_its_river_and_carries_its_road_level() {
    let mut decks = 0;
    for seed in [7, 11, 13] {
        let (land, _, _, _) = built(plan(true, seed));
        if land.ways.of(Surface::Track).len() == 0 {
            continue;
        }
        let course = land.ways.of(Surface::Track).course(0);
        for crossing in &land.crossings {
            assert!(
                crossing.deck >= crossing.water + CLEARANCE + 0.08 * crossing.width - 1e-9,
                "{seed}: a deck {} over water {}",
                crossing.deck,
                crossing.water
            );
            let at = |mark: &Mark| {
                course
                    .iter()
                    .position(|other| other.x == mark.x && other.z == mark.z)
                    .expect("a crossing's ends lie on its road")
            };
            let (from, to) = (at(&crossing.from), at(&crossing.to));
            for mark in &course[from.min(to)..=from.max(to)] {
                assert!(
                    (mark.level - crossing.deck).abs() < 1e-9,
                    "{seed}: the road at {} on a deck at {}",
                    mark.level,
                    crossing.deck
                );
            }
            decks += 1;
        }
    }
    assert!(decks > 0, "some road crosses its land's rivers");
}

/// A river running out into standing water fans its silt out before it:
/// built a little above the water at its mouth, shelving under it toward
/// its fringe, cut by its distributaries, and leaving the bed beyond its
/// spread as it was.
#[test]
fn a_delta_fans_its_silt_out_into_the_water() {
    let course: Vec<Mark> = (0..8u32)
        .map(|index| Mark {
            x: 100.0 + 10.0 * f64::from(index),
            z: -40.0,
            width: 6.0,
            level: 3.0,
            ..Mark::default()
        })
        .collect();
    let fan = delta(&course, 2.0).expect("a delta at the mouth");
    assert_eq!(fan.apex, (170.0, -40.0));
    assert!((fan.toward.0 - 1.0).abs() < 1e-12 && fan.toward.1.abs() < 1e-12);
    assert!((fan.length - (40.0 + 7.0 * 6.0)).abs() < 1e-12);
    assert!(delta(&course[..3], 2.0).is_none(), "too short a river");
    assert!(delta(&course, f64::NAN).is_none(), "no water to fan into");
    let bed = -1.5;
    let probe = |along: f64, across: f64| delta_bed(&fan, (170.0 + along, -40.0 + across), bed);
    let (mouth, silt) = probe(1.0, 0.0);
    assert!(mouth > fan.level - 0.8, "a mouth at {mouth}");
    assert!(silt > 0.5, "fresh silt at the mouth: {silt}");
    // Its fan's crest shelves from above the water at the mouth to below it.
    let crest = |along: f64| {
        (0..=400u32)
            .map(|step| probe(along, -60.0 + 0.3 * f64::from(step)).0)
            .fold(f64::NEG_INFINITY, f64::max)
    };
    assert!(crest(2.0) > fan.level, "built above the water at its mouth");
    assert!(
        crest(0.75 * fan.length) < fan.level,
        "under the water at its fringe"
    );
    assert_eq!(
        probe(1.0, 3.0 * fan.width + 10.0),
        (bed, 0.0),
        "beyond its spread"
    );
    assert_eq!(probe(fan.length + 1.0, 0.0), (bed, 0.0), "past its fringe");
    assert_eq!(
        probe(-2.0 * fan.width, 0.0),
        (bed, 0.0),
        "upstream of its mouth"
    );
    let across: Vec<f64> = (0..=200u32)
        .map(|step| probe(30.0, -20.0 + 0.2 * f64::from(step)).0)
        .collect();
    let (low, high) = across.iter().fold(
        (f64::INFINITY, f64::NEG_INFINITY),
        |(low, high), &height| (low.min(height), high.max(height)),
    );
    assert!(
        high - low > 0.5,
        "distributaries cut its fan: {low} to {high}"
    );
}

/// A land running on to the horizon lays the far grid on whole cells of the
/// horizon's, meets it at the far grid's border with no step, and leaves
/// out the horizon's cells the far grid covers.
#[test]
fn the_far_grid_lies_on_the_horizons_cells_and_meets_it_without_a_step() {
    let planned = Plan {
        horizon: Some(Horizon {
            reach: 6000.0,
            cells: 64,
        }),
        ..plan(true, 3)
    };
    let (origin, step) = planned.horizon_placing().expect("a horizon grid");
    let mut fields = alloc::vec![
        far_of(&planned),
        Heightfield::new(1, origin, 1.0, false).expect("a grid"),
        Heightfield::new(1, origin, 1.0, false).expect("a grid"),
        Heightfield::new(64, origin, step, false).expect("a grid"),
    ];
    let grids = Fields {
        far: 0,
        nests: [Some(1), None],
        water: Some(2),
        near_water: None,
        horizon: Some(3),
    };
    let mut build = Build::new(planned, grids).expect("a build");
    let runner = Threaded::new(8);
    loop {
        let done = build.step(&mut fields, &runner).expect("builds");
        if done && build.waiting() {
            build
                .site((0.0, 0.0), (0.0, 0.0), (None, &[]))
                .expect("sited");
        } else if done {
            break;
        }
    }
    let (far, horizon) = (&fields[0], &fields[3]);
    let ((far_x, far_z), far_step) = far.placing();
    let whole = |offset: f64| {
        let cells = offset / step;
        (cells - mathf::round(cells)).abs() < 1e-9
    };
    assert!(
        whole(far_x - origin.0) && whole(far_z - origin.1),
        "on the horizon's cells"
    );
    let far_side = far.side();
    assert!(
        whole(far_step * real(far_side - 1)),
        "the far grid spans whole cells of the horizon's"
    );
    for index in (0..far_side).step_by(8) {
        for (column, row) in [
            (index, 0),
            (index, far_side - 1),
            (0, index),
            (far_side - 1, index),
        ] {
            let (x, z) = (
                far_x + far_step * real(column),
                far_z + far_step * real(row),
            );
            let height = f64::from(far.heights()[row * far_side + column]);
            assert!(
                (height - horizon.height_at(x, z)).abs() < 2e-3,
                "({x}, {z}): {height} against the horizon's {}",
                horizon.height_at(x, z)
            );
        }
    }
    let down = Ray::new(Vec3::new(0.0, 1e4, 0.0), -Vec3::UP);
    assert!(
        horizon.intersect(&down, 0.0, 2e4).is_none(),
        "the horizon under the far grid"
    );
    let beyond = Ray::new(Vec3::new(far_x - 2.0 * step, 1e4, 0.0), -Vec3::UP);
    assert!(
        horizon.intersect(&beyond, 0.0, 2e4).is_some(),
        "the horizon beyond it"
    );
}
