//! Host tests of a stream's surface over its stones: the linear answer's
//! limits, the dip over a stone and the lee waves standing only behind it,
//! the pillow before a stone through the surface and the wake and foam
//! behind it, each place answering at its own depth and speed, the churn of
//! fast water and the white water over a ledge, no place standing past what
//! water can, and the same answer on any runner.

use alloc::vec;
use alloc::vec::Vec;

use tairix_parallel::Threaded;

use super::*;
use crate::channel::{manning, swiftest};

/// A plane channel `width` across and `depth` deep at its brim, which stands
/// at nought, filled `flowing` full and falling `slope`: a parabola across,
/// the same all along.
fn plane(width: f64, depth: f64, flowing: f64, slope: f64) -> Section {
    Section {
        along: 0.0,
        brim: 0.0,
        half: 0.5 * width,
        water: -(1.0 - flowing) * depth,
        slope,
        thalweg: 0.0,
        deepest: -depth,
        rise: [2.0, 2.0],
        running: manning(flowing * depth, slope),
        pool: 0.0,
        ledge: 0.0,
        seed: 0,
    }
}

/// A stretch `length` long over `sections` a metre apart.
fn stretch(length: f64, sections: Vec<Section>) -> Stretch {
    Stretch {
        from: 0.0,
        length,
        half: 2.0,
        spacing: 1.0,
        sections,
    }
}

/// A riffle four metres across, its water 15 cm deep along its middle,
/// running at about 0.8 m/s.
fn riffle() -> Stretch {
    stretch(12.0, vec![plane(4.0, 0.3, 0.5, 0.013); 13])
}

fn solve(stretch: Stretch, stones: Vec<Stone>, runner: &dyn JobRunner) -> Flow {
    let mut solving = Solving::new(stretch, stones, (0.02, (1024, 256))).expect("a solving");
    let mut last = solving.done();
    while !solving.step(runner).expect("solved") {
        let now = solving.done();
        assert!(now >= last, "{now} fell back from {last}");
        last = now;
    }
    assert!((solving.done() - 1.0).abs() < 1e-12);
    solving.finish().expect("a flow")
}

/// The surface along the course from `from` to `to`, `step` apart.
fn profile(flow: &Flow, (from, to, step): (f64, f64, f64), across: f64) -> Vec<f64> {
    let count = mathf::round_i32((to - from) / step).max(0);
    (0..count)
        .map(|index| flow.at(from + step * f64::from(index), across).0)
        .collect()
}

/// The surface `flow` stands at along the course, less what `calm`, its
/// water's own churning with no stone in it, stands at: the stones' answer.
fn answer((flow, calm): (&Flow, &Flow), span: (f64, f64, f64), across: f64) -> Vec<f64> {
    profile(flow, span, across)
        .iter()
        .zip(profile(calm, span, across))
        .map(|(rise, churned)| rise - churned)
        .collect()
}

fn largest(values: &[f64]) -> f64 {
    values
        .iter()
        .fold(0.0f64, |most, value| most.max(value.abs()))
}

/// The length of the wave water `depth` deep running at `speed` holds still
/// against it: where `U²k = (g + Tk²) tanh(kh)`.
fn still_wave(depth: f64, speed: f64) -> f64 {
    (1..4000)
        .map(|step| 0.01 * f64::from(step))
        .min_by(|&a, &b| {
            let miss =
                |k: f64| (speed * speed * k - (GRAVITY + TENSION * k * k) * tanh(k * depth)).abs();
            miss(a).total_cmp(&miss(b))
        })
        .expect("a wave number")
}

#[test]
fn a_plane_channel_is_deepest_along_its_course_and_runs_below_critical() {
    let section = plane(4.0, 0.3, 0.5, 0.013);
    assert!((section.depth(0.0) - 0.15).abs() < 1e-12);
    assert!(section.depth(2.0).abs() < 1e-12);
    assert!(section.depth(1.0) < section.depth(0.0));
    let speed = section.water_at(0.0).1;
    assert!((0.7..0.9).contains(&speed), "{speed}");
    // However steep, no stream runs past nine tenths of a long wave's speed.
    let steep = plane(4.0, 0.3, 0.5, 0.2);
    let swiftest = swiftest(0.15);
    let speed = steep.water_at(0.0).1;
    assert!(
        (speed - swiftest).abs() < 1e-12,
        "{speed} against {swiftest}"
    );
}

