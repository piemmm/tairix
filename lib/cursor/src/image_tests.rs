use tairix_geometry::Point;
use tairix_raster::{Color, Pixel, ResampleScratch, Surface};
use tairix_theme::CursorKind;

use super::CursorImage;
use crate::store::CURSOR_BASE_SIDE_PX;
use crate::theme::CursorTheme;

const INK: Color = Color::rgb(240, 20, 30);

/// A `side`-square image, opaque in the `inset`-wide block at its centre,
/// with its hotspot at `hotspot`.
fn block(side: u32, inset: u32, hotspot: Point) -> CursorImage {
    let mut surface = Surface::new(side, side).expect("allocates");
    surface.fill_rect(inset, inset, side - 2 * inset, side - 2 * inset, INK);
    CursorImage::new(surface, hotspot)
}

fn arrow(side: u32) -> CursorImage {
    CursorTheme::builtin()
        .cursor(CursorKind::Arrow)
        .rasterise(side)
        .expect("the built-in arrow rasterises")
}

/// The alpha summed over the image's columns `x0..x1` and rows `y0..y1`.
fn alpha_in(image: &CursorImage, (x0, y0): (u32, u32), (x1, y1): (u32, u32)) -> u64 {
    (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| (x, y)))
        .filter_map(|(x, y)| image.surface().get(x, y))
        .map(|pixel| u64::from(pixel.a))
        .sum()
}

#[test]
fn a_row_is_the_images_own_pixels_and_none_past_its_last() {
    let image = block(8, 2, Point::ORIGIN);
    for ly in 0..8 {
        let row = image.row(ly).expect("inside the image");
        assert_eq!(row.len(), 8);
        for (lx, pixel) in (0..).zip(row) {
            assert_eq!(Some(*pixel), image.surface().get(lx, ly));
        }
    }
    assert_eq!(image.row(8), None);
}

#[test]
fn a_shadow_lies_beneath_the_artwork_below_and_to_its_right() {
    let side = CURSOR_BASE_SIDE_PX;
    let original = block(side, 8, Point::new(8, 8));
    let shadowed = original.shadowed().expect("allocates");
    let (dx, dy) = (
        shadowed.hotspot().x - original.hotspot().x,
        shadowed.hotspot().y - original.hotspot().y,
    );
    assert!(dx >= 0 && dy >= 0);
    assert!(shadowed.width() > original.width());
    assert!(shadowed.height() > original.height());

    let (ox, oy) = (dx.unsigned_abs(), dy.unsigned_abs());
    for y in 0..side {
        for x in 0..side {
            let drawn = original.surface().get(x, y).expect("inside");
            if drawn.a == u8::MAX {
                assert_eq!(
                    shadowed.surface().get(x + ox, y + oy),
                    Some(drawn),
                    "the artwork covers its own shadow exactly at ({x}, {y})"
                );
            }
        }
    }

    // The block spans the image's middle half; light from the upper left
    // drops its shadow past the right and bottom edges, not the others.
    let (left, top) = (ox + 8, oy + 8);
    let (right, bottom) = (ox + side - 8, oy + side - 8);
    let past_right = alpha_in(&shadowed, (right, top), (shadowed.width(), bottom));
    let past_left = alpha_in(&shadowed, (0, top), (left, bottom));
    let past_bottom = alpha_in(&shadowed, (left, bottom), (right, shadowed.height()));
    let past_top = alpha_in(&shadowed, (left, 0), (right, top));
    assert!(
        past_right > past_left,
        "{past_right} right of it, {past_left} left"
    );
    assert!(
        past_bottom > past_top,
        "{past_bottom} below it, {past_top} above"
    );
    assert!(
        past_bottom > past_right,
        "it falls further down than across"
    );
}

#[test]
fn a_shadow_is_soft_and_never_as_dense_as_the_artwork() {
    let shadowed = block(CURSOR_BASE_SIDE_PX, 8, Point::ORIGIN)
        .shadowed()
        .expect("allocates");
    let beyond = shadowed.width() - 1;
    let edge = (0..shadowed.height())
        .filter_map(|y| shadowed.surface().get(beyond, y))
        .map(|pixel| pixel.a)
        .max()
        .unwrap_or(0);
    let densest = shadowed
        .surface()
        .pixels()
        .iter()
        .filter(|pixel| pixel.r == 0)
        .map(|pixel| pixel.a)
        .max()
        .unwrap_or(0);
    assert!(densest > 0, "the shadow draws");
    assert!(densest <= 100, "the shadow is translucent: {densest}");
    assert!(edge < densest, "it fades toward the image's edge");
}

#[test]
fn a_larger_pointer_casts_a_proportionally_larger_shadow() {
    let small = arrow(CURSOR_BASE_SIDE_PX);
    let large = arrow(CURSOR_BASE_SIDE_PX * 2);
    let grown = |image: &CursorImage| {
        let shadowed = image.shadowed().expect("allocates");
        (
            shadowed.width() - image.width(),
            shadowed.height() - image.height(),
        )
    };
    let (small_x, small_y) = grown(&small);
    let (large_x, large_y) = grown(&large);
    assert!(large_x > small_x && large_y > small_y);
}

#[test]
fn nothing_drawn_casts_no_shadow() {
    let empty = CursorImage::new(Surface::new(16, 16).expect("allocates"), Point::ORIGIN);
    let shadowed = empty.shadowed().expect("allocates");
    assert!(shadowed
        .surface()
        .pixels()
        .iter()
        .all(|pixel| *pixel == Pixel::TRANSPARENT));
}

#[test]
fn resampling_to_its_own_side_changes_nothing() {
    let image = arrow(CURSOR_BASE_SIDE_PX);
    let mut scratch = ResampleScratch::default();
    assert_eq!(
        image.resampled_to(image.width(), None, &mut scratch),
        Some(image.clone())
    );
}

#[test]
fn resampling_scales_the_image_and_its_hotspot() {
    let image = block(32, 8, Point::new(8, 4));
    let mut scratch = ResampleScratch::default();
    let doubled = image
        .resampled_to(64, None, &mut scratch)
        .expect("allocates");
    assert_eq!((doubled.width(), doubled.height()), (64, 64));
    assert_eq!(doubled.hotspot(), Point::new(16, 8));
    let centre = doubled.surface().get(32, 32).expect("inside");
    assert_eq!(
        centre,
        INK.premultiply(),
        "the block's interior stays solid"
    );

    let halved = image
        .resampled_to(16, None, &mut scratch)
        .expect("allocates");
    assert_eq!((halved.width(), halved.height()), (16, 16));
    assert_eq!(halved.hotspot(), Point::new(4, 2));
    assert_eq!(image.resampled_to(0, None, &mut scratch), None);
}

#[test]
fn a_recycled_buffer_draws_the_same_image_without_allocating_again() {
    let peak = arrow(CURSOR_BASE_SIDE_PX * 4);
    let mut scratch = ResampleScratch::default();
    let first = peak
        .resampled_to(CURSOR_BASE_SIDE_PX * 3, None, &mut scratch)
        .expect("allocates");
    let held = first.surface().pixels().as_ptr();
    let recycled = peak
        .resampled_to(CURSOR_BASE_SIDE_PX * 2, Some(first), &mut scratch)
        .expect("fits the buffer it was given");
    assert_eq!(
        recycled,
        peak.resampled_to(
            CURSOR_BASE_SIDE_PX * 2,
            None,
            &mut ResampleScratch::default()
        )
        .expect("allocates"),
    );
    assert_eq!(recycled.surface().pixels().as_ptr(), held);
}
