//! The transformed blit: placement, sampling, edges, and clipping.

use alloc::vec;
use alloc::vec::Vec;

use super::{Mosaic, Picture};
use crate::color::{Color, Pixel};
use crate::surface::Surface;
use tairix_bezier::Affine;
use tairix_fuzzseed::Prng;

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
    turned.blit_transformed(&src, Affine::translate(3.0, 2.0), 255);
    plain.blit(3, 2, &src);
    assert_eq!(turned.pixels(), plain.pixels());
}

/// A half-pixel offset blends each edge pixel half with the transparency
/// beside the source, while the interior stays the source colour.
#[test]
fn a_fractional_offset_blends_the_edges_against_transparency() {
    let src = solid(4, 4, Color::rgb(255, 255, 255));
    let mut surface = Surface::new(8, 8).expect("a surface");
    surface.blit_transformed(&src, Affine::translate(2.5, 2.0), 255);
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
    surface.blit_transformed(&src, turn, 255);
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
        surface.blit_transformed(&src, Affine::translate(2.0, 2.0), 255);
    });
    assert_eq!(surface.get(3, 3).map(|p| p.a), Some(255));
    assert_eq!(surface.get(4, 3).map(|p| p.a), Some(0), "past the clip");

    let mut untouched = Surface::new(8, 8).expect("a surface");
    untouched.blit_transformed(&src, Affine::scale(0.0, 1.0), 255);
    untouched.blit_transformed(&src, Affine::translate(-100.0, 5000.0), 255);
    assert!(untouched.pixels().iter().all(|p| p.a == 0));
}

/// A strip of a larger drawing, held at an origin, takes the part of a
/// transformed picture that falls on it, not only the part within its own
/// width of the drawing's corner.
#[test]
fn a_strip_at_an_origin_takes_its_part_of_the_picture() {
    let src = solid(4, 4, Color::rgb(0, 0, 0));
    let mut strip = Surface::new(10, 10).expect("a surface");
    strip.with_origin(20, 30, |strip| {
        strip.blit_transformed(&src, Affine::translate(24.0, 32.0), 255);
    });
    assert_eq!(strip.get(5, 3).map(|p| p.a), Some(255), "drawn at (25, 33)");
    assert_eq!(strip.get(3, 3).map(|p| p.a), Some(0), "left of the picture");
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
        255,
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
    surface.blit_transformed(&src, inside, 255);
    assert!(
        surface.pixels().iter().all(|pixel| pixel.a == 255),
        "the picture covers it"
    );
}

/// A picture of varied, translucent pixels, so every sample's weights show.
fn varied(width: u32, height: u32, seed: u64) -> Surface {
    let mut rng = Prng::new(seed);
    let mut surface = Surface::new(width, height).expect("a surface");
    for y in 0..height {
        for x in 0..width {
            let [r, g, b, a] = rng.next_u32().to_le_bytes();
            surface.set(x, y, Color::rgba(r, g, b, a).premultiply());
        }
    }
    surface
}

/// A transform that only scales and moves samples exactly the pixels the
/// general path samples, mirrored or not, through a clip and at any offset.
#[test]
fn a_scaled_blit_samples_to_the_bit_as_the_general_path() {
    let src = varied(37, 23, 7);
    let mut rng = Prng::new(11);
    let draw = |rng: &mut Prng, below: u32| rng.next_u32() % below;
    let mut drawn = 0;
    for round in 0..64_u32 {
        let scale = |rng: &mut Prng| {
            let magnitude = f64::from(draw(rng, 4000) + 50) / 1000.0;
            if draw(rng, 5) == 0 {
                -magnitude
            } else {
                magnitude
            }
        };
        let (sx, sy) = (scale(&mut rng), scale(&mut rng));
        // A mirrored picture is moved back across, so most land in view.
        let across = |s: f64, extent: u32| if s < 0.0 { -s * f64::from(extent) } else { 0.0 };
        let (tx, ty) = (
            f64::from(draw(&mut rng, 6000)) / 100.0 - 10.0 + across(sx, 37),
            f64::from(draw(&mut rng, 6000)) / 100.0 - 10.0 + across(sy, 23),
        );
        let transform = Affine::scale(sx, sy).then(Affine::translate(tx, ty));
        let inverse = transform.invert().expect("invertible");
        let clip = (
            draw(&mut rng, 20),
            draw(&mut rng, 20),
            30 + draw(&mut rng, 300),
            30 + draw(&mut rng, 90),
        );
        let ground = varied(320, 100, u64::from(round));
        for edge in [super::Edge::Fading, super::Edge::Framed] {
            let (mut scaled, mut turned) = (ground.clone(), ground.clone());
            scaled.with_clip(clip.0, clip.1, clip.2, clip.3, |surface| {
                surface.blit_through(&src, transform, 255, edge);
            });
            turned.with_clip(clip.0, clip.1, clip.2, clip.3, |surface| {
                let Some((x0, y0, x1, y1)) = super::reach(src.size(), transform, edge) else {
                    return;
                };
                let frames = super::frames(src.size(), inverse, edge);
                if let Some((columns, rows)) = surface.admitted(x0, y0, x1 - x0, y1 - y0) {
                    surface.blit_turned(&src, (inverse, 255, frames), columns, rows);
                }
            });
            assert_eq!(
                scaled.pixels(),
                turned.pixels(),
                "round {round}, {edge:?}: {transform:?}"
            );
            drawn += usize::from(scaled.pixels() != ground.pixels());
        }
    }
    assert!(drawn >= 96, "only {drawn} of 128 drew anything");
}