/// The surface's answer to the wave number `(along, across)` over water
/// `depth` deep running at `speed`.
fn transfer((along, across): (f64, f64), depth: f64, speed: f64) -> (Complex, Complex) {
    Deep::at(mathf::hypot(along, across), depth)
        .expect("a wave over water")
        .answer(along, speed)
}

/// A long, gentle rise of the bed under slow water draws the surface down by
/// the share the long-wave limit gives, `Fr²/(1 − Fr²)` of the rise; a ridge
/// lying along the flow, which slows nothing, draws no answer; and the
/// answer peaks at the wave the stream holds still, its crests running up
/// the stream as fast as the stream runs down.
#[test]
fn the_linear_answer_keeps_to_its_limits() {
    let (depth, speed) = (0.5, 1.0);
    let froude2 = speed * speed / (GRAVITY * depth);
    let (to_bed, _) = transfer((0.5, 0.0), depth, speed);
    assert!(to_bed.re < 0.0, "{to_bed:?}");
    let magnitude = mathf::hypot(to_bed.re, to_bed.im);
    assert!(
        (magnitude - froude2 / (1.0 - froude2)).abs() < 0.03,
        "{magnitude} against {}",
        froude2 / (1.0 - froude2)
    );
    let (along, _) = transfer((0.0, 5.0), depth, speed);
    assert!(mathf::hypot(along.re, along.im) < 1e-3, "{along:?}");
    let (depth, speed) = (0.15, 0.8);
    let mut held = (0.0, 0.0);
    for step in 1..4000 {
        let k = 0.01 * f64::from(step);
        let (to_bed, _) = transfer((k, 0.0), depth, speed);
        let size = mathf::hypot(to_bed.re, to_bed.im);
        if size > held.1 {
            held = (k, size);
        }
    }
    let still = still_wave(depth, speed);
    assert!(
        (held.0 - still).abs() < 0.1 * still,
        "{held:?} against {still}"
    );
}

/// Over a stone beneath slow water the surface dips, and behind it stand
/// waves as long as the wave the stream holds still; before it the water
/// barely stirs, since waves run only the way the stream carries them.
#[test]
fn lee_waves_stand_behind_a_stone_beneath_the_water_and_not_before_it() {
    let stone = Stone {
        at: (5.0, 0.0),
        reach: (0.12, 0.12),
        top: 0.08,
    };
    let flow = solve(riffle(), vec![stone], &tairix_parallel::SERIAL);
    let calm = solve(riffle(), Vec::new(), &tairix_parallel::SERIAL);
    let (over, _) = flow.at(5.0, 0.0);
    assert!(
        over - calm.at(5.0, 0.0).0 < -1e-4,
        "the surface over the stone stands at {over}"
    );
    let behind = answer((&flow, &calm), (5.4, 7.4, 0.005), 0.0);
    let before = answer((&flow, &calm), (3.0, 4.6, 0.005), 0.0);
    assert!(
        largest(&before) < 0.3 * largest(&behind),
        "before {} against behind {}",
        largest(&before),
        largest(&behind)
    );
    let crossings = behind
        .windows(2)
        .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
        .count();
    // The still wave's length in this water, about 0.42 m: some nine or ten
    // half-waves over two metres.
    let still = still_wave(0.15, plane(4.0, 0.3, 0.5, 0.013).water_at(0.0).1);
    let expected = 2.0 * (2.0 * still / TAU);
    assert!(
        (f64::from(u32::try_from(crossings).expect("a count")) - expected).abs() < 0.3 * expected,
        "{crossings} crossings against about {expected}"
    );
}

