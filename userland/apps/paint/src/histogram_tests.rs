use alloc::vec;

use tairix_image::IndexDepth;
use tairix_parallel::{Reversed, SERIAL};

use super::{Histogram, Plot};
use crate::canvas::{Canvas, CanvasBuilder, Kind, Sample};
use crate::mask::Mask;
use crate::op::Halted;
use crate::shape::Bounds;

/// The histogram of `canvas` held to `clip`, counted on one core.
fn counted(canvas: &Canvas, clip: Option<&Mask>) -> Histogram {
    Histogram::of(canvas, clip, &SERIAL, &|| true).expect("room")
}

#[test]
fn each_pixel_counts_by_its_opacity_and_its_share_of_the_selection() {
    let mut built = CanvasBuilder::new(4, 1, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    built.set(0, 0, Sample::Rgba([255, 0, 0, 255]));
    built.set(1, 0, Sample::Rgba([255, 0, 0, 128]));
    built.set(2, 0, Sample::Rgba([0, 0, 255, 255]));
    let canvas = built.finish();
    let whole = counted(&canvas, None);
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
    let held = counted(&canvas, Some(&left));
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
    let mean = counted(&canvas, None).mean().expect("counted");
    for channel in mean {
        assert!((channel - 0.5).abs() < 1e-12, "{channel}");
    }
    let clear = Canvas::new(3, 3, Kind::Rgba, Sample::Rgba([10, 20, 30, 0])).expect("fits");
    assert_eq!(counted(&clear, None).mean(), None);
}

#[test]
fn a_palette_picture_counts_its_colours() {
    let kind = Kind::Indexed {
        depth: IndexDepth::One,
        palette: vec![[10, 20, 30, 255], [200, 100, 50, 255]],
        masked: false,
    };
    let canvas = Canvas::new(3, 2, kind, Sample::Index(1, 255)).expect("fits");
    let histogram = counted(&canvas, None);
    assert_eq!(histogram.counts(Plot::Red)[200], 6 * 255 * 255);
    assert_eq!(histogram.colours().len(), 3);
}

#[test]
fn columns_stand_against_the_tallest_level_between_the_ends() {
    let mut built =
        CanvasBuilder::new(10, 1, Kind::Rgba, Sample::Rgba([0, 0, 0, 255])).expect("fits");
    built.set(9, 0, Sample::Rgba([128, 128, 128, 255]));
    let canvas = built.finish();
    let histogram = counted(&canvas, None);
    assert_eq!(
        histogram.scale(Plot::Red),
        255 * 255,
        "the spike at black is not the scale"
    );
    assert_eq!(
        histogram.columns(Plot::Red, 256).next().expect("a column"),
        1000,
        "held to the top"
    );
    assert_eq!(
        histogram
            .columns(Plot::Red, 256)
            .nth(128)
            .expect("a column"),
        1000
    );
    assert_eq!(
        histogram
            .columns(Plot::Red, 256)
            .nth(200)
            .expect("a column"),
        0
    );
    assert_eq!(
        histogram.columns(Plot::Red, 128).nth(64).expect("a column"),
        1000,
        "a column covers two levels"
    );
    let blank = Canvas::new(1, 1, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let empty = counted(&blank, None);
    assert_eq!(empty.columns(Plot::Luma, 10).nth(3).expect("a column"), 0);
}

/// The bands' counts add, so a count cut into bands across several cores, in
/// any order, is the one count to the bit, its mean among it.
#[test]
fn a_count_cut_into_bands_is_the_whole_count() {
    let canvas = crate::effect::patterned(150, 200);
    let part = Mask::rect(Bounds {
        x0: 3,
        y0: 17,
        x1: 120,
        y1: 190,
    })
    .expect("pixels");
    for clip in [None, Some(&part)] {
        let cut = Reversed::new(4);
        let banded = Histogram::of(&canvas, clip, &cut, &|| true).expect("room");
        assert!(cut.widest() > 1, "the rows were cut");
        assert_eq!(banded, counted(&canvas, clip));
    }
}

#[test]
fn a_count_no_longer_wanted_is_abandoned() {
    let canvas = crate::effect::patterned(40, 40);
    assert_eq!(
        Histogram::of(&canvas, None, &SERIAL, &|| false),
        Err(Halted::Abandoned)
    );
}
