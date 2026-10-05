//! Host tests of a scene's stones: a stream bed's stones as big as its
//! bankfull flow just stirs, worn from fresh to as far as its rock rounds, a
//! slate bed angular however far it has come, and a stone bedded into the
//! ground by the share of its height asked.

use alloc::vec::Vec;

use super::*;
use crate::course::{Courses, Mark, Reach};
use crate::detail::Detail;
use crate::land::NESTS;

fn brook(lithology: Lithology, run: f64) -> Brook {
    Brook {
        course: 0,
        station: station(0.01),
        form: Form {
            flowing: 0.5,
            ledges: 0.0,
            outcrops: 0.0,
            seed: 1,
        },
        run,
        lithology,
    }
}

/// A place 120 m along a stream 5 m across and 0.8 m deep, falling `fall`.
fn station(fall: f64) -> Station {
    Station {
        along: 120.0,
        brim: 0.0,
        width: 5.0,
        depth: 0.8,
        phase: 20.0,
        turn: 0.0,
        fall,
    }
}

/// The size class holding the most of a bed's stones at `station`.
fn commonest(station: &Station) -> usize {
    let sizes = Sizes::of(station, 1.2);
    (0..CLASSES)
        .max_by(|&a, &b| sizes.chances[a].total_cmp(&sizes.chances[b]))
        .expect("a class")
}

#[test]
fn a_beds_median_stone_is_the_one_its_bankfull_flow_just_stirs() {
    let station = brook(Lithology::Granite, 1000.0).station;
    // Shields: 0.8 m deep on a slope of a hundredth stirs stones of about
    // 11 cm, in the class from 9.6 cm.
    let median = station.depth * station.fall / (1.65 * 0.045);
    let class = commonest(&station);
    assert!(
        Sizes::least(class) <= median && median < 2.0 * Sizes::least(class),
        "class {class} for a median of {median}"
    );
    // A steep stream's median keeps to the largest its floods move.
    let class = commonest(&self::station(0.08));
    assert!(Sizes::least(class) <= MEDIAN && MEDIAN < 2.0 * Sizes::least(class));
    let sizes = Sizes::of(&station, 1.2);
    assert!(sizes
        .chances
        .iter()
        .all(|&chance| (0.0..=1.0).contains(&chance)));
}

#[test]
fn the_normal_distribution_keeps_to_its_table() {
    for (x, share) in [
        (0.0, 0.5),
        (1.0, 0.841_344_746),
        (-1.0, 0.158_655_254),
        (1.96, 0.975_002_105),
        (-3.0, 0.001_349_898),
    ] {
        assert!((normal(x) - share).abs() < 2e-7, "{x}: {}", normal(x));
    }
}

#[test]
fn a_stone_is_bedded_by_the_share_of_its_height_asked() {
    assert!((bedded(0.3, 1.0 / 3.0) - 0.1).abs() < 1e-12);
    assert!(bedded(0.3, 0.5).abs() < 1e-12);
    assert!((bedded(0.3, 0.0) - 0.3).abs() < 1e-12);
}

/// A bed's stones are grown from fresh to as worn as its farthest-carried
/// stone, a granite one rounding over several kilometres; a slate bed's stay
/// angular however far they have come, and split along their cleavage.
#[test]
fn a_beds_stones_wear_from_fresh_to_as_far_as_its_rock_rounds() {
    for (lithology, run) in [
        (Lithology::Granite, 4000.0),
        (Lithology::Limestone, 9000.0),
        (Lithology::Slate, 20_000.0),
    ] {
        let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
        let mut dice = Dice::keyed(7, 0);
        bed(
            &mut stage,
            &mut dice,
            (brook(lithology, run), (0.0, 0.0), 0.0),
            None,
        )
        .expect("a bed");
        let laid = stage.bed.as_ref().expect("a bed asked for");
        let farthest = (run / lithology.rounding()).min(lithology.most_wear());
        assert!(laid.wears[0].abs() < 1e-12);
        assert!(
            (laid.wears[WEARS - 1] - farthest).abs() < 1e-12,
            "{lithology:?}"
        );
        let rocks: Vec<(Habit, f64)> = stage
            .recipes
            .iter()
            .filter_map(|recipe| match *recipe {
                Recipe::Rock { habit, wear, .. } => Some((habit, wear)),
                _ => None,
            })
            .collect();
        assert_eq!(rocks.len(), SHAPES * WEARS);
        assert!(rocks
            .iter()
            .all(|&(habit, wear)| (0.0..=farthest + 1e-12).contains(&wear)
                && habit.cleaved == (lithology == Lithology::Slate)));
        // The nearest wear each carried stone is grown at.
        assert_eq!(laid.wear_of(0.0), 0);
        assert_eq!(laid.wear_of(farthest), WEARS - 1);
        assert_eq!(laid.wear_of(0.49 * farthest), 1);
        assert!(!laid.finished());
    }
}

/// A land holding one straight stream 5 m across and 0.8 m deep running
/// along `+z`, falling a hundredth, over flat ground with no grids.
fn straight_land() -> Land {
    straight_land_from(0.0)
}

