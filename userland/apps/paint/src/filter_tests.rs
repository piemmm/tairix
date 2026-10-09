use alloc::vec;

use tairix_image::IndexDepth;

use super::{apply, Filter, FilterError};
use crate::canvas::{Canvas, CanvasBuilder, Kind, Sample};
use crate::mask::Mask;
use crate::shape::Bounds;
use crate::tone::{Channel, ColourBalance, Curves, HueRange, HueRanges, Levels, WhiteBalance};

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
fn a_setting_is_held_to_its_bounds() {
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
            assert_eq!(parameter.value_of(parameter.permille_of(value)), value);
        }
    }
}

#[test]
fn the_adjustments_start_changing_nothing_and_the_filters_do_not() {
    for filter in Filter::ALL {
        let adjustment = matches!(
            filter,
            Filter::Brightness { .. }
                | Filter::HueSaturation(_)
                | Filter::ColourBalance(_)
                | Filter::Levels(_)
                | Filter::Curves(_)
                | Filter::WhiteBalance(_)
        );
        assert_eq!(filter.is_identity(), adjustment, "{filter:?}");
        assert!(filter.same_kind(&filter));
    }
    assert!(!Filter::Blur { radius: 2 }.same_kind(&Filter::Sharpen {
        amount: 1,
        radius: 2
    }));
    assert!(Filter::Blur { radius: 2 }.same_kind(&Filter::Blur { radius: 9 }));
    assert!(!Filter::Edges.has_settings());
    assert!(Filter::Levels(Levels::IDENTITY).has_settings());
}

#[test]
fn levels_curves_white_balance_and_colour_balance_map_through_the_worker() {
    let mut levels = Levels::IDENTITY;
    levels.of_mut(Channel::Composite).white = 128;
    let mut canvas = flat(2, 2, [64, 128, 200, 255]);
    apply(&mut canvas, &Filter::Levels(levels), None).expect("room");
    assert_eq!(rgba(&canvas, 0, 0), [128, 255, 255, 255]);
    let mut curves = Curves::IDENTITY;
    curves.of_mut(Channel::Red).set(1, (255, 0));
    let mut canvas = flat(2, 2, [255, 10, 10, 200]);
    apply(&mut canvas, &Filter::Curves(curves), None).expect("room");
    assert_eq!(rgba(&canvas, 1, 1), [0, 10, 10, 200]);
    let mut canvas = flat(2, 2, [128, 128, 128, 255]);
    let warm = WhiteBalance {
        kelvin: 9000,
        tint: 0,
    };
    apply(&mut canvas, &Filter::WhiteBalance(warm), None).expect("room");
    let [r, _, b, _] = rgba(&canvas, 0, 0);
    assert!(r > b, "warmed");
    let mut balance = ColourBalance::NEUTRAL;
    balance.keep_luminosity = false;
    balance.tones[1][1] = 100;
    let mut canvas = flat(2, 2, [128, 128, 128, 255]);
    apply(&mut canvas, &Filter::ColourBalance(balance), None).expect("room");
    let [r, g, _, _] = rgba(&canvas, 0, 0);
    assert!(g > r, "the midtones greened");
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
    let mut turned = HueRanges::IDENTITY;
    turned.of_mut(HueRange::Master).hue = 120;
    apply(&mut canvas, &Filter::HueSaturation(turned), None).expect("room");
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

/// A pixel the selection only partly chooses takes that share of the
/// filtered colour, weighed by alpha: a clear pixel the blur reaches turns
/// the colour spread onto it, never a darkening of the clear it was.
#[test]
fn a_blur_through_a_soft_selection_keeps_the_colour_it_spreads() {
    let mut built = CanvasBuilder::new(21, 21, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    for x in 0..10 {
        for y in 0..21 {
            built.set(x, y, Sample::Rgba([255, 0, 0, 255]));
        }
    }
    let mut canvas = built.finish();
    let mut whole = canvas.try_clone().expect("room");
    let blur = Filter::Blur { radius: 3 };
    apply(&mut whole, &blur, None).expect("room");
    let picture = Bounds {
        x0: 0,
        y0: 0,
        x1: 21,
        y1: 21,
    };
    let held = Bounds { x1: 12, ..picture };
    let soft = Mask::rect(held)
        .expect("pixels")
        .feathered(4, picture)
        .expect("room")
        .expect("pixels");
    apply(&mut canvas, &blur, Some(&soft)).expect("room");
    let mut reached = 0;
    for x in 10..21 {
        let share = soft.at(i64::from(x), 10);
        let spread = rgba(&whole, x, 10);
        if !(1..u8::MAX).contains(&share) || spread[3] == 0 {
            continue;
        }
        reached += 1;
        let [r, g, b, a] = rgba(&canvas, x, 10);
        assert_eq!([r, g, b], spread[..3], "pixel {x} takes the colour spread");
        let laid = (u32::from(spread[3]) * u32::from(share) + 127) / 255;
        assert_eq!(u32::from(a), laid, "pixel {x} as opaque as its share");
    }
    assert!(reached > 0, "the blur reached a partly chosen clear pixel");
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
