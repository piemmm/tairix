//! The pyramid: each level half the last, rounded up, down to a pixel, a
//! level fit for every scale, and a minified picture drawn without aliasing.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_bezier::Affine;

use super::Pyramid;
use crate::artwork::{Image, Node};
use crate::surface::Surface;

/// A `side`-pixel picture of one-pixel black and white squares.
fn checker(side: u32) -> Surface {
    let mut rgba = Vec::new();
    for y in 0..side {
        for x in 0..side {
            let level = if (x + y) % 2 == 0 { 0 } else { 255 };
            rgba.extend_from_slice(&[level, level, level, 255]);
        }
    }
    Surface::from_rgba8(side, side, &rgba).expect("a picture")
}

#[test]
fn each_level_halves_the_last_rounded_up_down_to_a_pixel() {
    let picture = Surface::new(5, 3).expect("a picture");
    let pyramid = Pyramid::new(picture).expect("its levels");
    assert_eq!(pyramid.depth(), 4, "5×3, 3×2, 2×1, 1×1");
    let sizes: Vec<(u32, u32)> = [1.0, 0.5, 0.25, 0.125, 0.01]
        .iter()
        .map(|&scale| {
            let level = pyramid.level_for(scale);
            (level.width(), level.height())
        })
        .collect();
    assert_eq!(sizes, vec![(5, 3), (5, 3), (3, 2), (2, 1), (1, 1)]);
    let lone = Pyramid::new(Surface::new(1, 1).expect("a pixel")).expect("a pyramid");
    assert_eq!(lone.depth(), 1);
    assert_eq!(lone.level_for(0.001).width(), 1);
}

#[test]
fn a_picture_drawn_far_smaller_is_averaged_rather_than_aliased() {
    let pyramid = Arc::new(Pyramid::new(checker(64)).expect("its levels"));
    let mut surface = Surface::new(8, 8).expect("a surface");
    let image = Node::Image(Image {
        pyramid,
        transform: Affine::scale(1.0 / 8.0, 1.0 / 8.0),
        opacity: 255,
    });
    assert!(surface.draw_artwork(&[image], 8));
    for pixel in surface.pixels() {
        assert!(
            (100..=156).contains(&pixel.r),
            "an eighth of a checker is grey, not black or white: {pixel:?}"
        );
    }
}

#[test]
fn a_picture_is_drawn_through_its_transform_at_its_opacity() {
    let mut rgba = vec![0_u8; 4 * 4 * 4];
    for quad in rgba.chunks_mut(4) {
        quad.copy_from_slice(&[200, 40, 40, 255]);
    }
    let picture = Surface::from_rgba8(4, 4, &rgba).expect("a picture");
    let pyramid = Arc::new(Pyramid::new(picture).expect("its levels"));
    let mut surface = Surface::new(16, 16).expect("a surface");
    // Four design units a pixel: the picture's 4×4 pixels fill 8×8 of the
    // 16-pixel surface's 64-unit grid, from (8, 8).
    let image = Node::Image(Image {
        pyramid: Arc::clone(&pyramid),
        transform: Affine::scale(8.0, 8.0).then(Affine::translate(32.0, 32.0)),
        opacity: 128,
    });
    assert!(surface.draw_artwork(core::slice::from_ref(&image), 64));
    let inside = surface.get(12, 12).expect("on the surface");
    assert!((120..=136).contains(&inside.a), "half strength: {inside:?}");
    assert_eq!(
        surface.get(2, 2).map(|p| p.a),
        Some(0),
        "outside the picture"
    );
    let same = Node::Image(Image {
        pyramid,
        transform: Affine::scale(8.0, 8.0).then(Affine::translate(32.0, 32.0)),
        opacity: 128,
    });
    assert_eq!(image, same, "the same picture drawn the same way");
}