/// [`straight_land`] with its water standing at `level` where it starts.
fn straight_land_from(level: f64) -> Land {
    let mut marks: Vec<Mark> = (0..=120)
        .map(|index| {
            let z = 2.5 * f64::from(index);
            Mark {
                x: 0.0,
                z,
                level: level - 0.01 * z,
                width: 5.0,
                depth: 0.8,
                ..Mark::default()
            }
        })
        .collect();
    crate::channel::survey(&mut marks, 1).expect("surveyed");
    let reach = Reach {
        per_width: 2.0,
        beyond: 10.0,
    };
    Land {
        far: 0,
        nests: [None; NESTS],
        water: None,
        near_water: None,
        horizon: None,
        rivers: Courses::new(&[marks], ((-500.0, -500.0), 1000.0), reach).expect("courses"),
        form: Some(brook(Lithology::Granite, 1000.0).form),
        roads: Courses::none(),
        road: None,
        crossings: Vec::new(),
        sea: None,
        centre: (0.0, 0.0),
        reach: 500.0,
    }
}

/// The bed of the tests' brook about an eye at its station looking along
/// `heading`, laid out and ready to be taken, and the stage it lies on.
fn laid_bed(heading: f64) -> (Stage, Bed) {
    let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
    let mut dice = Dice::keyed(3, 0);
    let brook = brook(Lithology::Granite, 1000.0);
    bed(&mut stage, &mut dice, (brook, (0.0, 120.0), heading), None).expect("a bed");
    let mut bed = stage.bed.take().expect("the bed");
    bed.pass = Pass::Done;
    (stage, bed)
}

/// The stretch a stream's flow is solved over runs the further way the eye
/// looks along it, down the stream or up it.
#[test]
fn a_streams_flow_is_solved_further_the_way_the_eye_looks() {
    let land = straight_land();
    for (heading, ahead) in [(0.0, 1.0), (PI, -1.0)] {
        let (_, mut bed) = laid_bed(heading);
        let (stretch, _) = bed.take(&land, (10.0, 30.0)).expect("a stretch");
        let (start, end) = (stretch.from, stretch.from + stretch.length);
        let (behind, before) = if ahead > 0.0 {
            (start, end)
        } else {
            (end, start)
        };
        assert!(
            (before - (120.0 + 30.0 * ahead)).abs() < 1e-9,
            "{heading}: to {before}"
        );
        assert!(
            (behind - (120.0 - 10.0 * ahead)).abs() < 1e-9,
            "{heading}: from {behind}"
        );
    }
}

/// A boulder stands in the flow as far along and across it as its length and
/// breadth reach, turned as it lies.
#[test]
fn a_boulder_stands_in_the_flow_as_it_lies() {
    let land = straight_land();
    let (mut stage, mut bed) = laid_bed(0.0);
    stage
        .footprints
        .index(land.centre, land.reach)
        .expect("indexed");
    let (mut along, mut across) = (0, 0);
    for index in 0..32u32 {
        let z = 100.0 + 4.0 * f64::from(index);
        for (offset, lying) in [(0.0, Lying::Fallen), (2.0, Lying::Bedded)] {
            let stone = Laid {
                at: (0.5, z + offset),
                size: 0.6,
                hard: 1.0,
                shape: 0,
                wear: 0,
                key: mix32(index ^ 0x51),
                lying,
            };
            let kept = bed.stones.len();
            let lie = bed.lie(&stage.fields, &land, stone).expect("lies");
            assert!(matches!(lie, Lie::Set { .. }), "by the brook");
            bed.set_out(&mut stage, lie).expect("set out");
            if lying != Lying::Fallen {
                continue;
            }
            let reach = bed.stones.get(kept).expect("in the flow").reach;
            let turn = TAU * unit(mix32(stone.key ^ 6));
            let (sin, cos) = (mathf::sin(turn).abs(), mathf::cos(turn).abs());
            if sin > cos + 0.2 {
                assert!(reach.0 > reach.1, "lying along the current: {reach:?}");
                along += 1;
            } else if cos > sin + 0.2 {
                assert!(reach.0 < reach.1, "lying across the current: {reach:?}");
                across += 1;
            }
        }
    }
    assert!(along > 0 && across > 0, "{along} along and {across} across");
}

