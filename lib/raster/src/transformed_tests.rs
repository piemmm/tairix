//! The transformed blit: placement, sampling, edges, and clipping.

use crate::affine::Affine;
use crate::color::Color;
use crate::surface::Surface;

fn solid(width: u32, height: u32, color: Color) -> Surface {
    let mut surface = Surface::new(width, height).expect("a surface");
    surface.fill(color);
    surface
}

/// A translation by whole pixels lands every source pixel exactly where a
/// plain blit would.
#[test]
fn a_whole_pixel_translation_is_a_plain_blit() {
    let src = solid(4, 3, Color::rgb(200, 40, 10));
    let mut turned = Surface::new(10, 8).expect("a surface");
    let mut plain = Surface::new(10, 8).expect("a surface");
    turned.blit_transformed(&src, Affine::translate(3.0, 2.0));
    plain.blit(3, 2, &src);
    assert_eq!(turned.pixels(), plain.pixels());
}

/// A half-pixel offset blends each edge pixel half with the transparency
/// beside the source, while the interior stays the source colour.
#[test]
fn a_fractional_offset_blends_the_edges_against_transparency() {
    let src = solid(4, 4, Color::rgb(255, 255, 255));
    let mut surface = Surface::new(8, 8).expect("a surface");
    surface.blit_transformed(&src, Affine::translate(2.5, 2.0));
    let at = |x: u32, y: u32| surface.get(x, y).expect("on the surface");
    assert_eq!(at(2, 3).a, 128, "the leading edge is half covered");
    assert_eq!(at(3, 3), Color::rgb(255, 255, 255).premultiply());
    assert_eq!(at(6, 3).a, 128, "the trailing edge is half covered");
    assert_eq!(at(1, 3).a, 0);
    assert_eq!(at(7, 3).a, 0);
}

/// A turned picture keeps its colour in the middle, reaches past the box it
/// started in at its corners, and every pixel it writes stays premultiplied.
#[test]
fn a_turned_picture_keeps_its_middle_and_stays_premultiplied() {
    let src = solid(20, 20, Color::rgba(30, 90, 200, 255));
    let mut surface = Surface::new(40, 40).expect("a surface");
    let turn = Affine::rotate_degrees_about(10.0, 10.0, 20.0).then(Affine::translate(10.0, 10.0));
    surface.blit_transformed(&src, turn);
    assert_eq!(
        surface.get(20, 20),
        Some(Color::rgba(30, 90, 200, 255).premultiply())
    );
    assert!(
        surface
            .pixels()
            .iter()
            .all(|p| p.r <= p.a && p.g <= p.a && p.b <= p.a),
        "a channel outgrew its alpha"
    );
    let written = surface.pixels().iter().filter(|p| p.a != 0).count();
    assert!(written > 20 * 20, "the turn spreads the picture: {written}");
}

/// The blit honours the clip window, and a transform that collapses area, or
/// one that carries the picture off the surface, draws nothing.
#[test]
fn the_transformed_blit_is_clipped_and_refuses_a_collapsed_transform() {
    let src = solid(4, 4, Color::rgb(0, 0, 0));
    let mut surface = Surface::new(8, 8).expect("a surface");
    surface.with_clip(0, 0, 4, 8, |surface| {
        surface.blit_transformed(&src, Affine::translate(2.0, 2.0));
    });
    assert_eq!(surface.get(3, 3).map(|p| p.a), Some(255));
    assert_eq!(surface.get(4, 3).map(|p| p.a), Some(0), "past the clip");

    let mut untouched = Surface::new(8, 8).expect("a surface");
    untouched.blit_transformed(&src, Affine::scale(0.0, 1.0));
    untouched.blit_transformed(&src, Affine::translate(-100.0, 5000.0));
    assert!(untouched.pixels().iter().all(|p| p.a == 0));
}

/// A magnified edge keeps its whole blend ramp, which reaches half a source
/// pixel past the picture at the picture's own scale.
#[test]
fn a_magnified_edge_keeps_its_whole_ramp() {
    let src = solid(2, 2, Color::rgb(255, 255, 255));
    let mut surface = Surface::new(40, 40).expect("a surface");
    surface.blit_transformed(
        &src,
        Affine::scale(8.0, 8.0).then(Affine::translate(10.0, 10.0)),
    );
    let at = |x: u32| surface.get(x, 16).expect("on the surface").a;
    assert!(at(7) > 0, "the ramp reaches four pixels out");
    assert_eq!(at(5), 0, "and no further");
}

/// A transform that throws the picture far past the surface walks only the
/// rows the surface has, and the surface inside the picture is covered.
#[test]
fn a_huge_transform_walks_only_the_surface() {
    let src = solid(2, 2, Color::rgb(255, 255, 255));
    let mut surface = Surface::new(8, 8).expect("a surface");
    let inside = Affine::scale(1.0e9, 1.0e9).then(Affine::translate(-1.0e9, -1.0e9));
    surface.blit_transformed(&src, inside);
    assert!(
        surface.pixels().iter().all(|pixel| pixel.a == 255),
        "the picture covers it"
    );
}
