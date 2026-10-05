use core::f64::consts::TAU;

use super::*;
use crate::sample::{mix32, unit};
use crate::vector::single;
use tairix_parallel::JobRunner;

/// A grid of `cells` a side, `step` apart from `origin`, filled from
/// `height` and sealed.
fn filled(
    cells: usize,
    origin: (f64, f64),
    step: f64,
    wrap: bool,
    height: impl Fn(f64, f64) -> f64,
) -> Heightfield {
    let mut field = unsealed(cells, origin, step, wrap, height);
    field.seal();
    field
}

/// `filled`, its sealing left to be done.
fn unsealed(
    cells: usize,
    origin: (f64, f64),
    step: f64,
    wrap: bool,
    height: impl Fn(f64, f64) -> f64,
) -> Heightfield {
    let mut field = Heightfield::new(cells, origin, step, wrap).expect("a grid");
    let (((origin_x, origin_z), step), side) = (field.placing(), field.side());
    for (start, band) in field.bands(0..side, 3) {
        for (offset, row) in band.chunks_mut(side).enumerate() {
            let z = origin_z + step * real(start + offset);
            for (column, cell) in row.iter_mut().enumerate() {
                *cell = single(height(origin_x + step * real(column), z));
            }
        }
    }
    field
}

/// Rugged ground: ridges and hollows a few cells across.
fn rugged(x: f64, z: f64) -> f64 {
    3.0 * mathf::sin(0.9 * x) * mathf::cos(0.7 * z) + 1.5 * mathf::sin(2.3 * x + 1.1 * z)
}

/// A ray from above the grid at a random place, looking down across it at
/// anything from steeply to a grazing few degrees.
fn looking_down(index: u32, span: f64, height: f64) -> Ray {
    let draw = |salt: u32| unit(mix32(mix32(index) ^ salt));
    let origin = Vec3::new(span * draw(1), height, span * draw(2));
    let heading = TAU * draw(3);
    let dip = 0.03 + 1.2 * draw(4);
    let dir = Vec3::new(
        mathf::cos(dip) * mathf::cos(heading),
        -mathf::sin(dip),
        mathf::cos(dip) * mathf::sin(heading),
    );
    Ray::new(origin, dir)
}

#[test]
fn a_grid_has_cells_and_steps_forward() {
    assert!(Heightfield::new(0, (0.0, 0.0), 1.0, false).is_none());
    assert!(Heightfield::new(4, (0.0, 0.0), 0.0, false).is_none());
    assert!(Heightfield::new(4, (0.0, 0.0), -1.0, false).is_none());
    assert!(Heightfield::new(4, (0.0, 0.0), f64::NAN, false).is_none());
    for cells in [1, 3, 4, 37, 1000] {
        let field = Heightfield::new(cells, (0.0, 0.0), 1.0, false).expect("a grid");
        assert_eq!(field.side(), cells + 1);
        // Each level halves the one below, the odd block out kept.
        let mut blocks = cells;
        for &(_, at) in &field.levels {
            assert_eq!(at, blocks);
            blocks = blocks.div_ceil(2);
        }
        assert_eq!(field.levels.last().map(|level| level.1), Some(1));
    }
}

/// A grid is written as its rows are filled, not zeroed whole when it is
/// made, and sealing it reads any row left unwritten as level ground.
#[test]
fn a_grid_holds_the_rows_written_and_seals_the_rest_level() {
    let mut field = Heightfield::new(9, (0.0, 0.0), 1.0, false).expect("a grid");
    assert!(field.heights().is_empty());
    assert!(field.carry_attributes());
    for (_, band) in field.bands(0..4, 2) {
        band.fill(2.0);
    }
    assert_eq!(field.heights().len(), 4 * 10);
    let (heights, attributes) = field.rows_mut(4..6);
    assert_eq!((heights.len(), attributes.len()), (20, 20));
    heights.fill(3.0);
    attributes.fill([9; CHANNELS]);
    field.seal();
    assert_eq!(field.heights().len(), 100);
    assert_eq!((field.low, field.high), (0.0, 3.0));
    assert!((field.mean - (40.0 * 2.0 + 20.0 * 3.0) / 100.0).abs() < 1e-12);
    assert_eq!(field.attributes_of(3, 5), [9; CHANNELS]);
    assert_eq!(field.attributes_of(3, 7), [0; CHANNELS]);
}

