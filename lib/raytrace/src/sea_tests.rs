use alloc::vec::Vec;

use tairix_mix::{mix32, unit};
use tairix_util::mathf;

use super::*;

/// Places over a few kilometres about the origin.
fn places(count: u32, reach: f64) -> impl Iterator<Item = (f64, f64)> {
    (0..count).map(move |index| {
        let key = mix32(index ^ 0x5151);
        (
            reach * (2.0 * unit(mix32(key ^ 1)) - 1.0),
            reach * (2.0 * unit(mix32(key ^ 2)) - 1.0),
        )
    })
}

#[test]
fn a_sea_repeats_every_period_and_keeps_to_its_height() {
    let period = 1024.0;
    let sea = Sea::new(period, (50.0, 4.0, 2.0), (0.4, 0.6), 17);
    let (mut highest, mut sum, mut squares): (f64, f64, f64) = (0.0, 0.0, 0.0);
    let samples = 20_000u32;
    for (x, z) in places(samples, 600.0) {
        let here = sea.height(x, z);
        assert!((sea.height(x + period, z) - here).abs() < 1e-9);
        assert!((sea.height(x, z - 2.0 * period) - here).abs() < 1e-9);
        highest = highest.max(here.abs());
        sum += here;
        squares += here * here;
    }
    let mean = sum / f64::from(samples);
    let deviation = mathf::sqrt(squares / f64::from(samples) - mean * mean);
    // Four standard deviations are the significant height the sea was
    // asked for, and its highest crest no more than about that.
    assert!(
        (4.0 * deviation - 2.0).abs() < 0.25,
        "significant height {}",
        4.0 * deviation
    );
    assert!(highest < 2.0, "the highest crest {highest}");
}

#[test]
fn a_sea_runs_before_its_wind() {
    // Along the wind the swell rises and falls; across it, much less.
    let sea = Sea::new(1024.0, (60.0, 4.0, 2.0), (0.0, 0.1), 3);
    let range = |step: (f64, f64)| {
        let (mut low, mut high) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in 0..400 {
            let at = f64::from(i) * 0.5;
            let height = sea.height(step.0 * at, step.1 * at);
            low = low.min(height);
            high = high.max(height);
        }
        high - low
    };
    assert!(range((1.0, 0.0)) > 2.0 * range((0.0, 1.0)));
}

/// No swell is shorter than the shortest the sea was given, so a grid of a
/// quarter of that holds every one without aliasing; and the longest is the
/// swell asked for.
#[test]
fn a_seas_swells_keep_within_their_lengths() {
    let sea = Sea::new(1024.0, (60.0, 4.0, 2.0), (0.7, 0.5), 9);
    let lengths: Vec<f64> = sea
        .waves
        .iter()
        .map(|wave| core::f64::consts::TAU / mathf::hypot(wave.kx, wave.kz))
        .collect();
    // Snapped to the period, a length moves by less than a part in ten.
    assert!(
        lengths.iter().all(|length| *length > 4.0 * 0.9),
        "{lengths:?}"
    );
    assert!(
        lengths.iter().any(|length| *length > 60.0 * 0.9),
        "{lengths:?}"
    );
}