/// Before a stone standing through the surface the water piles up, behind
/// it the wake falls away, and in brisk water foam sheds into the wake and
/// is carried on down the stream, thinning as it bursts.
#[test]
fn water_piles_before_a_stone_through_it_and_foams_in_its_wake() {
    let stone = Stone {
        at: (5.0, 0.0),
        reach: (0.2, 0.2),
        top: 0.35,
    };
    let flow = solve(riffle(), vec![stone], &tairix_parallel::SERIAL);
    let calm = solve(riffle(), Vec::new(), &tairix_parallel::SERIAL);
    let mean = |values: Vec<f64>| values.iter().sum::<f64>() / real(values.len().max(1));
    // Over a wave behind it, so the lee waves standing there average out.
    let before = mean(answer((&flow, &calm), (4.5, 4.78, 0.004), 0.0));
    let behind = mean(answer((&flow, &calm), (5.2, 5.62, 0.004), 0.0));
    let (_, foam) = flow.at(5.3, 0.0);
    assert!(before > 0.0, "before the stone {before}");
    assert!(behind < 0.0, "behind {behind}, before {before}");
    assert!(foam > 0.3, "foam behind the stone {foam}");
    let (_, further) = flow.at(7.0, 0.0);
    assert!(further < foam && further > 0.0, "{further} against {foam}");
    let (_, beside) = flow.at(5.3, 1.0);
    assert!(beside < 0.05, "foam off the wake {beside}");
}

/// The same stone answers far more where its water runs fast and shallow
/// than in a deep, slow pool, each place taking the answer of its own depth
/// and speed: the pool lies glassy while the riffle below it stands in
/// waves.
#[test]
fn a_stone_stirs_a_riffle_and_barely_a_pool() {
    let pool = Section {
        running: 0.1,
        ..plane(4.0, 0.8, 0.85, 0.013)
    };
    let fast = plane(4.0, 0.3, 0.5, 0.013);
    let sections: Vec<Section> = (0..=16)
        .map(|metre| if metre < 8 { pool } else { fast })
        .collect();
    let stones = vec![
        Stone {
            at: (4.0, 0.0),
            reach: (0.12, 0.12),
            top: 0.08,
        },
        Stone {
            at: (11.0, 0.0),
            reach: (0.12, 0.12),
            top: 0.08,
        },
    ];
    let flow = solve(
        stretch(16.0, sections.clone()),
        stones,
        &tairix_parallel::SERIAL,
    );
    let calm = solve(
        stretch(16.0, sections),
        Vec::new(),
        &tairix_parallel::SERIAL,
    );
    let pooled = largest(&answer((&flow, &calm), (3.5, 6.5, 0.005), 0.0));
    let riffled = largest(&answer((&flow, &calm), (10.5, 13.5, 0.005), 0.0));
    assert!(
        riffled > 10.0 * pooled,
        "the riffle stands {riffled}, the pool {pooled}"
    );
}

/// Water running near critical churns its surface even with no stone in it,
/// and slow water lies still; water pours glassy down a ledge's tongue and
/// breaks white where it lands at its foot, the white carried on below.
#[test]
fn fast_water_churns_and_breaks_white_below_a_ledge() {
    let fast = plane(4.0, 0.3, 0.4, 0.05);
    let tongue = Section { slope: 0.7, ..fast };
    let sections: Vec<Section> = (0..=120)
        .map(|tenth| {
            if (55..=60).contains(&tenth) {
                tongue
            } else {
                fast
            }
        })
        .collect();
    let ledge = Stretch {
        spacing: 0.1,
        ..stretch(12.0, sections)
    };
    let flow = solve(ledge, Vec::new(), &tairix_parallel::SERIAL);
    let churned = largest(&profile(&flow, (2.0, 4.0, 0.005), 0.0));
    let depth = fast.depth(0.0);
    assert!(
        churned > 0.3 * CHURN * depth && churned < CHURN * depth,
        "fast water churns {churned}"
    );
    let (_, down) = flow.at(5.8, 0.0);
    assert!(down < 0.05, "down its tongue {down}");
    let (_, landed) = flow.at(6.3, 0.0);
    assert!(landed > 0.9, "where it lands {landed}");
    let (_, below) = flow.at(7.3, 0.0);
    assert!(below > 0.1 && below < landed, "below it {below}");
    let (_, above) = flow.at(4.5, 0.0);
    assert!(above < 0.05, "above it {above}");
    let slow = Section {
        running: 0.05,
        ..plane(4.0, 0.8, 0.85, 0.001)
    };
    let still = solve(
        stretch(12.0, vec![slow; 13]),
        Vec::new(),
        &tairix_parallel::SERIAL,
    );
    assert!(largest(&profile(&still, (2.0, 10.0, 0.01), 0.0)) < 1e-9);
}

