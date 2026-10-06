use super::*;

fn woodland(cover: f64) -> Woodland {
    Woodland {
        cover,
        patch: 200.0,
        closure: (0.6, 1.0),
        stature: (0.8, 1.0),
        gaps: 0.3,
        most: 1000,
        open: (1.5, 0.3),
    }
}

#[test]
fn a_wood_covers_about_the_share_of_the_ground_asked_of_it() {
    for cover in [0.15, 0.4, 0.65, 0.9] {
        let (mut wooded_places, mut places) = (0.0, 0.0);
        for row in 0..160 {
            for column in 0..160 {
                let at = (f64::from(column) * 37.0, f64::from(row) * 37.0);
                wooded_places += wooded(&woodland(cover), 99, at);
                places += 1.0;
            }
        }
        let share = wooded_places / places;
        assert!((share - cover).abs() < 0.08, "{cover}: {share}");
    }
}

#[test]
fn a_canopy_opens_gaps_where_its_lattice_holds_them_and_none_where_it_is_closed() {
    let share_open = |gaps: f64| {
        let woodland = Woodland {
            gaps,
            ..woodland(0.9)
        };
        let mut open = 0.0;
        for row in 0..300 {
            for column in 0..300 {
                open += gap(
                    &woodland,
                    5,
                    (f64::from(column) * 7.3, f64::from(row) * 7.3),
                );
            }
        }
        open / (300.0 * 300.0)
    };
    assert!(share_open(0.0).abs() < 1e-12);
    let (few, many) = (share_open(0.2), share_open(0.6));
    assert!((0.01..0.06).contains(&few), "{few}");
    assert!((0.05..0.15).contains(&many), "{many}");
    // A gap's heart is open all through, its edge closing over it.
    let woodland = Woodland {
        gaps: 1.0,
        ..woodland(0.9)
    };
    let found = cells2(0.5, 0.5, 5 ^ 0x6a95, 0.8);
    let middle = (
        (0.5 + found.toward.x) * GAP_SPACING,
        (0.5 + found.toward.z) * GAP_SPACING,
    );
    assert!(gap(&woodland, 5, middle) > 0.99, "at {middle:?}");
}

#[test]
fn what_grows_beneath_a_wood_takes_to_its_gaps_and_edges() {
    let deep = thrives_beneath(0.99);
    let gap = thrives_beneath(0.5);
    let open = thrives_beneath(0.0);
    assert!(deep < 0.25, "{deep}");
    assert!(gap > 0.95, "{gap}");
    assert!(open > deep && open < 0.6 * gap, "{open}");
}

#[test]
fn a_wood_keeps_out_of_its_clearing_off_the_road_and_within_its_heights() {
    let lie = Lie {
        height: 40.0,
        upright: 0.98,
        green: 1.0,
        ..Lie::default()
    };
    let rooting = Rooting {
        above: Some((10.0, 20.0)),
        below: Some((60.0, 80.0)),
        clearing: Some(((100.0, 0.0), 15.0)),
        ..ANYWHERE
    };
    let suits = |lie: Lie, at: (f64, f64)| rooting.suits(&lie, at);
    let barred = |lie: Lie, at: (f64, f64)| suits(lie, at).abs() < 1e-12;
    assert!((suits(lie, (0.0, 0.0)) - 1.0).abs() < 1e-9);
    assert!(barred(lie, (110.0, 0.0)), "in the clearing");
    assert!(barred(Lie { road: 1.0, ..lie }, (0.0, 0.0)), "on the road");
    assert!(
        suits(Lie { path: 1.0, ..lie }, (0.0, 0.0)) < 0.1,
        "on a path"
    );
    assert!(
        barred(Lie { height: 5.0, ..lie }, (0.0, 0.0)),
        "below its heights"
    );
    assert!(
        barred(
            Lie {
                height: 90.0,
                ..lie
            },
            (0.0, 0.0)
        ),
        "above them"
    );
    assert!(
        barred(
            Lie {
                upright: 0.6,
                ..lie
            },
            (0.0, 0.0)
        ),
        "too steep"
    );
    // Under snow nothing grows green, but a wood that roots bare stands on.
    let snowed = Lie { green: 0.0, ..lie };
    assert!(barred(snowed, (0.0, 0.0)));
    assert!(
        Rooting {
            bare: 0.8,
            ..rooting
        }
        .suits(&snowed, (0.0, 0.0))
            > 0.79
    );
}

