//! Snapping a point to what a drawing offers: its guides, its page's edges
//! and centre, and its grid when shown.

use tairix_bezier::Point;
use tairix_drawing::{Drawing, GuideAxis};
use tairix_util::mathf::{fabs, round};

/// `point` moved, each way on its own, to the nearest of what it may snap to
/// within `reach`; left where it is where nothing is that near.
#[must_use]
pub fn snapped(drawing: &Drawing, point: Point, reach: f64, grid: bool) -> Point {
    let page = drawing.page().bounds();
    let cell = drawing.grid().spacing / f64::from(drawing.grid().subdivisions.max(1));
    let on_grid = |value: f64| (grid && cell > 0.0).then(|| round(value / cell) * cell);
    let guides = |axis: GuideAxis| {
        drawing
            .guides()
            .iter()
            .filter(move |guide| guide.axis == axis)
            .map(|guide| guide.position)
    };
    let nearest = |value: f64, candidates: &mut dyn Iterator<Item = f64>| {
        candidates
            .filter(|candidate| fabs(candidate - value) <= reach)
            .min_by(|a, b| fabs(a - value).total_cmp(&fabs(b - value)))
            .unwrap_or(value)
    };
    let mut across = [page.min.x, page.centre().x, page.max.x]
        .into_iter()
        .chain(guides(GuideAxis::Vertical))
        .chain(on_grid(point.x));
    let mut down = [page.min.y, page.centre().y, page.max.y]
        .into_iter()
        .chain(guides(GuideAxis::Horizontal))
        .chain(on_grid(point.y));
    Point::new(nearest(point.x, &mut across), nearest(point.y, &mut down))
}

#[cfg(test)]
mod tests {
    use tairix_bezier::Point;
    use tairix_drawing::{Children, Drawing, Grid, Guide, GuideAxis, Page, Unit};

    use super::snapped;

    #[test]
    fn a_point_snaps_to_what_is_near_and_no_further() {
        let drawing = Drawing::new(
            Page {
                width: 100.0,
                height: 50.0,
                unit: Unit::Px,
                background: None,
            },
            Children::new(),
            tairix_drawing::SharedSlice::try_from_slice(&[Guide {
                axis: GuideAxis::Vertical,
                position: 30.0,
            }])
            .expect("held"),
            Grid {
                spacing: 10.0,
                subdivisions: 1,
            },
        )
        .expect("a drawing");
        assert_eq!(
            snapped(&drawing, Point::new(31.0, 49.0), 2.0, false),
            Point::new(30.0, 50.0),
            "a guide and the page's edge"
        );
        assert_eq!(
            snapped(&drawing, Point::new(37.0, 21.0), 2.0, false),
            Point::new(37.0, 21.0),
            "nothing near"
        );
        assert_eq!(
            snapped(&drawing, Point::new(37.0, 21.0), 2.0, true),
            Point::new(37.0, 20.0),
            "the grid, within reach only"
        );
    }
}