/// A ridge across the stream, as a log lying across it makes, raises a train
/// of standing waves that the stream's eddies damp within a few of their
/// lengths, rather than carrying it down the stretch.
#[test]
fn a_train_of_standing_waves_dies_away_behind_a_ridge() {
    let stones: Vec<Stone> = (0..31)
        .map(|index| Stone {
            at: (3.0, -1.5 + 0.1 * f64::from(index)),
            reach: (0.08, 0.08),
            top: 0.08,
        })
        .collect();
    let flow = solve(riffle(), stones, &tairix_parallel::SERIAL);
    let swell = |from: f64| {
        (0..100)
            .map(|step| flow.at(from + 0.01 * f64::from(step), 0.0).0.abs())
            .fold(0.0f64, f64::max)
    };
    let (near, far) = (swell(3.3), swell(6.5));
    assert!(near > 1e-3, "the ridge raises a train {near}");
    assert!(
        far < 0.25 * near,
        "it carries {far} of {near} three metres on"
    );
}

/// However many stones crowd the water, no place stands higher than its
/// velocity head lets it, nor falls to its bed.
#[test]
fn no_place_stands_past_what_water_can() {
    let stones: Vec<Stone> = (0..400)
        .map(|index| {
            let key = crate::sample::mix32(index ^ 0x77);
            Stone {
                at: (
                    1.5 + 9.0 * crate::sample::unit(key),
                    3.2 * crate::sample::unit(crate::sample::mix32(key ^ 1)) - 1.6,
                ),
                reach: (0.15, 0.12),
                top: 0.05 + 0.3 * crate::sample::unit(crate::sample::mix32(key ^ 2)),
            }
        })
        .collect();
    let flow = solve(riffle(), stones, &Threaded::new(4));
    let section = plane(4.0, 0.3, 0.5, 0.013);
    for step in 0..2000u32 {
        let along = 0.006 * f64::from(step);
        for across in [-1.5, -0.7, 0.0, 0.4, 1.2] {
            let (rise, foam) = flow.at(along, across);
            // Held at each point of the grid, so read between them against
            // the most the points about it allow.
            let (highest, floor) = [across - flow.cell, across, across + flow.cell]
                .iter()
                .fold((0.0f64, 0.0f64), |(highest, floor), &at| {
                    (
                        highest.max(head(section.water_at(at).1)),
                        floor.max(FLOOR * section.depth(at)),
                    )
                });
            assert!(rise <= highest + 1e-9, "{rise} past the head {highest}");
            assert!(rise >= -floor - 1e-9, "{rise} below the floor");
            assert!((0.0..=1.0).contains(&foam));
        }
    }
    for (rise, (head, floor)) in [
        (0.5, (0.04, 0.1)),
        (-0.5, (0.04, 0.1)),
        (0.001, (0.04, 0.1)),
    ] {
        let kept = held(rise, head, floor);
        assert!(kept < head && kept > -floor);
    }
    assert!((held(0.001, 0.04, 0.1) - 0.001).abs() < 1e-6);
}

/// A stretch's answer is the same bit for bit however its rows are shared
/// out, and nothing outside the stretch.
#[test]
fn a_flow_is_the_same_on_any_runner() {
    let stones = vec![
        Stone {
            at: (4.0, 0.3),
            reach: (0.2, 0.15),
            top: 0.3,
        },
        Stone {
            at: (6.5, -0.5),
            reach: (0.1, 0.1),
            top: 0.06,
        },
    ];
    let alone = solve(riffle(), stones.clone(), &tairix_parallel::SERIAL);
    let shared = solve(riffle(), stones, &Threaded::new(5));
    assert_eq!(alone.rise, shared.rise);
    assert_eq!(alone.foam, shared.foam);
    assert_eq!(alone.at(-1.0, 0.0), (0.0, 0.0));
    assert_eq!(alone.at(5.0, 9.0), (0.0, 0.0));
}