#[test]
fn a_kind_takes_to_the_ground_it_favours_and_never_below_its_least() {
    let willow = Affinity {
        base: 0.25,
        wet: 1.6,
        rich: 0.0,
        least: 0.0,
    };
    let beech = Affinity {
        base: 1.0,
        wet: -1.2,
        rich: 0.5,
        least: 0.1,
    };
    let (dry, soaked) = (
        Lie {
            green: 1.0,
            ..Lie::default()
        },
        Lie {
            wet: 1.0,
            green: 1.0,
            ..Lie::default()
        },
    );
    assert!(willow.of(&soaked) > willow.of(&dry));
    assert!(beech.of(&dry) > beech.of(&soaked));
    assert!((beech.of(&soaked) - 0.3).abs() < 1e-9);
    assert!(
        (beech.of(&Lie {
            wet: 1.0,
            ..Lie::default()
        }) - 0.1)
            .abs()
            < 1e-9
    );
    assert!((Affinity::EVEN.of(&soaked) - 1.0).abs() < 1e-12);
}

#[test]
fn a_kind_scales_a_tree_within_its_own_bounds_and_stands_no_taller_than_its_tallest() {
    let habit = Habit {
        affinity: Affinity::EVEN,
        prototypes: [0, 1, 2, 3],
        heights: [4.0, 9.0, 15.0, 22.0],
        bark: 0,
        crown: 0.3,
        scaled: (0.75, 1.35),
    };
    assert!((habit.sized(11.0, 10.0) - 1.1).abs() < 1e-12);
    assert!((habit.sized(100.0, 10.0) - 1.35).abs() < 1e-12);
    assert!((habit.sized(1.0, 10.0) - 0.75).abs() < 1e-12);
    assert!((habit.tallest() - 22.0 * 1.35).abs() < 1e-12);
    assert_eq!(habit.nearest(14.0, 0.9), 2);
    assert_eq!(habit.nearest(21.0, 0.9), 3);
}

#[test]
fn a_kind_picks_the_grown_tree_nearest_a_height_as_ordering_them_all_by_nearness_does() {
    // The order every variant's nearness sorts them into, the earlier first
    // where two lie as near.
    let ordered = |habit: &Habit, height: f64, draw: f64| {
        let apart =
            |variant: usize| mathf::ln(height.max(1e-3) / habit.heights[variant].max(1e-3)).abs();
        let mut order: [usize; VARIANTS] = core::array::from_fn(|variant| variant);
        order.sort_by(|&a, &b| apart(a).total_cmp(&apart(b)));
        if apart(order[1]) < 0.25 && draw < 0.5 {
            order[1]
        } else {
            order[0]
        }
    };
    for heights in [
        [4.0, 9.0, 15.0, 22.0],
        [6.0, 4.0, 22.0, 13.0],
        [8.0, 8.0, 8.0, 12.0],
        [10.0, 11.0, 10.0, 11.0],
    ] {
        let habit = Habit {
            affinity: Affinity::EVEN,
            prototypes: [0, 1, 2, 3],
            heights,
            bark: 0,
            crown: 0.3,
            scaled: (0.75, 1.35),
        };
        for step in 0..=400u32 {
            let height = 0.5 + f64::from(step) * 0.08;
            for draw in [0.1, 0.49, 0.5, 0.9] {
                assert_eq!(
                    habit.nearest(height, draw),
                    ordered(&habit, height, draw),
                    "{heights:?} {height} {draw}"
                );
            }
        }
    }
}

#[test]
fn no_tree_wanted_at_most_so_tall_stands_taller_than_its_kinds_highest() {
    let habit = Habit {
        affinity: Affinity::EVEN,
        prototypes: [0, 1, 2, 3],
        heights: [6.0, 4.0, 22.0, 13.0],
        bark: 0,
        crown: 0.3,
        scaled: (0.75, 1.35),
    };
    for most in [1.0, 3.5, 5.0, 9.0, 14.0, 21.0, 26.0, 40.0] {
        let highest = habit.highest(most);
        for step in 0..=200u32 {
            let wanted = most * f64::from(step) / 200.0;
            for draw in [0.1, 0.9] {
                let natural = habit.heights[habit.nearest(wanted, draw)];
                let height = natural * habit.sized(wanted, natural);
                assert!(
                    height <= highest + 1e-9,
                    "{wanted} of {most}: {height} > {highest}"
                );
            }
        }
        assert!(highest <= habit.tallest() + 1e-9);
    }
}
