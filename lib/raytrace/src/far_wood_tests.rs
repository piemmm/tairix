extern crate std;

use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::land::NESTS;
use crate::prototype::{Part, Prototype, Tube};
use crate::wood::{Affinity, Rooting, Woodland, ANYWHERE, KINDS, VARIANTS};

/// A tree ten metres tall: a trunk, and a crown about its top.
fn tree() -> Prototype {
    let limb = |a: Vec3, b: Vec3, radius: f64| {
        Part::Tube(Tube::new(
            (a, b),
            ((radius, radius), (0.0, (b - a).length())),
            (0, 1),
            Vec3::new(1.0, 0.0, 0.0),
        ))
    };
    Prototype::new(
        vec![
            limb(Vec3::ZERO, Vec3::new(0.0, 7.0, 0.0), 0.3),
            limb(Vec3::new(0.0, 6.5, 0.0), Vec3::new(0.0, 8.0, 0.0), 2.0),
        ],
        vec![],
        vec![],
    )
    .expect("a tree")
}

/// Level ground 4 km across at nought, green all over.
fn ground() -> Vec<Heightfield> {
    rising(0.0)
}

/// Ground 4 km across, green all over, rising `slope` a metre for every
/// metre further along `z`.
fn rising(slope: f64) -> Vec<Heightfield> {
    let mut field =
        Heightfield::new(512, (-2000.0, -2000.0), 4000.0 / 512.0, false).expect("a grid");
    let side = field.side();
    assert!(field.carry_attributes());
    {
        let (heights, attributes) = field.rows_mut(0..side);
        for (index, height) in heights.iter_mut().enumerate() {
            let row = crate::vector::real(index / side);
            *height = crate::vector::single(slope * (-2000.0 + row * 4000.0 / 512.0));
        }
        attributes.fill([0, 128, 0, 0, 255, 0, 0, 0]);
    }
    field.seal();
    vec![field]
}

const GRIDS: Grids = Grids {
    far: 0,
    nests: [None; NESTS],
    water: None,
    near_water: None,
    horizon: None,
    sea: None,
    centre: (0.0, 0.0),
    reach: 2000.0,
};

fn reader(cover: f64) -> Reader {
    let habit = Habit {
        affinity: Affinity::EVEN,
        prototypes: [0; VARIANTS],
        heights: [10.0; VARIANTS],
        bark: 0,
        crown: 0.2,
        scaled: (0.75, 1.35),
    };
    let mut kinds = [None; KINDS];
    kinds[0] = Some(habit);
    Reader {
        woodland: Woodland {
            cover,
            patch: 260.0,
            closure: (0.45, 0.8),
            stature: (0.8, 1.0),
            gaps: 0.2,
            most: 1000,
            open: (1.5, 0.3),
        },
        rooting: Rooting { ..ANYWHERE },
        kinds,
        seeds: (3, 4),
    }
}

const STRETCH: Stretch = Stretch {
    eye: (0.0, 0.0),
    heading: 0.0,
    across: 1.15,
    from: 150.0,
    to: 1500.0,
    cell: 3.0,
    sown: 1.35,
};

/// The wood far off over `fields`, measured and surveyed as a scene does once
/// its trees' prototypes are grown.
fn wood(fields: &[Heightfield]) -> FarWood {
    built(&reader(0.9), fields)
}

fn built(reader: &Reader, fields: &[Heightfield]) -> FarWood {
    surveyed(
        FarWood::new((*reader, 11), (GRIDS, fields), STRETCH).expect("a wood"),
        fields,
    )
}

/// `wood` surveyed over `fields` a step at a time, as a scene's composing
/// takes it.
fn surveyed(wood: FarWood, fields: &[Heightfield]) -> FarWood {
    let prototypes = [tree()];
    let mut surveying = wood.surveying(&prototypes).expect("measured");
    let mut steps = 0;
    loop {
        steps += 1;
        match surveying
            .step((fields, &prototypes), &tairix_parallel::SERIAL)
            .expect("surveyed")
        {
            ControlFlow::Continue(more) => {
                assert!(more.done() < 1.0 && steps < 10_000);
                surveying = more;
            }
            ControlFlow::Break(wood) => return wood,
        }
    }
}

