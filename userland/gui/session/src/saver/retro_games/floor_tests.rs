//! Host tests of the floor: the exact swept coverage its converging lines are
//! drawn with, their mean where they crowd, the grid's flight towards the
//! viewer, the haze, and the reflection.

use tairix_util::mathf;
use tairix_wm::Scale;

use super::{contrast, swept_box, Crossing, Reflection, Row, CLEAR, FINEST, PIECE};
use crate::saver::retro_games::{Moment, View};

const SCREEN: (u32, u32) = (1280, 720);

fn view() -> View {
    View::new(SCREEN, Scale::ONE).expect("a view")
}

/// A band's coverage of a pixel by fine sampling of the pixel and the sweep.
fn sampled(from: f64, shear: f64, width: f64) -> f64 {
    let steps = 600u32;
    let mut lit = 0u32;
    for down in 0..steps {
        let centre = from + shear * (f64::from(down) + 0.5) / f64::from(steps);
        for across in 0..steps {
            let x = -0.5 + (f64::from(across) + 0.5) / f64::from(steps);
            if (x - centre).abs() < width / 2.0 {
                lit += 1;
            }
        }
    }
    f64::from(lit) / f64::from(steps * steps)
}

#[test]
fn a_swept_band_leaves_its_exact_area_in_a_pixel() {
    for (from, shear, width) in [
        (0.0, 0.0, 1.5),
        (0.3, 0.0, 0.4),
        (-2.0, 4.0, 1.2),
        (1.4, -3.0, 0.6),
        (0.1, 0.4, 2.5),
        (3.0, 1.0, 1.0),
    ] {
        let exact = swept_box(from, shear, width);
        assert!(
            (exact - sampled(from, shear, width)).abs() < 5e-3,
            "{from} {shear} {width}: {exact}"
        );
    }
}

/// The row and the lines crossing it `drop` pixels below the horizon.
fn row_at(drop: u32, moment: Moment) -> Row {
    let view = view();
    Row::new(&view, view.horizon + drop, moment)
}

/// The cores and glows the lines lend a whole row, gathered piece by piece as
/// the floor gathers them.
fn gathered(towards: &Crossing, width: u32) -> alloc::vec::Vec<(f64, f64)> {
    let mut row = alloc::vec::Vec::new();
    let mut start = 0;
    while start < width {
        let end = (start + u32::try_from(PIECE).expect("small")).min(width);
        let mut lines = [(0.0, 0.0); PIECE];
        for shape in towards.shapes(start..end) {
            towards.lend(&shape, &(start..end), &mut lines);
        }
        row.extend_from_slice(&lines[..usize::try_from(end - start).expect("small")]);
        start = end;
    }
    row
}

/// A line far out to the side is shallow enough to cross many columns within
/// one row: it is drawn as one unbroken stroke, and its core leaves the row
/// exactly the light of its area.
#[test]
fn a_shallow_line_is_an_unbroken_stroke_that_keeps_its_light() {
    let row = row_at(100, Moment::default());
    let towards = row.towards.expect("lines towards the horizon");
    let lines = gathered(&towards, SCREEN.0);
    let shape = towards.shape(-6, &(0..SCREEN.0));
    assert!(shape.shear.abs() > 5.0, "shallow: {}", shape.shear);
    assert!(shape.cored.start > 0, "wholly on the screen");
    let lit: alloc::vec::Vec<usize> = (shape.cored.start..shape.cored.end)
        .map(|x| usize::try_from(x).expect("small"))
        .filter(|x| lines[*x].0 > 1e-9)
        .collect();
    let (first, last) = (lit[0], lit[lit.len() - 1]);
    assert!(last - first > 5, "the stroke crosses many columns");
    assert_eq!(lit.len(), last - first + 1, "no column along it is dark");
    let light: f64 = lines[first..=last].iter().map(|lent| lent.0).sum();
    assert!(
        (light - shape.core).abs() < 1e-6,
        "{light} for a band {} wide",
        shape.core
    );
}

/// Where the lines crowd past drawing they are only their mean, and between
/// that and where they are drawn clear their contrast falls away smoothly.
#[test]
fn lines_crowding_towards_the_horizon_give_way_to_their_mean() {
    assert_eq!(contrast(FINEST).to_bits(), 0.0_f64.to_bits());
    assert_eq!(contrast(CLEAR).to_bits(), 1.0_f64.to_bits());
    let between = contrast(f64::midpoint(FINEST, CLEAR));
    assert!(between > 0.0 && between < 1.0, "{between}");
    let row = row_at(10, Moment::default());
    let towards = row.towards.expect("lines towards the horizon");
    assert!(
        towards.clear < towards.faint,
        "{} {}",
        towards.clear,
        towards.faint
    );
    let far =
        u32::try_from(mathf::round_i32(towards.centre + towards.faint + 4.0)).expect("on screen");
    assert!(far < SCREEN.0, "past drawing within the screen");
    let rel = f64::from(far) + 0.5 - towards.centre;
    let (_, core, glow) = towards.crowding(rel);
    assert_eq!(
        towards.faded(far, (0.0, 0.0)),
        (core, glow),
        "the mean alone"
    );
    assert_eq!(
        towards.faded(far, (1.0, 1.0)),
        (core, glow),
        "whatever was gathered"
    );
    let clear = u32::try_from(mathf::round_i32(towards.centre)).expect("on screen");
    assert_eq!(towards.faded(clear, (0.25, 0.5)), (0.25, 0.5), "kept clear");
    // Rows at the very horizon are the lines' mean alone, the same across them.
    assert!(row_at(0, Moment::default()).towards.is_none());
}

