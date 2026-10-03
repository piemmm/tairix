use alloc::vec;

use tairix_image::IndexDepth;

use super::{apply, Filter, FilterError};
use crate::canvas::{Canvas, CanvasBuilder, Kind, Sample};
use crate::mask::Mask;
use crate::shape::Bounds;

fn flat(width: u32, height: u32, colour: [u8; 4]) -> Canvas {
    Canvas::new(width, height, Kind::Rgba, Sample::Rgba(colour)).expect("fits")
}

fn rgba(canvas: &Canvas, x: u32, y: u32) -> [u8; 4] {
    match canvas.sample(x, y) {
        Some(Sample::Rgba(colour)) => colour,
        other => panic!("a colour, not {other:?}"),
    }
}

#[test]
fn a_setting_is_held_to_its_bounds_and_black_stays_below_white() {
    let mut levels = Filter::Levels {
        black: 0,
        white: 255,
        gamma: 100,
    };
    levels.set(1, 40);
    levels.set(0, 200);
    assert_eq!(
        (levels.value(0), levels.value(1)),
        (39, 40),
        "black held below white"
    );
    let mut blur = Filter::Blur { radius: 2 };
    blur.set(0, 1000);
    assert_eq!(blur.value(0), 64);
    blur.set(5, 3);
    assert_eq!(blur, Filter::Blur { radius: 64 }, "no such number");
    for filter in Filter::ALL {
        for (index, parameter) in filter.parameters().iter().enumerate() {
            let value = filter.value(index);
            assert!(
                (parameter.least..=parameter.most).contains(&value),
                "{filter:?} starts in bounds"
            );
        }
    }
}

#[test]
fn adjustments_map_each_colour() {
    let mut canvas = flat(4, 4, [100, 100, 100, 255]);
    apply(
        &mut canvas,
        &Filter::Brightness {
            brightness: 50,
            contrast: 0,
        },
        None,
    )
    .expect("room");
    assert_eq!(rgba(&canvas, 1, 1)[0], 228, "lighter by half the range");
    let mut canvas = flat(2, 2, [200, 10, 10, 255]);
    apply(
        &mut canvas,
        &Filter::HueSaturation {
            hue: 120,
            saturation: 0,
            lightness: 0,
        },
        None,
    )
    .expect("room");
    let [r, g, b, a] = rgba(&canvas, 0, 0);
    assert!(
        g > r && g > b && a == 255,
        "red turned a third of the way round is green"
    );
    let mut canvas = flat(2, 2, [90, 200, 30, 128]);
    apply(&mut canvas, &Filter::Desaturate, None).expect("room");
    let [r, g, b, a] = rgba(&canvas, 0, 0);
    assert!(r == g && g == b && a == 128, "grey, its alpha kept");
    apply(&mut canvas, &Filter::Threshold { level: 200 }, None).expect("room");
    assert_eq!(rgba(&canvas, 0, 0), [0, 0, 0, 128]);
    let mut canvas = flat(2, 2, [100, 160, 250, 255]);
    apply(&mut canvas, &Filter::Posterize { levels: 2 }, None).expect("room");
    assert_eq!(rgba(&canvas, 0, 0), [0, 255, 255, 255]);
}

#[test]
fn a_blur_spreads_a_pixel_and_is_held_to_the_selection() {
    let mut built =
        CanvasBuilder::new(21, 21, Kind::Rgba, Sample::Rgba([0, 0, 0, 255])).expect("fits");
    built.set(10, 10, Sample::Rgba([255; 4]));
    let mut canvas = built.finish();
    let unblurred = canvas.try_clone().expect("room");
    apply(&mut canvas, &Filter::Blur { radius: 3 }, None).expect("room");
    assert!(rgba(&canvas, 10, 10)[0] < 255, "the bright pixel spread");
    assert!(rgba(&canvas, 11, 10)[0] > 0, "onto its neighbours");
    assert_eq!(rgba(&canvas, 0, 0), [0, 0, 0, 255], "far off it, nothing");
    let mut held = unblurred;
    let left = Mask::rect(Bounds {
        x0: 0,
        y0: 0,
        x1: 10,
        y1: 21,
    })
    .expect("pixels");
    apply(&mut held, &Filter::Blur { radius: 3 }, Some(&left)).expect("room");
    assert_eq!(
        rgba(&held, 10, 10),
        [255; 4],
        "outside the selection, untouched"
    );
    assert!(
        rgba(&held, 9, 10)[0] > 0,
        "inside it, the light reaching in"
    );
}

