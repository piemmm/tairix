//! Host tests of a water lily's parts: each pad and petal cut to an outline
//! of its own, worn as its age says, and coloured to agree with the cut.

use super::*;
use crate::heightfield::CHANNELS;

/// Keys enough to see a lily's pads and petals vary.
fn keys(count: u32) -> impl Iterator<Item = u32> {
    (0..count).map(|index| mix32(index ^ 0x11e5))
}

#[test]
fn a_key_carries_its_parts_age_above_its_plants_colour() {
    for key in keys(64) {
        for step in 0..=20u32 {
            let age_drawn = f64::from(step) / 20.0;
            let keyed = aged(key, age_drawn);
            assert_eq!(keyed & 3, key & 3, "the plant's colour kept");
            assert_eq!(keyed & !AGE_MASK, key & !AGE_MASK, "the rest kept");
            assert!((age(keyed) - age_drawn).abs() <= 0.5 / f64::from(AGE_LEVELS - 1));
        }
        assert!((age(aged(key, 2.0)) - 1.0).abs() < 1e-12, "clamped");
        assert!(age(aged(key, -1.0)).abs() < 1e-12, "clamped");
    }
}

#[test]
fn no_two_pads_are_alike_and_few_are_round() {
    let reaches = |key: u32| -> [f64; 24] {
        core::array::from_fn(|step| {
            let angle = (PI - SINUS) * (f64::from(u32::try_from(step).unwrap_or(0)) / 12.0 - 1.0);
            Pad::of(aged(key, 0.4)).reach(angle)
        })
    };
    let mut drawn: alloc::vec::Vec<[f64; 24]> = alloc::vec::Vec::new();
    let mut uneven = 0;
    for key in keys(40) {
        let reach = reaches(key);
        let (least, most) = reach
            .iter()
            .fold((f64::INFINITY, 0.0_f64), |(low, high), &r| {
                (low.min(r), high.max(r))
            });
        assert!(least > 0.82 && most < 1.15, "{least}..{most}");
        assert!(most <= Pad::of(aged(key, 0.4)).most() + 1e-12);
        uneven += usize::from(most > 1.03 * least);
        let unlike =
            |other: &[f64; 24]| other.iter().zip(&reach).any(|(a, b)| (a - b).abs() > 1e-6);
        assert!(drawn.iter().all(unlike), "each its own");
        drawn.push(reach);
    }
    assert!(uneven >= 30, "{uneven} of 40 out of round");
}

#[test]
fn a_pad_keeps_its_middle_but_not_its_slit_nor_past_its_margin() {
    for key in keys(200) {
        for age_drawn in [0.0, 0.5, 1.0] {
            let pad = Pad::of(aged(key, age_drawn));
            assert!(pad.edges((0.05, 0.0)).kept(), "{key:#x} at its stalk");
            assert!(!pad.edges((-0.5, 0.0)).kept(), "its sinus's slit");
            assert!(!pad.edges((-0.5, 0.01)).kept(), "its sinus's slit");
            assert!(!pad.edges((1.3, 0.0)).kept(), "past its margin");
            assert!(!pad.edges((0.0, -1.3)).kept(), "past its margin");
            assert_eq!(
                Trim::Pad.keeps((0.3, 0.2), aged(key, age_drawn)),
                pad.edges((0.3, 0.2)).kept(),
                "the trim cuts as the pad's own outline does"
            );
        }
    }
}

