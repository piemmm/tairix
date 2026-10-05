use core::f64::consts::PI;

use super::*;
use crate::heightfield::CHANNELS;

const KINDS: [BarkKind; 9] = [
    BarkKind::Furrowed,
    BarkKind::Papery,
    BarkKind::Plated,
    BarkKind::Smooth,
    BarkKind::Banded,
    BarkKind::Scaly,
    BarkKind::Ringed,
    BarkKind::Taproot,
    BarkKind::Ribbed,
];

fn bark(kind: BarkKind) -> Bark {
    Bark {
        kind,
        light: Vec3::new(0.45, 0.35, 0.28),
        dark: Vec3::new(0.03, 0.025, 0.02),
        accent: Vec3::new(0.6, 0.25, 0.1),
        rise: 6.0,
        snow: 0.0,
        moss: 0.0,
        bare: 0.0,
        seed: 7,
    }
}

/// A point of bark seen from close by.
fn on(along: f64, angle: f64, girth: f64, key: u32) -> OnLimb {
    OnLimb::new(along, angle, girth, (key, 1e-4))
}

fn spot(at: (f64, f64), girth: f64, (key, width): (u32, f64)) -> Spot {
    Spot {
        p: Vec3::ZERO,
        normal: Vec3::new(1.0, 0.0, 0.0),
        height: at.0,
        width,
        mark: 0,
        along: 0.0,
        uv: at,
        girth,
        instance: key,
        front: true,
        ground: [0.0; CHANNELS],
        thatch: 0.0,
    }
}

#[test]
fn every_bark_closes_round_its_limb_with_no_seam() {
    for kind in KINDS {
        let bark = bark(kind);
        let mut worst: f64 = 0.0;
        for step in 0..60u32 {
            let along = 0.3 + f64::from(step) * 0.17;
            let (before, after) = (PI - 1e-7, -PI + 1e-7);
            let (a, b) = (on(along, before, 0.3, 1), on(along, after, 0.3, 1));
            worst = worst.max((bark.height(&a) - bark.height(&b)).abs());
            let colours =
                [before, after].map(|angle| bark.colour(&spot((along, angle), 0.3, (1, 1e-4))));
            worst = worst.max((colours[0] - colours[1]).max_element().abs());
        }
        assert!(
            worst < 1e-4,
            "{kind:?} is cut where its angle wraps: {worst}"
        );
    }
}

/// How many fissures a walk once round the limb crosses.
fn fissures_round(bark: &Bark, along: f64, girth: f64) -> u32 {
    let steps = 4000u32;
    let mut crossed = 0;
    let mut deep = false;
    for step in 0..=steps {
        let angle = TAU * f64::from(step) / f64::from(steps);
        let height = bark.height(&on(along, angle, girth, 3));
        if height < 0.2 && !deep {
            crossed += 1;
        }
        deep = height < 0.2;
    }
    crossed
}

#[test]
fn a_barks_pattern_keeps_its_real_size_on_a_limb_of_any_girth() {
    for kind in [BarkKind::Plated, BarkKind::Furrowed] {
        let bark = bark(kind);
        let (thin, thick): (u32, u32) = (0..6)
            .map(|row| {
                let along = 0.5 + f64::from(row) * 0.7;
                (
                    fissures_round(&bark, along, 0.12),
                    fissures_round(&bark, along, 0.36),
                )
            })
            .fold((0, 0), |(a, b), (c, d)| (a + c, b + d));
        let ratio = f64::from(thick) / f64::from(thin.max(1));
        assert!(thin > 6, "{kind:?}: {thin} fissures round a thin trunk");
        assert!(
            (2.0..4.5).contains(&ratio),
            "{kind:?}: {thin} round a thin trunk, {thick} round one thrice as thick"
        );
    }
}

/// A fissure reads darker than the plates and ridges beside it, and the
/// hollow between scales darker than their crowns.
#[test]
fn fissures_are_darker_than_the_plates_and_ridges_between_them() {
    for (kind, (low, high_above), darker) in [
        (BarkKind::Plated, (0.25, 0.7), 0.45),
        (BarkKind::Furrowed, (0.25, 0.7), 0.45),
        (BarkKind::Scaly, (0.5, 0.8), 0.7),
    ] {
        let bark = bark(kind);
        let (mut deep, mut high) = ((0.0, 0u32), (0.0, 0u32));
        for step in 0..6000u32 {
            let along = 0.4 + f64::from(step % 200) * 0.013;
            let angle = TAU * unit(mix32(step));
            let at = on(along, angle, 0.3, 5);
            let lit = bark
                .colour(&spot((along, angle), 0.3, (5, 1e-4)))
                .luminance();
            match bark.height(&at) {
                h if h < low => deep = (deep.0 + lit, deep.1 + 1),
                h if h > high_above => high = (high.0 + lit, high.1 + 1),
                _ => {}
            }
        }
        assert!(
            deep.1 > 100 && high.1 > 100,
            "{kind:?}: {} deep, {} high",
            deep.1,
            high.1
        );
        let (deep, high) = (deep.0 / f64::from(deep.1), high.0 / f64::from(high.1));
        assert!(
            deep < darker * high,
            "{kind:?}: its hollows read {deep}, its crowns {high}"
        );
    }
}

