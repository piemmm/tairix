//! Host tests of a river's channel: its water always over its deepest line
//! and never above its brim, falling in steps — ponded over its pools, quick
//! down its riffles, all at once over a ledge — and evenly in spate; its
//! pools deeper than its crests; its deep water swung to the outside of a
//! bend with a sandy bar rising inside; its water fastest where shallowest;
//! its banks rising from the brim to the land; and its course counted in
//! units of pool and riffle by its width.

use alloc::vec::Vec;

use super::*;
use crate::course::Reach;

/// The seed the tests' channels are drawn under.
const SEED: u32 = 0x5eed;

/// A straight course `length` long along +z, `width` across and `depth`
/// deep, its brim falling `fall` a metre from nought, surveyed under the
/// tests' seed.
fn straight(length: f64, (width, depth): (f64, f64), fall: f64) -> Courses {
    let marks: Vec<Mark> = (0..=mathf::round_i32(length / 2.5))
        .map(|index| {
            let z = 2.5 * f64::from(index);
            Mark {
                x: 0.0,
                z,
                level: -fall * z,
                width,
                depth,
                ..Mark::default()
            }
        })
        .collect();
    surveyed(marks)
}

fn surveyed(mut marks: Vec<Mark>) -> Courses {
    survey(&mut marks, SEED).expect("surveyed");
    let reach = Reach {
        per_width: 2.0,
        beyond: 10.0,
    };
    Courses::new(&[marks], ((-2000.0, -2000.0), 4000.0), reach).expect("courses")
}

fn form(flowing: f64, ledges: f64) -> Form {
    Form {
        flowing,
        ledges,
        outcrops: 0.3,
        seed: SEED,
    }
}

/// The channel `along` the course's only river.
fn section(courses: &Courses, along: f64, form: &Form) -> Section {
    Section::new(&Station::on(courses, 0, along).expect("a station"), form)
}

/// The channel and its banks `along` the course's only river.
fn banked(courses: &Courses, along: f64, form: &Form) -> Banked {
    Banked::new(&Station::on(courses, 0, along).expect("a station"), form)
}

/// Each place `step` apart from `from` to `to` along the course.
fn places(from: f64, to: f64, step: f64) -> impl Iterator<Item = f64> {
    let count = mathf::round_i32((to - from) / step);
    (0..count).map(move |index| from + step * f64::from(index))
}

#[test]
fn a_rivers_water_keeps_over_its_deepest_line_and_below_its_brim() {
    let courses = straight(600.0, (4.0, 0.8), 0.01);
    for flowing in [0.3, 0.5, 1.0] {
        for ledges in [0.0, 1.0] {
            let form = form(flowing, ledges);
            for along in places(20.0, 580.0, 0.37) {
                let at = section(&courses, along, &form);
                assert!(at.water <= at.brim + 1e-9, "{along}: {at:?}");
                assert!(
                    at.water - at.deepest >= LEAST_DEPTH - 1e-9,
                    "{along}: {at:?}"
                );
                assert!((at.bed(at.half) - at.brim).abs() < 1e-9);
                assert!((at.bed(-at.half) - at.brim).abs() < 1e-9);
                assert!((at.bed(at.thalweg) - at.deepest).abs() < 1e-9);
                for across in [-1.9, -1.0, 0.0, 0.7, 1.6] {
                    let bed = at.bed(across);
                    assert!(bed >= at.deepest - 1e-9 && bed <= at.brim + 1e-9);
                }
            }
        }
    }
}

/// Along a river at low water its surface falls, never rises, and never
/// jumps; it lies almost level over its pools and falls fast down its
/// riffles; in spate it fills its channel to the brim.
#[test]
fn the_water_falls_in_steps_at_low_water_and_evenly_in_spate() {
    let fall = 0.01;
    let courses = straight(600.0, (4.0, 0.8), fall);
    let low = form(0.35, 0.0);
    let step = 0.05;
    let (mut ponded, mut quick) = (0usize, 0usize);
    let mut last: Option<f64> = None;
    for along in places(30.0, 570.0, step) {
        let water = section(&courses, along, &low).water;
        if let Some(before) = last {
            let slope = (before - water) / step;
            assert!(slope > -1e-9, "{along}: the water rises {slope}");
            assert!(slope < 0.2, "{along}: the water jumps {slope}");
            if slope < 0.4 * fall {
                ponded += 1;
            }
            if slope > 2.0 * fall {
                quick += 1;
            }
        }
        last = Some(water);
    }
    let all = f64::from(u32::try_from(mathf::round_i32(540.0 / step)).expect("a count"));
    let share = |count: usize| f64::from(u32::try_from(count).expect("a count")) / all;
    assert!(share(ponded) > 0.3, "ponded over {}", share(ponded));
    assert!(share(quick) > 0.08, "quick over {}", share(quick));
    let spate = form(1.0, 0.0);
    for along in places(30.0, 570.0, 1.3) {
        let at = section(&courses, along, &spate);
        assert!((at.water - at.brim).abs() < 1e-9, "{along}: {at:?}");
    }
}

