use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::stage::testkit::{feed, parse, Memory};

const SURFACE: Surface = Surface {
    eig: (1, 1),
    origin: (0, 0),
    window: Bounds {
        left: 0,
        bottom: 0,
        right: 63,
        top: 63,
    },
};

const INTERIOR: u32 = class::INTERIOR | class::INTERIOR_BOUNDARY;

const ONCE: Use = Use {
    each: false,
    make_clip: false,
    within_clip: false,
};

/// A square `side` pixels from pixel `(x, y)`, in internal units at two
/// OS units a pixel.
fn square(x: i32, y: i32, side: i32) -> Vec<Element> {
    let u = 512;
    let (x0, y0, x1, y1) = (x * u, y * u, (x + side) * u, (y + side) * u);
    parse(&alloc::format!(
        "|2 {x0} {y0} |8 {x1} {y0} |8 {x1} {y1} |8 {x0} {y1} |5"
    ))
}

fn plotted(path: &[Element], using: Use, clip: Vec<Span>) -> (Vec<Span>, Fill) {
    let mut memory = Memory::default();
    let mut fill = Fill::new(SURFACE, INTERIOR, using, clip, usize::MAX);
    feed(&mut fill, &mut memory, path).expect("filled");
    (memory.plotted, fill)
}

#[test]
fn a_square_fills_the_pixels_whose_centres_it_holds() {
    let (spans, _) = plotted(&square(2, 3, 4), ONCE, Vec::new());
    assert_eq!(spans, [(3, 2, 5), (4, 2, 5), (5, 2, 5), (6, 2, 5)]);
}

#[test]
fn the_origin_and_eigen_factors_place_a_point_to_the_256th_of_a_pixel() {
    let surface = Surface {
        eig: (1, 2),
        origin: (3, 5),
        ..SURFACE
    };
    assert_eq!(
        surface.place(512, 1024),
        ((512 + 768) >> 1, (1024 + 1280) >> 2)
    );
    assert_eq!(surface.place(-1, 0), (767 >> 1, 1280 >> 2));
}

#[test]
fn a_subpath_at_a_time_plots_each_alone_so_overlaps_plot_twice() {
    let mut path = square(2, 2, 4);
    path.extend(square(4, 4, 4));
    let each = Use { each: true, ..ONCE };
    let (spans, _) = plotted(&path, each, Vec::new());
    let twice = spans
        .iter()
        .filter(|&&(row, left, _)| row == 4 && left <= 4)
        .count();
    assert_eq!(twice, 2, "row 4 is plotted by both squares: {spans:?}");
    let (once, _) = plotted(&path, ONCE, Vec::new());
    assert!(once.len() < spans.len());
}

#[test]
fn a_clip_made_plots_nothing_and_holds_a_later_fill_within_it() {
    let make = Use {
        make_clip: true,
        ..ONCE
    };
    let (spans, fill) = plotted(&square(0, 0, 4), make, Vec::new());
    assert!(spans.is_empty());
    let clip = normalised(fill.made);
    assert_eq!(clip, [(0, 0, 3), (1, 0, 3), (2, 0, 3), (3, 0, 3)]);
    let within = Use {
        within_clip: true,
        ..ONCE
    };
    let (spans, fill) = plotted(&square(2, 2, 4), within, clip.clone());
    assert_eq!(spans, [(2, 2, 3), (3, 2, 3)]);
    assert_eq!(fill.into_clip(), clip, "the clip is kept");
}

#[test]
fn before_any_clip_is_made_a_fill_within_it_plots_nothing() {
    let within = Use {
        within_clip: true,
        ..ONCE
    };
    let (spans, _) = plotted(&square(2, 2, 4), within, Vec::new());
    assert!(spans.is_empty());
}

#[test]
fn a_curve_must_be_flattened_and_dots_add_no_segment_twice() {
    let mut fill = Fill::new(SURFACE, INTERIOR, ONCE, Vec::new(), usize::MAX);
    let mut memory = Memory::default();
    assert_eq!(
        feed(&mut fill, &mut memory, &parse("|2 0 0 |6 0 9 9 9 9 0")),
        Err(Stop::Refused(Refusal::NeedsFlattening))
    );
    let mut fill = Fill::new(SURFACE, INTERIOR, ONCE, Vec::new(), usize::MAX);
    for element in parse("|2 0 0 |8 512 0 |8 512 0 |8 512 0 |7 512 0") {
        fill.element(&mut memory, element).expect("taken");
    }
    assert_eq!(fill.segments.len(), 1);
    let mut fill = Fill::new(SURFACE, INTERIOR, ONCE, Vec::new(), usize::MAX);
    for element in parse("|2 512 512 |8 512 512") {
        fill.element(&mut memory, element).expect("taken");
    }
    assert_eq!(fill.segments.len(), 1, "a lone dot still marks its pixel");
}

#[test]
fn spans_merge_where_they_touch() {
    assert_eq!(
        normalised(vec![(1, 5, 6), (0, 0, 2), (1, 0, 3), (1, 4, 4), (0, 4, 5)]),
        [(0, 0, 2), (0, 4, 5), (1, 0, 6)]
    );
}

#[test]
fn a_fill_past_its_bound_is_refused_rather_than_held() {
    let mut fill = Fill::new(SURFACE, INTERIOR, ONCE, Vec::new(), 3);
    let mut memory = Memory::default();
    let path = parse("|2 0 0 |8 512 0 |8 512 512 |8 0 512 |5");
    assert_eq!(feed(&mut fill, &mut memory, &path), Err(Stop::NoRoom));
    assert!(memory.plotted.is_empty());
    assert!(most_segments(1 << 20) > 1000);
}
