//! Host tests of grid fitting: edges land on their phase of the pixel grid,
//! the points between them follow in proportion, and nothing changes order.

use alloc::vec;
use alloc::vec::Vec;

use tairix_svg::geom::SubPath;

use super::{GridFit, Phase};

/// The side every drawing here is fitted in: its centre, 12, is far from
/// the edges under test.
const SIDE: u32 = 24;

fn fitted(mut contours: Vec<SubPath>, phase: Phase) -> Vec<SubPath> {
    let fit = GridFit::new(SIDE, [(contours.as_slice(), phase)]);
    fit.apply(&mut contours);
    contours
}

/// Whether two fitted coordinates are the same grid point.
fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn rectangle(left: f64, top: f64, right: f64, bottom: f64) -> SubPath {
    SubPath::closed(vec![
        (left, top),
        (right, top),
        (right, bottom),
        (left, bottom),
    ])
}

#[test]
fn a_filled_edge_lands_on_a_pixel_boundary() {
    let fitted = fitted(vec![rectangle(1.3, 2.7, 10.6, 8.2)], Phase::Boundary);
    assert_eq!(
        fitted[0].points,
        vec![(1.0, 3.0), (11.0, 3.0), (11.0, 8.0), (1.0, 8.0)]
    );
}

#[test]
fn an_odd_stroke_s_centre_lands_on_a_pixel_centre() {
    let line = SubPath::open(vec![(4.2, 1.9), (4.2, 9.3)]);
    let fitted = fitted(vec![line], Phase::of_stroke(1.0));
    assert_eq!(fitted[0].points, vec![(4.5, 1.5), (4.5, 9.5)]);
}

#[test]
fn a_stroke_s_phase_follows_its_whole_width() {
    assert_eq!(Phase::of_stroke(1.0), Phase::Centre);
    assert_eq!(Phase::of_stroke(2.0), Phase::Boundary);
    assert_eq!(Phase::of_stroke(3.0), Phase::Centre);
}

#[test]
fn a_point_between_two_edges_moves_in_proportion() {
    let shape = SubPath::closed(vec![(2.2, 2.2), (6.7, 2.2), (4.45, 6.7)]);
    let fitted = fitted(vec![shape], Phase::Boundary);
    assert_eq!(
        fitted[0].points[2],
        (4.5, 7.0),
        "the apex halfway between the sides"
    );
}

#[test]
fn points_beyond_the_outermost_edges_move_with_them() {
    let fit = GridFit::new(
        SIDE,
        [([rectangle(2.2, 2.2, 6.7, 6.7)].as_slice(), Phase::Boundary)],
    );
    let mut stray = vec![SubPath::open(vec![(1.2, 9.7)])];
    fit.apply(&mut stray);
    let (x, y) = stray[0].points[0];
    assert!(near(x, 1.0) && near(y, 10.0), "({x}, {y})");
}

#[test]
fn distinct_edges_snapping_to_one_pixel_keep_a_pixel_between_them() {
    let fitted = fitted(vec![rectangle(3.6, 1.0, 4.3, 5.0)], Phase::Boundary);
    let xs: Vec<f64> = fitted[0].points.iter().map(|point| point.0).collect();
    assert_eq!(
        xs,
        vec![3.0, 4.0, 4.0, 3.0],
        "the outer edge gives way outward"
    );
}

#[test]
fn edges_all_but_one_share_their_pixel() {
    let fitted = fitted(vec![rectangle(3.6, 1.0, 3.9, 5.0)], Phase::Boundary);
    let xs: Vec<f64> = fitted[0].points.iter().map(|point| point.0).collect();
    assert_eq!(xs, vec![4.0, 4.0, 4.0, 4.0]);
}