/// A grid sealed across many cores, in whatever order they take its bands,
/// holds exactly what one sealed alone does.
#[test]
fn a_grid_seals_the_same_however_its_bands_are_shared() {
    let sealed = |runner: &dyn JobRunner| {
        let mut field = filled(300, (-20.0, -20.0), 0.13, false, rugged);
        let mut unsealed = Heightfield::new(300, (-20.0, -20.0), 0.13, false).expect("a grid");
        let side = unsealed.side();
        for (start, band) in unsealed.bands(0..side, side) {
            band.copy_from_slice(
                &field.heights()[start * side..(start + band.len() / side) * side],
            );
        }
        let mut sealing = Sealing::BEGUN;
        let mut steps = 0;
        let mut was = 0.0;
        while !sealing.step(&mut unsealed, runner) {
            let now = sealing.done(&unsealed);
            assert!(now >= was && now < 1.0, "{was} then {now}");
            was = now;
            steps += 1;
        }
        assert!(sealing.done(&unsealed) >= 1.0);
        // Each step seals at most a few bands a core, never the whole grid.
        assert!(
            steps * SEAL_CELLS * runner.width() >= 300 * 300,
            "{steps} steps"
        );
        field.seal();
        assert_eq!(field.maxima, unsealed.maxima);
        unsealed
    };
    let alone = sealed(&tairix_parallel::SERIAL);
    for runner in [
        &tairix_parallel::Reversed::new(3) as &dyn JobRunner,
        &tairix_parallel::Threaded::new(4),
    ] {
        let shared = sealed(runner);
        assert_eq!(shared.maxima, alone.maxima);
        assert_eq!(
            (
                shared.low.to_bits(),
                shared.high.to_bits(),
                shared.mean.to_bits()
            ),
            (
                alone.low.to_bits(),
                alone.high.to_bits(),
                alone.mean.to_bits()
            )
        );
    }
}

/// Heights of every magnitude, whose sum rounds as it is grouped, keep one
/// mean however many cores seal the grid they lie in.
#[test]
fn a_grids_mean_sums_alike_however_many_cores_seal_it() {
    // The first bands sum far past what the fine heights after them can be
    // added to without rounding.
    let mixed = |x: f64, z: f64| {
        if z < -12.0 {
            1.0e8 + 64.0 * rugged(x, z)
        } else {
            0.37 + 1.0e-3 * rugged(x, z)
        }
    };
    let mean = |runner: &dyn JobRunner| {
        let mut field = unsealed(300, (-20.0, -20.0), 0.13, false, mixed);
        let mut sealing = Sealing::BEGUN;
        while !sealing.step(&mut field, runner) {}
        field.mean.to_bits()
    };
    let alone = mean(&tairix_parallel::SERIAL);
    for width in [2, 3, 4, 8] {
        assert_eq!(
            mean(&tairix_parallel::Threaded::new(width)),
            alone,
            "{width} cores"
        );
    }
    assert_eq!(mean(&tairix_parallel::Reversed::new(3)), alone);
}

