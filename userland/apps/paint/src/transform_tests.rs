use alloc::vec;

use tairix_image::{desktop_palette, IndexDepth};

use super::{apply, Anchor, Depth, PaletteChoice, Transform, TransformError, Turn};
use crate::canvas::{Canvas, CanvasBuilder, Kind, Sample};

/// A `width`×`height` picture whose pixel `(x, y)` records where it was.
fn marked(width: u32, height: u32) -> Canvas {
    let mut built =
        CanvasBuilder::new(width, height, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    for y in 0..height {
        for x in 0..width {
            let (x8, y8) = (
                u8::try_from(x).expect("small"),
                u8::try_from(y).expect("small"),
            );
            built.set(x, y, Sample::Rgba([x8, y8, 0, 255]));
        }
    }
    built.finish()
}

fn at(canvas: &Canvas, x: u32, y: u32) -> [u8; 4] {
    canvas.colour_at(x, y).expect("on the canvas")
}

#[test]
fn a_quarter_turn_puts_the_left_column_along_the_top() {
    let turned = apply(&marked(3, 2), Transform::Turn(Turn::Quarter)).expect("turns");
    assert_eq!((turned.width(), turned.height()), (2, 3));
    assert_eq!(
        at(&turned, 0, 0),
        [0, 1, 0, 255],
        "the bottom left comes to the top left"
    );
    assert_eq!(at(&turned, 1, 0), [0, 0, 0, 255]);
    assert_eq!(at(&turned, 1, 2), [2, 0, 0, 255]);
    let back = apply(&turned, Transform::Turn(Turn::ThreeQuarters)).expect("turns");
    assert_eq!(back, marked(3, 2));
}

#[test]
fn a_half_turn_twice_is_where_it_started_across_tiles() {
    let picture = marked(130, 70);
    let once = apply(&picture, Transform::Turn(Turn::Half)).expect("turns");
    assert_eq!(at(&once, 0, 0), [129, 69, 0, 255]);
    assert_eq!(
        apply(&once, Transform::Turn(Turn::Half)).expect("turns"),
        picture
    );
}

#[test]
fn mirroring_reverses_rows_or_columns() {
    let picture = marked(70, 3);
    let across = apply(&picture, Transform::Flip { vertical: false }).expect("flips");
    assert_eq!(at(&across, 0, 1), [69, 1, 0, 255]);
    let down = apply(&picture, Transform::Flip { vertical: true }).expect("flips");
    assert_eq!(at(&down, 5, 0), [5, 2, 0, 255]);
}

#[test]
fn scaling_by_the_nearest_pixel_keeps_the_colours_it_had() {
    let doubled = apply(
        &marked(4, 4),
        Transform::Scale {
            width: 8,
            height: 8,
            smooth: false,
        },
    )
    .expect("scales");
    assert_eq!(at(&doubled, 0, 0), [0, 0, 0, 255]);
    assert_eq!(at(&doubled, 1, 1), [0, 0, 0, 255]);
    assert_eq!(at(&doubled, 7, 7), [3, 3, 0, 255]);
    let halved = apply(
        &marked(8, 8),
        Transform::Scale {
            width: 4,
            height: 4,
            smooth: false,
        },
    )
    .expect("scales");
    assert_eq!(
        at(&halved, 1, 1),
        [3, 3, 0, 255],
        "each takes the pixel at its centre"
    );
}

#[test]
fn smooth_scaling_of_one_colour_is_that_colour() {
    let flat = Canvas::new(40, 30, Kind::Rgba, Sample::Rgba([10, 200, 30, 255])).expect("fits");
    let scaled = apply(
        &flat,
        Transform::Scale {
            width: 17,
            height: 90,
            smooth: true,
        },
    )
    .expect("scales");
    assert_eq!((scaled.width(), scaled.height()), (17, 90));
    assert_eq!(at(&scaled, 8, 45), [10, 200, 30, 255]);
}

#[test]
fn a_bigger_canvas_places_the_picture_at_its_anchor() {
    let fill = Sample::Rgba([9, 9, 9, 9]);
    let grown = apply(
        &marked(2, 2),
        Transform::Resize {
            width: 4,
            height: 4,
            anchor: Anchor::Centre,
            fill,
        },
    )
    .expect("grows");
    assert_eq!(at(&grown, 0, 0), [9, 9, 9, 9]);
    assert_eq!(at(&grown, 1, 1), [0, 0, 0, 255]);
    assert_eq!(at(&grown, 2, 2), [1, 1, 0, 255]);
    let shrunk = apply(
        &marked(4, 4),
        Transform::Resize {
            width: 2,
            height: 2,
            anchor: Anchor::BottomRight,
            fill,
        },
    )
    .expect("shrinks");
    assert_eq!(at(&shrunk, 0, 0), [2, 2, 0, 255]);
}

#[test]
fn a_crop_keeps_its_rectangle_and_refuses_one_off_the_picture() {
    let cropped = apply(
        &marked(10, 10),
        Transform::Crop {
            x: 3,
            y: 4,
            width: 2,
            height: 5,
        },
    )
    .expect("crops");
    assert_eq!((cropped.width(), cropped.height()), (2, 5));
    assert_eq!(at(&cropped, 1, 4), [4, 8, 0, 255]);
    let off = apply(
        &marked(10, 10),
        Transform::Crop {
            x: 9,
            y: 0,
            width: 2,
            height: 1,
        },
    );
    assert_eq!(off, Err(TransformError::BadSize));
}

fn palette_picture(masked: bool) -> Canvas {
    let kind = Kind::Indexed {
        depth: IndexDepth::Two,
        palette: vec![[0, 0, 0, 255], [255, 255, 255, 255], [255, 0, 0, 255]],
        masked,
    };
    let mut built = CanvasBuilder::new(3, 1, kind, Sample::Index(1, 255)).expect("fits");
    built.set(0, 0, Sample::Index(2, if masked { 0 } else { 255 }));
    built.finish()
}

#[test]
fn a_mask_is_added_opaque_and_taken_away_with_its_pixels_filled() {
    let plain = palette_picture(false);
    let with = apply(&plain, Transform::Mask { on: true, fill: 0 }).expect("masks");
    assert!(with.kind().masked());
    assert_eq!(with.sample(0, 0), Some(Sample::Index(2, 255)));
    let hidden = palette_picture(true);
    let without = apply(&hidden, Transform::Mask { on: false, fill: 1 }).expect("unmasks");
    assert_eq!(without.sample(0, 0), Some(Sample::Index(1, 255)));
    assert_eq!(
        apply(&plain, Transform::Mask { on: false, fill: 0 }),
        Err(TransformError::NotApplicable)
    );
    assert_eq!(
        apply(&marked(2, 2), Transform::Mask { on: true, fill: 0 }),
        Err(TransformError::NotApplicable)
    );
}

#[test]
fn converting_to_the_desktop_palette_takes_the_nearest_wimp_colour() {
    let mut built = CanvasBuilder::new(3, 1, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    built.set(0, 0, Sample::Rgba([255, 255, 255, 255]));
    built.set(1, 0, Sample::Rgba([200, 0, 10, 255]));
    built.set(2, 0, Sample::Rgba([0, 0, 0, 0]));
    let converted = apply(
        &built.finish(),
        Transform::Convert {
            depth: Depth::Indexed(IndexDepth::Four),
            palette: PaletteChoice::Desktop,
            dither: false,
        },
    )
    .expect("converts");
    let wimp = desktop_palette(IndexDepth::Four);
    assert_eq!(converted.kind().palette().map(<[_]>::len), Some(wimp.len()));
    assert!(converted.kind().masked(), "a clear pixel needs a mask");
    assert_eq!(
        converted.sample(0, 0),
        Some(Sample::Index(0, 255)),
        "Wimp colour 0 is white"
    );
    assert_eq!(
        converted.sample(1, 0),
        Some(Sample::Index(11, 255)),
        "Wimp colour 11 is red"
    );
    assert!(matches!(converted.sample(2, 0), Some(Sample::Index(_, 0))));
}

#[test]
fn dithering_a_grey_between_black_and_white_mixes_them() {
    let grey = Canvas::new(16, 16, Kind::Rgba, Sample::Rgba([128, 128, 128, 255])).expect("fits");
    let dithered = apply(
        &grey,
        Transform::Convert {
            depth: Depth::Indexed(IndexDepth::One),
            palette: PaletteChoice::Desktop,
            dither: true,
        },
    )
    .expect("converts");
    let mut white = 0;
    for y in 0..16 {
        for x in 0..16 {
            if let Some(Sample::Index(0, _)) = dithered.sample(x, y) {
                white += 1;
            }
        }
    }
    assert!((100..=156).contains(&white), "about half white: {white}");
    let flat = apply(
        &grey,
        Transform::Convert {
            depth: Depth::Indexed(IndexDepth::One),
            palette: PaletteChoice::Desktop,
            dither: false,
        },
    )
    .expect("converts");
    let first = flat.sample(0, 0);
    assert!(
        (0..16).all(|x| flat.sample(x, 5) == first),
        "undithered is one colour"
    );
}

#[test]
fn converting_a_palette_picture_to_colour_shows_the_same() {
    let picture = palette_picture(true);
    let colour = apply(
        &picture,
        Transform::Convert {
            depth: Depth::Rgba,
            palette: PaletteChoice::Desktop,
            dither: false,
        },
    )
    .expect("converts");
    for x in 0..3 {
        assert_eq!(colour.colour_at(x, 0), picture.colour_at(x, 0));
    }
}

#[test]
fn an_optimised_palette_of_a_few_colours_keeps_them_exactly() {
    let picture = palette_picture(false);
    let colour = apply(
        &picture,
        Transform::Convert {
            depth: Depth::Rgba,
            palette: PaletteChoice::Desktop,
            dither: false,
        },
    )
    .expect("converts");
    let back = apply(
        &colour,
        Transform::Convert {
            depth: Depth::Indexed(IndexDepth::Eight),
            palette: PaletteChoice::Optimised,
            dither: true,
        },
    )
    .expect("converts");
    for x in 0..3 {
        assert_eq!(back.colour_at(x, 0), picture.colour_at(x, 0));
    }
    assert!(!back.kind().masked(), "nothing was clear, so no mask");
}

/// A smooth scale made a band at a time is the whole resample, byte for
/// byte, across the joins between bands.
#[test]
fn a_banded_smooth_scale_is_the_whole_resample() {
    use tairix_image::PictureSource;
    use tairix_raster::{resample, Region, Rgba8Image};
    let picture = marked(37, 150);
    let scaled = apply(
        &picture,
        Transform::Scale {
            width: 60,
            height: 200,
            smooth: true,
        },
    )
    .expect("scales");
    let mut pixels = vec![0u8; 37 * 150 * 4];
    for (y, row) in (0..).zip(pixels.as_chunks_mut::<{ 37 * 4 }>().0) {
        picture.read_row(y, row, &mut []);
    }
    let image = Rgba8Image::new(37, 150, &pixels).expect("an image");
    let whole = Region {
        x: 0,
        y: 0,
        width: 37,
        height: 150,
    };
    let reference = resample(&image, whole, 60, 200).expect("resamples");
    let mut row = vec![0u8; 60 * 4];
    for (y, expected) in (0..).zip(reference.as_chunks::<{ 60 * 4 }>().0) {
        scaled.read_row(y, &mut row, &mut []);
        assert_eq!(row, expected, "row {y}");
    }
}

/// Every turn of a picture several tiles across, wider than tall, puts each
/// pixel exactly where the one orientation mapping says.
#[test]
fn every_turn_places_each_pixel_as_the_mapping_says() {
    use tairix_raster::Reorient;
    let (w, h) = (150u32, 70u32);
    let mut built =
        crate::canvas::CanvasBuilder::new(w, h, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    for y in 0..h {
        for x in 0..w {
            let (x8, y8) = (
                u8::try_from(x).expect("small"),
                u8::try_from(y).expect("small"),
            );
            built.set(x, y, Sample::Rgba([x8, y8, x8 ^ y8, 255]));
        }
    }
    let picture = built.finish();
    for (turn, how) in [
        (Turn::Quarter, Reorient::QuarterTurnRight),
        (Turn::Half, Reorient::HalfTurn),
        (Turn::ThreeQuarters, Reorient::QuarterTurnLeft),
    ] {
        let turned = super::apply(&picture, Transform::Turn(turn)).expect("room");
        assert_eq!((turned.width(), turned.height()), how.applied_size(w, h));
        for y in 0..h {
            for x in 0..w {
                let (to_x, to_y) = how.place(x, y, w, h);
                assert_eq!(
                    turned.sample(to_x, to_y),
                    picture.sample(x, y),
                    "{how:?} ({x}, {y})"
                );
            }
        }
    }
}

/// A column read is the pixels a pixel at a time would give, down several
/// tiles and from any row.
#[test]
fn a_column_read_is_each_pixel_down_it() {
    let (w, h) = (3u32, 150u32);
    let mut built =
        crate::canvas::CanvasBuilder::new(w, h, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    for y in 0..h {
        built.set(
            1,
            y,
            Sample::Rgba([u8::try_from(y).expect("small"), 0, 0, 255]),
        );
    }
    let picture = built.finish();
    let mut column = vec![Sample::Rgba([9; 4]); 140];
    picture.column_samples(1, 5, &mut column);
    for (y, sample) in (5..).zip(&column) {
        assert_eq!(Some(*sample), picture.sample(1, y), "row {y}");
    }
}
