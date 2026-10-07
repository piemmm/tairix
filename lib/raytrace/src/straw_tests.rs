use alloc::vec::Vec;
use core::f64::consts::PI;

use super::*;
use crate::vector::real;

const LIGHT: Vec3 = Vec3::new(0.8, 0.65, 0.3);
const DARK: Vec3 = Vec3::new(0.6, 0.48, 0.2);
const TWINE_BLUE: Vec3 = Vec3::new(0.05, 0.2, 0.8);

fn straw(bound: Bound, half: Vec3) -> Straw {
    Straw {
        stalks: [LIGHT, DARK],
        binding: TWINE_BLUE,
        bound,
        half,
        seed: 7,
    }
}

/// Its colour on face `along` at `uv`, a footprint `width` across.
fn at(straw: &Straw, along: f64, uv: (f64, f64), width: f64) -> Vec3 {
    straw.colour(&Spot {
        along,
        uv,
        width,
        mark: 3,
        ..Spot::default()
    })
}

/// Its colour round a round unit's side, `up` its axis and `angle` about it,
/// a footprint `width` across.
fn round_at(straw: &Straw, (up, angle): (f64, f64), width: f64) -> Vec3 {
    straw.colour(&Spot {
        along: 2.0,
        uv: (up, angle),
        girth: straw.half.x,
        width,
        mark: 3,
        ..Spot::default()
    })
}

/// Close up each stalk is its own shade; far off they settle to one, about
/// the same mean.
#[test]
fn a_bales_stalks_settle_to_their_mean_far_off() {
    let bale = straw(Bound::Oblong { twines: 2 }, Vec3::new(0.45, 0.18, 0.23));
    let places: Vec<(f64, f64)> = (0..400)
        .map(|step| (0.2 + 0.0004 * f64::from(step), 0.003 * f64::from(step % 97)))
        .collect();
    let spread = |width: f64| {
        let shades: Vec<f64> = places
            .iter()
            .map(|&uv| at(&bale, 2.0, uv, width).luminance())
            .collect();
        let mean = shades.iter().sum::<f64>() / real(shades.len());
        let deviation = shades
            .iter()
            .map(|shade| (shade - mean).abs())
            .fold(0.0, f64::max);
        (mean, deviation)
    };
    let (near, near_spread) = spread(1e-4);
    let (far, far_spread) = spread(0.5);
    assert!(
        near_spread > 4.0 * far_spread.max(1e-9),
        "{near_spread} near, {far_spread} far"
    );
    assert!(
        (near - far).abs() < 0.08 * far,
        "{near} near against {far} far"
    );
}

/// An oblong bale's two strings loop round it lengthwise, two fifths of its
/// half width either side of its middle: along its top and over its ends,
/// never across its sides.
#[test]
fn a_bales_twine_loops_round_it_lengthwise() {
    let half = Vec3::new(0.45, 0.18, 0.23);
    let bale = straw(Bound::Oblong { twines: 2 }, half);
    let blue = |colour: Vec3| colour.z > 2.0 * colour.x;
    assert!(blue(at(&bale, 1.0, (0.1, -0.4), 1e-4)), "along its top");
    assert!(
        blue(at(&bale, 1.0, (-0.7, 0.4), 1e-4)),
        "the length of its top"
    );
    assert!(blue(at(&bale, 0.0, (0.4, 0.2), 1e-4)), "over its end");
    assert!(!blue(at(&bale, 1.0, (0.1, 0.0), 1e-4)), "nowhere between");
    for step in 0..40 {
        let up = -0.9 + 0.045 * f64::from(step);
        assert!(
            !blue(at(&bale, 2.0, (0.1, up), 1e-4)),
            "across its side at {up}"
        );
    }
}

/// A sheaf's head is its ears, and a band ties its waist.
#[test]
fn a_sheaf_stands_to_its_ears() {
    let ears = Vec3::new(0.3, 0.9, 0.1);
    let sheaf = straw(Bound::Sheaf { ears }, Vec3::new(0.11, 0.5, 0.11));
    let headed = round_at(&sheaf, (0.45, 0.9), 1e-4);
    let foot = round_at(&sheaf, (-0.3, 0.9), 1e-4);
    assert!(headed.y > 2.0 * headed.x, "{headed:?}");
    assert!(foot.x > foot.y * 1.1, "{foot:?}");
    let waist = round_at(&sheaf, (0.025, 0.9), 1e-4);
    assert!(waist.z > 2.0 * waist.x, "{waist:?}");
    let top = at(&sheaf, 1.0, (0.2, -0.3), 1e-4);
    assert!(top.y > 2.0 * top.x, "its top is its ears: {top:?}");
}

/// The `index`th of a test's draws under `salt`, `0.0..1.0`.
fn draw(index: u32, salt: u32) -> f64 {
    unit(mix32(index.wrapping_mul(0x9e37_79b9) ^ salt))
}