fn geometry<'a>(
    fields: &'a [Heightfield],
    prototypes: &'a [Prototype],
    woods: &'a [FarWood],
) -> Geometry<'a> {
    Geometry {
        faces: &[],
        fields,
        prototypes,
        lawns: &[],
        far_woods: woods,
        stands: &[],
        materials: &[],
        view: None,
    }
}

/// The nearest of `wood`'s trees `ray` meets in `(near, far)`, found by
/// meeting every tree whose trunk stands within its crowns' reach of the
/// ray's track, one by one.
fn met_one_by_one(
    wood: &FarWood,
    ray: &Ray,
    (near, far): (f64, f64),
    geometry: Geometry<'_>,
) -> Option<Hit> {
    let end = ray.at(far.min(4000.0));
    let (low, high) = (
        (
            ray.origin.x.min(end.x) - wood.reach,
            ray.origin.z.min(end.z) - wood.reach,
        ),
        (
            ray.origin.x.max(end.x) + wood.reach,
            ray.origin.z.max(end.z) + wood.reach,
        ),
    );
    let index = |at: f64, origin: f64| crate::noise::cell(((at - origin) / wood.cell).max(0.0)).0;
    let mut best: Option<Hit> = None;
    for row in index(low.1, wood.origin.1)..=index(high.1, wood.origin.1) {
        for column in index(low.0, wood.origin.0)..=index(high.0, wood.origin.0) {
            let place = wood.place((column, row));
            let Some(((prototype, pose, scale, key), _)) = wood.placed(place, geometry.fields)
            else {
                continue;
            };
            let reach = best.map_or(far, |hit| hit.t);
            if let Some(hit) =
                meet_placed((prototype, &pose, scale, key), ray, (near, reach), geometry)
            {
                best = Some(hit);
            }
        }
    }
    best
}

/// The nearest of `wood`'s trees `ray` meets as the scene would find it,
/// tile by tile.
fn met_walking(wood: &FarWood, ray: &Ray, far: f64, geometry: Geometry<'_>) -> Option<Hit> {
    let tiles: Vec<Tile> = wood.tiles(geometry.fields).collect();
    let mut best: Option<Hit> = None;
    for tile in &tiles {
        let reach = best.map_or(far, |hit| hit.t);
        if let Some(hit) = wood.intersect(tile, ray, (1e-6, reach), geometry) {
            best = Some(hit);
        }
    }
    best
}

#[test]
fn a_ray_meets_the_nearest_of_a_far_woods_trees_as_meeting_them_one_by_one_finds() {
    let fields = ground();
    let prototypes = vec![tree()];
    let woods = vec![wood(&fields)];
    let geometry = geometry(&fields, &prototypes, &woods);
    let wood = &woods[0];
    let (mut met, mut missed) = (0, 0);
    for index in 0..60u32 {
        let across = (f64::from(index) / 59.0 - 0.5) * 1.6;
        let fall = 0.004 + 0.02 * f64::from(index % 7) / 6.0;
        let ray = Ray::new(
            Vec3::new(0.3 * f64::from(index), 14.0, 0.0),
            Vec3::new(mathf::sin(across), -fall, mathf::cos(across)).normalized(),
        );
        // Stopped by the ground where the ray comes down to it.
        let far = 14.0 / fall;
        let walked = met_walking(wood, &ray, far, geometry);
        let one_by_one = met_one_by_one(wood, &ray, (1e-6, far), geometry);
        match (walked, one_by_one) {
            (Some(walked), Some(one_by_one)) => {
                assert!(
                    (walked.t - one_by_one.t).abs() < 1e-9,
                    "{index}: {} {}",
                    walked.t,
                    one_by_one.t
                );
                assert_eq!(walked.mark, one_by_one.mark);
                met += 1;
            }
            (None, None) => missed += 1,
            (walked, one_by_one) => panic!("{index}: {walked:?} against {one_by_one:?}"),
        }
    }
    assert!(met > 40, "{met} met, {missed} missed");
}

