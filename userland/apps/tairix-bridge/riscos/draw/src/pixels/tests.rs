extern crate std;

use alloc::vec::Vec;
use std::sync::LazyLock;

use tairix_drawing::riscos::units::DRAW_UNIT_SHIFT;
use tairix_riscos_filing::session::{modules_with, session, Setup, SLOT_BYTES};
use tairix_riscos_kernel::module::Builtin;

use super::*;

static MODULES: LazyLock<Vec<&'static Builtin>> =
    LazyLock::new(|| modules_with(&[&tairix_riscos_vdu::MODULES, &crate::MODULES]));

/// The spans the path through `points`, in pixels from the screen's bottom
/// left, fills.
fn filled(points: &[(i32, i32)]) -> Vec<Span> {
    let mut s = session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: MODULES.as_slice(),
        bytes: SLOT_BYTES,
        keys: &[],
        disc: None,
    });
    let mut os = s.kernel.os();
    let surface = render::surface(&mut os).expect("a graphics mode");
    let unit = |(x, y): (i32, i32)| {
        Point::new(
            x << (DRAW_UNIT_SHIFT + surface.eig.0),
            y << (DRAW_UNIT_SHIFT + surface.eig.1),
        )
    };
    let mut path = Vec::new();
    let (first, rest) = points.split_first().expect("a point");
    path.push(Element::Move {
        to: unit(*first),
        winds: true,
    });
    path.extend(rest.iter().map(|&p| Element::Line { to: unit(p) }));
    let mut spans = Vec::new();
    fill_interior(&mut os, &path, &surface, &mut |_, plotted| {
        spans.extend_from_slice(plotted);
        Ok(())
    })
    .expect("filled");
    spans.sort_unstable();
    spans
}

#[test]
fn a_rectangle_on_pixel_edges_fills_exactly_its_pixels_as_a_big_glyph_did() {
    let spans = filled(&[(125, 200), (135, 200), (135, 220), (125, 220)]);
    let expected: Vec<Span> = (200..220).map(|row| (row, 125, 134)).collect();
    assert_eq!(spans, expected);
}

#[test]
fn a_slope_through_pixel_centres_keeps_them_as_a_big_glyph_s_did() {
    let spans = filled(&[(100, 100), (118, 100), (100, 118)]);
    let expected: Vec<Span> = (0..18).map(|j| (100 + j, 100, 117 - j)).collect();
    assert_eq!(
        spans, expected,
        "each row's last pixel centre lies on the slope"
    );
}