/// Over a grid of any size, a ray meets the nearest patch of any of its
/// cells it crosses, as testing every cell's patch in turn finds.
#[test]
fn a_grid_of_any_size_is_met_at_the_nearest_of_its_cells() {
    let mut met = 0;
    for (salt, cells) in (0u32..).zip([1usize, 2, 3, 5, 37, 100]) {
        let step = 12.0 / real(cells);
        let field = filled(cells, (0.0, 0.0), step, false, rugged);
        for index in 0..300 {
            let ray = looking_down(index + 1000 * salt, 12.0, 7.0);
            let inverse = reciprocal(ray.dir);
            let mut nearest: Option<f64> = None;
            for row in 0..cells {
                for column in 0..cells {
                    let corner = (step * real(column), step * real(row));
                    let cell = Aabb {
                        min: Vec3::new(corner.0, field.low - 1.0, corner.1),
                        max: Vec3::new(corner.0 + step, field.high + 1.0, corner.1 + step),
                    };
                    let Some((enter, leave)) = cell.span(&ray, inverse, f64::INFINITY) else {
                        continue;
                    };
                    if let Some(hit) =
                        field.patch(&ray, (column, row), corner, (enter.max(1e-9), leave))
                    {
                        nearest = Some(nearest.map_or(hit.t, |t: f64| t.min(hit.t)));
                    }
                }
            }
            let found = field.intersect(&ray, 1e-9, f64::INFINITY).map(|hit| hit.t);
            match (found, nearest) {
                (Some(found), Some(nearest)) => {
                    assert!(
                        (found - nearest).abs() < 1e-6 * (1.0 + nearest),
                        "{cells} cells, ray {index}: {found} against {nearest}"
                    );
                    met += 1;
                }
                (None, None) => {}
                (found, nearest) => {
                    panic!("{cells} cells, ray {index}: {found:?} against {nearest:?}")
                }
            }
        }
    }
    assert!(met > 400, "{met} met");
}

#[test]
fn sealing_finds_the_extremes_and_the_pyramid_tops_out_at_the_highest() {
    let field = filled(16, (-8.0, -8.0), 1.0, false, rugged);
    let (low, high) = field
        .heights
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), h| {
            (low.min(f64::from(*h)), high.max(f64::from(*h)))
        });
    assert_eq!((field.low, field.high), (low, high));
    let (top, blocks) = *field.levels.last().expect("levels");
    assert_eq!(blocks, 1);
    assert!((f64::from(field.maxima[top]) - high).abs() < 1e-12);
    let bounds = field.bounds().expect("a bounded grid");
    assert_eq!((bounds.min.y, bounds.max.y), (low, high));
    assert_eq!((bounds.min.x, bounds.max.x), (-8.0, 8.0));
}

#[test]
fn a_sloping_plane_is_met_where_it_lies() {
    let plane = |x: f64, z: f64| 0.3 * x - 0.2 * z + 1.0;
    let field = filled(32, (0.0, 0.0), 0.5, false, plane);
    let normal = Vec3::new(-0.3, 1.0, 0.2).normalized();
    let mut met = 0;
    for index in 0..500 {
        let ray = looking_down(index, 16.0, 12.0);
        // Where the ray meets the plane itself: a ray nearly along it, or
        // meeting it at the grid's very edge, says little either way.
        let rate = ray.dir.y - 0.3 * ray.dir.x + 0.2 * ray.dir.z;
        if rate.abs() < 0.1 {
            continue;
        }
        let t = -(ray.origin.y - plane(ray.origin.x, ray.origin.z)) / rate;
        let at = ray.at(t);
        let within =
            |low: f64, high: f64| (low..=high).contains(&at.x) && (low..=high).contains(&at.z);
        if within(-0.01, 16.01) && !within(0.01, 15.99) {
            continue;
        }
        match field.intersect(&ray, 1e-9, f64::INFINITY) {
            Some(hit) => {
                assert!(
                    within(0.0, 16.0),
                    "a hit off the grid at {:?}",
                    ray.at(hit.t)
                );
                assert!(
                    (hit.t - t).abs() < 1e-5 * (1.0 + t),
                    "{} against {t}",
                    hit.t
                );
                assert!((hit.normal - normal).length() < 1e-5);
                assert!((hit.shading - normal).length() < 1e-5);
                met += 1;
            }
            None => assert!(!within(0.0, 16.0), "a miss at {at:?}, on the grid"),
        }
    }
    assert!(met > 100, "{met} of 500 met the plane");
}

