//! Host tests of a land's levels: a level refined in pieces is the level
//! refined whole, a piece that would read short of its parent or lie off the
//! land is refused, and snow lies on a land as the wind drifted it.

use alloc::vec::Vec;

use tairix_terrain::droplet::{Placed, Unkept};

use super::*;
use crate::coarse::Field;
use crate::plan::{Character, Grain, Grains, Lattice, Lattices, Level, Plan, LEVELS};
use crate::snow::Snowpack;
use crate::testing::{hills, refined, runner, worn};

/// Level 0 at `ratio` times the coarse lattice's density and level 1 at
/// twice that, droplets running over both.
fn two_levels(ratio: u32) -> [Option<Level>; LEVELS] {
    let mut levels = [None; LEVELS];
    levels[0] = Some(Level {
        ratio,
        droplets: 0.02,
    });
    levels[1] = Some(Level {
        ratio: 2,
        droplets: 0.04,
    });
    levels
}

/// Whether `piece` holds, bit for bit, what `whole` holds over its square.
fn matches(piece: &Piece, whole: &Piece) -> Result<(), (i64, i64)> {
    let (end_x, end_z) = piece.square.end().expect("a square");
    for z in piece.square.origin.1..end_z {
        for x in piece.square.origin.0..end_x {
            let (Some(own), Some(all)) = (piece.square.index((x, z)), whole.square.index((x, z)))
            else {
                return Err((x, z));
            };
            if piece.heights[own].to_bits() != whole.heights[all].to_bits()
                || piece.attributes[own] != whole.attributes[all]
            {
                return Err((x, z));
            }
        }
    }
    Ok(())
}

/// The square `side` a side from `offset` vertices into `within`.
fn square_in(within: Placed, offset: (i64, i64), side: u32) -> Placed {
    Placed::square(
        (within.origin.0 + offset.0, within.origin.1 + offset.1),
        side,
    )
}

/// Level 0 refined over the whole land and refined in pieces, a corner, an
/// edge and the middle, each held with its droplets' halo about it, is the
/// one land to the bit, heights and all each vertex carries.
#[test]
fn a_level_refined_in_pieces_is_the_level_refined_whole() {
    let runner = runner();
    let plan = hills((4000.0, 400), true, two_levels(4), 5);
    let coarse = worn(plan, &runner);
    let domain = coarse.lattices().domain(0).expect("a domain");
    let whole = refined(&coarse, (0, domain), None, &runner);
    let middle = i64::from(domain.size.0 / 2) - 100;
    for offset in [(0, 0), (middle, 0), (middle, middle)] {
        let square = square_in(domain, offset, 201);
        assert_ne!(
            coarse.lattices().held(0, square),
            Ok(domain),
            "the piece at {offset:?} is worn from less than all the land"
        );
        let piece = refined(&coarse, (0, square), None, &runner);
        assert_eq!(matches(&piece, &whole), Ok(()), "the piece at {offset:?}");
    }
}

/// Ground the water wore away reads as worn at level 0 and on the finer
/// level below it, as ground it laid down reads as laid: the one is never
/// lost to the other.
#[test]
fn worn_ground_reads_as_worn_on_every_level() {
    let runner = runner();
    let coarse = worn(hills((3000.0, 128), true, two_levels(2), 21), &runner);
    let far = refined(
        &coarse,
        (0, coarse.lattices().domain(0).expect("a domain")),
        None,
        &runner,
    );
    let region = square_in(
        coarse.lattices().domain(1).expect("a domain"),
        (40, 40),
        161,
    );
    let near = refined(&coarse, (1, region), Some(&far), &runner);
    for (level, piece) in [(0, &far), (1, &near)] {
        let count = |kept: &dyn Fn(u8) -> bool| {
            piece
                .attributes
                .iter()
                .filter(|attributes| kept(attributes[SEDIMENT]))
                .count()
        };
        let (worn, laid) = (count(&|byte| byte < 120), count(&|byte| byte > 136));
        let all = piece.attributes.len();
        assert!(worn > all / 50, "level {level}: {worn} of {all} worn");
        assert!(laid > 0, "level {level}: none of {all} laid");
    }
}