#[test]
fn no_two_trees_wear_the_same_bark() {
    for kind in [
        BarkKind::Plated,
        BarkKind::Furrowed,
        BarkKind::Papery,
        BarkKind::Scaly,
    ] {
        let bark = bark(kind);
        let (mut same, mut differ) = (0.0f64, 0.0);
        for step in 0..200u32 {
            let (along, angle) = (0.5 + f64::from(step) * 0.031, f64::from(step) * 0.7);
            let height = |key: u32| bark.height(&on(along, angle, 0.25, key));
            same = same.max((height(11) - height(11)).abs());
            differ += (height(11) - height(12)).abs();
        }
        assert!(
            same == 0.0,
            "{kind:?}: one tree's bark is its own every time"
        );
        assert!(
            differ / 200.0 > 0.05,
            "{kind:?}: two trees wear the same bark"
        );
    }
}

#[test]
fn a_birch_is_white_above_its_black_foot_and_brown_in_its_twigs() {
    let birch = Bark {
        light: Vec3::new(0.85, 0.83, 0.8),
        dark: Vec3::new(0.02, 0.018, 0.016),
        accent: Vec3::new(0.7, 0.62, 0.52),
        ..bark(BarkKind::Papery)
    };
    let mean = |along: f64, girth: f64| {
        (0..400u32)
            .map(|step| {
                let angle = TAU * f64::from(step) / 400.0;
                birch
                    .colour(&spot(
                        (along + 0.001 * f64::from(step % 7), angle),
                        girth,
                        (9, 1e-4),
                    ))
                    .luminance()
            })
            .sum::<f64>()
            / 400.0
    };
    let (trunk, foot, twig) = (mean(7.0, 0.14), mean(0.3, 0.2), mean(9.0, 0.006));
    assert!(trunk > 0.6, "white up the trunk: {trunk}");
    assert!(
        foot < 0.45 * trunk,
        "black about the foot: {foot} against {trunk}"
    );
    assert!(twig < 0.2, "brown in the twigs: {twig}");
}

#[test]
fn a_pine_turns_orange_above_its_rise() {
    let pine = bark(BarkKind::Plated);
    let warmth = |along: f64| {
        let colour = (0..500u32)
            .map(|step| {
                let angle = TAU * unit(mix32(step ^ 0x33));
                pine.colour(&spot(
                    (along + 0.01 * f64::from(step % 50), angle),
                    0.2,
                    (4, 1e-4),
                ))
            })
            .fold(Vec3::ZERO, |sum, colour| sum + colour);
        colour.x / colour.z.max(1e-9)
    };
    let (low, high) = (warmth(1.5), warmth(11.0));
    assert!(
        high > 1.3 * low,
        "redder up the trunk: {high} against {low}"
    );
}

#[test]
fn bark_seen_from_far_off_settles_to_its_mean() {
    for kind in [
        BarkKind::Plated,
        BarkKind::Furrowed,
        BarkKind::Papery,
        BarkKind::Scaly,
    ] {
        // High on the trunk, clear of a birch's black foot, and below where
        // a pine's bark turns: those are a trunk's own changes, which show
        // from any distance.
        let bark = Bark {
            rise: 100.0,
            ..bark(kind)
        };
        let spread = |width: f64| {
            let heights: [f64; 300] = core::array::from_fn(|step| {
                let step = u32::try_from(step).unwrap_or(0);
                let at = OnLimb::new(
                    8.0 + 0.01 * f64::from(step),
                    f64::from(step) * 0.37,
                    0.3,
                    (2, width),
                );
                bark.height(&at)
            });
            let mean = heights.iter().sum::<f64>() / 300.0;
            heights.iter().map(|h| (h - mean) * (h - mean)).sum::<f64>() / 300.0
        };
        let (near, far) = (spread(1e-4), spread(2.0));
        assert!(
            far < 0.1 * near,
            "{kind:?} still shimmers far off: {far} against {near}"
        );
    }
}

