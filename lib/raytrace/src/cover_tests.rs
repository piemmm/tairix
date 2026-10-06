//! Host tests of what grows on stone: moss taking what faces the sky,
//! spreading from the joints and lodging where it can, lichen's species
//! keeping to their stone and its old colonies crusts, a cover's depth within
//! its bounds, and what grows carried intact to its colour.

use alloc::vec::Vec;

use super::*;
use crate::sample::{mix32, unit};

fn cover(substrate: Substrate) -> Cover {
    Cover {
        moss: 0.9,
        lichen: 0.9,
        damp: 0.8,
        drought: 0.0,
        foot: -10.0,
        shade: Vec3::new(1.0, 0.0, 0.0),
        substrate,
        seed: 11,
    }
}

/// A point scattered over a metre-wide patch about the middle, keyed
/// `index`.
fn scattered(index: u32) -> Vec3 {
    let draw = |salt: u32| unit(mix32(index ^ salt));
    Vec3::new(draw(1) - 0.5, draw(2) - 0.5, draw(3) - 0.5) * 2.0
}

/// `p` on a face turned `normal` of a unit as welcoming to moss as
/// `affinity`, well away from its joints.
fn lodged(p: Vec3, normal: Vec3, affinity: f64) -> Lodging {
    Lodging {
        p,
        normal,
        affinity,
        joint: 1.0,
    }
}

fn mossed(cover: &Cover, normal: Vec3, affinity: f64) -> usize {
    (0..4000)
        .filter(|&index| {
            matches!(
                cover.growth(&lodged(scattered(index), normal, affinity)),
                Growth::Moss(_)
            )
        })
        .count()
}

#[test]
fn growth_survives_the_coordinates_it_is_carried_in() {
    let mut every = Vec::new();
    every.push(Growth::Bare);
    every.push(Growth::Moss(0.375));
    every.extend(
        Lichen::ALL
            .iter()
            .map(|&lichen| Growth::Lichen(lichen, 0.625)),
    );
    for growth in every {
        assert_eq!(Growth::of_carried(growth.carried()), growth);
    }
}

#[test]
fn moss_takes_the_tops_and_spares_the_undersides() {
    let cover = cover(Substrate::Calcareous);
    let (top, side, under) = (
        mossed(&cover, Vec3::UP, 0.5),
        mossed(&cover, Vec3::new(0.0, 0.0, 1.0), 0.5),
        mossed(&cover, -Vec3::UP, 0.5),
    );
    assert!(top > 4 * side.max(1), "{top} on a top, {side} on a side");
    assert_eq!(under, 0, "moss on an underside");
}

#[test]
fn moss_lodges_in_joints_before_dressed_faces() {
    let cover = cover(Substrate::Calcareous);
    let shaded = Vec3::new(1.0, 0.0, 0.0);
    let (joint, rough, dressed, trodden) = (
        mossed(&cover, shaded, 1.0),
        mossed(&cover, shaded, 0.65),
        mossed(&cover, shaded, 0.25),
        mossed(&cover, shaded, 0.0),
    );
    assert!(
        joint > rough && rough > dressed,
        "{joint}, {rough}, {dressed}"
    );
    assert_eq!(trodden, 0, "moss lodged where it is never let");
    // The side turned from the shade holds the fewest.
    let sunny = mossed(&cover, -shaded, 1.0);
    assert!(
        sunny < joint,
        "{sunny} on the sunny side, {joint} in the shade"
    );
}

#[test]
fn lichens_keep_to_their_stone_and_the_bright_ones_to_the_tops() {
    let species = |substrate, normal| {
        let cover = Cover {
            moss: 0.0,
            ..cover(substrate)
        };
        (0..6000)
            .filter_map(
                |index| match cover.growth(&lodged(scattered(index), normal, 0.5)) {
                    Growth::Lichen(lichen, inside) => {
                        assert!(inside > 0.0 && inside <= 1.0, "{inside}");
                        Some(lichen)
                    }
                    Growth::Bare => None,
                    Growth::Moss(_) => panic!("moss with none to grow"),
                },
            )
            .collect::<Vec<_>>()
    };
    let lime = species(Substrate::Calcareous, Vec3::UP);
    let acid = species(Substrate::Siliceous, Vec3::UP);
    assert!(lime.len() > 100 && acid.len() > 100);
    assert!(!lime
        .iter()
        .any(|&lichen| matches!(lichen, Lichen::Rhizocarpon | Lichen::White)));
    assert!(!acid
        .iter()
        .any(|&lichen| matches!(lichen, Lichen::Caloplaca | Lichen::Verrucaria)));
    let bright = |found: &[Lichen]| {
        let orange = found
            .iter()
            .filter(|&&lichen| matches!(lichen, Lichen::Xanthoria | Lichen::Caloplaca))
            .count();
        crate::vector::share(orange, found.len())
    };
    let side = species(Substrate::Calcareous, Vec3::new(0.0, 0.0, 1.0));
    assert!(
        bright(&lime) > 1.5 * bright(&side),
        "{} on tops, {} on sides",
        bright(&lime),
        bright(&side)
    );
}

#[test]
fn a_covers_depth_keeps_within_its_reach_and_its_slope() {
    let cover = cover(Substrate::Siliceous);
    let shown = Shown {
        cushions: 1.0,
        shoots: 1.0,
        crusts: 1.0,
    };
    let steepest = cover.steepest(shown);
    let step = 2e-5;
    for index in 0..20_000u32 {
        let p = scattered(index);
        let way = scattered(index ^ 0x5eed).normalized();
        let (here, there) = (
            cover.depth(&lodged(p, Vec3::UP, 1.0), shown),
            cover.depth(&lodged(p + way * step, Vec3::UP, 1.0), shown),
        );
        assert!((0.0..=DEEPEST).contains(&here), "{index}: {here}");
        // A step that crosses where moss takes or a colony begins may jump
        // by a crust; the rest rises no faster than the cover says.
        if (here > 0.0) == (there > 0.0) {
            assert!(
                (there - here).abs() <= steepest * step * 1.05 + 1e-9,
                "{index}: rose {} over {step}",
                there - here
            );
        }
    }
}

#[test]
fn nothing_grows_without_a_cover() {
    let bare = Cover {
        moss: 0.0,
        lichen: 0.0,
        ..cover(Substrate::Calcareous)
    };
    assert!((0..2000)
        .all(|index| bare.growth(&lodged(scattered(index), Vec3::UP, 1.0)) == Growth::Bare));
}

#[test]
fn moss_spreads_from_the_joints() {
    let cover = Cover {
        moss: 0.3,
        ..cover(Substrate::Calcareous)
    };
    let share = |joint: f64| {
        (0..6000)
            .filter(|&index| {
                let at = Lodging {
                    joint,
                    ..lodged(scattered(index), Vec3::UP, 0.45)
                };
                matches!(cover.growth(&at), Growth::Moss(_))
            })
            .count()
    };
    let (beside, away) = (share(0.01), share(0.5));
    assert!(
        beside > 3 * away / 2,
        "{beside} beside a joint, {away} away from one"
    );
}

#[test]
fn old_colonies_are_crusts() {
    for substrate in [Substrate::Calcareous, Substrate::Siliceous] {
        for key in 0..2000 {
            let lichen = Lichen::of(key, substrate, 1.0, true);
            assert!(
                !lichen.foliose() && !matches!(lichen, Lichen::Caloplaca),
                "{lichen:?} grown old"
            );
        }
    }
}