/// A land, its level 0 and a finer level of it come out the same to the bit
/// however their work is shared: on one core, across a few, and handed out
/// in reverse across many.
#[test]
fn a_land_is_the_same_however_its_work_is_shared() {
    let land = |runner: &dyn tairix_parallel::JobRunner| {
        let coarse = worn(hills((3000.0, 128), true, two_levels(2), 13), runner);
        let domain = coarse.lattices().domain(0).expect("a domain");
        let far = refined(&coarse, (0, domain), None, runner);
        let region = square_in(
            coarse.lattices().domain(1).expect("a domain"),
            (40, 40),
            121,
        );
        let near = refined(&coarse, (1, region), Some(&far), runner);
        (far, near)
    };
    let (far, near) = land(&tairix_parallel::SERIAL);
    let threaded = tairix_parallel::Threaded::new(3);
    let reversed = tairix_parallel::Reversed::new(9);
    for runner in [&threaded as &dyn tairix_parallel::JobRunner, &reversed] {
        let (again, closer) = land(runner);
        assert_eq!(matches(&again, &far), Ok(()), "level 0");
        assert_eq!(matches(&closer, &near), Ok(()), "level 1");
    }
    assert!(reversed.widest() > 1, "the work was shared at all");
}

/// A finer level refined over a square of it and in pieces within that
/// square, each read from its parent, is one land to the bit.
#[test]
fn a_finer_level_refined_in_pieces_is_the_level_refined_whole() {
    let runner = runner();
    let plan = hills((4000.0, 400), true, two_levels(2), 9);
    let coarse = worn(plan, &runner);
    let far = refined(
        &coarse,
        (0, coarse.lattices().domain(0).expect("a domain")),
        None,
        &runner,
    );
    let domain = coarse.lattices().domain(1).expect("a domain");
    let region = square_in(domain, (300, 350), 801);
    let whole = refined(&coarse, (1, region), Some(&far), &runner);
    for offset in [(0, 0), (600, 0), (300, 300)] {
        let square = square_in(region, offset, 201);
        assert_ne!(
            coarse.lattices().held(1, square),
            coarse.lattices().held(1, region),
            "the piece is worn from less than the region is"
        );
        let piece = refined(&coarse, (1, square), Some(&far), &runner);
        assert_eq!(matches(&piece, &whole), Ok(()), "the piece at {offset:?}");
    }
}

/// A piece lying off its land is refused, a finer one whose parent holds
/// less than it reads is refused rather than read short, and level 0, which
/// reads the coarse land itself, reads no parent.
#[test]
fn a_piece_off_its_land_or_short_of_its_parent_is_refused() {
    let runner = runner();
    let plan = hills((1500.0, 64), false, two_levels(2), 3);
    let coarse = worn(plan, &runner);
    let domain = coarse.lattices().domain(0).expect("a domain");
    let off = Placed::square((domain.origin.0 - 1, domain.origin.1), 9);
    assert_eq!(
        Refining::new(&coarse, (0, off), Keeps::Everything, &mut Unkept).err(),
        Some(Error::Shape)
    );
    assert_eq!(
        coarse.lattices().needs(0, domain).err(),
        Some(Error::Shape),
        "level 0 reads no parent"
    );
    let square = square_in(coarse.lattices().domain(1).expect("a domain"), (40, 40), 33);
    let reads = coarse.lattices().needs(1, square).expect("what it reads");
    let short = Placed {
        origin: reads.origin,
        size: (reads.size.0 - 1, reads.size.1),
    };
    let piece = refined(&coarse, (0, short), None, &runner);
    let mut refining =
        Refining::new(&coarse, (1, square), Keeps::Everything, &mut Unkept).expect("a refinement");
    assert_eq!(
        refining
            .step(&coarse, Some(piece.parent()), &mut Unkept, &runner)
            .err(),
        Some(Error::Shape)
    );
    assert_eq!(
        refining.step(&coarse, None, &mut Unkept, &runner).err(),
        Some(Error::Shape),
        "a finer level reads its parent"
    );
}