#[test]
fn a_ray_over_the_crowns_or_short_of_the_wood_meets_nothing_and_a_shadow_ray_agrees() {
    let fields = ground();
    let prototypes = vec![tree()];
    let woods = vec![wood(&fields)];
    let geometry = geometry(&fields, &prototypes, &woods);
    let wood = &woods[0];
    let tiles: Vec<Tile> = wood.tiles(&fields).collect();
    let skyward = Ray::new(
        Vec3::new(0.0, 14.0, 0.0),
        Vec3::new(0.0, 0.05, 1.0).normalized(),
    );
    assert!(met_walking(wood, &skyward, 1e9, geometry).is_none());
    // Behind the eye, out of the view.
    let behind = Ray::new(Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, 0.0, -1.0));
    assert!(met_walking(wood, &behind, 1e9, geometry).is_none());
    // Short of where the wood begins.
    let level = Ray::new(Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, 0.0, 1.0));
    assert!(met_walking(wood, &level, 140.0, geometry).is_none());
    let hit = met_walking(wood, &level, 1e9, geometry).expect("a tree ahead");
    assert!(
        hit.t > STRETCH.from - wood.reach && hit.t < STRETCH.to,
        "{}",
        hit.t
    );
    for tile in &tiles {
        let span = (1e-6, 1e9);
        assert_eq!(
            wood.occludes(tile, &level, span, geometry),
            wood.intersect(tile, &level, span, geometry).is_some()
        );
    }
}

#[test]
fn a_hit_on_a_far_tree_places_that_tree_again_and_carries_its_bark_and_key() {
    let fields = ground();
    let prototypes = vec![tree()];
    let woods = vec![wood(&fields)];
    let geometry = geometry(&fields, &prototypes, &woods);
    let wood = &woods[0];
    let level = Ray::new(Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, 0.0, 1.0));
    let hit = met_walking(wood, &level, 1e9, geometry).expect("a tree ahead");
    let cell = hit.member.expect("its cell");
    let (pose, key) = wood.placing(cell, &fields).expect("placed again");
    // The point met lies on the prototype as that placing places it.
    let local = pose.point_to_local(level.at(hit.t));
    let bounds = prototypes[0].bounds();
    let ((_, placed_pose, scale, placed_key), _) =
        wood.placed(wood.place(cell), &fields).expect("its tree");
    assert_eq!(placed_pose, pose);
    assert_eq!(placed_key, key);
    let within = local * (1.0 / scale);
    assert!(
        within.x >= bounds.min.x - 1e-6 && within.x <= bounds.max.x + 1e-6,
        "{within:?}"
    );
    assert_eq!(hit.mark, 1 ^ key);
    assert_eq!(hit.material, Some(0));
}

#[test]
fn a_far_wood_keeps_to_its_ring_across_the_view_and_stands_no_tree_off_its_patches() {
    let fields = ground();
    let wood = wood(&fields);
    let mut standing = 0;
    for row in 0..1300u32 {
        for column in (0..1300u32).step_by(3) {
            let place = wood.place((column, row));
            if wood.tree_of(place, &fields).is_none() {
                continue;
            }
            standing += 1;
            let (x, z) = place.0;
            let distance = mathf::hypot(x, z);
            assert!(
                (STRETCH.from..=STRETCH.to).contains(&distance),
                "{distance}"
            );
            assert!(mathf::atan2(x, z).abs() <= STRETCH.across + 1e-9);
        }
    }
    assert!(standing > 1000, "{standing}");
    // A wood covering the least share of the ground stands the fewest trees
    // over the same cells, only in the patches it keeps.
    let stood = |wood: &FarWood| {
        (0..400u32)
            .flat_map(|row| (0..400u32).map(move |column| (column + 500, row + 900)))
            .filter(|&cell| wood.tree_of(wood.place(cell), &fields).is_some())
            .count()
    };
    let bare = FarWood::new((reader(0.0), 11), (GRIDS, &fields), STRETCH).expect("a wood");
    let (few, many) = (stood(&bare), stood(&wood));
    assert!(20 * few < many, "{few} against {many}");
}