#[test]
fn a_pad_gathers_bites_tears_holes_and_fraying_as_it_ages() {
    let worn = |age_drawn: f64| {
        keys(300)
            .map(|key| {
                let pad = Pad::of(aged(key, age_drawn));
                pad.marks.0 + pad.marks.1 + pad.marks.2
            })
            .sum::<usize>()
    };
    let (young, old) = (worn(0.05), worn(0.95));
    assert!(old > 3 * young, "{young} beside {old}");
    assert!(keys(300).all(|key| Pad::of(aged(key, 0.3)).fray <= 0.0));
    assert!(keys(300).any(|key| Pad::of(aged(key, 1.0)).fray > 0.02));
    // An old pad's surface is pierced: some of a grid over it falls in a
    // wound.
    let wounded = keys(100)
        .filter(|&key| {
            let pad = Pad::of(aged(key, 1.0));
            (0..1600u32).any(|step| {
                let (x, y) = (
                    f64::from(step % 40) / 20.0 - 0.975,
                    f64::from(step / 40) / 20.0 - 0.975,
                );
                pad.edges((x, y)).wound < 0.0
            })
        })
        .count();
    assert!(wounded > 70, "{wounded} of 100");
}

#[test]
fn a_bite_a_split_and_a_hole_each_cut_away_what_they_cover() {
    let bite = Bite {
        centre: (0.9, 0.0),
        way: (1.0, 0.0),
        axes: (0.05, 0.1),
    };
    assert!(bite.clear((0.9, 0.0), 7) < 0.0);
    assert!(bite.clear((0.9, 0.07), 7) < 0.0, "longer across");
    assert!(bite.clear((0.9, 0.2), 7) > 0.0);
    assert!(bite.clear((0.7, 0.0), 7) > 0.0);
    let split = Split {
        origin: (0.0, 0.0),
        way: (1.0, 0.0),
        from: 0.6,
        to: 1.0,
        gape: 0.04,
    };
    assert!(split.clear((0.5, 0.0)) > 0.0, "closed before it starts");
    assert!(split.clear((0.95, 0.005)) < 0.0, "gaping toward the edge");
    assert!(split.clear((0.95, 0.03)) > 0.0);
    let hole = Hole {
        centre: (0.3, 0.3),
        radius: 0.02,
    };
    assert!(hole.clear((0.3, 0.3), 3) < 0.0 && hole.clear((0.3, 0.36), 3) > 0.0);
}

#[test]
fn a_petal_is_widest_past_its_middle_a_sepal_below_it_and_a_stamen_narrow() {
    for key in keys(100) {
        for sheet in [Sheet::Petal, Sheet::Sepal, Sheet::Stamen, Sheet::Broad] {
            let petal = Petal::of(aged(key, 0.2), sheet);
            let widths: alloc::vec::Vec<f64> = (0..=50u32)
                .map(|step| petal.half_width(f64::from(step) / 50.0, 1.0))
                .collect();
            let widest = widths
                .iter()
                .enumerate()
                .fold((0, 0.0_f64), |held, (at, &width)| {
                    if width > held.1 {
                        (at, width)
                    } else {
                        held
                    }
                })
                .0;
            let widest = f64::from(u32::try_from(widest).unwrap_or(0)) / 50.0;
            match sheet {
                Sheet::Petal => assert!((0.45..0.75).contains(&widest), "{widest}"),
                Sheet::Sepal => assert!((0.25..0.5).contains(&widest), "{widest}"),
                Sheet::Stamen => assert!(petal.breadth < 0.1, "{}", petal.breadth),
                Sheet::Broad => assert!(widest > 0.55 && petal.breadth > 0.35, "{widest}"),
            }
            assert!(
                widths.last().copied().unwrap_or(1.0) < 0.02,
                "drawn in at its tip"
            );
            assert!(petal.edges((0.5, 0.0)).kept());
            assert!(!petal.edges((0.5, 1.3 * petal.breadth)).kept());
            assert!(!petal.edges((1.02, 0.0)).kept());
            let at = (0.4, 0.3 * petal.breadth);
            assert_eq!(
                Trim::Sheet(sheet).keeps(at, aged(key, 0.2)),
                petal.edges(at).kept()
            );
        }
    }
    // No two petals of a flower quite alike, nor its two sides.
    let lean: f64 = keys(50)
        .map(|key| Petal::of(key, Sheet::Petal).lean.abs())
        .sum();
    assert!(lean > 50.0 * 0.03, "{lean}");
}

