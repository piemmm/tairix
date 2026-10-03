use alloc::vec;

use tairix_image::IndexDepth;

use super::{lay, mix, Gradient};
use crate::canvas::{Canvas, Kind, Sample};
use crate::colour::Ink;
use crate::mask::Mask;
use crate::shape::{Bounds, Point, FX};
use crate::tool::GradientShape;

fn across(shape: GradientShape, inks: (Ink, Ink)) -> Gradient {
    Gradient {
        from: Point::centre_of(0, 5),
        to: Point::centre_of(10, 5),
        shape,
        inks,
    }
}

const BLACK_TO_WHITE: (Ink, Ink) = (Ink::Colour([0, 0, 0, 255]), Ink::Colour([255; 4]));

#[test]
fn bands_run_along_the_drag_and_hold_past_either_end() {
    let gradient = across(GradientShape::Linear, BLACK_TO_WHITE);
    assert_eq!(gradient.at(0, 5), 0);
    assert_eq!(gradient.at(10, 5), 255);
    assert_eq!(gradient.at(5, 5), 127);
    assert_eq!(gradient.at(5, 40), 127, "the same band however far across");
    assert_eq!(gradient.at(-30, 5), 0, "held before its start");
    assert_eq!(gradient.at(90, 5), 255, "and past its end");
    let click = Gradient {
        to: Point::centre_of(0, 5),
        ..gradient
    };
    assert!(!click.spans());
    assert!(gradient.spans());
}

#[test]
fn rings_spread_out_from_where_the_drag_began() {
    let gradient = across(GradientShape::Radial, BLACK_TO_WHITE);
    assert_eq!(gradient.at(0, 5), 0);
    assert_eq!(gradient.at(0, 10), gradient.at(5, 5), "the same ring");
    assert_eq!(gradient.at(0, 15), 255);
    assert_eq!(
        Point::centre_of(10, 5).x - Point::centre_of(0, 5).x,
        10 * FX
    );
}

#[test]
fn a_colour_fading_to_clear_keeps_its_hue() {
    let kind = Kind::Rgba;
    let half = mix((Ink::Colour([200, 40, 10, 255]), Ink::Clear), 128, &kind);
    assert_eq!(
        &half[..3],
        &[200, 40, 10],
        "no darkening toward the clear end"
    );
    assert_eq!(half[3], 127);
    assert_eq!(mix((Ink::Clear, Ink::Clear), 99, &kind), [0; 4]);
    assert_eq!(mix(BLACK_TO_WHITE, 0, &kind), [0, 0, 0, 255]);
    assert_eq!(mix(BLACK_TO_WHITE, 255, &kind), [255; 4]);
}

#[test]
fn a_gradient_is_laid_within_the_selection_held() {
    let mut canvas = Canvas::new(20, 10, Kind::Rgba, Sample::Rgba([9, 9, 9, 255])).expect("fits");
    let gradient = across(GradientShape::Linear, BLACK_TO_WHITE);
    let held = Mask::rect(Bounds {
        x0: 2,
        y0: 2,
        x1: 8,
        y1: 8,
    })
    .expect("pixels");
    let tiles = lay(&mut canvas, &gradient, Some(&held)).expect("room");
    assert_eq!(tiles.len(), 1);
    assert_eq!(
        canvas.sample(1, 5),
        Some(Sample::Rgba([9, 9, 9, 255])),
        "outside it"
    );
    let Some(Sample::Rgba(laid)) = canvas.sample(5, 5) else {
        panic!("a colour");
    };
    assert_eq!(laid[0], 127, "half way along");
    let mut whole = Canvas::new(20, 10, Kind::Rgba, Sample::Rgba([9, 9, 9, 255])).expect("fits");
    lay(&mut whole, &gradient, None).expect("room");
    assert_eq!(whole.sample(19, 9), Some(Sample::Rgba([255; 4])));
}

#[test]
fn a_palette_picture_takes_a_dither_of_its_two_entries() {
    let kind = Kind::Indexed {
        depth: IndexDepth::Two,
        palette: vec![[0, 0, 0, 255], [255, 255, 255, 255]],
        masked: false,
    };
    let mut canvas = Canvas::new(16, 4, kind, Sample::Index(0, 255)).expect("fits");
    let gradient = Gradient {
        from: Point::centre_of(0, 0),
        to: Point::centre_of(15, 0),
        shape: GradientShape::Linear,
        inks: (Ink::Index(0), Ink::Index(1)),
    };
    lay(&mut canvas, &gradient, None).expect("room");
    let lit = |x0: u32, x1: u32| {
        (x0..x1)
            .flat_map(|x| (0..4).map(move |y| (x, y)))
            .filter(|&(x, y)| canvas.sample(x, y) == Some(Sample::Index(1, 255)))
            .count()
    };
    assert_eq!(lit(0, 1), 0, "the first entry alone at the start");
    assert_eq!(lit(15, 16), 4, "the second alone at the end");
    assert!(
        lit(0, 8) < lit(8, 16),
        "more of the second the further along"
    );
    assert!(lit(6, 10) > 0 && lit(6, 10) < 16, "mixed in the middle");
}
