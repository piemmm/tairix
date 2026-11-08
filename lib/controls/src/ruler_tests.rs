//! The ruler: its step, how it spells figures, and where it puts them.

use alloc::vec;
use alloc::vec::Vec;

use tairix_geometry::{Rect, Scale};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;

use super::{spelt, Ruler, RulerAxis, RulerStep};

fn ruler(per_unit: f64, zero: f64) -> Ruler {
    Ruler::new(per_unit, zero).expect("a ruler")
}

#[test]
fn the_step_is_the_least_one_two_or_five_that_reads() {
    let step = |per_unit| ruler(per_unit, 0.0).step(60.0);
    assert_eq!(
        step(1.0),
        RulerStep {
            step: 100.0,
            decimals: 0
        }
    );
    assert_eq!(
        step(2.0),
        RulerStep {
            step: 50.0,
            decimals: 0
        }
    );
    assert_eq!(
        step(4.0),
        RulerStep {
            step: 20.0,
            decimals: 0
        }
    );
    assert_eq!(
        step(1000.0),
        RulerStep {
            step: 0.1,
            decimals: 1
        }
    );
    // A step finer than a hundredth spells its figures to its own place,
    // where two places would read 0.01 for both 0.005 and 0.01.
    let fine = step(20_000.0);
    assert_eq!(fine.decimals, 3);
    assert!((fine.step - 0.005).abs() < 1e-12, "{fine:?}");
    assert_ne!(
        spelt(fine.step, fine.decimals),
        spelt(2.0 * fine.step, fine.decimals)
    );
    for per_unit in [0.001, 0.37, 1.0, 9.6, 96.0, 24_576.0] {
        let RulerStep { step, .. } = ruler(per_unit, 0.0).step(60.0);
        assert!(step * per_unit >= 60.0 - 1e-9, "{per_unit}: {step}");
        assert!(
            step * per_unit / 2.5 < 60.0,
            "the least that reads, {per_unit}: {step}"
        );
    }
    assert!(Ruler::new(0.0, 0.0).is_none());
    assert!(Ruler::new(f64::NAN, 0.0).is_none());
    assert!(Ruler::new(1.0, f64::INFINITY).is_none());
}

#[test]
fn figures_are_spelt_to_the_steps_place_unsigned_at_zero() {
    assert_eq!(spelt(0.015, 3).as_str(), "0.015");
    assert_eq!(spelt(0.01, 3).as_str(), "0.01");
    assert_eq!(spelt(2.0, 1).as_str(), "2");
    assert_eq!(spelt(-1.5, 1).as_str(), "-1.5");
    assert_eq!(spelt(-0.0, 0).as_str(), "0");
    assert_eq!(spelt(250.0, 0).as_str(), "250");
    assert_eq!(spelt(1.0e15, 0).as_str(), "1000000000000000");
}

/// Columns of the ruler's bottom row that hold its tick colour.
fn ticks(surface: &Surface, theme: &Theme, row: u32) -> Vec<u32> {
    let ink = Color::from(theme.palette().on_surface_muted).premultiply();
    (0..surface.width())
        .filter(|&x| surface.get(x, row) == Some(ink))
        .collect()
}

#[test]
fn a_tick_stands_at_every_step_from_the_views_zero() {
    let theme = Theme::light();
    let mut surface = Surface::new(300, 18).expect("a ruler");
    let bounds = Rect::new(0, 0, 300, 18);
    ruler(1.0, 10.0).paint(
        &mut surface,
        bounds,
        RulerAxis::Across,
        (Scale::ONE, &theme),
    );
    assert_eq!(ticks(&surface, &theme, 17), vec![10, 110, 210]);
    let plate = Color::from(theme.palette().surface).premultiply();
    assert_eq!(surface.get(109, 17), Some(plate), "the plate between them");
}

#[test]
fn a_ruler_scrolled_far_out_draws_only_what_its_length_holds() {
    let theme = Theme::light();
    let mut surface = Surface::new(300, 18).expect("a ruler");
    let bounds = Rect::new(0, 0, 300, 18);
    ruler(1.0, -1.0e12).paint(
        &mut surface,
        bounds,
        RulerAxis::Across,
        (Scale::ONE, &theme),
    );
    let shown = ticks(&surface, &theme, 17);
    assert!(!shown.is_empty() && shown.len() <= 7, "{shown:?}");
    for pair in shown.windows(2) {
        assert_eq!(pair[1] - pair[0], 100, "a step apart");
    }
}

#[test]
fn a_ruler_down_the_side_ticks_across_it_and_keeps_its_figures_inside() {
    let theme = Theme::light();
    let mut surface = Surface::new(40, 300).expect("a view");
    let bounds = Rect::new(0, 0, 18, 300);
    let before = surface.clone();
    ruler(0.01, 5.0).paint(&mut surface, bounds, RulerAxis::Down, (Scale::ONE, &theme));
    let ink = Color::from(theme.palette().on_surface_muted).premultiply();
    // A tick crosses the whole ruler; a figure's strokes never do.
    let rows: Vec<u32> = (0..300)
        .filter(|&y| (0..18).all(|x| surface.get(x, y) == Some(ink)))
        .collect();
    assert_eq!(
        rows,
        vec![5, 105, 205],
        "a tick every 10000 units, at 0.01 a pixel"
    );
    let spilt = (0..300).any(|y| (18..40).any(|x| surface.get(x, y) != before.get(x, y)));
    assert!(!spilt, "a wide figure is cut at the ruler's edge");
}
