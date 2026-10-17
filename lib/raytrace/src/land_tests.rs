//! Host tests of building a land: it lies as its grids hold it, its rivers
//! run downhill, its road keeps out of their water but where it bridges
//! them, and a land with no water standing on it keeps no water grid.

use alloc::vec::Vec;

use tairix_land::channel::{Banked, Station};
use tairix_land::plan::{Level, Rivers, Surface, Wear};
use tairix_land::relief::{Landform, Terrain};
use tairix_parallel::Threaded;

use super::*;

/// Rolling hills `reach` either way of the middle, drained by rivers if
/// `rivers`, a track crossing them, a finer level 300 either way about the
/// eye: coarse enough to build in a moment.
fn plan(rivers: bool, seed: u32) -> Plan {
    let reach = 1500.0;
    let mut levels = [None; LEVELS];
    levels[0] = Some(Level {
        ratio: 2,
        droplets: 0.02,
    });
    levels[1] = Some(Level {
        ratio: 5,
        droplets: 0.04,
    });
    Plan {
        land: LandPlan {
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
            cells: 128,
            levels,
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
            snow_line: None,
            snowpack: None,
            pond: None,
            growth: 1.0,
            seed: seed ^ 0x55,
        },
        nests: [Some(300.0), None],
        near_water: None,
        horizon: None,
    }
}

/// `plan` built into grids of its own, sited at the land's middle: the land,
/// its grids, its rivers, and whether water stands anywhere on it.
fn built(plan: Plan) -> (Land, Vec<Heightfield>, Courses, bool) {
    built_cut(plan, &[])
}

/// `plan` built as [`built`] builds it, `cuttings` dug into it.
fn built_cut(plan: Plan, cuttings: &[Vec<Mark>]) -> (Land, Vec<Heightfield>, Courses, bool) {
    let placeholder = || Heightfield::new(1, (0.0, 0.0), 1.0, false).expect("a grid");
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

/// A built land reads as its grids hold it, every quantity in its range; and
/// a track wears its own line along its whole length but where it bridges a
/// river, bare in its two ruts and trodden between them, as strongly as the
/// grid drawing it resolves a way narrower than its cells.
#[test]
fn a_built_land_lies_as_its_grids_hold_it() {
    let (land, fields, _, _) = built(plan(true, 3));
    let (mut worn, mut marks) = (0_u32, 0_u32);
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
        for mark in land
            .ways
            .of(Surface::Track)
            .course(course)
            .iter()
            .step_by(4)
        {
            let bridged = land.crossings.iter().any(|crossing| {
                let span = mathf::hypot(
                    crossing.to.x - crossing.from.x,
                    crossing.to.z - crossing.from.z,
                );
                let middle = (
                    f64::midpoint(crossing.from.x, crossing.to.x),
                    f64::midpoint(crossing.from.z, crossing.to.z),
                );
                mathf::hypot(mark.x - middle.0, mark.z - middle.1)
                    < 0.5 * span + 3.0 * crossing.width
            });
            if bridged {
                continue;
            }
            let lie = land.grids.lie(&fields, mark.x, mark.z);
            marks += 1;
            worn += u32::from(lie.road.max(lie.path) > 0.15);
        }
    }
    assert!(marks > 20, "a track runs across the land: {marks}");
    assert_eq!(worn, marks, "the track wears its own line");
}

#[test]
fn rivers_run_downhill_and_the_road_keeps_out_of_their_water() {
    let (land, fields, rivers, _) = built(plan(true, 7));
    assert!(
        !rivers.is_empty(),
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
        let lattices = planned.land.lattices().expect("its lattices");
        let cells =
            usize::try_from(lattices.domain(0).expect("a domain").size.0).expect("a side") - 1;
        let (land, fields, courses, watered) = built(planned);
        assert_eq!(
            !courses.is_empty(),
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
            *slot = [0, 128, road, 0, 255, 0, 0, 0];
        }
    }
    grid.seal();
    for step in 0..=20u32 {
        let x = f64::from(step) / 20.0;
        let lie = lie_on(&grid, x, 1.5);
        assert!(
            (lie.road - (1.0 - x)).abs() < 1e-9,
            "at {x}: road {}",
            lie.road
        );
        assert!(lie.path == 0.0, "at {x}: read as a path {}", lie.path);
    }
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
            let [_, sediment, ..] = grid.attributes_of(column, row);
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
    let (land, fields, _, _) = built_cut(plan(false, 3), &[]);
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
    let (cut, dug, _, _) = built_cut(plan(false, 3), &[marks]);
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
    let placeholder = || Heightfield::new(1, (0.0, 0.0), 1.0, false).expect("a grid");
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
        Heightfield::new(1, origin, 1.0, false).expect("a grid"),
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

/// A finer level's square sited anywhere about the eye lies on its parent's
/// vertices, so its border runs along the parent's cell edges, and holds
/// everything its piece reads of its parent within the parent's square,
/// however near the parent's edge the eye stands.
#[test]
fn a_finer_square_lies_on_its_parents_vertices_and_reads_within_it() {
    let mut planned = plan(true, 3);
    planned.land.levels[2] = Some(Level {
        ratio: 7,
        droplets: 0.05,
    });
    let lattices = planned.land.lattices().expect("its lattices");
    let far = lattices.domain(0).expect("a domain");
    let parent = sited(&lattices, 1, far, ((0.0, 0.0), (0.0, 0.0), 400.0)).expect("a square");
    for focus in [(0.0, 0.0), (250.0, -120.0), (-900.0, 900.0), (1e4, 1e4)] {
        let square = sited(&lattices, 2, parent, (focus, (0.3, -0.2), 40.0)).expect("a square");
        let ratio = i64::from(lattices.ratio(2).expect("a ratio"));
        assert_eq!(
            square.origin.0.rem_euclid(ratio),
            0,
            "{focus:?}: {square:?}"
        );
        assert_eq!(
            square.origin.1.rem_euclid(ratio),
            0,
            "{focus:?}: {square:?}"
        );
        assert_eq!(
            i64::from(square.size.0 - 1).rem_euclid(ratio),
            0,
            "{focus:?}: whole cells of its parent"
        );
        let reads = lattices.needs(2, square).expect("what it reads");
        assert!(
            parent.holds(reads),
            "{focus:?}: {reads:?} within {parent:?}"
        );
    }
}