#[test]
fn a_ring_s_extremes_land_on_the_grid_and_it_keeps_its_order() {
    let ring = crate::symbol::flatten(&[crate::symbol::circle(6.3, 6.3, 3.8)], 0.01)
        .expect("a circle flattens");
    let source = ring[0].points.clone();
    let fitted = fitted(ring, Phase::of_stroke(1.0));
    let points = &fitted[0].points;
    let xs = points.iter().map(|point| point.0);
    let (low, high) = xs.fold((f64::MAX, f64::MIN), |(lo, hi), x| (lo.min(x), hi.max(x)));
    assert_eq!(
        (low, high),
        (2.5, 10.5),
        "the ring's sides on pixel centres"
    );
    for (i, a) in source.iter().enumerate() {
        for (j, b) in source.iter().enumerate() {
            if a.0 < b.0 {
                assert!(points[i].0 <= points[j].0, "x order kept");
            }
            if a.1 < b.1 {
                assert!(points[i].1 <= points[j].1, "y order kept");
            }
        }
    }
}

#[test]
fn a_drawing_already_on_the_grid_is_left_as_it_is() {
    let square = vec![rectangle(2.0, 3.0, 9.0, 12.0)];
    assert_eq!(fitted(square.clone(), Phase::Boundary), square);
}

#[test]
fn parts_fitted_together_keep_where_they_met() {
    let box_edge = [rectangle(1.4, 1.4, 8.6, 8.6)];
    let lid = [SubPath::open(vec![(1.4, 3.3), (8.6, 3.3)])];
    let fit = GridFit::new(
        SIDE,
        [
            (box_edge.as_slice(), Phase::Boundary),
            (lid.as_slice(), Phase::Boundary),
        ],
    );
    let mut lid = lid.to_vec();
    fit.apply(&mut lid);
    assert_eq!(
        lid[0].points,
        vec![(1.0, 3.0), (9.0, 3.0)],
        "the lid still spans the box"
    );
}

#[test]
fn a_drawing_symmetric_about_the_centre_fits_symmetric() {
    for side in [13u32, 14, 15, 16] {
        let centre = f64::from(side) / 2.0;
        // Edges a whole half pixel either side of the centre are ties, which
        // must break away from it on both sides alike.
        let plate = [rectangle(
            centre - 2.5,
            centre - 4.3,
            centre + 2.5,
            centre + 4.3,
        )];
        let stems = [
            SubPath::open(vec![(centre - 3.2, 1.7), (centre - 3.2, 9.4)]),
            SubPath::open(vec![(centre + 3.2, 1.7), (centre + 3.2, 9.4)]),
        ];
        let fit = GridFit::new(
            side,
            [
                (plate.as_slice(), Phase::Boundary),
                (stems.as_slice(), Phase::of_stroke(1.0)),
            ],
        );
        let (mut plate, mut stems) = (plate.to_vec(), stems.to_vec());
        fit.apply(&mut plate);
        fit.apply(&mut stems);
        let (left, right) = (plate[0].points[0].0, plate[0].points[1].0);
        assert!(
            near(centre - left, right - centre),
            "{side}: plate {left}..{right}"
        );
        let (low, high) = (stems[0].points[0].0, stems[1].points[0].0);
        assert!(
            near(centre - low, high - centre),
            "{side}: stems {low}, {high}"
        );
        assert!(
            near(low * 2.0 % 2.0, 1.0),
            "{side}: a one-pixel stroke on a pixel centre"
        );
    }
}

#[test]
fn a_middle_stroke_the_centre_cannot_hold_takes_the_drawing_half_a_pixel_aside() {
    // An up arrow at 14 pixels: its shaft and apex on the centre, which is a
    // pixel boundary, and a one-pixel stroke must lie on a pixel's centre.
    let centre = 7.0;
    let arrow = [
        SubPath::open(vec![(centre, 11.7), (centre, 2.3)]),
        SubPath::open(vec![
            (centre - 3.5, 5.8),
            (centre, 2.3),
            (centre + 3.5, 5.8),
        ]),
    ];
    let fit = GridFit::new(14, [(arrow.as_slice(), Phase::of_stroke(1.0))]);
    let mut arrow = arrow.to_vec();
    fit.apply(&mut arrow);
    let shaft = arrow[0].points[0].0;
    let (left, apex, right) = (
        arrow[1].points[0].0,
        arrow[1].points[1].0,
        arrow[1].points[2].0,
    );
    assert!(near(shaft, centre + 0.5), "the shaft on a pixel centre");
    assert!(near(apex, shaft), "the apex stays on the shaft");
    assert!(
        near(apex - left, right - apex),
        "the head stays symmetric about it"
    );
}