/// A pool lies deeper than the crests about it, and over a long reach the
/// bed keeps, on average, the channel's own depth below its brim.
#[test]
fn pools_lie_deeper_than_crests_and_the_bed_keeps_its_depth() {
    let courses = straight(800.0, (4.0, 0.8), 0.01);
    let form = form(0.4, 0.0);
    let (mut shallowest, mut deepest, mut below, mut count) = (f64::INFINITY, 0.0f64, 0.0, 0.0);
    for along in places(40.0, 760.0, 0.25) {
        let at = section(&courses, along, &form);
        let depth = at.water - at.deepest;
        shallowest = shallowest.min(depth);
        deepest = deepest.max(depth);
        below += at.brim - at.deepest;
        count += 1.0;
    }
    assert!(deepest > 3.0 * shallowest, "{shallowest}..{deepest}");
    let mean = below / count;
    assert!((mean - 0.8).abs() < 0.25 * 0.8, "the bed lies {mean} down");
}

/// Where bedded rock holds a reach its fall comes all at once over a ledge
/// of bare rock, far more than any riffle falls, into a plunge pool below.
#[test]
fn a_ledge_gathers_its_reachs_fall_into_one_drop_of_bare_rock() {
    let fall = 0.01;
    let courses = straight(800.0, (4.0, 0.8), fall);
    let stepped = form(0.35, 1.0);
    let riffled = form(0.35, 0.0);
    let drop = |form: &Form| {
        places(40.0, 760.0, 0.1)
            .map(|along| {
                let (here, below) = (
                    section(&courses, along, form),
                    section(&courses, along + 2.0, form),
                );
                (here.water - below.water, along)
            })
            .fold(
                (0.0f64, 0.0),
                |most, now| if now.0 > most.0 { now } else { most },
            )
    };
    let (ledge, at) = drop(&stepped);
    let (riffle, _) = drop(&riffled);
    let unit = SPACING * 4.0 / (1.0 + fall / STEPPED_FALL) * fall;
    assert!(ledge > 1.2 * unit, "a ledge drops {ledge} against {unit}");
    assert!(riffle < 0.8 * unit, "a riffle drops {riffle}");
    let lip = (0..20)
        .map(|tenth| section(&courses, at + 0.1 * f64::from(tenth), &stepped))
        .fold(0.0f64, |most, at| most.max(at.ledge));
    assert!(lip > 0.9, "the ledge's rock {lip}");
    let bare = banked(&courses, at + 0.3, &stepped);
    assert!(bare.laid(0.0) < -0.5, "{}", bare.laid(0.0));
    let plunge = (0..40)
        .map(|step| {
            let below = section(&courses, at + 0.25 * f64::from(step), &stepped);
            below.water - below.deepest
        })
        .fold(0.0f64, f64::max);
    let crest = section(&courses, at - 0.2, &stepped);
    assert!(
        plunge > 2.0 * (crest.water - crest.deepest),
        "a plunge pool {plunge} below a lip {}",
        crest.water - crest.deepest
    );
}