#[test]
fn a_far_woods_tiles_cover_its_ring_and_bound_its_crowns() {
    let fields = rising(0.02);
    let prototypes = [tree()];
    let wood = built(&reader(0.9), &fields);
    let tiles: Vec<Tile> = wood.tiles(&fields).collect();
    assert!(!tiles.is_empty() && tiles.len() < (TILES * TILES) as usize);
    let mut covered = 0;
    for row in (0..1334u32).step_by(5) {
        for column in (0..1334u32).step_by(5) {
            let place = wood.place((column, row));
            let Some(((prototype, pose, scale, _), _)) = wood.placed(place, &fields) else {
                continue;
            };
            let ((x, z), top) = (
                place.0,
                pose.at.y + prototypes[prototype as usize].bounds().max.y * scale,
            );
            let tile = tiles
                .iter()
                .find(|tile| {
                    tile.from.0 <= x && x <= tile.to.0 && tile.from.1 <= z && z <= tile.to.1
                })
                .expect("a tile over every tree");
            let bounds = tile.bounds();
            assert!(
                bounds.min.y <= pose.at.y && bounds.max.y >= top,
                "({x}, {z}): {top} in {bounds:?}"
            );
            covered += 1;
        }
    }
    assert!(covered > 500, "{covered}");
    // Unsurveyed, a tile reaches as high as the tallest tree over its ground.
    let unsurveyed = FarWood::new((reader(0.9), 11), (GRIDS, &fields), STRETCH).expect("a wood");
    for tile in unsurveyed.tiles(&fields) {
        assert!(tile.bounds().max.y >= tile.ground.1 + unsurveyed.tallest);
    }
    // Measured as grown: the crown's limbs swing out past what was asked of
    // it, and its top stands no lower.
    assert!(wood.reach > unsurveyed.reach && wood.tallest >= unsurveyed.tallest);
    assert!(wood.spread >= unsurveyed.spread);
}

#[test]
fn a_place_alone_bounds_the_tree_its_ground_grows() {
    let fields = ground();
    let wood = wood(&fields);
    let mut grown = 0;
    for row in (500..900u32).step_by(2) {
        for column in (500..800u32).step_by(2) {
            let place = wood.place((column, row));
            let Some(sprout) = wood.reader.sprout(place, None) else {
                continue;
            };
            if let Some((tree, _)) = wood.reader.grow(&sprout, (&wood.grids, &fields)) {
                assert!(tree.height <= wood.reader.highest(&sprout) + 1e-9);
                grown += 1;
            }
        }
    }
    assert!(grown > 100, "{grown}");
}