/// A framed picture covers exactly its rectangle however far it is
/// magnified: whole to its edge, nothing past it, and an edge crossing a
/// pixel covering as much of it as it does.
#[test]
fn a_framed_picture_covers_exactly_its_rectangle() {
    let src = solid(2, 2, Color::rgb(255, 255, 255));
    let mut surface = Surface::new(40, 40).expect("a surface");
    surface.blit_framed(
        &src,
        Affine::scale(8.0, 8.0).then(Affine::translate(10.0, 10.5)),
        255,
    );
    let at = |x: u32, y: u32| surface.get(x, y).expect("on the surface").a;
    assert_eq!(at(10, 16), 255, "whole up to its edge");
    assert_eq!(at(25, 16), 255);
    assert_eq!(at(9, 16), 0, "nothing past it");
    assert_eq!(at(26, 16), 0);
    assert_eq!(at(16, 10), 128, "half the pixel its top edge crosses");
    assert_eq!(at(16, 26), 128, "and its foot");
    assert_eq!(
        at(10, 10),
        128,
        "a corner's share along the edge it crosses"
    );

    let mut turned = Surface::new(40, 40).expect("a surface");
    let turn = Affine::rotate_degrees_about(30.0, 1.0, 1.0)
        .then(Affine::scale(8.0, 8.0))
        .then(Affine::translate(12.0, 12.0));
    turned.blit_framed(&src, turn, 255);
    assert_eq!(turned.get(20, 20).map(|p| p.a), Some(255), "its middle");
    assert!(
        turned.pixels().iter().all(|p| p.r <= p.a),
        "every pixel stays premultiplied"
    );
}

/// Sampling a framed picture never reaches past its edge pixels, so a
/// magnified edge keeps its edge pixel's colour rather than fading it.
#[test]
fn a_framed_picture_is_sampled_within_its_edge_pixels() {
    let mut src = Surface::new(2, 1).expect("a surface");
    src.set(0, 0, Color::rgb(255, 0, 0).premultiply());
    src.set(1, 0, Color::rgb(0, 0, 255).premultiply());
    let mut surface = Surface::new(40, 10).expect("a surface");
    surface.blit_framed(&src, Affine::scale(10.0, 10.0), 255);
    assert_eq!(surface.get(1, 5), Some(Color::rgb(255, 0, 0).premultiply()));
    assert_eq!(
        surface.get(18, 5),
        Some(Color::rgb(0, 0, 255).premultiply())
    );
    let middle = surface.get(10, 5).expect("on the surface");
    assert!(
        middle.r > 100 && middle.b > 100 && middle.a == 255,
        "{middle:?}"
    );
}

/// Tiles drawn as a mosaic are the picture they were cut from: a sample
/// straddling a seam blends both sides, leaving none.
#[test]
fn a_mosaic_is_drawn_as_the_picture_it_was_cut_from() {
    let whole = varied(12, 8, 3);
    let mut cut = Vec::new();
    for row in 0..2 {
        for column in 0..3 {
            let mut tile = Surface::new(4, 4).expect("a tile");
            tile.overwrite(-(column * 4), -(row * 4), &whole);
            cut.push(Some(tile));
        }
    }
    let mosaic = Mosaic::new(4, 3, &cut).expect("a mosaic");
    assert_eq!(mosaic.size(), (12, 8));
    for transform in [
        Affine::scale(3.7, 2.2).then(Affine::translate(1.3, 0.6)),
        Affine::scale(0.6, 0.45).then(Affine::translate(4.25, 2.5)),
        Affine::rotate_degrees(17.0).then(Affine::translate(9.0, 2.0)),
    ] {
        let mut from_tiles = Surface::new(48, 24).expect("a surface");
        let mut from_whole = Surface::new(48, 24).expect("a surface");
        from_tiles.blit_transformed(&mosaic, transform, 255);
        from_whole.blit_transformed(&whole, transform, 255);
        assert_eq!(from_tiles.pixels(), from_whole.pixels(), "{transform:?}");
    }
}

