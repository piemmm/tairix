use alloc::vec;

use tairix_image::IndexDepth;

use super::{Histogram, Plot};
use crate::canvas::{Canvas, CanvasBuilder, Kind, Sample};
use crate::mask::Mask;
use crate::shape::Bounds;

#[test]
fn each_pixel_counts_by_its_opacity_and_its_share_of_the_selection() {
    let mut built = CanvasBuilder::new(4, 1, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    built.set(0, 0, Sample::Rgba([255, 0, 0, 255]));
    built.set(1, 0, Sample::Rgba([255, 0, 0, 128]));
    built.set(2, 0, Sample::Rgba([0, 0, 255, 255]));
    let canvas = built.finish();
    let whole = Histogram::of(&canvas, None).expect("room");
    assert_eq!(whole.counts(Plot::Red)[255], 255 * 255 + 128 * 255);
    assert_eq!(whole.counts(Plot::Blue)[255], 255 * 255);
    assert_eq!(
        whole.counts(Plot::Red)[0],
        255 * 255,
        "the blue pixel's red"
    );
    assert_eq!(whole.counts(Plot::Green)[0], (255 + 128 + 255) * 255);
    assert_eq!(whole.counts(Plot::Luma)[77], 255 * 255 + 128 * 255);
    let left = Mask::rect(Bounds {
        x0: 0,
        y0: 0,
        x1: 1,
        y1: 1,
    })
    .expect("pixels");
    let held = Histogram::of(&canvas, Some(&left)).expect("room");
    assert_eq!(
        held.counts(Plot::Red)[255],
        255 * 255,
        "the chosen pixel alone"
    );
    assert_eq!(held.counts(Plot::Blue)[255], 0);
}

#[test]
fn the_mean_is_taken_in_linear_light() {
    let mut built =
        CanvasBuilder::new(2, 1, Kind::Rgba, Sample::Rgba([0, 0, 0, 255])).expect("fits");
    built.set(1, 0, Sample::Rgba([255, 255, 255, 255]));
    let canvas = built.finish();
    let mean = Histogram::of(&canvas, None)
        .expect("room")
        .mean()
        .expect("counted");
    for channel in mean {
        assert!((channel - 0.5).abs() < 1e-12, "{channel}");
    }
    let clear = Canvas::new(3, 3, Kind::Rgba, Sample::Rgba([10, 20, 30, 0])).expect("fits");
    assert_eq!(Histogram::of(&clear, None).expect("room").mean(), None);
}

#[test]
fn a_palette_picture_counts_its_colours() {
    let kind = Kind::Indexed {
        depth: IndexDepth::One,
        palette: vec![[10, 20, 30, 255], [200, 100, 50, 255]],
        masked: false,
    };
    let canvas = Canvas::new(3, 2, kind, Sample::Index(1, 255)).expect("fits");
    let histogram = Histogram::of(&canvas, None).expect("room");
    assert_eq!(histogram.counts(Plot::Red)[200], 6 * 255 * 255);
    assert_eq!(histogram.colours().len(), 3);
}

#[test]
fn columns_stand_against_the_tallest_level_between_the_ends() {
    let mut built =
        CanvasBuilder::new(10, 1, Kind::Rgba, Sample::Rgba([0, 0, 0, 255])).expect("fits");
    built.set(9, 0, Sample::Rgba([128, 128, 128, 255]));
    let canvas = built.finish();
    let histogram = Histogram::of(&canvas, None).expect("room");
    assert_eq!(
        histogram.scale(Plot::Red),
        255 * 255,
        "the spike at black is not the scale"
    );
    assert_eq!(histogram.column(Plot::Red, 0, 256), 1000, "held to the top");
    assert_eq!(histogram.column(Plot::Red, 128, 256), 1000);
    assert_eq!(histogram.column(Plot::Red, 200, 256), 0);
    assert_eq!(
        histogram.column(Plot::Red, 64, 128),
        1000,
        "a column covers two levels"
    );
    let empty = Histogram::of(
        &Canvas::new(1, 1, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits"),
        None,
    )
    .expect("room");
    assert_eq!(empty.column(Plot::Luma, 3, 10), 0);
}
