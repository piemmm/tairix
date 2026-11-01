use std::path::Path;

use super::canvas::{circle, oriented, rgb, rounded, stroke, Canvas, Mask, Paint, Relief, SIDE};
use super::text::{Fonts, Typeface};
use super::{draw_all, draw_one, MASTERS};

fn workspace() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("xtask sits two levels below the workspace root")
}

fn fonts() -> Fonts {
    Fonts::load(workspace()).expect("the tree's faces")
}

/// The area `mask` covers, in units.
fn area(mask: &Mask) -> f64 {
    let pixels: f64 = mask.values().iter().map(|&v| f64::from(v)).sum();
    pixels / 16.0
}

#[test]
fn every_master_passes_the_builds_own_check_of_an_icon_master() {
    for master in draw_all(&fonts()).expect("drawn") {
        crate::commands::image_apps::verify_raster_master(&master.file(), &master.png)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            master.png.len() <= tairix_icon::MAX_ARTWORK_BYTES,
            "{}",
            master.file()
        );
    }
}

#[test]
fn a_master_is_the_same_bytes_every_time_it_is_drawn() {
    let fonts = fonts();
    for &(kind, draw) in &MASTERS[..3] {
        let first = draw_one(kind, draw, &fonts).expect("drawn");
        let second = draw_one(kind, draw, &fonts).expect("drawn");
        assert!(first.png == second.png, "{kind:?}");
    }
}

#[test]
fn each_icon_has_one_drawing() {
    for (at, &(kind, _)) in MASTERS.iter().enumerate() {
        assert!(
            MASTERS[at + 1..].iter().all(|&(other, _)| other != kind),
            "{kind:?} drawn twice"
        );
    }
}

#[test]
fn a_shape_covers_exactly_its_area() {
    let square = Mask::of(&[vec![
        (10.0, 10.0),
        (110.0, 10.0),
        (110.0, 60.5),
        (10.0, 60.5),
    ]]);
    assert!((area(&square) - 100.0 * 50.5).abs() < 0.01);
    let disc = Mask::of(&[circle(128.0, 128.0, 50.0)]);
    let exact = core::f64::consts::PI * 2500.0;
    assert!((area(&disc) - exact).abs() / exact < 0.001);
    let card = Mask::of(&[rounded(20.0, 20.0, 120.0, 80.0, 0.0)]);
    assert!((area(&card) - 6000.0).abs() < 0.01);
}

#[test]
fn the_pieces_of_a_stroke_add_rather_than_cancel() {
    let line = Mask::of(&stroke(
        &[(40.0, 128.0), (100.0, 128.0), (200.0, 128.0)],
        10.0,
    ));
    let expected = 160.0 * 10.0 + core::f64::consts::PI * 25.0;
    assert!(
        (area(&line) - expected).abs() / expected < 0.002,
        "{}",
        area(&line)
    );
    let backwards = vec![(10.0, 10.0), (10.0, 20.0), (20.0, 20.0), (20.0, 10.0)];
    let forwards = oriented(backwards.clone());
    assert_eq!(forwards.first(), backwards.last());
    assert_eq!(oriented(forwards.clone()), forwards);
}

#[test]
fn shapes_combine_as_sets() {
    let left = Mask::of(&[rounded(20.0, 20.0, 120.0, 120.0, 0.0)]);
    let right = Mask::of(&[rounded(70.0, 20.0, 170.0, 120.0, 0.0)]);
    assert!((area(&left.clone().union(&right)) - 15_000.0).abs() < 0.01);
    assert!((area(&left.clone().minus(&right)) - 5_000.0).abs() < 0.01);
    assert!((area(&left.within(&right)) - 5_000.0).abs() < 0.01);
}

#[test]
fn a_blur_spreads_a_shape_and_keeps_its_area() {
    let disc = Mask::of(&[circle(128.0, 128.0, 30.0)]);
    let soft = disc.blurred(4.0);
    assert!((area(&soft) - area(&disc)).abs() / area(&disc) < 0.001);
    let edge = 128 * 4 * SIDE + (128 + 30) * 4;
    assert!(soft.values()[edge] > 0.3 && soft.values()[edge] < 0.7);
    let moved = disc.shifted(10.0, -5.0);
    assert!((area(&moved) - area(&disc)).abs() < 0.01);
    assert!(moved.values()[(123 * 4) * SIDE + 138 * 4] > 0.99);
}

#[test]
fn a_canvas_reduces_to_the_coverage_and_colour_it_was_painted() {
    let mut canvas = Canvas::new();
    let half = Mask::of(&[rounded(0.0, 0.0, 256.0, 128.0, 0.0)]);
    canvas.paint(&half, Paint::Solid(rgb(0x33_66_99)), Relief::FLAT, 0.5);
    let master = canvas.master();
    assert_eq!(&master[..4], &[0x33, 0x66, 0x99, 128]);
    let below = 200 * 256 * 4;
    assert_eq!(&master[below..below + 4], &[0, 0, 0, 0]);
    let fade = Paint::vertical(rgb(0x00_00_00), rgb(0xff_ff_ff), 0.0, 256.0);
    let mut graded = Canvas::new();
    graded.paint(
        &Mask::of(&[rounded(0.0, 0.0, 256.0, 256.0, 0.0)]),
        fade,
        Relief::FLAT,
        1.0,
    );
    let shade = graded.master();
    assert!(shade[2] < 2 && shade[255 * 256 * 4 + 2] > 253);
}

#[test]
fn a_raised_surface_is_lit_from_above_and_shaded_below() {
    let mut canvas = Canvas::new();
    let card = Mask::of(&[rounded(60.0, 60.0, 196.0, 196.0, 8.0)]);
    canvas.paint(&card, Paint::Solid(rgb(0x80_80_80)), Relief::of(8.0), 1.0);
    let master = canvas.master();
    let at = |x: usize, y: usize| master[(y * 256 + x) * 4];
    assert!(at(128, 62) > at(128, 128), "the top edge catches the light");
    assert!(
        at(128, 194) < at(128, 128),
        "the bottom edge falls in shade"
    );
}

#[test]
fn lettering_is_set_centred_on_where_it_is_asked() {
    let set = fonts()
        .set(Typeface::Sans(800.0), "BASIC", 40.0, 1.0, (128.0, 100.0))
        .expect("set");
    let (left, top, right, bottom) = set.ink;
    assert!((f64::midpoint(left, right) - 128.0).abs() < 1e-9);
    assert!((f64::midpoint(top, bottom) - 100.0).abs() < 1e-9);
    assert!(right - left > 100.0 && bottom - top > 25.0);
    assert!(area(&set.mask) > 1000.0);
    assert!(fonts()
        .set(Typeface::Serif(700.0), "\u{0}", 40.0, 0.0, (128.0, 100.0))
        .is_err());
}
