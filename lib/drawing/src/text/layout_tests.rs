//! Tests for setting a text: faces from runs, the baseline grid, paint
//! parts, carets and pens, bounds, and text along a path.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_bezier::{Path, PathEl, Point, Subpath};
use tairix_collections::SharedSlice;
use tairix_colour::Rgba;

use crate::shape::PathShape;
use crate::style::{Paint, Stroke, Style};
use crate::testing::{shared, Ems, Wide};
use crate::text::{Baseline, Glyph, Glyphs, Run, RunPaint, RunStyle, Text, TextAlign, TextPath};
use crate::DrawingError;

const RED: Rgba = Rgba::new(255, 0, 0, 255);

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

fn styled(content: &str, runs: &[(usize, usize, RunStyle)]) -> Text {
    let runs: Vec<Run> = runs
        .iter()
        .map(|(start, end, style)| Run {
            start: *start,
            end: *end,
            style: Arc::new(style.clone()),
        })
        .collect();
    let text = Text {
        runs: SharedSlice::try_from_vec(runs).expect("held"),
        ..text(content)
    };
    assert_eq!(text.check(), Ok(()));
    text
}

/// The path through `points`, as an open polyline.
fn polyline(points: &[(f64, f64)]) -> Arc<PathShape> {
    let mut subpath = Subpath::new(Point::new(points[0].0, points[0].1));
    subpath.elements.extend(
        points[1..]
            .iter()
            .map(|&(x, y)| PathEl::Line(Point::new(x, y))),
    );
    let path = Path {
        subpaths: alloc::vec![subpath],
    };
    Arc::new(PathShape::from_path(&path).expect("a path"))
}

/// `content` along a line east from the origin and then south, `start` along.
fn along(content: &str, start: f64) -> Text {
    let mut text = text(content);
    text.baseline = Baseline::Path(Arc::new(TextPath {
        path: polyline(&[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)]),
        start,
    }));
    assert_eq!(text.check(), Ok(()));
    text
}

fn near(a: Point, b: Point) -> bool {
    a.distance(b) < 1e-9
}

#[test]
fn a_run_sets_its_characters_in_its_own_face() {
    let larger = RunStyle {
        size: Some(20.0),
        letter_spacing: Some(2.0),
        ..RunStyle::default()
    };
    let text = styled("abcd", &[(1, 3, larger)]);
    let set = text.set(&Ems).expect("set");
    let origins: Vec<Point> = set
        .glyphs
        .iter()
        .map(|placed| placed.at.map(Point::ZERO))
        .collect();
    assert_eq!(
        origins,
        [
            Point::new(100.0, 100.0),
            Point::new(110.0, 100.0),
            Point::new(132.0, 100.0),
            Point::new(154.0, 100.0),
        ]
    );
    let runs: Vec<Option<u32>> = set.glyphs.iter().map(|placed| placed.run).collect();
    assert_eq!(runs, [None, Some(0), Some(0), None]);
    let offsets: Vec<usize> = set.glyphs.iter().map(|placed| placed.offset).collect();
    assert_eq!(offsets, [0, 1, 2, 3]);
    let em = |index: usize| {
        let at = set.glyphs[index].at;
        at.map(Point::new(1.0, 0.0)).x - at.map(Point::ZERO).x
    };
    assert!((em(0) - 10.0).abs() < 1e-9 && (em(1) - 20.0).abs() < 1e-9);
}

#[test]
fn a_baseline_shift_raises_its_glyphs_without_moving_the_line() {
    let raised = RunStyle {
        baseline_shift: Some(0.5),
        ..RunStyle::default()
    };
    let lowered = RunStyle {
        baseline_shift: Some(-0.25),
        size: Some(20.0),
        ..RunStyle::default()
    };
    let text = styled("abc\nd", &[(1, 2, raised), (2, 3, lowered)]);
    let set = text.set(&Ems).expect("set");
    let heights: Vec<f64> = set
        .glyphs
        .iter()
        .map(|placed| placed.at.map(Point::ZERO).y)
        .collect();
    assert_eq!(heights, [100.0, 95.0, 105.0, 112.0]);
}

