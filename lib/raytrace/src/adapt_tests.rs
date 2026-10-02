//! Host tests of local adaptation: a frame one exposure holds left alone, a
//! window drawn down and a dark room lifted with no halo either side of the
//! edge between them, detail kept within each, the sun still blown out, and
//! the correction within its bounds and easing in.

use alloc::vec::Vec;

use super::*;
use crate::sample::{mix32, unit};
use crate::tone::filmic;

/// The measured points a test frame is spread over, as a widescreen
/// picture's are.
const POINTS: (usize, usize) = (128, 72);

/// The exposed luminance a test frame's key stands at.
const KEY: f64 = 0.17;

/// Four samples a point over the frame, each `stops` from the key that
/// `light` says its place has, and a quarter of a stop of texture either way.
fn frame(light: impl Fn(f64, f64) -> f64) -> Vec<Sample> {
    let mut samples = Vec::new();
    for index in 0..POINTS.0 * POINTS.1 * 4 {
        let point = index / 4;
        let key = mix32(u32::try_from(index).expect("few samples"));
        let across = (real(point % POINTS.0) + unit(key)) / real(POINTS.0);
        let down = (real(point / POINTS.0) + unit(mix32(key))) / real(POINTS.1);
        let texture = 0.5 * unit(mix32(key ^ 0x3c)) - 0.25;
        samples.push(Sample {
            across,
            down,
            stops: light(across, down) + texture,
        });
    }
    samples
}

/// The luminance `stops` from the key.
fn luminance(stops: f64) -> f64 {
    KEY * mathf::exp(stops * core::f64::consts::LN_2)
}

/// The correction, in stops, a sample `stops` from the key at `film` takes.
fn corrected(grid: &Adaptation, film: (f64, f64), stops: f64) -> f64 {
    mathf::ln(grid.factor(film, luminance(stops))) / core::f64::consts::LN_2
}

#[test]
fn a_frame_one_exposure_holds_keeps_no_grid() {
    let gentle = frame(|across, down| 1.2 * (across - 0.5) + 0.8 * (down - 0.5));
    let grid = Adaptation::measured(&gentle, KEY, POINTS).expect("a grid");
    assert!(!grid.corrects(), "within a stop and a half everywhere");
    for stops in [-3.0, 0.0, 2.0, 9.0] {
        assert!(
            (grid.factor((0.3, 0.6), luminance(stops)) - 1.0).abs() == 0.0,
            "nothing changed, even at {stops}"
        );
    }
}

#[test]
fn a_window_is_drawn_down_and_its_room_lifted_with_no_halo_at_its_edge() {
    // A dark room filling the frame's left, six stops under the key, and a
    // sunlit window filling its right, six over.
    let room = frame(|across, _| if across < 0.5 { -6.0 } else { 6.0 });
    let grid = Adaptation::measured(&room, KEY, POINTS).expect("a grid");
    assert!(grid.corrects());
    let dark = |across: f64| corrected(&grid, (across, 0.5), -6.0);
    let bright = |across: f64| corrected(&grid, (across, 0.5), 6.0);
    assert!(
        bright(0.8) < -1.5 && bright(0.8) >= -2.0,
        "drawn down: {}",
        bright(0.8)
    );
    assert!(dark(0.2) > 0.6 && dark(0.2) <= 1.0, "lifted: {}", dark(0.2));
    // Hard by the edge each side reads its own side alone: the room no
    // brighter for the window beside it, the window no darker for the room.
    for (near, far) in [(0.48, 0.1), (0.45, 0.25)] {
        assert!(
            (dark(near) - dark(far)).abs() < 0.05,
            "{near}: {} {}",
            dark(near),
            dark(far)
        );
    }
    for (near, far) in [(0.52, 0.9), (0.55, 0.75)] {
        assert!(
            (bright(near) - bright(far)).abs() < 0.05,
            "{near}: {} {}",
            bright(near),
            bright(far)
        );
    }
    // The window's view keeps its shades: a stop apart before, still near
    // enough a stop apart after.
    let apart =
        (6.5 + corrected(&grid, (0.8, 0.5), 6.5)) - (5.5 + corrected(&grid, (0.8, 0.5), 5.5));
    assert!(apart > 0.8, "its detail kept: {apart}");
    let shown = |stops: f64| filmic(luminance(stops + corrected(&grid, (0.8, 0.5), stops)));
    assert!(
        shown(6.5) < 0.99,
        "the view no longer white: {}",
        shown(6.5)
    );
    assert!(shown(6.5) - shown(5.5) > 0.03, "and its shades apart");
}

#[test]
fn the_sun_still_blows_out() {
    let sky = frame(|_, down| if down < 0.6 { 3.0 } else { -1.0 });
    let grid = Adaptation::measured(&sky, KEY, POINTS).expect("a grid");
    let sun = corrected(&grid, (0.5, 0.3), 16.0);
    assert!((-2.0..=0.0).contains(&sun), "{sun}");
    assert!(filmic(luminance(16.0 + sun)) > 0.999);
}

#[test]
fn the_correction_holds_its_bounds_and_eases_in_without_a_step() {
    let mut last = correction(-20.0);
    for step in -2000..=2000 {
        let base = f64::from(step) / 100.0;
        let now = correction(base);
        assert!((-MOST_DOWN..=MOST_UP).contains(&now), "{base}: {now}");
        if base.abs() <= HELD {
            assert!(now == 0.0, "{base}: {now}");
        }
        assert!(now <= last + 1e-12, "never rising with the base: {base}");
        assert!((now - last).abs() < 0.006, "no step at {base}");
        last = now;
    }
    // Half the excess at first, either way.
    let (over, under) = (correction(HELD + 2.0), correction(-HELD - 2.0));
    let eased = 2.0 - 0.5 * KNEE;
    assert!((over + MOST_DOWN * tanh(SHARE * eased / MOST_DOWN)).abs() < 1e-12);
    assert!((under - MOST_UP * tanh(SHARE * eased / MOST_UP)).abs() < 1e-12);
}