/// An absent tile is transparent, and a mosaic its tiles cannot make is
/// refused.
#[test]
fn an_absent_tile_shows_nothing_and_a_misfit_mosaic_is_refused() {
    let tile = solid(4, 4, Color::rgb(10, 200, 30));
    let tiles = [Some(&tile), None];
    let mosaic = Mosaic::new(4, 2, &tiles).expect("a mosaic");
    assert_eq!(mosaic.pixel(1, 1), Color::rgb(10, 200, 30).premultiply());
    assert_eq!(mosaic.pixel(5, 1).a, 0, "the absent tile");
    assert_eq!(mosaic.pixel(9, 1).a, 0, "past the mosaic");
    assert!(
        Mosaic::new(3, 2, &tiles).is_none(),
        "a side not a power of two"
    );
    assert!(Mosaic::new(4, 3, &tiles).is_none(), "not a whole row");
    let odd = solid(4, 2, Color::rgb(0, 0, 0));
    assert!(
        Mosaic::new(4, 1, &[Some(&odd)]).is_none(),
        "a tile of another size"
    );
    assert!(Mosaic::new(4, 0, &tiles).is_none(), "no column");
}

/// The run of row `y` from column `x`, read a pixel at a time.
fn by_pixel<P: Picture>(picture: &P, x: i64, y: i64, len: usize) -> Vec<Pixel> {
    let (width, height) = picture.size();
    (x..)
        .take(len)
        .map(|at| match (u32::try_from(at), u32::try_from(y)) {
            (Ok(column), Ok(row)) if column < width && row < height => picture.pixel(column, row),
            _ => Pixel::TRANSPARENT,
        })
        .collect()
}

/// A row read is the pixels a pixel-at-a-time read gives, transparent past
/// every edge, for a surface and for a mosaic with a tile missing.
#[test]
fn a_row_read_is_what_reading_each_pixel_gives() {
    let whole = varied(8, 4, 5);
    let mut cut = Vec::new();
    for row in 0..2 {
        for column in 0..4 {
            let mut tile = Surface::new(2, 2).expect("a tile");
            tile.overwrite(-(column * 2), -(row * 2), &whole);
            cut.push(((row, column) != (1, 2)).then_some(tile));
        }
    }
    let mosaic = Mosaic::new(2, 4, &cut).expect("a mosaic");
    for (x, y, len) in [
        (-3, 1, 14),
        (0, 0, 8),
        (5, 3, 2),
        (2, -1, 4),
        (1, 4, 3),
        (9, 2, 3),
        (3, 2, 0),
    ] {
        let mut read = vec![
            Pixel {
                r: 1,
                g: 1,
                b: 1,
                a: 1
            };
            len
        ];
        whole.row(x, y, &mut read);
        assert_eq!(read, by_pixel(&whole, x, y, len), "surface at {x}, {y}");
        mosaic.row(x, y, &mut read);
        assert_eq!(read, by_pixel(&mosaic, x, y, len), "mosaic at {x}, {y}");
    }
    assert_eq!(mosaic.pixel(5, 3).a, 0, "the missing tile");
}

/// A mosaic with a tile missing, mirrored and thrown partly off the surface,
/// draws as the picture with that tile cleared would.
#[test]
fn a_mirrored_mosaic_with_a_tile_missing_is_drawn_seamlessly() {
    let mut whole = varied(12, 8, 9);
    let mut cut = Vec::new();
    for row in 0..2 {
        for column in 0..3 {
            let mut tile = Surface::new(4, 4).expect("a tile");
            tile.overwrite(-(column * 4), -(row * 4), &whole);
            cut.push(((row, column) != (0, 1)).then_some(tile));
        }
    }
    let clear = Surface::new(4, 4).expect("a clear tile");
    whole.overwrite(4, 0, &clear);
    let mosaic = Mosaic::new(4, 3, &cut).expect("a mosaic");
    for transform in [
        Affine::scale(-2.5, 1.75).then(Affine::translate(20.0, -3.0)),
        Affine::scale(1.5, -3.25).then(Affine::translate(-6.5, 22.0)),
        Affine::scale(-0.4, -0.6).then(Affine::translate(9.0, 7.0)),
    ] {
        let mut from_tiles = varied(40, 30, 1);
        let mut from_whole = from_tiles.clone();
        from_tiles.blit_transformed(&mosaic, transform, 255);
        from_whole.blit_transformed(&whole, transform, 255);
        assert_eq!(from_tiles.pixels(), from_whole.pixels(), "{transform:?}");
    }
}