#[test]
fn every_bark_stays_within_its_bounds() {
    for kind in KINDS {
        let bark = Bark {
            moss: 0.8,
            snow: 0.5,
            ..bark(kind)
        };
        for step in 0..3000u32 {
            let along = 20.0 * unit(mix32(step));
            let angle = TAU * unit(mix32(step ^ 1)) - PI;
            let girth = 0.003 + 0.6 * unit(mix32(step ^ 2));
            let width = 1e-4 * mathf::exp(9.0 * unit(mix32(step ^ 3)));
            let height = bark.height(&OnLimb::new(along, angle, girth, (step, width)));
            assert!((0.0..=1.0).contains(&height), "{kind:?}: {height}");
            let colour = bark.colour(&spot((along, angle), girth, (step, width)));
            let least = colour.x.min(colour.y).min(colour.z);
            assert!(
                least >= 0.0 && colour.max_element() <= 1.0,
                "{kind:?}: {colour:?}"
            );
        }
    }
}

#[test]
fn a_scar_is_found_the_short_way_round_from_either_side() {
    let (mut found, mut agreed) = (0, 0);
    for step in 0..400u32 {
        let along = 0.1 + f64::from(step) * 0.05;
        let a = scar(&on(along, PI - 1e-6, 0.3, 0), 3, 0.35);
        let b = scar(&on(along, -PI + 1e-6, 0.3, 0), 3, 0.35);
        if let (Some((x, y, key)), Some((u, v, other))) = (a, b) {
            found += 1;
            agreed += u32::from(key == other && (x - u).abs() < 1e-4 && (y - v).abs() < 1e-9);
        }
    }
    assert!(found > 300, "{found}");
    assert_eq!(found, agreed);
}

/// How fast a bark may rise, a metre along its limb or round it, bounds
/// every rise found over a dense sampling of it, its finest detail and all,
/// so a march that steps by it never steps over the surface it cuts. A
/// palm's rings step, and are left out.
#[test]
fn a_barks_steepest_bounds_how_fast_it_rises() {
    // Read finer than a shading normal reads it, to catch its sharpest.
    const STEP: f64 = 2e-4;
    for kind in KINDS.into_iter().filter(|&kind| kind != BarkKind::Ringed) {
        let bark = Bark {
            seed: 23,
            ..bark(kind)
        };
        let mut steepest = 0.0f64;
        for step in 0..30_000u32 {
            let along = 0.05 + f64::from(step % 300) * 0.0191;
            let angle = f64::from(step / 300) * 0.0617;
            let girth = [0.07, 0.25, 0.55][(step % 3) as usize];
            let at = OnLimb::new(along, angle, girth, (step % 11, 0.0));
            let here = bark.height(&at);
            let along_limb = (bark.height(&at.moved(STEP, 0.0)) - here) / STEP;
            let round_it = (bark.height(&at.moved(0.0, STEP)) - here) / STEP;
            steepest = steepest.max(along_limb.abs()).max(round_it.abs());
        }
        assert!(
            steepest < bark.steepest(),
            "{kind:?} rises {steepest} a metre against {}",
            bark.steepest()
        );
    }
}

/// Bark sloughs off dead wood in sheets about as much of it as asked, the
/// wood it bares sunk below the bark it fell from and brown, never a pale,
/// smooth plane: checked dark along its grain; sound bark keeps all of itself.
#[test]
fn dead_bark_sloughs_away_in_sheets_baring_the_wood_beneath() {
    let samples = |bare: f64| {
        let bark = Bark {
            bare,
            ..bark(BarkKind::Furrowed)
        };
        let (mut gone, mut bared, mut checks) = (0u32, Vec3::ZERO, (0.0, 0u32));
        for step in 0..4000u32 {
            let at = on(
                0.3 + f64::from(step % 80) * 0.05,
                f64::from(step / 80) * 0.126,
                0.3,
                3,
            );
            if bark.gone(&at) < 0.99 {
                continue;
            }
            gone += 1;
            let colour = bark.colour(&spot((at.along, at.angle), 0.3, (3, 1e-4)));
            let height = bark.height(&at);
            assert!(height <= BARED + 0.05, "sunk below the bark: {height}");
            bared += colour;
            if height < 0.5 * BARED {
                checks = (checks.0 + colour.luminance(), checks.1 + 1);
            }
        }
        (
            f64::from(gone) / 4000.0,
            bared * (1.0 / f64::from(gone.max(1))),
            checks,
        )
    };
    let ((sound, ..), (half, bared, (checked, cracks))) = (samples(0.0), samples(0.5));
    assert!(sound < 0.02, "sound bark keeps itself: {sound}");
    assert!((0.25..0.75).contains(&half), "about half sloughed: {half}");
    assert!(
        bared.x > bared.z && bared.luminance() < 0.25,
        "brown, not silvered: {bared:?}"
    );
    assert!(cracks > 10, "{cracks} cracks");
    assert!(
        checked / f64::from(cracks) < 0.7 * bared.luminance(),
        "checks read dark"
    );
}