/// A branch lying across the brook is one obstacle to the water, not a row
/// of them: every stone the flow takes it as touches the next, so the bed
/// rises and the water parts all along it.
#[test]
fn drift_in_the_water_reaches_the_flow_as_one_obstacle() {
    // Its water stands half a metre over the flat ground where the branch
    // falls in, so the branch lies in it.
    let land = straight_land_from(1.7);
    let (mut stage, mut bed) = laid_bed(0.0);
    stage
        .footprints
        .index(land.centre, land.reach)
        .expect("indexed");
    let mut dice = Dice::keyed(5, 0);
    let drift = Drift::new(
        &mut stage,
        &mut dice,
        (
            crate::compose::plants::Kind::Oak,
            crate::tree::Season::Summer,
        ),
    )
    .expect("drift");
    let piece = drift.piece(&mut dice, false).expect("a branch");
    let kept = bed.stones.len();
    let outcome = bed
        .lay_wood(
            &mut stage,
            (&land, &drift),
            (piece, 1.0),
            ((-1.5, 120.0), FRAC_PI_2),
            7,
        )
        .expect("laid");
    assert!(matches!(outcome, Lay::Lain(_)), "the branch found room");
    let flow = bed.stones.get(kept..).expect("its stones");
    assert!(flow.len() >= 2, "{} stones in the water", flow.len());
    for pair in flow.windows(2) {
        let gap = mathf::hypot(pair[1].at.0 - pair[0].at.0, pair[1].at.1 - pair[0].at.1);
        assert!(
            gap <= pair[0].reach.0 + pair[1].reach.0 + 1e-9,
            "a gap of {gap} between stones {} and {} across",
            pair[0].reach.0,
            pair[1].reach.0
        );
    }
}

/// A boulder is never set through what already stands there: ground another
/// piece has claimed — a trunk, the eye's own — keeps it out, where a bedded
/// stone, which claims nothing, still lies in its gravel.
#[test]
fn a_boulder_is_never_set_through_what_already_stands() {
    let land = straight_land();
    let (mut stage, mut bed) = laid_bed(0.0);
    stage
        .footprints
        .index(land.centre, land.reach)
        .expect("indexed");
    // A trunk's claim where a boulder would fall, and two boulders whose
    // ground overlaps though no nearer than thinning keeps stones apart,
    // each with a pebble beside it that only the boulder would crowd out.
    let trunk = (0.5, 100.0);
    stage.claim(trunk, 0.3).expect("claimed");
    let found = [
        (trunk, 0.8, Lying::Fallen),
        ((0.5, 120.0), 0.7, Lying::Fallen),
        ((0.5, 120.62), 0.65, Lying::Fallen),
        ((0.75, 100.0), 0.05, Lying::Bedded),
        ((0.75, 120.62), 0.05, Lying::Bedded),
    ];
    bed.found = Runs::default();
    bed.ranking = Ranking::default();
    assert!(bed.found.reserve(found.len()) && bed.ranking.reserve(found.len()));
    for (index, &(at, size, lying)) in (0u32..).zip(&found) {
        bed.found.push(Found {
            at,
            size,
            key: mix32(0x70 ^ index),
            lying,
        });
        bed.ranking.add(size, index);
    }
    while !bed.ranking.ranked() {
        bed.ranking.rank(&tairix_parallel::SERIAL).expect("ranked");
    }
    bed.chains =
        Some(Chains::new((land.centre, land.reach), KEPT_APART, MOST_SIDE).expect("chains"));
    bed.laid.clear();
    bed.pass = Pass::Thinning;
    while bed.pass == Pass::Thinning {
        bed.thin(&mut stage).expect("thinned");
    }
    let kept: Vec<(f64, f64)> = bed.laid.iter().map(|stone| stone.at).collect();
    assert!(!kept.contains(&trunk), "nothing falls through the trunk");
    assert!(kept.contains(&(0.5, 120.0)));
    assert!(
        !kept.contains(&(0.5, 120.62)),
        "nor through the boulder kept first"
    );
    assert!(
        kept.contains(&(0.75, 100.0)) && kept.contains(&(0.75, 120.62)),
        "and no pebble went for a boulder that was never set: {kept:?}"
    );
    assert!(
        stage.clear((0.5, 120.0), 0.1),
        "a boulder kept takes no ground until it is set out"
    );
    bed.place(&mut stage, (&land, &tairix_parallel::SERIAL), 0)
        .expect("placed");
    assert!(
        !stage.clear((0.5, 120.0), 0.1),
        "set out, it holds its ground"
    );
}

/// The branches the current brings pile against a spanning trunk's upstream
/// side as close as the ground either keeps lets them, rather than finding no
/// room beside it.
#[test]
fn branches_jam_against_a_spanning_trunk() {
    let land = straight_land_from(1.7);
    let (mut stage, mut bed) = laid_bed(0.0);
    stage
        .footprints
        .index(land.centre, land.reach)
        .expect("indexed");
    let mut dice = Dice::keyed(5, 0);
    let drift = Drift::new(
        &mut stage,
        &mut dice,
        (
            crate::compose::plants::Kind::Oak,
            crate::tree::Season::Summer,
        ),
    )
    .expect("drift");
    let piece = drift.piece(&mut dice, true).expect("a trunk");
    let Lay::Lain(trunk) = bed
        .lay_wood(
            &mut stage,
            (&land, &drift),
            (piece, 1.0),
            ((-3.0, 120.0), FRAC_PI_2),
            7,
        )
        .expect("laid")
    else {
        panic!("the trunk found room");
    };
    let before = stage.objects.len();
    bed.jam(&mut stage, &land, &drift, (&mut Dice::keyed(11, 0), trunk))
        .expect("jammed");
    assert!(
        stage.objects.len() > before,
        "no branch jammed against the trunk"
    );
}