/// Every hit on rugged ground lies on the surface the grid describes, and
/// nothing of that surface stands in the ray's way before it: no cell's
/// patch is met beyond the cell, and no nearer cell is passed over.
#[test]
fn every_hit_is_the_nearest_meeting_with_the_surface() {
    let field = filled(64, (0.0, 0.0), 0.25, false, rugged);
    let mut met = 0;
    for index in 0..2000 {
        let ray = looking_down(index, 16.0, 8.0);
        let Some(hit) = field.intersect(&ray, 1e-9, f64::INFINITY) else {
            continue;
        };
        met += 1;
        let at = ray.at(hit.t);
        let surface = field.height_at(at.x, at.z);
        assert!(
            (at.y - surface).abs() < 1e-5,
            "ray {index} met {at:?} above {surface}"
        );
        let mut t = 0.0;
        while t < hit.t - 1e-3 {
            let before = ray.at(t);
            let inside = (0.0..16.0).contains(&before.x) && (0.0..16.0).contains(&before.z);
            assert!(
                !inside || before.y >= field.height_at(before.x, before.z) - 1e-6,
                "ray {index} passed under the surface at {before:?} before meeting it at {at:?}"
            );
            t += 0.01;
        }
    }
    assert!(met > 500, "{met} of 2000 met the ground");
}

#[test]
fn a_ray_above_or_below_everything_meets_nothing() {
    let field = filled(16, (0.0, 0.0), 1.0, false, rugged);
    let over = Ray::new(Vec3::new(-5.0, 20.0, 8.0), Vec3::new(1.0, 0.0, 0.0));
    assert!(field.intersect(&over, 1e-9, f64::INFINITY).is_none());
    let under = Ray::new(Vec3::new(8.0, -20.0, 8.0), Vec3::new(0.0, -1.0, 0.0));
    assert!(field.intersect(&under, 1e-9, f64::INFINITY).is_none());
    let short = Ray::new(Vec3::new(8.0, 20.0, 8.0), Vec3::new(0.0, -1.0, 0.0));
    assert!(
        field.intersect(&short, 1e-9, 5.0).is_none(),
        "the ground lies beyond the reach"
    );
}

#[test]
fn a_block_with_no_surface_is_never_descended_into() {
    // The half west of x = 0 has no surface: its blocks top out at minus
    // infinity, a box the slab test must never be handed.
    let field = filled(16, (-8.0, -8.0), 1.0, false, |x, z| {
        if x < 0.0 {
            f64::NEG_INFINITY
        } else {
            rugged(x, z)
        }
    });
    let mut present = 0;
    for index in 0..2000 {
        let ray = looking_down(index, 16.0, 6.0);
        let ray = Ray::new(ray.origin - Vec3::new(8.0, 0.0, 8.0), ray.dir);
        field.descend(
            &ray,
            (0.0, 0.0),
            0.0,
            (0.0, f64::INFINITY),
            |(column, _), _, _| {
                assert!(column >= 8, "ray {index} reached absent column {column}");
                present += 1;
                None
            },
        );
    }
    assert!(present > 2000, "only {present} present cells reached");
}

/// What one cell reached is told, as bits: its column and row, its corner,
/// and the span over it.
type Reached = (usize, usize, u64, u64, u64, u64);

/// The walk a block at a time, each judged as it is popped: the order and
/// the spans `descend` must reproduce.
fn descend_one_block_at_a_time(
    field: &Heightfield,
    ray: &Ray,
    (offset, lift): ((f64, f64), f64),
    (from, to): (f64, f64),
    mut reached: impl FnMut(Reached) -> Option<f64>,
) {
    let inverse = reciprocal(ray.dir);
    let Some(top) = field.levels.len().checked_sub(1) else {
        return;
    };
    let mut stack = alloc::vec![(top, 0usize, 0usize)];
    let mut reach = to;
    while let Some((level, column, row)) = stack.pop() {
        let peak = field.peak(level, column, row);
        if peak == ABSENT {
            continue;
        }
        let cells = 1usize << level;
        let x0 = field.origin.0 + offset.0 + field.step * real(column * cells);
        let z0 = field.origin.1 + offset.1 + field.step * real(row * cells);
        let size = field.step * real(cells);
        let block = Aabb {
            min: Vec3::new(x0, field.low, z0),
            max: Vec3::new(x0 + size, f64::from(peak) + lift, z0 + size),
        };
        let Some((enter, leave)) = block.padded().span(ray, inverse, reach) else {
            continue;
        };
        let enter = fmax(enter, from);
        if enter >= leave {
            continue;
        }
        if level == 0 {
            let told = (
                column,
                row,
                x0.to_bits(),
                z0.to_bits(),
                enter.to_bits(),
                leave.to_bits(),
            );
            if let Some(cut) = reached(told) {
                reach = fmin(reach, cut);
            }
            continue;
        }
        let first = (usize::from(ray.dir.x < 0.0), usize::from(ray.dir.z < 0.0));
        for (a, b) in [
            (1 - first.0, 1 - first.1),
            (first.0, 1 - first.1),
            (1 - first.0, first.1),
            first,
        ] {
            stack.push((level - 1, 2 * column + a, 2 * row + b));
        }
    }
}

