use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_image::{IndexDepth, PictureSource};

use super::{Canvas, CanvasBuilder, CanvasError, Kind, Sample, TILE};

fn palette() -> Vec<[u8; 4]> {
    vec![[0, 0, 0, 255], [255, 255, 255, 255], [255, 0, 0, 255]]
}

fn indexed(masked: bool) -> Kind {
    Kind::Indexed {
        depth: IndexDepth::Four,
        palette: palette(),
        masked,
    }
}

#[test]
fn a_blank_canvas_shares_one_tile_per_extent() {
    let canvas = Canvas::new(200, 130, Kind::Rgba, Sample::Rgba([1, 2, 3, 4])).expect("fits");
    assert_eq!(canvas.tile_count(), 4 * 3);
    // Interior, right column, bottom row, corner: four distinct tiles.
    let distinct = (0..canvas.tile_count())
        .map(|index| Arc::as_ptr(canvas.tile(index)))
        .collect::<alloc::collections::BTreeSet<_>>();
    assert_eq!(distinct.len(), 4);
    assert_eq!(canvas.sample(199, 129), Some(Sample::Rgba([1, 2, 3, 4])));
    assert_eq!(canvas.sample(200, 0), None);
}

#[test]
fn a_write_copies_only_the_tile_it_lands_on() {
    let mut canvas = Canvas::new(130, 64, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let before = Arc::clone(canvas.tile(0));
    let tile = canvas.tile_mut(0).expect("copies");
    tile.planes_mut().0[..4].copy_from_slice(&[9, 9, 9, 9]);
    assert_eq!(
        before.samples()[..4],
        [0, 0, 0, 0],
        "the shared tile is untouched"
    );
    assert_eq!(canvas.sample(0, 0), Some(Sample::Rgba([9, 9, 9, 9])));
    assert_eq!(canvas.sample(64, 0), Some(Sample::Rgba([0; 4])));
}

#[test]
fn edge_tiles_have_their_own_extent() {
    let canvas = Canvas::new(70, 65, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let corner = canvas.tile_rect(3);
    assert_eq!(
        (corner.x, corner.y, corner.width, corner.height),
        (TILE, TILE, 6, 1)
    );
    assert_eq!(canvas.tile(3).samples().len(), 6 * 4);
}

#[test]
fn sizes_and_palettes_out_of_bounds_are_refused() {
    let fill = Sample::Rgba([0; 4]);
    assert_eq!(
        Canvas::new(0, 1, Kind::Rgba, fill),
        Err(CanvasError::BadSize)
    );
    assert_eq!(
        Canvas::new(super::MAX_SIDE + 1, 1, Kind::Rgba, fill),
        Err(CanvasError::BadSize)
    );
    let empty = Kind::Indexed {
        depth: IndexDepth::One,
        palette: Vec::new(),
        masked: false,
    };
    assert_eq!(
        Canvas::new(1, 1, empty, Sample::Index(0, 255)),
        Err(CanvasError::BadPalette)
    );
    assert_eq!(
        Canvas::new(1, 1, indexed(false), Sample::Index(3, 255)),
        Err(CanvasError::BadPalette),
        "an index past the palette"
    );
    assert_eq!(
        Canvas::new(1, 1, indexed(false), Sample::Index(0, 7)),
        Err(CanvasError::BadPalette),
        "opacity with no mask to hold it"
    );
    assert_eq!(
        Canvas::new(1, 1, Kind::Rgba, Sample::Index(0, 255)),
        Err(CanvasError::BadPalette)
    );
}

#[test]
fn a_masked_index_shows_its_entry_at_the_mask_opacity() {
    let canvas = Canvas::new(2, 1, indexed(true), Sample::Index(2, 0)).expect("fits");
    assert_eq!(canvas.colour_at(0, 0), Some([255, 0, 0, 0]));
    let mut built = CanvasBuilder::new(2, 1, indexed(true), Sample::Index(0, 255)).expect("fits");
    built.row(0, &[2, 1], &[255, 128]);
    let canvas = built.finish();
    assert_eq!(canvas.colour_at(0, 0), Some([255, 0, 0, 255]));
    assert_eq!(canvas.colour_at(1, 0), Some([255, 255, 255, 128]));
}

#[test]
fn rows_read_back_as_they_were_built_across_tiles() {
    let width = 150u32;
    let mut built = CanvasBuilder::new(width, 3, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let row: Vec<u8> = (0..width * 4)
        .map(|at| u8::try_from(at % 251).unwrap_or(0))
        .collect();
    built.row(1, &row, &[]);
    let canvas = built.finish();
    let mut samples = vec![0u8; width as usize * 4];
    canvas.read_row(1, &mut samples, &mut []);
    assert_eq!(samples, row);
    let mut colours = vec![[0u8; 4]; 10];
    canvas.row_colours(1, 60, &mut colours);
    for (at, colour) in colours.iter().enumerate() {
        let base = (60 + at) * 4;
        assert_eq!(colour[..], row[base..base + 4]);
    }
}

#[test]
fn a_row_of_the_wrong_length_is_ignored() {
    let mut built = CanvasBuilder::new(4, 1, indexed(true), Sample::Index(0, 255)).expect("fits");
    built.row(0, &[1, 1, 1], &[0, 0, 0, 0]);
    built.row(0, &[1, 1, 1, 1], &[0, 0]);
    assert_eq!(built.finish().sample(0, 0), Some(Sample::Index(0, 255)));
}

#[test]
fn a_palette_is_swapped_only_for_one_as_long() {
    let mut canvas = Canvas::new(1, 1, indexed(false), Sample::Index(0, 255)).expect("fits");
    assert_eq!(canvas.swap_palette(vec![[1; 4]]), None);
    let old = canvas
        .swap_palette(vec![[1; 4], [2; 4], [3; 4]])
        .expect("swaps");
    assert_eq!(old, palette());
    assert_eq!(canvas.colour_at(0, 0), Some([1; 4]));
}

#[test]
fn transparency_is_found_in_the_mask_the_palette_and_the_alpha() {
    let opaque = Canvas::new(3, 3, indexed(true), Sample::Index(1, 255)).expect("fits");
    assert!(!opaque.has_transparency());
    let clear = Canvas::new(3, 3, indexed(true), Sample::Index(1, 0)).expect("fits");
    assert!(clear.has_transparency() && !clear.has_partial_alpha());
    let alpha = Kind::Indexed {
        depth: IndexDepth::One,
        palette: vec![[0, 0, 0, 128], [9, 9, 9, 255]],
        masked: false,
    };
    let unused = Canvas::new(3, 3, alpha.clone(), Sample::Index(1, 255)).expect("fits");
    assert!(
        !unused.has_transparency(),
        "a clear palette entry no pixel shows"
    );
    let tinted = Canvas::new(3, 3, alpha, Sample::Index(0, 255)).expect("fits");
    assert!(
        tinted.has_transparency(),
        "a pixel of a palette entry that is not opaque"
    );
    let rgba = Canvas::new(3, 3, Kind::Rgba, Sample::Rgba([1, 1, 1, 200])).expect("fits");
    assert!(rgba.has_partial_alpha());
}

/// A palette pixel's alpha is what it shows: its entry's, under its mask.
#[test]
fn a_palette_pixels_partial_alpha_is_its_entry_under_its_mask() {
    let translucent = Kind::Indexed {
        depth: IndexDepth::One,
        palette: vec![[0, 0, 0, 128], [9, 9, 9, 255]],
        masked: false,
    };
    let unmasked = Canvas::new(3, 3, translucent, Sample::Index(0, 255)).expect("fits");
    assert!(
        unmasked.has_partial_alpha(),
        "an unmasked picture showing a translucent entry"
    );
    let clear_entry = Kind::Indexed {
        depth: IndexDepth::One,
        palette: vec![[0, 0, 0, 0], [9, 9, 9, 255]],
        masked: true,
    };
    let soft_over_clear = Canvas::new(3, 3, clear_entry, Sample::Index(0, 128)).expect("fits");
    assert!(
        !soft_over_clear.has_partial_alpha(),
        "a soft mask over a clear entry shows nothing at all"
    );
}

#[test]
fn what_is_shared_is_charged_to_its_holders_together() {
    let tile = TILE as usize * TILE as usize * 4;
    let canvas = Canvas::new(128, 64, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    assert_eq!(
        canvas.charged_bytes(),
        tile,
        "both slots hold one shared tile, charged once between them"
    );
    let held = Arc::clone(canvas.tile(0));
    let mut canvas = canvas;
    canvas.tile_mut(1).expect("copies");
    assert_eq!(
        canvas.charged_bytes(),
        tile + tile / 2,
        "the copy is the canvas's alone; the original it shares with another"
    );
    drop(held);
    assert_eq!(canvas.charged_bytes(), canvas.bytes());
    assert_eq!(canvas.bytes(), 128 * 64 * 4);
}

/// Why a canvas could not be made reads as a reason, not a variant name.
#[test]
fn a_canvas_refusal_says_why() {
    let err = Canvas::new(0, 4, Kind::Rgba, Sample::Rgba([0; 4])).expect_err("an empty side");
    assert_eq!(err, CanvasError::BadSize);
    assert_eq!(alloc::format!("{err}"), "the size is too large or empty");
}

/// A copy shares every tile until one is written, so copying a picture for
/// a worker costs the list of its tiles, not its pixels.
#[test]
fn a_copy_shares_every_tile() {
    let canvas = Canvas::new(130, 70, Kind::Rgba, Sample::Rgba([1, 2, 3, 255])).expect("fits");
    let copy = canvas.try_clone().expect("room");
    assert_eq!(copy, canvas);
    assert!((0..canvas.tile_count()).all(|index| Arc::ptr_eq(canvas.tile(index), copy.tile(index))));
}

/// A row written whole, across tiles, reads back as written.
#[test]
fn a_row_written_whole_reads_back_across_tiles() {
    let mut built = CanvasBuilder::new(130, 3, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let row: Vec<Sample> = (0..130u32)
        .map(|x| Sample::Rgba([u8::try_from(x).expect("small"), 1, 2, 255]))
        .collect();
    built.set_row(1, &row);
    built.set_row(2, &row[..129]);
    let canvas = built.finish();
    let mut read = vec![Sample::Rgba([9; 4]); 130];
    canvas.row_samples(1, 0, &mut read);
    assert_eq!(read, row);
    canvas.row_samples(2, 0, &mut read);
    assert!(
        read.iter().all(|&sample| sample == Sample::Rgba([0; 4])),
        "a short row is ignored"
    );
}