#[test]
fn a_stretch_with_no_water_has_no_flow() {
    let dry = Section {
        water: -0.3,
        ..plane(4.0, 0.3, 0.5, 0.013)
    };
    for stretch in [stretch(12.0, vec![dry; 13]), stretch(12.0, Vec::new())] {
        assert!(!stretch.flows());
        assert!(Solving::new(stretch, Vec::new(), (0.02, (64, 64))).is_none());
    }
    assert!(riffle().flows());
}

/// A grid takes as many points as its stretch wants, a power of two, and
/// never more than it is allowed, however many that is.
#[test]
fn a_grid_keeps_within_the_points_it_is_allowed() {
    let solving = Solving::new(riffle(), Vec::new(), (0.005, (300, 100))).expect("a solving");
    assert_eq!(solving.size, (256, 64));
    let solving = Solving::new(riffle(), Vec::new(), (0.5, (300, 100))).expect("a solving");
    assert_eq!(solving.size, (32, 8));
}

/// A wake halfway between two of the grid's points still sheds its foam,
/// into the nearer the far one.
#[test]
fn a_wake_halfway_between_two_points_still_sheds() {
    let mut solving = Solving::new(riffle(), Vec::new(), (0.02, (1024, 256))).expect("a solving");
    let ((columns, _), cell) = (solving.size, solving.cell);
    solving.shed.push(Shed {
        across: 0.0,
        along: solving.stretch.from + 10.5 * cell,
        half: 0.2,
        share: 0.5,
    });
    let foam = solving.shed_foam().expect("foam");
    let row = whole(solving.stretch.half / cell);
    assert_eq!(foam[row * columns + 11], byte(0.5));
    assert_eq!(foam[row * columns + 10], 0);
}

/// A stone parts the water in proportion to how fast the stream runs, so its
/// answer falls away with the speed as the bed's does: one wave number's
/// answer to each keeps the same ratio whatever the speed.
#[test]
fn a_stones_answer_keeps_pace_with_the_beds() {
    let deep = Deep::at(2.0, 0.3).expect("a wave over water");
    let ratio = |speed: f64| {
        let (to_bed, to_sources) = deep.answer(2.0, speed);
        mathf::hypot(to_sources.re, to_sources.im) / mathf::hypot(to_bed.re, to_bed.im)
    };
    for speed in [0.4, 0.8, 1.2] {
        let drift = ratio(speed) / ratio(0.6) - 1.0;
        assert!(
            drift.abs() < 0.01,
            "at {speed} m/s the ratio drifts {drift}"
        );
    }
}

/// A ladder's rungs run evenly in their logarithm, and a value takes the two
/// rungs about it, weighted toward the nearer, held at either end.
#[test]
fn a_ladder_weighs_the_rungs_about_a_value() {
    let ladder = Ladder::new(0.1, 1.0, 3);
    assert!((ladder.value(0) - 0.1).abs() < 1e-12);
    assert!((ladder.value(1) - mathf::sqrt(0.1)).abs() < 1e-12);
    assert!((ladder.value(2) - 1.0).abs() < 1e-12);
    for value in [0.05, 0.1, 0.2, 0.5, 1.0, 3.0] {
        let place = ladder.place(value);
        let total: f64 = (0..3).map(|rung| Ladder::weight(place, rung)).sum();
        assert!((total - 1.0).abs() < 1e-12, "{value}: {total}");
    }
    assert_eq!(ladder.place(0.05), (0, 0.0));
    let (below, t) = ladder.place(3.0);
    assert!(below == 1 && (t - 1.0).abs() < 1e-12);
    let (below, t) = ladder.place(mathf::sqrt(0.1) * 1.01);
    assert!(below == 1 && t > 0.0 && t < 0.05, "{below} {t}");
}