#[test]
fn lines_keep_the_text_s_own_grid_whatever_a_run_sets() {
    let huge = RunStyle {
        size: Some(40.0),
        ..RunStyle::default()
    };
    let text = styled("a\nb", &[(0, 1, huge)]);
    let set = text.set(&Ems).expect("set");
    assert_eq!(set.glyphs[1].at.map(Point::ZERO), Point::new(100.0, 112.0));
}

#[test]
fn a_setting_is_painted_in_parts_that_follow_the_runs_paints() {
    let red = RunStyle {
        fill: Some(RunPaint::Colour(RED)),
        ..RunStyle::default()
    };
    let larger = RunStyle {
        size: Some(20.0),
        ..RunStyle::default()
    };
    let unstroked = RunStyle {
        fill: Some(RunPaint::Colour(RED)),
        stroke: Some(RunPaint::None),
        ..RunStyle::default()
    };
    let text = styled(
        "abcde",
        &[
            (1, 2, red.clone()),
            (2, 3, larger),
            (3, 4, red),
            (4, 5, unstroked),
        ],
    );
    let set = text.set(&Ems).expect("set");
    let parts: Vec<_> = set
        .painted(&text)
        .map(|part| (part.glyphs, part.fill, part.stroke))
        .collect();
    assert_eq!(
        parts,
        [
            (0..1, None, None),
            (1..2, Some(RunPaint::Colour(RED)), None),
            (2..3, None, None),
            (3..4, Some(RunPaint::Colour(RED)), None),
            (4..5, Some(RunPaint::Colour(RED)), Some(RunPaint::None)),
        ]
    );
    let style = Style {
        stroke: Stroke {
            paint: Paint::Colour(Rgba::new(0, 0, 255, 255)),
            ..Stroke::default()
        },
        ..Style::default()
    };
    let last = set.painted(&text).last().expect("a part").style(&style);
    assert_eq!(last.fill, Paint::Colour(RED));
    assert_eq!(last.stroke.paint, Paint::None);
    assert!(
        (last.stroke.width - style.stroke.width).abs() < 1e-12,
        "the object's width"
    );
}

#[test]
fn the_caret_takes_the_size_of_the_character_before_it() {
    let larger = RunStyle {
        size: Some(20.0),
        ..RunStyle::default()
    };
    let text = styled("abc", &[(1, 2, larger)]);
    let sizes: Vec<f64> = (0..=3)
        .map(|at| text.caret(&Ems, at).expect("measured").size)
        .collect();
    assert_eq!(sizes, [10.0, 10.0, 20.0, 10.0]);
    let caret = text.caret(&Ems, 2).expect("measured");
    assert_eq!(caret.pen, Point::new(130.0, 100.0));
    assert_eq!(caret.direction, Point::new(1.0, 0.0));
}

#[test]
fn a_line_has_a_pen_before_each_character_and_one_past_the_last() {
    let text = styled(
        "ab\ncd",
        &[(
            4,
            5,
            RunStyle {
                weight: Some(700),
                ..RunStyle::default()
            },
        )],
    );
    let pens = text.pens(&Ems, 1, 3..5).expect("measured");
    let offsets: Vec<usize> = pens.iter().map(|pen| pen.offset).collect();
    assert_eq!(offsets, [3, 4, 5]);
    assert_eq!(pens[0].point, Point::new(100.0, 112.0));
    assert_eq!(pens[2].point, Point::new(120.0, 112.0));
    assert_eq!(pens[1].face.weight, 700);
    assert_eq!(pens[2].face.weight, 700, "the last pen has the last face");
}

