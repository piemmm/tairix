//! The picture model: what a picture may be built from, and how a row of one
//! flattens.

use alloc::vec;

use super::{
    flatten_row, masked_colour, IndexDepth, Picture, PictureError, PictureKind, PictureSource,
    Pixels,
};

#[test]
fn a_depth_names_its_bits_and_colours() {
    for (depth, bits) in IndexDepth::ALL.into_iter().zip([1, 2, 4, 8]) {
        assert_eq!(depth.bits(), bits);
        assert_eq!(depth.colours(), 1 << bits);
        assert_eq!(IndexDepth::from_bits(bits), Some(depth));
    }
    assert_eq!(IndexDepth::from_bits(3), None);
}

#[test]
fn the_shallowest_depth_holding_a_palette_is_chosen() {
    assert_eq!(IndexDepth::holding(0), None);
    assert_eq!(IndexDepth::holding(1), Some(IndexDepth::One));
    assert_eq!(IndexDepth::holding(2), Some(IndexDepth::One));
    assert_eq!(IndexDepth::holding(3), Some(IndexDepth::Two));
    assert_eq!(IndexDepth::holding(16), Some(IndexDepth::Four));
    assert_eq!(IndexDepth::holding(17), Some(IndexDepth::Eight));
    assert_eq!(IndexDepth::holding(256), Some(IndexDepth::Eight));
    assert_eq!(IndexDepth::holding(257), None);
}

#[test]
fn a_truecolour_picture_needs_four_bytes_a_pixel() {
    assert!(Picture::rgba(2, 1, vec![0; 8]).is_ok());
    assert_eq!(
        Picture::rgba(2, 1, vec![0; 7]).err(),
        Some(PictureError::LengthMismatch)
    );
    assert_eq!(
        Picture::rgba(0, 1, vec![]).err(),
        Some(PictureError::ZeroDimension)
    );
}

#[test]
fn an_indexed_picture_refuses_what_would_not_resolve() {
    let two = vec![[0, 0, 0, 255], [255, 255, 255, 255]];
    assert!(Picture::indexed(2, 1, IndexDepth::One, two.clone(), vec![0, 1], None).is_ok());
    assert_eq!(
        Picture::indexed(2, 1, IndexDepth::One, two.clone(), vec![0, 2], None).err(),
        Some(PictureError::IndexOutOfRange)
    );
    assert_eq!(
        Picture::indexed(2, 1, IndexDepth::One, vec![], vec![0, 0], None).err(),
        Some(PictureError::EmptyPalette)
    );
    assert_eq!(
        Picture::indexed(2, 1, IndexDepth::One, vec![[0; 4]; 3], vec![0, 0], None).err(),
        Some(PictureError::PaletteTooLong)
    );
    assert_eq!(
        Picture::indexed(2, 1, IndexDepth::One, two, vec![0, 1], Some(vec![0])).err(),
        Some(PictureError::LengthMismatch)
    );
}

#[test]
fn a_mask_multiplies_an_entrys_own_alpha() {
    assert_eq!(masked_colour([10, 20, 30, 255], 255), [10, 20, 30, 255]);
    assert_eq!(masked_colour([10, 20, 30, 255], 0), [10, 20, 30, 0]);
    assert_eq!(masked_colour([10, 20, 30, 128], 128), [10, 20, 30, 64]);
}

#[test]
fn an_indexed_row_flattens_through_its_palette_and_mask() {
    let palette = [[255, 0, 0, 255], [0, 0, 255, 128]];
    let kind = PictureKind::Indexed {
        depth: IndexDepth::One,
        palette: &palette,
        masked: true,
    };
    let mut out = [0u8; 12];
    flatten_row(kind, &[0, 1, 0], &[255, 255, 0], &mut out);
    assert_eq!(out, [255, 0, 0, 255, 0, 0, 255, 128, 255, 0, 0, 0]);
}

#[test]
fn an_index_past_the_palette_flattens_to_transparent_rather_than_faulting() {
    let palette = [[1, 2, 3, 255]];
    let kind = PictureKind::Indexed {
        depth: IndexDepth::One,
        palette: &palette,
        masked: false,
    };
    let mut out = [9u8; 4];
    flatten_row(kind, &[1], &[], &mut out);
    assert_eq!(out, [0, 0, 0, 0]);
}

#[test]
fn a_picture_is_read_a_row_at_a_time() {
    let picture = Picture::indexed(
        2,
        2,
        IndexDepth::Two,
        vec![[0; 4], [1; 4], [2; 4]],
        vec![0, 1, 2, 1],
        Some(vec![255, 0, 255, 255]),
    )
    .expect("valid");
    let mut samples = [0u8; 2];
    let mut mask = [0u8; 2];
    picture.read_row(1, &mut samples, &mut mask);
    assert_eq!((samples, mask), ([2, 1], [255, 255]));
    assert!(matches!(
        picture.kind(),
        PictureKind::Indexed { masked: true, .. }
    ));
    let flat = picture.to_rgba().expect("memory");
    assert_eq!(&flat[4..8], &[1, 1, 1, 0]);
    assert!(matches!(picture.into_pixels(), Pixels::Indexed { .. }));
}

#[test]
fn a_row_read_into_buffers_too_short_fills_what_fits_rather_than_faulting() {
    let picture = Picture::indexed(
        2,
        2,
        IndexDepth::Two,
        vec![[0; 4], [1; 4], [2; 4]],
        vec![0, 1, 2, 1],
        Some(vec![255, 0, 7, 9]),
    )
    .expect("valid");
    let mut samples = [0u8; 1];
    picture.read_row(1, &mut samples, &mut []);
    assert_eq!(samples, [2]);
    let mut wide = [0u8; 3];
    let mut mask = [0u8; 3];
    picture.read_row(1, &mut wide, &mut mask);
    assert_eq!((wide, mask), ([2, 1, 0], [7, 9, 0]));
    picture.read_row(u32::MAX, &mut wide, &mut mask);
    assert_eq!(
        (wide, mask),
        ([2, 1, 0], [7, 9, 0]),
        "a row past the end is left alone"
    );
}

/// Straight-alpha source-over: nothing over anything leaves it, the opaque
/// replaces it, and between them the colours mix by the alpha each keeps,
/// rounded to the nearest.
#[test]
fn a_colour_over_another_mixes_by_the_alpha_each_keeps() {
    use super::over;
    let below = [200, 100, 0, 255];
    assert_eq!(over(below, [9, 9, 9, 0]), below);
    assert_eq!(over(below, [9, 8, 7, 255]), [9, 8, 7, 255]);
    assert_eq!(over(below, [0, 0, 200, 128]), [100, 50, 100, 255]);
    assert_eq!(over([0, 0, 0, 0], [10, 20, 30, 40]), [10, 20, 30, 40]);
    assert_eq!(
        over([255, 255, 255, 128], [0, 0, 0, 128]),
        [85, 85, 85, 192]
    );
}
