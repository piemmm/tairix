//! Tests for the text object: what it holds, its estimated bounds, where a
//! caret stands, and what a setting covers.

use alloc::sync::Arc;

use tairix_bezier::Point;
use tairix_collections::SharedSlice;

use super::{Baseline, Text, TextAlign};
use crate::testing::{shared, Ems, Wide};
use crate::DrawingError;

fn text(content: &str) -> Text {
    Text {
        content: shared(content),
        family: shared("sans-serif"),
        size: 10.0,
        weight: 400,
        italic: false,
        letter_spacing: 0.0,
        align: TextAlign::Start,
        baseline: Baseline::Lines {
            origin: Point::new(100.0, 100.0),
            line_height: 1.2,
        },
        runs: SharedSlice::new(),
    }
}

#[test]
fn estimated_bounds_hold_every_line() {
    let two = text("Hello\nWorld!");
    let bounds = two.estimated_bounds();
    assert!(bounds.min.y <= 100.0 - 10.0, "above the first baseline");
    assert!(bounds.max.y >= 112.0, "below the second");
    assert!(
        bounds.max.x >= 100.0 + 6.0 * 10.0,
        "as wide as the widest line"
    );
    let mut centred = text("Hello");
    centred.align = TextAlign::Middle;
    let centre = centred.estimated_bounds().centre();
    assert!((centre.x - 100.0).abs() < 1e-9, "centred on the origin");
}

#[test]
fn estimated_bounds_hold_every_setting_of_every_glyph() {
    for content in ["\t\tW", "WWWW\n\tW", "abc", "a b\tc\nd"] {
        for spacing in [-20.0, -3.0, 0.0, 4.0] {
            for align in [TextAlign::Start, TextAlign::Middle, TextAlign::End] {
                let mut wide = text(content);
                wide.letter_spacing = spacing;
                wide.align = align;
                let estimate = wide.estimated_bounds().inflated(1e-9);
                let set = wide.set(&Wide).expect("set");
                assert!(!set.glyphs.is_empty());
                for placed in &set.glyphs {
                    let inked = placed.glyph.ink().transformed(placed.at);
                    assert!(
                        estimate.contains_bounds(inked),
                        "{content:?} at {spacing} {align:?}: {inked:?} past {estimate:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_pen_stands_where_set_text_puts_the_next_character() {
    let mut two = text("ab\nhi c");
    two.letter_spacing = 1.0;
    let pen = |text: &Text, at| text.caret(&Ems, at).expect("measured").pen;
    assert_eq!(pen(&two, 0), Point::new(100.0, 100.0));
    assert_eq!(
        pen(&two, 2),
        Point::new(100.0 + 2.0 * 11.0, 100.0),
        "the first line's end"
    );
    assert_eq!(
        pen(&two, 3),
        Point::new(100.0, 112.0),
        "the second line's start"
    );
    assert_eq!(
        pen(&two, 5),
        Point::new(100.0 + 10.0 + 1.0 + 5.0 + 1.0, 112.0)
    );
    assert_eq!(
        pen(&two, 99),
        pen(&two, two.content.len()),
        "past the end is the end"
    );
    let set = two.set(&Ems).expect("set");
    let c = set.glyphs.last().expect("the c").at.map(Point::ZERO);
    assert_eq!(c, pen(&two, two.content.len() - 1), "where set puts the c");
    let mut centred = text("ab");
    centred.align = TextAlign::Middle;
    assert_eq!(
        pen(&centred, 0),
        Point::new(90.0, 100.0),
        "half the line before the origin"
    );
    let unknown = text("é?");
    assert_eq!(
        pen(&unknown, 1),
        pen(&unknown, 0),
        "within a character is its start"
    );
    assert!(
        (pen(&unknown, 3).x - pen(&unknown, 2).x - 6.0).abs() < 1e-9,
        "an unknown glyph's advance"
    );
}

#[test]
fn text_refuses_what_a_drawing_cannot_hold() {
    assert_eq!(text("ok\tfine").check(), Ok(()));
    assert_eq!(text("bell\u{7}").check(), Err(DrawingError::Invalid));
    let mut weightless = text("x");
    weightless.weight = 0;
    assert_eq!(weightless.check(), Err(DrawingError::Invalid));
    let long: alloc::string::String =
        core::iter::repeat_n('x', crate::limits::MAX_TEXT_BYTES + 1).collect();
    assert_eq!(text(&long).check(), Err(DrawingError::TooComplex));
}

#[test]
fn a_setting_covers_what_its_outline_covers_however_its_glyphs_are_placed() {
    use tairix_bezier::{Affine, FillRule, Path, PathEl, Subpath};

    let ring = |corners: [(f64, f64); 4]| {
        let mut ring = Subpath::new(Point::new(corners[0].0, corners[0].1));
        for (x, y) in &corners[1..] {
            ring.elements.push(PathEl::Line(Point::new(*x, *y)));
        }
        ring.closed = true;
        ring
    };
    let holed = Arc::new(super::Glyph::new(
        Path {
            subpaths: alloc::vec![
                ring([(0.1, -0.8), (0.9, -0.8), (0.9, 0.0), (0.1, 0.0)]),
                ring([(0.3, -0.6), (0.3, -0.2), (0.7, -0.2), (0.7, -0.6)]),
            ],
        },
        1.0,
    ));
    let placed = |at: Affine| super::Placed {
        glyph: Arc::clone(&holed),
        at,
        run: None,
        offset: 0,
    };
    let size = Affine::scale(10.0, 10.0);
    let setting = super::Setting {
        glyphs: alloc::vec![
            placed(size.then(Affine::translate(10.0, 50.0))),
            placed(
                size.then(Affine::scale(-1.0, 1.0))
                    .then(Affine::translate(40.0, 50.0))
            ),
            placed(Affine::scale(0.0, 10.0)),
        ],
        complete: true,
    };
    let outline = setting.outline(|_| true).expect("held");
    for x in 0..50 {
        for y in 35..55 {
            let point = Point::new(f64::from(x) + 0.5, f64::from(y) + 0.5);
            assert_eq!(
                setting.covers(point),
                outline.contains(point, FillRule::NonZero),
                "at {point:?}"
            );
        }
    }
    assert!(setting.covers(Point::new(11.5, 49.5)), "the glyph's rim");
    assert!(!setting.covers(Point::new(15.0, 46.0)), "its hole");
    assert!(setting.covers(Point::new(38.5, 49.5)), "the mirrored rim");
    assert!(!setting.covers(Point::new(35.0, 46.0)), "the mirrored hole");
}