#[test]
fn a_survey_passes_by_the_ground_above_a_woods_tree_line_and_misses_no_tree_below_it() {
    // Rising a tenth, the tree line at 60 m: no tree roots past 600 m ahead.
    let fields = rising(0.1);
    let prototypes = vec![tree()];
    let lined = Reader {
        rooting: Rooting {
            below: Some((40.0, 60.0)),
            ..ANYWHERE
        },
        ..reader(0.9)
    };
    let woods = vec![built(&lined, &fields)];
    let geometry = geometry(&fields, &prototypes, &woods);
    let wood = &woods[0];
    let survey = wood.survey.as_ref().expect("surveyed");
    let cell_at = |(x, z): (f64, f64)| {
        (
            crate::noise::cell((x - wood.origin.0) / wood.cell).0,
            crate::noise::cell((z - wood.origin.1) / wood.cell).0,
        )
    };
    let reached = |at: (f64, f64)| {
        survey
            .of(cell_at(at))
            .1
            .is_some_and(|block| block.high.is_finite())
    };
    assert!(!reached((0.0, 900.0)), "above the tree line");
    assert!(reached((0.0, 300.0)), "below it");
    assert!(!reached((0.0, -900.0)), "behind the eye, out of the view");
    // A block's crowns stand no higher than its highest ground and its
    // tallest tree, rounded outward.
    let span = f64::from(BLOCK) * wood.cell;
    let block = survey.of(cell_at((0.0, 300.0))).1.expect("surveyed");
    let (ground, crowns) = (f64::from(block.low), f64::from(block.high));
    assert!(crowns > ground && crowns <= ground + 0.1 * 3.0 * span + wood.tallest + 1e-3);
    let (mut met, mut missed) = (0, 0);
    for index in 0..40u32 {
        let across = (f64::from(index) / 39.0 - 0.5) * 1.6;
        let ray = Ray::new(
            Vec3::new(0.0, 40.0, 0.0),
            Vec3::new(mathf::sin(across), 0.02, mathf::cos(across)).normalized(),
        );
        let walked = met_walking(wood, &ray, 1e4, geometry);
        let one_by_one = met_one_by_one(wood, &ray, (1e-6, 1e4), geometry);
        match (walked, one_by_one) {
            (Some(walked), Some(one_by_one)) => {
                assert!((walked.t - one_by_one.t).abs() < 1e-9, "{index}");
                met += 1;
            }
            (None, None) => missed += 1,
            (walked, one_by_one) => panic!("{index}: {walked:?} against {one_by_one:?}"),
        }
    }
    assert!(met > 20, "{met} met, {missed} missed");
}

#[test]
fn a_survey_marks_exactly_the_cells_that_keep_a_tree_and_bounds_every_crown_about_them() {
    let fields = rising(0.02);
    let prototypes = [tree()];
    let wood = built(&reader(0.9), &fields);
    let survey = wood.survey.as_ref().expect("surveyed");
    let spread = wood.spread;
    let (mut standing, mut read) = (0, 0);
    // Close over a stretch of the ring, sparsely over the whole lattice.
    let close = (550..850u32).flat_map(|column| (650..1100u32).map(move |row| (column, row)));
    let sparse = (0..1334u32)
        .step_by(7)
        .flat_map(|column| (0..1334u32).step_by(7).map(move |row| (column, row)));
    for cell in close.chain(sparse) {
        read += 1;
        let placed = wood.placed(wood.place(cell), &fields);
        assert_eq!(wood.standing(cell).is_some(), placed.is_some(), "{cell:?}");
        let Some(((prototype, pose, scale, _), _)) = placed else {
            continue;
        };
        standing += 1;
        let top = pose.at.y + prototypes[prototype as usize].bounds().max.y * scale;
        for down in 0..=2 * spread {
            for across in 0..=2 * spread {
                let about = (cell.0 + across - spread, cell.1 + down - spread);
                let high = survey
                    .of(about)
                    .1
                    .map_or(f64::NEG_INFINITY, |block| f64::from(block.high));
                assert!(high >= top, "{cell:?} over {about:?}: {high} under {top}");
            }
        }
    }
    assert!(standing > 2000, "{standing} of {read}");
}

#[test]
fn a_survey_keeps_to_the_ground_its_ring_across_the_view_covers() {
    let fields = ground();
    let wood = wood(&fields);
    let survey = wood.survey.as_ref().expect("surveyed");
    let span = f64::from(BLOCK) * wood.cell;
    let all_about = (2.0 * STRETCH.to / span).powi(2);
    assert!(
        crate::vector::real(survey.blocks.len()) < 0.5 * all_about,
        "{}",
        survey.blocks.len()
    );
    assert!(survey.marks.len() < survey.blocks.len());
    // Looking out off the traced square, it surveys nothing and stands no
    // tree.
    let outward = Stretch {
        eye: (1900.0, 0.0),
        heading: core::f64::consts::FRAC_PI_2,
        across: 0.3,
        ..STRETCH
    };
    let away = surveyed(
        FarWood::new((reader(0.9), 11), (GRIDS, &fields), outward).expect("a wood"),
        &fields,
    );
    let survey = away.survey.as_ref().expect("surveyed");
    assert!(survey.blocks.is_empty() && survey.marks.is_empty());
    assert!(away.standing((1300, 666)).is_none());
    assert_eq!(away.tiles(&fields).count(), 0);
}