/// The lines across the floor move towards the viewer as the flight goes on,
/// and a cell flown is the grid as it was.
#[test]
fn the_grid_streams_towards_the_viewer() {
    // Where the one line crossing the rows nearest the viewer lies.
    let nearest = |flown: f64| {
        let moment = Moment {
            flown,
            ..Moment::default()
        };
        let rows = 200..SCREEN.1 - view().horizon;
        let (weighted, total) = rows.fold((0.0, 0.0), |(weighted, total), drop| {
            let across = row_at(drop, moment).light.b;
            (weighted + f64::from(drop) * across, total + across)
        });
        weighted / total
    };
    let (before, after) = (nearest(0.1), nearest(0.2));
    assert!(
        after > before + 1.0,
        "the line came nearer: {before} -> {after}"
    );
    let moment = |flown: f64| Moment {
        flown,
        ..Moment::default()
    };
    for drop in [20, 150, 290] {
        let (now, later) = (row_at(drop, moment(3.3)), row_at(drop, moment(4.3)));
        assert!((now.light.b - later.light.b).abs() < 1e-9, "{drop}");
    }
}

/// Between the lines across it, the floor brightens into the haze towards the
/// horizon.
#[test]
fn the_haze_brightens_the_floor_towards_the_horizon() {
    let darkest = |rows: core::ops::Range<u32>| {
        rows.map(|drop| {
            let light = row_at(drop, Moment::default()).light;
            light.r + light.g + light.b
        })
        .fold(f64::MAX, f64::min)
    };
    let (horizon, middle, near) = (darkest(0..4), darkest(40..90), darkest(200..300));
    assert!(
        horizon > middle && middle > near,
        "{horizon} {middle} {near}"
    );
}

/// The sun's reflection lies beneath it, narrow by the horizon and widening
/// towards the viewer, and every row of the floor holds some of it.
#[test]
fn the_reflection_lies_beneath_the_sun_and_widens_towards_the_viewer() {
    let view = view();
    let reflection = |drop: u32| {
        let row = row_at(drop, Moment::default());
        Reflection::new(&view, &row, Moment::default()).expect("a reflection")
    };
    let (near_horizon, below) = (reflection(2), reflection(250));
    assert!(
        near_horizon.half < below.half / 2.0,
        "{} {}",
        near_horizon.half,
        below.half
    );
    for drop in 0..(SCREEN.1 - view.horizon) {
        let at = reflection(drop);
        assert!((at.centre - view.centre).abs() < at.half, "{drop}");
        assert!(at.columns.end <= SCREEN.0 && at.columns.start < at.columns.end);
        let (sun_left, sun_right) = (view.centre - view.sun.1, view.centre + view.sun.1);
        assert!(f64::from(at.columns.start) >= sun_left - 2.0, "{drop}");
        assert!(f64::from(at.columns.end) <= sun_right + 2.0, "{drop}");
    }
}

/// The ripples break the reflection into lit and dark runs, and run on as the
/// flight does.
#[test]
fn the_ripples_break_the_reflection_and_run_with_the_flight() {
    let view = view();
    let strengths = |flown: f64| {
        let moment = Moment {
            flown,
            ..Moment::default()
        };
        let row = row_at(250, moment);
        let mut reflection = Reflection::new(&view, &row, moment).expect("a reflection");
        let shine = reflection.shine;
        let columns = reflection.columns.clone();
        let light = reflection.light;
        columns
            .map(|x| {
                let lent = reflection.next(x);
                lent.r / light.r / shine
            })
            .collect::<alloc::vec::Vec<f64>>()
    };
    let now = strengths(1.0);
    assert!(now.iter().any(|strength| *strength > 0.8), "lit");
    let dark = (0..40)
        .map(|step| strengths(1.0 + f64::from(step) * 0.037))
        .any(|row| {
            row[row.len() / 4..row.len() * 3 / 4]
                .iter()
                .any(|s| *s < 0.2)
        });
    assert!(
        dark,
        "broken somewhere within the column as the flight goes on"
    );
    assert!(now != strengths(1.3), "the ripples moved");
}