#[test]
fn estimated_bounds_hold_every_glyph_whatever_its_run_sets() {
    let runs = [
        RunStyle {
            size: Some(30.0),
            ..RunStyle::default()
        },
        RunStyle {
            letter_spacing: Some(-25.0),
            ..RunStyle::default()
        },
        RunStyle {
            baseline_shift: Some(2.0),
            size: Some(5.0),
            ..RunStyle::default()
        },
        RunStyle {
            baseline_shift: Some(-3.0),
            ..RunStyle::default()
        },
    ];
    for align in [TextAlign::Start, TextAlign::Middle, TextAlign::End] {
        for (index, run) in runs.iter().enumerate() {
            let mut lines = styled("ab\tc\nWW", &[(index, index + 3, run.clone())]);
            lines.align = align;
            let mut on_path = lines.clone();
            on_path.content = shared("ab\tcWW");
            on_path.baseline = along("x", 40.0).baseline;
            for text in [lines, on_path] {
                let estimate = text.estimated_bounds().inflated(1e-9);
                let set = text.set(&Wide).expect("set");
                assert!(!set.glyphs.is_empty());
                for placed in &set.glyphs {
                    let inked = placed.glyph.ink().transformed(placed.at);
                    assert!(
                        estimate.contains_bounds(inked),
                        "{run:?} {align:?}: {inked:?} past {estimate:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_run_s_stroke_reaches_past_the_glyphs_where_the_object_s_does_not() {
    let stroked = RunStyle {
        stroke: Some(RunPaint::Colour(RED)),
        ..RunStyle::default()
    };
    let text = styled("ab", &[(0, 1, stroked)]);
    let style = Style {
        stroke: Stroke {
            paint: Paint::None,
            width: 4.0,
            ..Stroke::default()
        },
        ..Style::default()
    };
    assert!(text.stroke_reach(&style) >= 2.0);
    assert!(text.repaints());
    assert!(!self::text("ab").repaints());
    assert!(self::text("ab").stroke_reach(&style).abs() < 1e-12);
}

#[test]
fn text_on_a_path_lies_along_it_turned_to_its_direction() {
    let text = along("ab", 95.0);
    let set = text.set(&Ems).expect("set");
    assert_eq!(set.glyphs.len(), 2);
    let a = set.glyphs[0].at;
    assert!(
        near(a.map(Point::ZERO), Point::new(95.0, 0.0)),
        "a's midpoint is the corner"
    );
    let b = set.glyphs[1].at;
    assert!(
        near(b.map(Point::ZERO), Point::new(100.0, 5.0)),
        "b's midpoint is 10 down the south leg"
    );
    let x_axis = b.map(Point::new(1.0, 0.0)) - b.map(Point::ZERO);
    assert!(near(x_axis, Point::new(0.0, 10.0)), "turned to run south");
}

#[test]
fn a_glyph_whose_midpoint_is_off_the_path_is_not_drawn() {
    let offsets = |text: &Text| -> Vec<usize> {
        text.set(&Ems)
            .expect("set")
            .glyphs
            .iter()
            .map(|placed| placed.offset)
            .collect()
    };
    assert_eq!(
        offsets(&along("abc", -16.0)),
        [2],
        "a and b before the start"
    );
    assert_eq!(
        offsets(&along("abc", -15.0)),
        [1, 2],
        "b's midpoint on the start"
    );
    assert_eq!(offsets(&along("abc", 180.0)), [0, 1]);
    assert_eq!(offsets(&along("abc", 196.0)), [], "all past the end");
}

#[test]
fn alignment_stands_a_line_on_a_path_against_its_start() {
    let mut text = along("ab", 50.0);
    text.align = TextAlign::Middle;
    let set = text.set(&Ems).expect("set");
    assert!(near(
        set.glyphs[0].at.map(Point::ZERO),
        Point::new(40.0, 0.0)
    ));
    text.align = TextAlign::End;
    let set = text.set(&Ems).expect("set");
    assert!(near(
        set.glyphs[1].at.map(Point::ZERO),
        Point::new(40.0, 0.0)
    ));
}

#[test]
fn a_raised_glyph_on_a_path_stands_off_it_to_the_left_of_its_way() {
    let raised = RunStyle {
        baseline_shift: Some(0.5),
        ..RunStyle::default()
    };
    let mut text = along("ab", 105.0);
    text.runs = SharedSlice::try_from_vec(alloc::vec![Run {
        start: 0,
        end: 1,
        style: Arc::new(raised),
    }])
    .expect("held");
    let set = text.set(&Ems).expect("set");
    assert!(
        near(set.glyphs[0].at.map(Point::ZERO), Point::new(105.0, 5.0)),
        "raised five east of the south leg"
    );
}

#[test]
fn a_caret_on_a_path_is_held_to_it_and_turned_with_it() {
    let text = along("ab", 95.0);
    let caret = text.caret(&Ems, 1).expect("measured");
    assert!(near(caret.pen, Point::new(100.0, 5.0)));
    assert!(near(caret.direction, Point::new(0.0, 1.0)));
    let held = along("ab", 195.0).caret(&Ems, 2).expect("measured");
    assert!(near(held.pen, Point::new(100.0, 100.0)), "held to the end");
    let before = along("ab", -50.0).caret(&Ems, 0).expect("measured");
    assert!(near(before.pen, Point::ZERO), "held to the start");
}

#[test]
fn a_text_on_a_path_is_one_line_of_a_bounded_path() {
    let mut broken = along("ab", 0.0);
    broken.content = shared("a\nb");
    assert_eq!(broken.check(), Err(DrawingError::Invalid));
    let points: Vec<(f64, f64)> = (0..=crate::limits::MAX_TEXT_PATH_NODES)
        .map(|index| (f64::from(u32::try_from(index).expect("small")), 0.0))
        .collect();
    let mut long = along("ab", 0.0);
    long.baseline = Baseline::Path(Arc::new(TextPath {
        path: polyline(&points),
        start: 0.0,
    }));
    assert_eq!(long.check(), Err(DrawingError::TooComplex));
    let mut lost = along("ab", 0.0);
    lost.baseline = Baseline::Path(Arc::new(TextPath {
        path: polyline(&[(0.0, 0.0), (1.0, 0.0)]),
        start: f64::INFINITY,
    }));
    assert_eq!(lost.check(), Err(DrawingError::Invalid));
}

/// What a font was asked to prepare, in order.
#[derive(Default)]
struct Asked(RefCell<Vec<(alloc::string::String, alloc::string::String)>>);

impl Glyphs for Asked {
    fn glyph(&self, family: &str, weight: u16, italic: bool, c: char) -> Option<Arc<Glyph>> {
        Ems.glyph(family, weight, italic, c)
    }

    fn prepare(&self, family: &str, _weight: u16, _italic: bool, text: &str) {
        self.0.borrow_mut().push((family.into(), text.into()));
    }
}

#[test]
fn each_run_of_one_font_is_prepared_together() {
    let serif = RunStyle {
        family: Some(shared("serif")),
        ..RunStyle::default()
    };
    let red = RunStyle {
        fill: Some(RunPaint::Colour(RED)),
        ..RunStyle::default()
    };
    let text = styled("abcdef", &[(1, 3, serif), (4, 5, red)]);
    let asked = Asked::default();
    text.set(&asked).expect("set");
    let asked = asked.0.borrow();
    let asked: Vec<(&str, &str)> = asked
        .iter()
        .map(|(family, text)| (family.as_str(), text.as_str()))
        .collect();
    assert_eq!(
        asked,
        [("sans-serif", "a"), ("serif", "bc"), ("sans-serif", "def")],
        "a run changing no font joins its neighbours"
    );
}

#[test]
fn a_point_finds_the_character_boundary_nearest_it() {
    let two = text("abc\nde");
    let at =
        |text: &Text, x: f64, y: f64| text.offset_at(&Ems, Point::new(x, y)).expect("measured");
    assert_eq!(at(&two, 100.0, 97.0), 0);
    assert_eq!(at(&two, 114.0, 96.0), 1, "nearer b's start than its end");
    assert_eq!(at(&two, 116.0, 96.0), 2);
    assert_eq!(at(&two, 500.0, 99.0), 3, "past the line is its end");
    assert_eq!(
        at(&two, 111.0, 108.0),
        5,
        "the second line's letters are nearer"
    );
    assert_eq!(at(&two, 0.0, 900.0), 4, "below every line is the last");
    let on_path = along("ab", 95.0);
    assert_eq!(
        on_path
            .offset_at(&Ems, Point::new(101.0, 13.0))
            .expect("measured"),
        2,
        "the pen past b, down the south leg"
    );
}