/// The `index`th of the places about a unit's faces and arrises, `half` its
/// size each way, from deep in its seams out past its stalks' reach: round
/// about its `y` where `round`, its side as narrow as a turned unit's head.
fn about(index: u32, half: Vec3, round: bool) -> Vec3 {
    let depth = BRISTLE * (-1.9 + 2.9 * draw(index, 3));
    let side = if draw(index, 2) < 0.5 { -1.0 } else { 1.0 };
    let (a, b) = (2.0 * draw(index, 4) - 1.0, 2.0 * draw(index, 5) - 1.0);
    let pick = draw(index, 1);
    if round {
        let turn = TAU * draw(index, 6);
        let (sin, cos) = (mathf::sin(turn), mathf::cos(turn));
        if pick < 0.4 {
            let radius = half.x * mathf::sqrt(draw(index, 7));
            return Vec3::new(radius * cos, side * (half.y + depth), radius * sin);
        }
        let radius = half.x * (0.6 + 0.4 * draw(index, 7)) + depth;
        return Vec3::new(radius * cos, a * half.y, radius * sin);
    }
    let out = |extent: f64| side * (extent + depth);
    if pick < 1.0 / 3.0 {
        Vec3::new(out(half.x), a * half.y, b * half.z)
    } else if pick < 2.0 / 3.0 {
        Vec3::new(a * half.x, out(half.y), b * half.z)
    } else {
        Vec3::new(a * half.x, b * half.y, out(half.z))
    }
}

/// However a unit is bound, and however much of its relief shows, its
/// stalks never stand past their reach, and about every face and arris they
/// rise no faster than the steepest the march is told of.
#[test]
fn straws_relief_rises_no_faster_than_its_steepest() {
    let units = [
        (Bound::Oblong { twines: 2 }, Vec3::new(0.45, 0.18, 0.23)),
        (Bound::Oblong { twines: 5 }, Vec3::new(1.2, 0.45, 0.6)),
        (Bound::Rolled, Vec3::new(0.68, 0.6, 0.68)),
        (Bound::Sheaf { ears: LIGHT }, Vec3::new(0.11, 0.5, 0.11)),
        (Bound::Cock, Vec3::new(0.75, 0.6, 0.08)),
    ];
    let showing = [
        Shows::ALL,
        Shows {
            stalks: 0.0,
            bundles: 1.0,
            clumps: 1.0,
        },
        Shows {
            stalks: 0.0,
            bundles: 0.0,
            clumps: 0.6,
        },
    ];
    let step = 1e-6;
    for (bound, half) in units {
        let unit = straw(bound, half);
        let round = !matches!(bound, Bound::Oblong { .. });
        for shows in showing {
            let steepest = unit.steepest(shows);
            let mut fastest = 0.0f64;
            for index in 0..6000 {
                let q = about(index, half, round);
                let way = Vec3::new(
                    draw(index, 8) - 0.5,
                    draw(index, 9) - 0.5,
                    draw(index, 10) - 0.5,
                )
                .normalized();
                let proud = unit.proud(q, half, shows);
                assert!(proud <= BRISTLE, "{bound:?} stands {proud} proud at {q:?}");
                let rise = (unit.proud(q + way * step, half, shows)
                    - unit.proud(q - way * step, half, shows))
                    / (2.0 * step);
                fastest = fastest.max(rise.abs());
            }
            assert!(
                fastest <= steepest,
                "{bound:?} showing {shows:?} rises {fastest}, past its steepest {steepest}"
            );
        }
    }
}

/// A round bale's ends show its rolled layers, darker at each seam; its side
/// its net.
#[test]
fn a_round_bale_shows_its_rolled_layers_and_its_net() {
    let half = Vec3::new(0.68, 0.6, 0.68);
    let bale = straw(Bound::Rolled, half);
    let shades: Vec<f64> = (0..200)
        .map(|step| at(&bale, 1.0, (0.02 + 0.004 * f64::from(step), 0.0), 1e-4).luminance())
        .collect();
    let (darkest, lightest) = shades
        .iter()
        .fold((f64::INFINITY, 0.0f64), |(low, high), &shade| {
            (low.min(shade), high.max(shade))
        });
    assert!(darkest < 0.75 * lightest, "{darkest} against {lightest}");
    let netted = (0..400)
        .filter(|&step| {
            let colour = round_at(&bale, (0.06, 0.003 * PI * f64::from(step)), 1e-4);
            colour.z > colour.x
        })
        .count();
    assert!((4..200).contains(&netted), "{netted} of 400 on the net");
}