#[test]
fn pixelating_makes_squares_of_their_mean() {
    let mut built =
        CanvasBuilder::new(8, 4, Kind::Rgba, Sample::Rgba([0, 0, 0, 255])).expect("fits");
    built.set(0, 0, Sample::Rgba([255, 255, 255, 255]));
    let mut canvas = built.finish();
    apply(&mut canvas, &Filter::Pixelate { cell: 4 }, None).expect("room");
    let first = rgba(&canvas, 0, 0);
    assert_eq!(first[0], 16, "one bright pixel in sixteen");
    assert_eq!(rgba(&canvas, 3, 3), first, "the whole square alike");
    assert_eq!(
        rgba(&canvas, 4, 0),
        [0, 0, 0, 255],
        "the next square its own"
    );
}

#[test]
fn noise_is_the_same_however_often_it_is_laid() {
    let noise = Filter::Noise {
        amount: 50,
        seed: 7,
    };
    let mut once = flat(16, 16, [128, 128, 128, 255]);
    let mut again = flat(16, 16, [128, 128, 128, 255]);
    apply(&mut once, &noise, None).expect("room");
    apply(&mut again, &noise, None).expect("room");
    assert_eq!(once, again, "the preview is what is laid");
    let differ = (0..16).any(|x| rgba(&once, x, 0) != rgba(&once, 0, 0));
    assert!(differ, "speckled");
}

#[test]
fn edges_light_where_colours_change() {
    let mut built =
        CanvasBuilder::new(10, 4, Kind::Rgba, Sample::Rgba([0, 0, 0, 255])).expect("fits");
    for x in 5..10 {
        for y in 0..4 {
            built.set(x, y, Sample::Rgba([255, 255, 255, 255]));
        }
    }
    let mut canvas = built.finish();
    apply(&mut canvas, &Filter::Edges, None).expect("room");
    assert_eq!(rgba(&canvas, 1, 1)[0], 0, "flat, dark");
    assert_eq!(rgba(&canvas, 5, 1)[0], 255, "at the edge, lit");
}

#[test]
fn a_palette_picture_takes_adjustments_through_its_palette_alone() {
    let palette = vec![[200, 10, 10, 255], [10, 10, 10, 255]];
    let kind = Kind::Indexed {
        depth: IndexDepth::One,
        palette: palette.clone(),
        masked: false,
    };
    let mut canvas = Canvas::new(4, 4, kind, Sample::Index(0, 255)).expect("fits");
    assert_eq!(
        apply(&mut canvas, &Filter::Desaturate, None),
        Err(FilterError::NeedsColour)
    );
    let grey = Filter::Desaturate
        .mapped_palette(&palette)
        .expect("an adjustment");
    assert!(grey.iter().all(|&[r, g, b, _]| r == g && g == b));
    assert!(Filter::Blur { radius: 1 }
        .mapped_palette(&palette)
        .is_none());
}

#[test]
fn inverting_turns_colours_over_within_the_selection_and_leaves_opacity() {
    let mut picture = flat(4, 1, [10, 20, 30, 40]);
    let chosen = Mask::rect(Bounds {
        x0: 0,
        y0: 0,
        x1: 2,
        y1: 1,
    })
    .expect("pixels");
    let written = apply(&mut picture, &Filter::Invert, Some(&chosen)).expect("inverts");
    assert!(!written.is_empty());
    assert_eq!(rgba(&picture, 1, 0), [245, 235, 225, 40]);
    assert_eq!(
        rgba(&picture, 2, 0),
        [10, 20, 30, 40],
        "outside the selection"
    );
    let palette = [[0, 10, 250, 255], [255, 255, 255, 128]];
    assert_eq!(
        Filter::Invert.mapped_palette(&palette),
        Some(vec![[255, 245, 5, 255], [0, 0, 0, 128]])
    );
    assert!(Filter::Invert.parameters().is_empty(), "applied at once");
    assert!(
        !Filter::ALL.contains(&Filter::Invert),
        "a command of its own"
    );
}