/// A land under snow stands above the same land bare by the depth the wind
/// left lying there, which its vertices carry: deep in the lee of its hills
/// and thin where it scoured them, so the land is drifted, not evenly
/// mantled, and grows only where the snow lies too thin to bury its grass.
#[test]
fn snow_lies_on_a_land_as_the_wind_drifted_it() {
    let runner = runner();
    let mut levels = two_levels(2);
    if let Some(far) = levels[0].as_mut() {
        far.droplets = 0.0;
    }
    let mut bare = hills((1500.0, 128), false, levels, 7);
    bare.relief.form = crate::relief::Landform::Hills {
        scale: 260.0,
        height: 50.0,
        seed: 7,
    };
    (bare.relief.rim, bare.relief.tilt) = (None, (0.0, 0.0));
    (bare.road, bare.ridges, bare.roughness) = (None, 0.0, 0.6);
    let pack = Snowpack {
        heading: 0.6,
        fallen: 0.4,
        seed: 3,
    };
    let snowy = Plan {
        snowpack: Some(pack),
        ..bare.clone()
    };
    let far = |plan: Plan| {
        let land = worn(plan, &runner);
        let domain = land.lattices().domain(0).expect("a domain");
        refined(&land, (0, domain), None, &runner)
    };
    let (bare, snowy) = (far(bare), far(snowy));
    let side = usize::try_from(snowy.square.size.0).expect("a side");
    let (mut depths, mut deepest, mut thinnest) = (Vec::new(), 0.0f64, f64::INFINITY);
    for row in (8..side - 8).step_by(7) {
        for column in (8..side - 8).step_by(7) {
            let at = row * side + column;
            let [_, _, _, _, green, kept, ..] = snowy.attributes[at];
            let depth = snow::depth_of(f64::from(kept) / 255.0);
            let risen = f64::from(snowy.heights[at] - bare.heights[at]);
            assert!(
                (risen - depth).abs() < 0.01 + 0.03 * depth,
                "({column}, {row}): risen {risen} by snow {depth}"
            );
            // The least depth the byte kept stands for buries the least.
            let least = snow::depth_of(mathf::fmax(f64::from(kept) - 0.5, 0.0) / 255.0);
            let showing = 1.0 - snow::buries(least, GRASS_BURIED);
            assert!(
                f64::from(green) / 255.0 <= showing + 0.01,
                "({column}, {row}): {green} grows through {depth} of snow"
            );
            deepest = mathf::fmax(deepest, depth);
            thinnest = mathf::fmin(thinnest, depth);
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

/// A finer vertex reads its parent's quantities as the parent's own vertices
/// carry them, right to the land's last vertex, where it takes the parent's
/// last rather than the one before it.
#[test]
fn a_finer_vertex_reads_its_parents_last_vertex_at_the_lands_edge() {
    let square = Placed::square((0, 0), 3);
    let heights = [0.0_f32; 9];
    let mut attributes = [[0_u8; crate::attributes::CHANNELS]; 9];
    for (index, carried) in attributes.iter_mut().enumerate() {
        carried[WET] = u8::try_from(index * 20).expect("a byte");
    }
    let reading = Reading {
        parent: Parent {
            square,
            heights: &heights,
            attributes: &attributes,
        },
        domain: square,
        ratio: 2.0,
        step: 1.0,
    };
    let wet = |at: (i64, i64)| reading.quantities(at)[WET];
    for (child, vertex) in [
        ((0, 0), 0),
        ((2, 0), 1),
        ((4, 0), 2),
        ((4, 4), 8),
        ((0, 4), 6),
    ] {
        let expected = f64::from(attributes[vertex][WET]) / 255.0;
        assert!(
            (wet(child) - expected).abs() < 1e-12,
            "the child's {child:?} reads {} against its parent vertex's {expected}",
            wet(child)
        );
    }
    // Between two of the parent's vertices, halfway.
    let between = f64::from(attributes[0][WET]).midpoint(f64::from(attributes[1][WET])) / 255.0;
    assert!((wet((1, 0)) - between).abs() < 1e-12);
}

/// A flat field 64 cells of 30 m a side, refined to level 0 at 5 m with no
/// droplets, its grain `grains`: what it holds is its detail alone.
fn flat(grains: Grains) -> Piece {
    flat_at((10.0, None, 90.0), grains)
}

/// A field lying flat at `level` under a sea at `sea` if it has one, 64 cells
/// of 30 m a side, its longest wave `longest`, refined to level 0 at 5 m with
/// no droplets, its grain `grains`.
fn flat_at((level, sea, longest): (f64, Option<f64>, f64), grains: Grains) -> Piece {
    let runner = runner();
    let mut levels = [None; LEVELS];
    levels[0] = Some(Level {
        ratio: 6,
        droplets: 0.0,
    });
    let lattices = Lattices::new(
        Lattice {
            step: 30.0,
            origin: (-32, -32),
            cells: 64,
        },
        levels,
    )
    .expect("lattices");
    let field = Field {
        lattices,
        heights: alloc::vec![level; 65 * 65],
        filled: alloc::vec![level; 65 * 65],
        wetness: alloc::vec![0.0; 65 * 65],
        grains,
        character: Character {
            longest,
            growth: 1.0,
            snow_line: None,
            sea,
            snowpack: None,
            seed: 11,
        },
    };
    let land = Coarse::of_field(field, &runner).expect("a field");
    let domain = land.lattices().domain(0).expect("a domain");
    refined(&land, (0, domain), None, &runner)
}

/// The mean and spread of a piece's heights.
fn spread_of(piece: &Piece) -> (f64, f64) {
    let count = f64::from(u32::try_from(piece.heights.len()).expect("a count"));
    let mean = piece.heights.iter().map(|&h| f64::from(h)).sum::<f64>() / count;
    let square = piece
        .heights
        .iter()
        .map(|&h| (f64::from(h) - mean) * (f64::from(h) - mean))
        .sum::<f64>()
        / count;
    (mean, mathf::sqrt(square))
}

/// A land's grain shapes the relief its levels add: none where it is not
/// rough, hummocks, ridges and dunes each a relief of their own about the
/// land's level, and where the grain varies, rough where it is rough and
/// smooth where it is not.
#[test]
fn a_lands_grain_shapes_its_relief() {
    let even = |roughness: f64, ridges: f64, dunes: f64| {
        flat(Grains::Even(Grain {
            ridges,
            dunes,
            ..Grain::hummocks(roughness)
        }))
    };
    let smooth = even(0.0, 0.0, 0.0);
    assert!(smooth
        .heights
        .iter()
        .all(|&h| (f64::from(h) - 10.0).abs() < 1e-9));
    let forms = [
        even(8.0, 0.0, 0.0),
        even(8.0, 1.0, 0.0),
        even(8.0, 0.0, 1.0),
    ];
    for (form, piece) in forms.iter().enumerate() {
        let (mean, spread) = spread_of(piece);
        assert!(spread > 0.2, "form {form} raises a relief: {spread}");
        assert!(
            (mean - 10.0).abs() < 0.4 * spread + 0.05,
            "form {form} about the land's level: {mean}"
        );
    }
    for (one, other) in [(0, 1), (0, 2), (1, 2)] {
        assert_ne!(
            forms[one].heights, forms[other].heights,
            "forms {one} and {other} differ"
        );
    }
    // Rough along the west of the land, smooth along its east.
    let grains = (0..65 * 65)
        .map(|index| Grain::hummocks(if index % 65 < 32 { 8.0 } else { 0.0 }))
        .collect();
    let half = flat(Grains::Sampled(grains));
    let side = usize::try_from(half.square.size.0).expect("a side");
    let column = |at: usize| -> f64 {
        (0..side)
            .map(|row| (f64::from(half.heights[row * side + at]) - 10.0).abs())
            .fold(0.0, mathf::fmax)
    };
    assert!(column(side / 8) > 0.2, "rough in the west");
    assert!(column(side - 2) < 1e-9, "smooth in the east");
}

/// A flat field like [`flat`]'s, eight cells of 30 m a side refined straight
/// to a metre: fine enough for a stride's knobs.
fn flat_fine(grains: Grains) -> Piece {
    flat_refined(&[30], grains)
}

/// [`flat_fine`]'s field refined through a level for each of `ratios`, and
/// the last level's whole square.
fn flat_refined(ratios: &[u32], grains: Grains) -> Piece {
    let runner = runner();
    let mut levels = [None; LEVELS];
    for (level, &ratio) in levels.iter_mut().zip(ratios) {
        *level = Some(Level {
            ratio,
            droplets: 0.0,
        });
    }
    let lattices = Lattices::new(
        Lattice {
            step: 30.0,
            origin: (-4, -4),
            cells: 8,
        },
        levels,
    )
    .expect("lattices");
    let field = Field {
        lattices,
        heights: alloc::vec![10.0; 9 * 9],
        filled: alloc::vec![10.0; 9 * 9],
        wetness: alloc::vec![0.0; 9 * 9],
        grains,
        character: Character {
            longest: 90.0,
            growth: 1.0,
            snow_line: None,
            sea: None,
            snowpack: None,
            seed: 7,
        },
    };
    let land = Coarse::of_field(field, &runner).expect("a field");
    let mut piece = refined(
        &land,
        (0, land.lattices().domain(0).expect("a domain")),
        None,
        &runner,
    );
    for level in 1..ratios.len() {
        let domain = land.lattices().domain(level).expect("a domain");
        piece = refined(&land, (level, domain), Some(&piece), &runner);
    }
    piece
}

/// The knobs a field is studded with stand as high refined through a level of
/// a metre to one of half a metre as refined straight to it: each level adds
/// only what it resolves of them beyond its parent.
#[test]
fn knobs_are_added_once_however_many_levels_resolve_them() {
    let knobbed = || {
        Grains::Even(Grain {
            knobs: 0.5,
            ..Grain::hummocks(0.0)
        })
    };
    let (_, straight) = spread_of(&flat_refined(&[60], knobbed()));
    let (_, stepped) = spread_of(&flat_refined(&[30, 2], knobbed()));
    assert!(
        (stepped - straight).abs() < 0.25 * straight,
        "{stepped} through two levels against {straight} straight"
    );
}

/// The mean height a piece's interior vertices stand from their four
/// neighbours' mean: how much of its relief is in its shortest waves.
fn short_waves(piece: &Piece) -> f64 {
    let side = usize::try_from(piece.square.size.0).expect("a side");
    let at = |x: usize, y: usize| f64::from(piece.heights[y * side + x]);
    let (mut total, mut count) = (0.0, 0.0);
    for y in 1..side - 1 {
        for x in 1..side - 1 {
            let around = (at(x - 1, y) + at(x + 1, y) + at(x, y - 1) + at(x, y + 1)) / 4.0;
            total += mathf::fabs(at(x, y) - around);
            count += 1.0;
        }
    }
    total / count
}

/// Gullies raise a relief of their own, leaning to the longest waves leaves
/// less of the shortest and leaning to the shortest more, and knobs stud only
/// a level fine enough to hold them.
#[test]
fn gullies_a_lean_and_knobs_each_shape_the_relief() {
    let with = |grain: Grain| flat(Grains::Even(grain));
    let hummocks = with(Grain::hummocks(8.0));
    let gullied = with(Grain {
        gullies: 1.0,
        ..Grain::hummocks(8.0)
    });
    let ridged = with(Grain {
        ridges: 1.0,
        ..Grain::hummocks(8.0)
    });
    let (mean, spread) = spread_of(&gullied);
    assert!(spread > 0.2, "gullies raise a relief: {spread}");
    assert!(
        (mean - 10.0).abs() < 0.4 * spread + 0.05,
        "about the land's level: {mean}"
    );
    assert_ne!(
        gullied.heights, hummocks.heights,
        "gullies are not hummocks"
    );
    assert_ne!(gullied.heights, ridged.heights, "gullies are not ridges");

    let swollen = with(Grain {
        lean: 1.0,
        ..Grain::hummocks(8.0)
    });
    assert!(
        short_waves(&swollen) < 0.8 * short_waves(&hummocks),
        "a swell leaves {} of the shortest waves against {}",
        short_waves(&swollen),
        short_waves(&hummocks)
    );
    let broken = with(Grain {
        lean: -1.0,
        ..Grain::hummocks(8.0)
    });
    assert!(
        short_waves(&broken) > 1.25 * short_waves(&hummocks),
        "broken ground leaves {} of the shortest waves against {}",
        short_waves(&broken),
        short_waves(&hummocks)
    );

    let knobbed = Grain {
        knobs: 0.5,
        ..Grain::hummocks(0.0)
    };
    assert!(
        with(knobbed)
            .heights
            .iter()
            .all(|&h| (f64::from(h) - 10.0).abs() < 1e-9),
        "a level of 5 m holds no knobs of 3 m"
    );
    let studded = flat_fine(Grains::Even(knobbed));
    let (_, spread) = spread_of(&studded);
    assert!(
        (0.05..0.5).contains(&spread),
        "knobs half a metre high: {spread}"
    );
    assert!(short_waves(&studded) > 0.01, "they are the shortest waves");
}

/// Where a level is too coarse for even the land's longest wave, it adds no
/// detail, and the band starts at the first level that holds it.
#[test]
fn a_level_too_coarse_for_the_band_adds_none_of_it() {
    let rough = Grains::Even(Grain {
        ridges: 0.5,
        dunes: 0.2,
        ..Grain::hummocks(8.0)
    });
    // Level 0's step is 5 m: a 15 m wave is under four of its steps.
    let short = flat_at((10.0, None, 15.0), rough.clone());
    assert!(short
        .heights
        .iter()
        .all(|&h| (f64::from(h) - 10.0).abs() < 1e-9));
    let long = flat_at((10.0, None, 90.0), rough);
    assert!(long
        .heights
        .iter()
        .any(|&h| (f64::from(h) - 10.0).abs() > 0.1));
}

/// Ground under the sea shelves away from its shore steeply for a shore's
/// face, and beneath that lies as the land does, however deep.
#[test]
fn the_sea_floor_shelves_only_its_shores_face() {
    let smooth = Grains::Even(Grain::hummocks(0.0));
    for (depth, below) in [(2.0, 4.0 * 2.0 + 3.0 * 0.3), (100.0, 100.0 + 30.0)] {
        let floor = flat_at((-depth, Some(0.0), 90.0), smooth.clone());
        assert!(
            floor
                .heights
                .iter()
                .all(|&h| (f64::from(h) + below).abs() < 1e-4),
            "{depth}: {:?} rather than {}",
            floor.heights.first(),
            -below
        );
    }
}

/// A finer level read from a parent that keeps only its heights is the level
/// read from one keeping all it carries, where no snow lies to read from it.
#[test]
fn a_parent_keeping_only_its_heights_refines_the_same_heights() {
    let runner = runner();
    let coarse = worn(hills((1500.0, 128), true, two_levels(2), 5), &runner);
    let far = refined(
        &coarse,
        (0, coarse.lattices().domain(0).expect("a domain")),
        None,
        &runner,
    );
    let square = square_in(
        coarse.lattices().domain(1).expect("a domain"),
        (300, 300),
        65,
    );
    let refine = |parent: Parent<'_>| {
        let mut refining = Refining::new(&coarse, (1, square), Keeps::Everything, &mut Unkept)
            .expect("a refinement");
        while !refining
            .step(&coarse, Some(parent), &mut Unkept, &runner)
            .expect("refines")
        {}
        refining.finish().expect("a piece")
    };
    let whole = refine(far.parent());
    let bare = refine(Parent {
        attributes: &[],
        ..far.parent()
    });
    assert_eq!(whole.heights, bare.heights);
}

/// A refinement keeping only its heights leaves the heights of one keeping
/// everything, to the bit, on level 0 and the level after, and carries
/// nothing.
#[test]
fn a_piece_keeping_only_its_heights_has_the_heights_of_one_keeping_all() {
    let runner = runner();
    let coarse = worn(hills((1500.0, 128), true, two_levels(2), 5), &runner);
    let refine = |(level, square), keeps, parent: Option<Parent<'_>>| {
        let mut refining =
            Refining::new(&coarse, (level, square), keeps, &mut Unkept).expect("a refinement");
        while !refining
            .step(&coarse, parent, &mut Unkept, &runner)
            .expect("refines")
        {}
        refining.finish().expect("a piece")
    };
    let domain = coarse.lattices().domain(0).expect("a domain");
    let (far, bare) = (
        refine((0, domain), Keeps::Everything, None),
        refine((0, domain), Keeps::Heights, None),
    );
    assert_eq!(far.heights, bare.heights);
    assert!(bare.attributes.is_empty() && !far.attributes.is_empty());
    let square = square_in(
        coarse.lattices().domain(1).expect("a domain"),
        (300, 300),
        65,
    );
    let fine = refine((1, square), Keeps::Everything, Some(far.parent()));
    let only = refine((1, square), Keeps::Heights, Some(bare.parent()));
    assert_eq!(fine.heights, only.heights);
    assert!(only.attributes.is_empty());
}

/// Where snow lies a finer level takes its parent's snow out of the ground
/// beneath it, so a parent carrying none is refused rather than read as bare.
#[test]
fn a_parent_keeping_only_its_heights_is_refused_where_snow_lies() {
    let runner = runner();
    let snowy = Plan {
        snowpack: Some(Snowpack {
            heading: 0.6,
            fallen: 0.4,
            seed: 3,
        }),
        ..hills((1500.0, 128), false, two_levels(2), 7)
    };
    let coarse = worn(snowy, &runner);
    let domain = coarse.lattices().domain(0).expect("a domain");
    let mut far =
        Refining::new(&coarse, (0, domain), Keeps::Heights, &mut Unkept).expect("level 0");
    while !far
        .step(&coarse, None, &mut Unkept, &runner)
        .expect("refines")
    {}
    let far = far.finish().expect("a piece");
    let square = square_in(
        coarse.lattices().domain(1).expect("a domain"),
        (300, 300),
        65,
    );
    let mut fine =
        Refining::new(&coarse, (1, square), Keeps::Heights, &mut Unkept).expect("a refinement");
    assert_eq!(
        fine.step(&coarse, Some(far.parent()), &mut Unkept, &runner)
            .err(),
        Some(Error::Shape)
    );
}

/// A parent keeping only its heights reads as plain ground, not as ground the
/// water wore away everywhere.
#[test]
fn a_parent_keeping_only_its_heights_reads_as_plain_ground() {
    let coarse = worn(hills((1500.0, 128), true, two_levels(2), 5), &runner());
    let heights = [0.0; 16];
    let reading = Reading {
        parent: Parent {
            square: Placed::square((0, 0), 4),
            heights: &heights,
            attributes: &[],
        },
        domain: coarse.lattices().domain(0).expect("a domain"),
        ratio: 2.0,
        step: 2.0,
    };
    assert_eq!(
        reading.quantities((3, 3)).map(f64::to_bits),
        crate::attributes::PLAIN.map(f64::to_bits)
    );
}