/// A course bending toward its positive side swings its deep water to the
/// outside, its bank there cut steep, while a gentle bar rises inside, bared
/// by the low water and sanded in patches.
#[test]
fn in_a_bend_the_deep_water_swings_out_and_a_sandy_bar_rises_inside() {
    let (width, depth) = (4.0, 0.8);
    let form = form(0.4, 0.0);
    let mut sanded = 0.0f64;
    for unit in 0..24 {
        let station = Station {
            along: 37.0 * f64::from(unit),
            brim: 0.0,
            width,
            depth,
            phase: f64::from(unit) + 0.55,
            turn: 0.35 / width,
            fall: 0.01,
        };
        let banked = Banked::new(&station, &form);
        let at = &banked.section;
        if Unit::at(station.phase, &form).kind != Kind::Riffle {
            continue;
        }
        assert!(at.thalweg < 0.0, "{unit}: {at:?}");
        assert!(at.rise[0] > at.rise[1], "{unit}: {at:?}");
        let (outside, inside) = (at.edge(-1.0), at.edge(1.0));
        assert!(
            at.half - inside > at.half + outside,
            "{unit}: the inner water stops at {inside}, the outer at {outside}"
        );
        assert!(
            banked.banks[0].face < banked.banks[1].face,
            "{unit}: {banked:?}"
        );
        sanded = sanded.max(banked.laid(0.75 * at.half));
        assert!(
            banked.laid(-0.95 * at.half) <= ALLUVIUM + 1e-12,
            "{unit}: the cut bank's foot is earth, not sand"
        );
    }
    assert!(sanded > 0.3, "the bars sand to {sanded}");
}

/// The water carries as much past its pools as over its crests, so it runs
/// fastest where shallowest, slows toward the edges, and nowhere past nine
/// tenths of critical.
#[test]
fn the_water_runs_fastest_where_shallowest_and_never_past_critical() {
    let courses = straight(600.0, (4.0, 0.8), 0.01);
    let form = form(0.4, 0.0);
    let mut extremes = (f64::INFINITY, 0.0, 0.0, f64::INFINITY);
    for along in places(30.0, 570.0, 0.2) {
        let at = section(&courses, along, &form);
        let depth = at.water - at.deepest;
        let speed = at.water_at(at.thalweg).1;
        if depth < extremes.0 {
            extremes.0 = depth;
            extremes.1 = speed;
        }
        if depth > extremes.2 {
            extremes.2 = depth;
            extremes.3 = speed;
        }
        // Across the channel the deeper water runs the faster.
        let mut waters: Vec<(f64, f64)> = [-1.5, -0.8, 0.0, 0.5, 1.2]
            .iter()
            .map(|&across| at.water_at(across))
            .collect();
        waters.sort_by(|one, other| one.0.total_cmp(&other.0));
        for &(depth, speed) in &waters {
            if depth > 0.0 {
                assert!(speed <= swiftest(depth) + 1e-9);
            } else {
                assert!(speed.abs() < 1e-12);
            }
        }
        assert!(waters.windows(2).all(|pair| pair[0].1 <= pair[1].1 + 1e-9));
    }
    let (_, fast, _, slow) = extremes;
    assert!(
        fast > 2.0 * slow,
        "over the crests {fast}, in the pools {slow}"
    );
}

/// A bank rises from the brim over its faces, slumped and bulging along
/// them but never dipping below the brim, its top lifted into a levee or let
/// down a little, and beyond it settles to the land.
#[test]
fn a_bank_rises_from_its_brim_to_the_land() {
    let courses = straight(600.0, (4.0, 0.8), 0.01);
    let form = form(0.4, 0.0);
    let top = 1.0;
    for along in places(30.0, 570.0, 3.1) {
        let at = banked(&courses, along, &form);
        let (half, brim) = (at.section.half, at.section.brim);
        for side in [-1.0, 1.0] {
            let reach = at.bank_reach(side, 0.02);
            for step in 0..400 {
                let beyond = 0.025 * f64::from(step);
                let ground = at.ground(side * (half + beyond), top, 0.02);
                assert!(ground >= brim - 1e-9, "{along}: {ground}");
            }
            assert!((at.ground(side * half, top, 0.02) - brim).abs() < 1e-9);
            let far = at.ground(side * (half + reach + 4.0 * half + 1.0), top, 0.02);
            assert!((far - top).abs() < 1e-9, "{along}: the land at {far}");
        }
    }
}

