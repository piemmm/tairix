//! Host tests of the sky and the sun: the night, the disc's shading, its bands
//! sinking and thickening, and the repaint that keeps to the disc.

use tairix_util::mathf;
use tairix_wm::{Color, Scale, Surface};

use super::{disc_colour, Sky, BANDS_FROM, BAND_PERIOD};
use crate::saver::retro_games::mountains::Mountains;
use crate::saver::retro_games::{Moment, View};

const SCREEN: (u32, u32) = (1920, 1080);

fn sky() -> (View, Sky) {
    let view = View::new(SCREEN, Scale::ONE).expect("a view");
    let mountains = Mountains::new(&view, 11).expect("ranges");
    (view, Sky::new(view, &mountains).expect("a sky"))
}

fn at(time: f64) -> Moment {
    Moment {
        time,
        ..Moment::default()
    }
}

#[test]
fn the_night_deepens_upward_and_the_disc_is_gold_above_and_ember_below() {
    let (view, sky) = sky();
    let (top, low) = (sky.night(0), sky.night(view.horizon - 1));
    assert!(low.b > top.b && low.g > top.g, "{top:?} {low:?}");
    let (centre, radius) = view.sun;
    let (crown, foot) = (
        disc_colour(&view, centre - radius),
        disc_colour(&view, f64::from(view.horizon)),
    );
    assert!(
        crown.g > foot.g + 40.0,
        "gold above ember: {crown:?} {foot:?}"
    );
    assert!(crown.r > 200.0 && foot.r > 200.0);
}

/// Rows `rows` of the disc as dark as a band makes them: how far each falls
/// short of the disc's own light there.
fn darkening(
    sky: &Sky,
    view: &View,
    rows: core::ops::Range<u32>,
    time: f64,
) -> alloc::vec::Vec<f64> {
    rows.map(|y| {
        let lit = disc_colour(view, f64::from(y) + 0.5);
        lit.g - sky.disc(y, time / BAND_PERIOD).g
    })
    .collect()
}

/// The bands sink towards the horizon as time passes, and the lower a band
/// the thicker.
#[test]
fn the_bands_sink_towards_the_horizon_and_thicken_as_they_go() {
    let (view, sky) = sky();
    let (centre, radius) = view.sun;
    let from = u32::try_from(mathf::round_i32(mathf::ceil(centre - BANDS_FROM * radius)))
        .expect("on screen");
    let rows = from..view.horizon;
    let still = darkening(&sky, &view, rows.clone(), 0.0);
    assert!(still.iter().any(|dark| *dark > 5.0), "banded");
    let above = u32::try_from(mathf::round_i32(centre - radius)).expect("on screen");
    assert_eq!(
        sky.disc(above, 0.0),
        disc_colour(&view, f64::from(above) + 0.5),
        "not above"
    );
    // A band in the middle of the banded part, followed by where its darkness
    // centres within a window about it, moves down as the bands sink.
    let runs: alloc::vec::Vec<(usize, usize)> = {
        let mut runs = alloc::vec::Vec::new();
        let mut at = 0;
        while at < still.len() {
            if still[at] > 0.5 {
                let start = at;
                while at < still.len() && still[at] > 0.5 {
                    at += 1;
                }
                runs.push((start, at));
            }
            at += 1;
        }
        runs
    };
    let (start, end) = runs[runs.len() / 2];
    let window = start.saturating_sub(3)..(end + 3).min(still.len());
    let centred = |time: f64| {
        let dark = darkening(&sky, &view, rows.clone(), time);
        let (weighted, total) = window.clone().fold((0.0, 0.0), |(weighted, total), at| {
            (weighted + dark[at] * index_f64(at), total + dark[at])
        });
        weighted / total
    };
    let (now, later) = (centred(0.0), centred(BAND_PERIOD / 16.0));
    assert!(later > now, "sank: {now} -> {later}");
    // Bands thicken downward: the lowest whole band is wider than the
    // highest, the horizon cutting the last short.
    let widths: alloc::vec::Vec<usize> = still
        .split(|dark| *dark <= 0.5)
        .map(<[f64]>::len)
        .filter(|width| *width > 0)
        .collect();
    assert!(widths.len() >= 4, "{widths:?}");
    assert!(widths[widths.len() - 2] > widths[1], "{widths:?}");
}

/// Repainting the bands writes only the disc's own columns of the banded part.
#[test]
fn repainting_the_bands_writes_the_discs_columns_alone() {
    let (view, sky) = sky();
    let mut surface = Surface::new(SCREEN.0, SCREEN.1).expect("a surface");
    surface.fill(Color::rgb(1, 2, 3));
    sky.paint_bands(&mut surface, at(3.0));
    let (centre, radius) = view.sun;
    let zone = sky.zone();
    for y in 0..SCREEN.1 {
        for x in 0..SCREEN.0 {
            let pixel = surface.get(x, y).expect("in bounds");
            let written = (pixel.r, pixel.g, pixel.b) != (1, 2, 3);
            let inside = zone.contains(tairix_wm::Point::new(
                i32::try_from(x).expect("small"),
                i32::try_from(y).expect("small"),
            ));
            let apart = mathf::hypot(
                f64::from(x) + 0.5 - view.centre,
                f64::from(y) + 0.5 - centre,
            );
            if !inside || apart > radius + 2.5 {
                assert!(!written, "({x}, {y}) was written");
            }
            if inside && apart < radius - 1.0 {
                assert!(written, "({x}, {y}) of the disc was not repainted");
            }
        }
    }
}

fn index_f64(at: usize) -> f64 {
    f64::from(u32::try_from(at).expect("small"))
}