fn spot(key: u32, (at, size): ((f64, f64), f64), front: bool) -> Spot {
    Spot {
        p: Vec3::ZERO,
        normal: Vec3::UP,
        height: 0.0,
        width: 1e-4,
        mark: key ^ 0x5a5a,
        along: 0.0,
        uv: (at.0 * size, at.1 * size),
        girth: size,
        instance: 0x5a5a,
        front,
        ground: [0.0; CHANNELS],
        thatch: 0.0,
    }
}

const GREENS: [Vec3; 4] = [Vec3::new(0.03, 0.09, 0.02); 4];

#[test]
fn a_pad_opens_bronze_greens_then_yellows_and_browns_as_it_dies() {
    let pads = Lily::Pads(GREENS);
    let mean = |age_drawn: f64| {
        keys(60).fold(Vec3::ZERO, |sum, key| {
            sum + pads.colour(&spot(aged(key, age_drawn), ((0.35, 0.2), 0.12), true))
        }) * (1.0 / 60.0)
    };
    let (young, grown, dying) = (mean(0.0), mean(0.35), mean(1.0));
    assert!(young.x > young.y, "bronze: {young:?}");
    assert!(grown.y > 1.5 * grown.x, "green: {grown:?}");
    assert!(dying.x > dying.y && dying.y > dying.z, "brown: {dying:?}");
    // Its underside is red while it is young.
    let under = keys(60).fold(Vec3::ZERO, |sum, key| {
        sum + pads.colour(&spot(aged(key, 0.1), ((0.35, 0.2), 0.12), false))
    });
    assert!(under.x > under.y, "{under:?}");
}

#[test]
fn a_pads_wounds_dry_dark_where_its_outline_is_cut() {
    let pads = Lily::Pads(GREENS);
    let (mut near_sum, mut clear_sum, mut checked) = (0.0, 0.0, 0);
    for key in keys(300) {
        let keyed = aged(key, 0.8);
        let pad = Pad::of(keyed);
        let Some(hole) = pad
            .holes
            .get(..pad.marks.2)
            .and_then(<[Hole]>::first)
            .copied()
        else {
            continue;
        };
        // Just outside the hole's rim, and beside it past where its scar
        // has faded.
        let near = (hole.centre.0 + 1.15 * hole.radius, hole.centre.1);
        let clear = (hole.centre.0 + hole.radius + 0.1, hole.centre.1);
        if !pad.edges(near).kept() || pad.edges(clear).wound < 0.1 || pad.edges(clear).margin < 0.1
        {
            continue;
        }
        let shade = |at| pads.colour(&spot(keyed, (at, 0.12), true)).luminance();
        near_sum += shade(near);
        clear_sum += shade(clear);
        checked += 1;
    }
    assert!(checked > 30, "{checked}");
    assert!(near_sum < 0.7 * clear_sum, "{near_sum} beside {clear_sum}");
}

#[test]
fn a_petal_creams_and_browns_from_its_tip_as_it_fades() {
    let whites = [Vec3::new(0.85, 0.85, 0.82); 4];
    let petals = Lily::Petals(whites);
    let at = |key: u32, age_drawn: f64, u: f64| {
        petals.colour(&spot(aged(key, age_drawn), ((u, 0.0), 0.06), true))
    };
    for key in keys(40) {
        let (fresh, faded) = (at(key, 0.2, 0.9), at(key, 1.0, 0.95));
        assert!(
            faded.luminance() < 0.6 * fresh.luminance(),
            "{fresh:?} {faded:?}"
        );
    }
    let mean = |u: f64| keys(40).map(|key| at(key, 0.8, u).luminance()).sum::<f64>();
    assert!(mean(0.95) < 0.8 * mean(0.4), "brown from its tip");
    let hearts = Lily::Hearts([Vec3::new(0.7, 0.45, 0.04); 4]);
    let heart = |age_drawn: f64| hearts.colour(&spot(aged(7, age_drawn), ((0.7, 0.0), 1.0), true));
    assert!(heart(1.0).y < heart(0.1).y, "an old heart dulls");
}