/// However its faces and shelf wander, and however narrow its course, no
/// bank's say over the ground reaches past the farthest a channel is said
/// to have any.
#[test]
fn no_bank_has_a_say_past_the_farthest() {
    let form = form(0.4, 0.5);
    for width in [0.05, 0.2, 0.6, 2.0, 6.0, 20.0] {
        for step in [0.02, 0.1, 0.5] {
            let farthest = farthest_say(width, step);
            for along in places(0.0, 200.0, 1.3) {
                for turn in [-0.35, 0.0, 0.35] {
                    let station = Station {
                        along,
                        brim: 0.0,
                        width,
                        depth: 0.2 * width,
                        phase: along / (SPACING * width.max(LEAST_WIDTH)),
                        turn: turn / width,
                        fall: 0.01,
                    };
                    let banked = Banked::new(&station, &form);
                    for side in [-1.0, 1.0] {
                        let says = banked.section.half + banked.bank_reach(side, step) + 2.0 * step;
                        assert!(
                            says <= farthest,
                            "{width} by {step}: {says} past {farthest}"
                        );
                    }
                }
            }
        }
    }
}

/// Beside land lower than its brim a perched river's bank falls from the
/// brim to that land, never above the brim, and beyond it stands on the
/// land itself.
#[test]
fn a_perched_rivers_bank_falls_to_the_land() {
    let courses = straight(600.0, (4.0, 0.8), 0.01);
    let form = form(0.4, 0.0);
    for along in places(30.0, 570.0, 3.1) {
        let at = banked(&courses, along, &form);
        let (half, brim) = (at.section.half, at.section.brim);
        let low = brim - 1.0;
        for side in [-1.0, 1.0] {
            let reach = at.bank_reach(side, 0.02);
            for step in 0..400 {
                let ground = at.ground(side * (half + 0.025 * f64::from(step)), low, 0.02);
                assert!(ground <= brim + 1e-9, "{along}: {ground} over the brim");
                assert!(ground >= low - 0.2, "{along}: {ground} far below the land");
            }
            let far = at.ground(side * (half + reach + 4.0 * half + 1.0), low, 0.02);
            assert!((far - low).abs() < 1e-9, "{along}: the land at {far}");
        }
    }
}

/// Along a straight reach the deep water swings to one side a unit along
/// and to the other the next, over a ledge's units as over a riffle's.
#[test]
fn the_deep_water_alternates_from_unit_to_unit() {
    for ledges in [0.0, 0.6, 1.0] {
        let form = form(0.4, ledges);
        let mut last: Option<bool> = None;
        for unit in 0..60 {
            let station = Station {
                along: 24.0 * f64::from(unit),
                brim: 0.0,
                width: 4.0,
                depth: 0.8,
                phase: f64::from(unit) + 0.5,
                turn: 0.0,
                fall: 0.01,
            };
            let positive = Section::new(&station, &form).thalweg >= 0.0;
            if let Some(last) = last {
                assert_ne!(positive, last, "{ledges}: unit {unit} swings as the last");
            }
            last = Some(positive);
        }
    }
}

/// A straight course counts its units of pool and riffle some six of its
/// widths apart, fewer the steeper it falls, falls as its brim does and
/// bends nowhere; a course curving toward its positive side bends that way,
/// and the other way the other way.
#[test]
fn a_course_is_counted_in_units_by_its_width_and_its_bends_are_read() {
    let length = 1200.0;
    let courses = straight(length, (4.0, 0.8), 0.01);
    let marks = courses.course(0);
    assert!(marks.windows(2).all(|pair| pair[1].phase > pair[0].phase));
    let counted = marks.last().expect("a mark").phase - marks.first().expect("a mark").phase;
    let expected = length / (SPACING * 4.0 / (1.0 + 0.01 / STEPPED_FALL));
    assert!(
        (counted - expected).abs() < 0.35 * expected,
        "{counted} units against {expected}"
    );
    for mark in &marks[8..marks.len() - 8] {
        assert!((mark.fall - 0.01).abs() < 1e-9, "{}", mark.fall);
        assert!(mark.turn.abs() < 1e-9, "{}", mark.turn);
    }
    let radius = 40.0;
    for sign in [1.0, -1.0] {
        // Along +z at first, curving toward -x for a positive turn.
        let arc: Vec<Mark> = (0..60)
            .map(|index| {
                let angle = 0.04 * f64::from(index);
                Mark {
                    x: -sign * radius * (1.0 - mathf::cos(angle)),
                    z: radius * mathf::sin(angle),
                    width: 4.0,
                    depth: 0.8,
                    ..Mark::default()
                }
            })
            .collect();
        let courses = surveyed(arc);
        for mark in &courses.course(0)[5..55] {
            assert!(
                (mark.turn - sign / radius).abs() < 0.02 / radius,
                "{sign}: {}",
                mark.turn
            );
        }
    }
}