#[test]
fn a_wood_matched_to_trees_standing_as_its_own_would_fills_the_ground_as_they_do() {
    let fields = ground();
    let stretch = Stretch {
        from: 1500.0,
        to: 1900.0,
        ..STRETCH
    };
    let wood = || FarWood::new((reader(0.9), 11), (GRIDS, &fields), stretch).expect("a wood");
    // The trees a wood filling a third of the ground stands over the ring it
    // is matched over, every place of it read.
    let aimed = FarWood {
        filled: 0.3,
        ..wood()
    };
    let ring = FarWood {
        from: MATCHED * aimed.from,
        to: aimed.from,
        ..aimed.clone()
    };
    let stood: Vec<(f64, f64)> = (0..1334u32)
        .flat_map(|row| (0..1334u32).map(move |column| (column, row)))
        .map(|cell| ring.place(cell))
        .filter(|&place| ring.tree_of(place, &fields).is_some())
        .map(|(at, _)| at)
        .collect();
    assert!(stood.len() > 5000, "{}", stood.len());
    let mut matching = wood().matching(stood.into_iter()).expect("matching");
    let (mut steps, mut done) = (0, 0.0);
    let matched = loop {
        steps += 1;
        match matching
            .step(&fields, &tairix_parallel::SERIAL)
            .expect("read")
        {
            ControlFlow::Continue(more) => {
                assert!(more.done() > done && more.done() < 1.0);
                done = more.done();
                matching = more;
            }
            ControlFlow::Break(wood) => break wood,
        }
    };
    assert!(steps > 2, "{steps}");
    assert!(
        (matched.filled / 0.3 - 1.0).abs() < 0.1,
        "{}",
        matched.filled
    );
    // Where none stood, it keeps the share its crowns fill when packed.
    let none = wood().matching(core::iter::empty()).expect("matching");
    let mut matching = none;
    let unmatched = loop {
        match matching
            .step(&fields, &tairix_parallel::SERIAL)
            .expect("read")
        {
            ControlFlow::Continue(more) => matching = more,
            ControlFlow::Break(wood) => break wood,
        }
    };
    assert!((unmatched.filled - PACKED).abs() < 1e-12);
}

#[test]
fn every_crown_lies_over_a_tile_however_near_the_rings_edge_it_stands() {
    let fields = ground();
    // Its far edge just short of a tile's, 1500 m ahead: a crown standing at
    // the edge reaches over the next tile alone.
    let stretch = Stretch {
        to: 1498.0,
        ..STRETCH
    };
    let wood = surveyed(
        FarWood::new((reader(0.9), 11), (GRIDS, &fields), stretch).expect("a wood"),
        &fields,
    );
    let tiles: Vec<Tile> = wood.tiles(&fields).collect();
    let over = |(x, z): (f64, f64)| {
        tiles
            .iter()
            .any(|tile| tile.from.0 <= x && x <= tile.to.0 && tile.from.1 <= z && z <= tile.to.1)
    };
    let mut overhanging = 0;
    for row in 0..1334u32 {
        for column in 0..1334u32 {
            let place = wood.place((column, row));
            if wood.tree_of(place, &fields).is_none() {
                continue;
            }
            let (x, z) = place.0;
            for corner in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let reached = (x + corner.0 * wood.reach, z + corner.1 * wood.reach);
                assert!(over(reached), "({x}, {z}) reaching {reached:?}");
            }
            overhanging += u32::from(z + wood.reach > 1500.0);
        }
    }
    assert!(overhanging > 0, "a crown reaches past the ring's edge");
}
