use alloc::vec::Vec;

use super::*;
use crate::heightfield::CHANNELS;
use crate::pigment::Spot;

fn timber(weathering: f64, paint: Option<(Vec3, f64)>) -> Timber {
    Timber {
        bases: [Vec3::new(0.5, 0.36, 0.22), Vec3::new(0.62, 0.46, 0.3)],
        weathering,
        damp: 0.5,
        foot: 0.0,
        paint,
        seed: 7,
    }
}

fn spot(mark: u32, face: f64, (along, across): (f64, f64), height: f64) -> Spot {
    Spot {
        p: Vec3::new(along, height, across),
        normal: Vec3::new(0.0, 0.0, 1.0),
        width: 0.001,
        height,
        mark,
        along: face,
        uv: (along, across),
        girth: 0.0,
        instance: 0,
        front: true,
        ground: [0.0; CHANNELS],
        thatch: 0.0,
        cover: None,
    }
}

fn mean(timber: &Timber, face: f64) -> Vec3 {
    let mut sum = Vec3::ZERO;
    for mark in 0..200u32 {
        sum = sum + timber.colour(&spot(mark, face, (0.3, 0.1), 1.0));
    }
    sum * (1.0 / 200.0)
}

#[test]
fn timber_silvers_as_it_weathers() {
    let (fresh, old) = (mean(&timber(0.0, None), 2.0), mean(&timber(1.0, None), 2.0));
    let warmth = |colour: Vec3| colour.x - colour.z;
    assert!(warmth(old) < 0.5 * warmth(fresh), "{fresh:?} against {old:?}");
}

#[test]
fn every_board_is_its_own_shade() {
    let fresh = timber(0.2, None);
    let shades: Vec<f64> = (0..50u32)
        .map(|mark| fresh.colour(&spot(mark, 2.0, (0.0, 0.0), 1.0)).luminance())
        .collect();
    let (low, high) = shades.iter().fold((f64::MAX, f64::MIN), |(low, high), &v| (low.min(v), high.max(v)));
    assert!(high - low > 0.04, "{low}..{high}");
    assert_eq!(fresh.colour(&spot(9, 2.0, (0.2, 0.3), 1.0)), fresh.colour(&spot(9, 2.0, (0.2, 0.3), 1.0)));
}

#[test]
fn paint_flakes_away_the_more_it_has_weathered() {
    let white = Vec3::splat(0.85);
    let held = |flaked: f64| {
        let painted = timber(0.6, Some((white, flaked)));
        (0..400)
            .filter(|&index| {
                let place = (f64::from(index % 20) / 20.0, f64::from(index / 20) / 20.0);
                painted.colour(&spot(3, 2.0, place, 1.0)).luminance() > 0.5
            })
            .count()
    };
    assert!(held(0.2) > held(0.8) + 80, "{} against {}", held(0.2), held(0.8));
}

#[test]
fn damp_timber_greens_at_its_foot() {
    let old = timber(1.0, None);
    let green = |height: f64| {
        let colour = old.colour(&spot(5, 2.0, (0.0, 0.0), height));
        colour.y - 0.5 * (colour.x + colour.z)
    };
    assert!(green(-0.2) > green(2.0));
}