#[test]
fn crossing_a_blocks_children_together_reaches_what_one_block_at_a_time_does() {
    let holed = |x: f64, z: f64| {
        if x < -2.0 && z > 3.0 {
            f64::NEG_INFINITY
        } else {
            rugged(x, z)
        }
    };
    let fields = [
        filled(16, (-8.0, -8.0), 1.0, false, holed),
        filled(64, (-24.0, -24.0), 0.75, false, rugged),
        filled(37, (-9.0, -9.0), 0.5, false, holed),
        filled(50, (-24.0, -24.0), 0.9, false, rugged),
    ];
    let mut compared = 0;
    for field in &fields {
        for (offset, lift) in [((0.0, 0.0), 0.0), ((0.0, 0.0), 0.4), ((-30.0, 12.5), 0.0)] {
            for (from, to) in [(0.0, f64::INFINITY), (2.0, 40.0)] {
                // Walked whole, and cut short every third cell as the land's
                // and the lawns' visitors cut it when they meet something.
                for cut_every in [usize::MAX, 3] {
                    for index in 0..600 {
                        let span = field.span();
                        let base = looking_down(index, span, 7.0);
                        let ray = Ray::new(
                            base.origin
                                + Vec3::new(
                                    field.origin.0 + offset.0,
                                    0.0,
                                    field.origin.1 + offset.1,
                                ),
                            base.dir,
                        );
                        let walk = |sink: &mut alloc::vec::Vec<Reached>, told: Reached| {
                            sink.push(told);
                            sink.len()
                                .is_multiple_of(cut_every)
                                .then(|| f64::from_bits(told.5))
                        };
                        let mut lanes = alloc::vec::Vec::new();
                        field.descend(
                            &ray,
                            offset,
                            lift,
                            (from, to),
                            |(column, row), corner, span| {
                                let told = (
                                    column,
                                    row,
                                    corner.0.to_bits(),
                                    corner.1.to_bits(),
                                    span.0.to_bits(),
                                    span.1.to_bits(),
                                );
                                walk(&mut lanes, told)
                            },
                        );
                        let mut single = alloc::vec::Vec::new();
                        descend_one_block_at_a_time(
                            field,
                            &ray,
                            (offset, lift),
                            (from, to),
                            |told| walk(&mut single, told),
                        );
                        assert_eq!(lanes, single, "ray {index}, offset {offset:?}, lift {lift}");
                        compared += lanes.len();
                    }
                }
            }
        }
    }
    assert!(compared > 20_000, "only {compared} cells compared");
}

#[test]
fn shading_normals_are_smooth_across_cell_walls() {
    let field = filled(32, (0.0, 0.0), 0.5, false, rugged);
    for wall in 2..30 {
        let x = 0.5 * f64::from(wall);
        let down = |x: f64| {
            field.intersect(
                &Ray::new(Vec3::new(x, 20.0, 7.3), -Vec3::UP),
                1e-9,
                f64::INFINITY,
            )
        };
        let (left, right) = (down(x - 1e-7).expect("met"), down(x + 1e-7).expect("met"));
        assert!(
            (left.shading - right.shading).length() < 1e-4,
            "wall {wall}"
        );
    }
}

