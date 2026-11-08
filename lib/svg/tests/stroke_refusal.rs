//! The stroker under real allocation failure: it runs in the processes that
//! draw — the compositor's cursors and icons, an editor's canvas — so a
//! refused request is answered as [`SvgError::OutOfMemory`], never an abort,
//! and nothing the stroke held outlives the answer.

use tairix_fuzzseed::meter::refused_at_each;
use tairix_svg::geom::{LineCap, LineJoin, StrokeStyle, SubPath};
use tairix_svg::stroke::stroke_outline;
use tairix_svg::SvgError;

#[global_allocator]
static ALLOCATOR: tairix_fuzzseed::meter::Metered = tairix_fuzzseed::meter::Metered;

#[test]
fn a_stroke_the_heap_refuses_is_answered_not_aborted() {
    let zigzag = SubPath {
        points: vec![
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (20.0, 4.0),
            (30.0, 12.0),
        ],
        closed: false,
    };
    let ring = SubPath {
        points: vec![
            (40.0, 0.0),
            (50.0, 0.0),
            (50.0, 10.0),
            (40.0, 10.0),
            (40.0, 0.0),
        ],
        closed: true,
    };
    let dot = SubPath {
        points: vec![(60.0, 5.0)],
        closed: false,
    };
    let subpaths = [zigzag, ring, dot];
    for (join, cap, dashes) in [
        (LineJoin::Round, LineCap::Round, vec![]),
        (LineJoin::Miter, LineCap::Square, vec![3.0, 2.0, 1.0]),
        (LineJoin::Bevel, LineCap::Butt, vec![4.0, 1.5]),
    ] {
        let style = StrokeStyle {
            width: 2.5,
            cap,
            join,
            miter_limit: 4.0,
            dashes,
            dash_offset: 1.0,
        };
        refused_at_each(
            || stroke_outline(&subpaths, &style, 0.05, 1 << 16),
            |error| *error == SvgError::OutOfMemory,
        );
    }
}