/// A round bale's end is as dark on the whole far off as its seams make it
/// near, and its seams part only clear of its tightly rolled core.
#[test]
fn a_round_bales_end_keeps_its_shade_far_off() {
    let bale = straw(Bound::Rolled, Vec3::new(0.68, 0.6, 0.68));
    let mean = |width: f64| {
        let shades = (0..4000).map(|index| {
            let (radius, turn) = (
                0.6 * mathf::sqrt(draw(index, 13)) + 0.08,
                TAU * draw(index, 14),
            );
            bale.layered(
                bale.lying(
                    1.0,
                    (
                        radius * mathf::cos(turn) / 0.68,
                        radius * mathf::sin(turn) / 0.68,
                    ),
                    0.0,
                ),
                width,
            )
        });
        shades.sum::<f64>() / 4000.0
    };
    let (near, far) = (mean(1e-4), mean(0.5));
    assert!(
        (near - far).abs() < 0.04 * far,
        "{near} near against {far} far"
    );
    let core = bale.lying(1.0, (0.2 * CORE / 0.68, 0.0), 0.0);
    assert!(bale.rolled(core).0 <= 0.0, "a seam parts its core");
}

/// A unit, the face of it a test reads, and where on that face its `index`th
/// place lies.
type Lying<'a> = (&'a Straw, f64, fn(u32) -> (f64, f64));

/// The crests and gaps stalks lie in stand on the whole where their shades
/// are centred: laid straight and crossed, laid round, as cut ends and
/// heaped.
#[test]
fn straws_crests_and_gaps_stand_at_their_means() {
    fn share(index: u32, salt: u32) -> f64 {
        2.0 * draw(index, salt) - 1.0
    }
    let bale = straw(Bound::Oblong { twines: 2 }, Vec3::new(0.45, 0.18, 0.23));
    let rolled = straw(Bound::Rolled, Vec3::new(0.68, 0.6, 0.68));
    let cock = straw(Bound::Cock, Vec3::new(0.75, 0.6, 0.08));
    let lyings: [Lying<'_>; 4] = [
        (&bale, 2.0, |index| (share(index, 11), share(index, 12))),
        (&bale, 0.0, |index| (share(index, 11), share(index, 12))),
        (&rolled, 2.0, |index| {
            (0.6 * share(index, 11), PI * share(index, 12))
        }),
        (&cock, 2.0, |index| {
            (0.6 * share(index, 11), PI * share(index, 12))
        }),
    ];
    let count = 40_000u32;
    for (unit, face, place) in lyings {
        let (mut crest, mut gap, mut means) = (0.0, 0.0, (0.0, 0.0));
        for index in 0..count {
            let fibres = unit.fibres(unit.lying(face, place(index), unit.half.x), Shows::ALL);
            crest += fibres.crest;
            gap += fibres.gap;
            means = fibres.means;
        }
        let (crest, gap) = (crest / f64::from(count), gap / f64::from(count));
        assert!(
            (crest - means.0).abs() < 0.02,
            "{:?} face {face}: its crests stand at {crest}, not {}",
            unit.bound,
            means.0
        );
        assert!(
            (gap - means.1).abs() < 0.02,
            "{:?} face {face}: its gaps part {gap}, not {}",
            unit.bound,
            means.1
        );
    }
}

/// A feature too fine for its footprint is never looked up, reading nought,
/// and the shade comes out just as looking it up and fading it away would
/// have left it.
#[test]
fn straws_shade_looks_up_only_what_shows() {
    let bale = straw(Bound::Oblong { twines: 2 }, Vec3::new(0.45, 0.18, 0.23));
    let rolled = straw(Bound::Rolled, Vec3::new(0.68, 0.6, 0.68));
    let cock = straw(Bound::Cock, Vec3::new(0.75, 0.6, 0.08));
    for (unit, face) in [
        (&bale, 2.0),
        (&bale, 0.0),
        (&rolled, 1.0),
        (&rolled, 2.0),
        (&cock, 2.0),
    ] {
        for index in 0..2000 {
            let uv = (2.0 * draw(index, 21) - 1.0, 2.0 * draw(index, 22) - 1.0);
            let lying = unit.lying(face, uv, unit.half.x);
            let all = unit.fibres(lying, Shows::ALL);
            let (crest, gap) = all.means;
            for width in [1e-4, 2e-3, 4e-3, 9e-3, 0.02, 0.06, 0.3] {
                let faded = STALKS_MEAN
                    * (1.0
                        + fade(0.22 * (all.crest - crest), width / STALK)
                        + fade(0.3 * all.bundle - 0.45 * (all.gap - gap), width / BROAD)
                        + fade(0.18 * all.clump, width / CLUMP.0));
                let shade = unit.shade(lying, width);
                assert!(
                    (shade - faded).abs() < 1e-12,
                    "{:?} face {face} at {uv:?}, {width} across: {shade}, not {faded}",
                    unit.bound
                );
            }
        }
    }
    let none = bale.fibres(bale.lying(2.0, (0.3, 0.4), 0.0), Shows::NONE);
    assert_eq!(
        (none.crest, none.bundle, none.clump, none.gap),
        (0.0, 0.0, 0.0, 0.0)
    );
}

/// The stalks laid askew turn half a radian from the rest.
#[test]
fn askew_stalks_turn_half_a_radian() {
    assert!((ASKEW.0 - mathf::cos(0.5)).abs() < 1e-15, "{ASKEW:?}");
    assert!((ASKEW.1 - mathf::sin(0.5)).abs() < 1e-15, "{ASKEW:?}");
}
