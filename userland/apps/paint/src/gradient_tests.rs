use alloc::vec;

use tairix_image::IndexDepth;

use super::{lay, Gradient};
use crate::canvas::{Canvas, Kind, Sample};
use crate::colour::Ink;
use crate::compose::between;
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
    let laying = gradient.on(&Kind::Rgba);
    assert_eq!(laying.share(0, 5), 0);
    assert_eq!(laying.share(10, 5), 255);
    assert_eq!(laying.share(5, 5), 127);
    assert_eq!(laying.share(5, 40), 127, "the same band however far across");
    assert_eq!(laying.share(-30, 5), 0, "held before its start");
    assert_eq!(laying.share(90, 5), 255, "and past its end");
    let click = Gradient {
        to: Point::centre_of(0, 5),
        ..gradient
    };
    assert!(!click.spans());
    assert!(gradient.spans());
    assert_eq!(
        click.on(&Kind::Rgba).share(3, 3),
        255,
        "a click lays its second ink"
    );
}

#[test]
fn rings_spread_out_from_where_the_drag_began() {
    let laying = across(GradientShape::Radial, BLACK_TO_WHITE).on(&Kind::Rgba);
    assert_eq!(laying.share(0, 5), 0);
    assert_eq!(laying.share(0, 10), laying.share(5, 5), "the same ring");
    assert_eq!(laying.share(0, 15), 255);
    assert_eq!(
        Point::centre_of(10, 5).x - Point::centre_of(0, 5).x,
        10 * FX
    );
}

/// A ring's share is read off where each share begins rather than a root a
/// pixel, and is exactly `⌊√d²⌋·255 / ⌊√length⌋`, wherever the drag ran.
#[test]
fn a_rings_share_is_exactly_its_distance_over_the_drag() {
    let drags = [
        (Point::centre_of(0, 5), Point::centre_of(10, 5)),
        (Point { x: 0, y: 0 }, Point { x: 1, y: 0 }),
        (
            Point {
                x: 5 * FX + 3,
                y: 7 * FX,
            },
            Point {
                x: 5 * FX + 3,
                y: 7 * FX + 1,
            },
        ),
        (
            Point { x: -9000, y: 400 },
            Point {
                x: 123_456,
                y: -777,
            },
        ),
        (Point::centre_of(3, 4), Point::centre_of(40, 31)),
    ];
    for (from, to) in drags {
        let gradient = Gradient {
            from,
            to,
            ..across(GradientShape::Radial, BLACK_TO_WHITE)
        };
        let laying = gradient.on(&Kind::Rgba);
        let (dx, dy) = (i128::from(to.x - from.x), i128::from(to.y - from.y));
        let radius = (dx * dx + dy * dy).isqrt().max(1);
        for y in -60..60 {
            for x in -60..60 {
                let vx = i128::from(x * FX + FX / 2 - from.x);
                let vy = i128::from(y * FX + FX / 2 - from.y);
                let want = ((vx * vx + vy * vy).isqrt() * 255 / radius).clamp(0, 255);
                assert_eq!(
                    i128::from(laying.share(x, y)),
                    want,
                    "pixel ({x}, {y}) of the drag {from:?} to {to:?}"
                );
            }
        }
    }
}

#[test]
fn a_colour_picture_takes_the_blend_mixed_once_for_each_share() {
    let red = [200, 40, 10, 255];
    let gradient = across(GradientShape::Linear, (Ink::Colour(red), Ink::Clear));
    let laying = gradient.on(&Kind::Rgba);
    for x in -2..=12 {
        let share = laying.share(x, 5);
        assert_eq!(
            laying.laid((x, 5), Sample::Rgba([0; 4]), u8::MAX),
            Sample::Rgba(between(red, [0; 4], share)),
            "pixel {x}, {share} of the way along"
        );
    }
    let clear = across(GradientShape::Radial, (Ink::Clear, Ink::Clear)).on(&Kind::Rgba);
    let below = Sample::Rgba([9, 9, 9, 255]);
    assert_eq!(
        clear.laid((3, 5), below, u8::MAX),
        below,
        "clear lays nothing"
    );
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