/// A ray crossing a wrapping grid's tiles to their reach without meeting
/// them meets the grid's mean level beyond — the sea runs on to the horizon —
/// while one rising from above it, or with the reach beyond its own, does not.
#[test]
fn a_wrapping_grid_lies_at_its_mean_level_past_its_reach() {
    let span = 16.0;
    let wave = |x: f64, z: f64| 0.3 + mathf::sin(TAU * x / span) * mathf::cos(TAU * 2.0 * z / span);
    let field = filled(64, (0.0, 0.0), span / 64.0, true, wave);
    assert!((field.mean - 0.3).abs() < 0.02, "{}", field.mean);
    let shallow = Ray::new(
        Vec3::new(0.0, 30.0, 0.0),
        Vec3::new(1.0, -0.002, 0.3).normalized(),
    );
    let hit = field
        .intersect(&shallow, 1e-9, f64::INFINITY)
        .expect("the far sea");
    assert!(hit.t > WRAP_REACH, "{}", hit.t);
    assert!((shallow.at(hit.t).y - field.mean).abs() < 1e-6);
    assert_eq!(hit.normal, Vec3::UP);
    assert!(
        field.intersect(&shallow, 1e-9, WRAP_REACH).is_none(),
        "short of it"
    );
    let rising = Ray::new(
        Vec3::new(0.0, 30.0, 0.0),
        Vec3::new(1.0, 0.002, 0.3).normalized(),
    );
    assert!(field.intersect(&rising, 1e-9, f64::INFINITY).is_none());
}

#[test]
fn a_wrapping_grid_repeats_across_the_plane_without_end() {
    let span = 16.0;
    let wave = |x: f64, z: f64| mathf::sin(TAU * x / span) * mathf::cos(TAU * 2.0 * z / span);
    let field = filled(64, (0.0, 0.0), span / 64.0, true, wave);
    assert!(field.bounds().is_none());
    for index in 0..200 {
        let draw = |salt: u32| unit(mix32(mix32(index) ^ salt));
        let (x, z) = (span * draw(1), span * draw(2));
        let there = field.height_at(x, z);
        assert!((field.height_at(x + 3.0 * span, z - 2.0 * span) - there).abs() < 1e-6);
        assert!((field.height_at(x - span, z + 5.0 * span) - there).abs() < 1e-6);
    }
    // Far from the first tile, and over many tiles, the surface is still
    // met where it lies.
    let mut met = 0;
    for index in 0..500 {
        let base = looking_down(index, span, 3.0);
        let ray = Ray::new(
            base.origin + Vec3::new(-250.0, 0.0, 130.0),
            Vec3::new(base.dir.x, -0.02, base.dir.z).normalized(),
        );
        let Some(hit) = field.intersect(&ray, 1e-9, f64::INFINITY) else {
            continue;
        };
        met += 1;
        let at = ray.at(hit.t);
        assert!(
            (at.y - field.height_at(at.x, at.z)).abs() < 1e-5,
            "ray {index} at {at:?}"
        );
    }
    assert!(met > 400, "{met} of 500 met the sea");
}

/// Two grids of a scene's are taken apart, one to write and one to read, in
/// either order; the same grid twice, or one past the last, is none.
#[test]
fn grids_are_taken_apart_and_never_past_the_last() {
    let mut fields: alloc::vec::Vec<Heightfield> = (1..=3)
        .map(|cells| Heightfield::new(cells, (0.0, 0.0), 1.0, false).expect("a grid"))
        .collect();
    for (written, read) in [(0, 2), (2, 0), (1, 2), (2, 1)] {
        let (write, from) = apart(&mut fields, written, read).expect("two grids apart");
        assert_eq!((write.side(), from.side()), (written + 2, read + 2));
    }
    assert!(apart(&mut fields, 1, 1).is_none());
    assert!(apart(&mut fields, 0, 3).is_none());
    assert!(apart(&mut fields, 7, 1).is_none());
}
