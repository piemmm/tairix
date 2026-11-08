//! Unit tests for what a `<textPath>` names and where along it a text starts.

use tairix_bezier::Point;

use super::{geometry, start, Along};
use crate::error::SvgError;
use crate::xml;

/// The root element `document` parses to.
fn element_of(document: &str) -> xml::Element<'_> {
    xml::parse(document, xml::Limits::COMPACT).expect("a document")
}

fn near(a: Point, b: Point) -> bool {
    a.distance(b) < 1e-9
}

/// What a bare `<textPath>` naming `target` is set along.
fn named(
    target: &xml::Element<'_>,
    max: usize,
) -> Result<Option<(tairix_bezier::Path, Option<f64>)>, SvgError> {
    geometry(
        &element_of("<textPath/>"),
        Some(target),
        (100.0, 100.0),
        max,
    )
}

/// The point and unit direction `distance` along what `target` makes a path.
fn along(target: &str, distance: f64) -> (Point, Point) {
    let (path, _) = named(&element_of(target), 64)
        .expect("read")
        .expect("a path");
    Along::new(&path, false)
        .expect("measured")
        .at(distance)
        .expect("on the path")
}

#[test]
fn a_path_is_set_along_in_the_text_s_space_with_its_stated_length() {
    let target = element_of(r#"<path d="M0 0 H100" transform="translate(5 6)" pathLength="20"/>"#);
    let (path, stated) = named(&target, 16).expect("read").expect("a path");
    assert_eq!(
        path.subpaths[0].start,
        Point::new(5.0, 6.0),
        "its own transform applied"
    );
    assert_eq!(stated, Some(20.0));
    let unstated = element_of(r#"<path d="M0 0 H100" pathLength="0"/>"#);
    assert_eq!(named(&unstated, 16).expect("read").expect("a path").1, None);
    let group = element_of("<g/>");
    assert!(
        named(&group, 16).expect("read").is_none(),
        "no shape at all"
    );
    let long = element_of(r#"<path d="M0 0 L1 1 L2 2 L3 3"/>"#);
    assert_eq!(named(&long, 3).err(), Some(SvgError::TooComplex));
}

#[test]
fn a_basic_shape_is_the_path_svg_2_makes_it() {
    let (start, heading) = along(r#"<circle cx="50" cy="50" r="40"/>"#, 0.0);
    assert!(
        near(start, Point::new(90.0, 50.0)),
        "from its rightmost point"
    );
    assert!(
        near(heading, Point::new(0.0, 1.0)),
        "clockwise: down the page"
    );
    let quarter = core::f64::consts::FRAC_PI_2 * 40.0;
    let (bottom, _) = along(r#"<circle cx="50" cy="50" r="40"/>"#, quarter);
    assert!(bottom.distance(Point::new(50.0, 90.0)) < 0.05, "{bottom:?}");
    let (corner, heading) = along(
        r#"<rect x="10" y="20" width="60" height="40" rx="5"/>"#,
        0.0,
    );
    assert!(
        near(corner, Point::new(15.0, 20.0)),
        "past the top-left corner"
    );
    assert!(near(heading, Point::new(1.0, 0.0)), "along the top");
    let (square, _) = along(r#"<rect x="10" y="20" width="60" height="40"/>"#, 70.0);
    assert!(near(square, Point::new(70.0, 30.0)), "down the right side");
    let (line, heading) = along(r#"<line x1="0" y1="0" x2="0" y2="10"/>"#, 4.0);
    assert!(near(line, Point::new(0.0, 4.0)) && near(heading, Point::new(0.0, 1.0)));
    let (closed, _) = along(r#"<polygon points="0,0 10,0 10,10"/>"#, 24.0);
    let back = 10.0 - 4.0 * core::f64::consts::FRAC_1_SQRT_2;
    assert!(
        near(closed, Point::new(back, back)),
        "on the side that closes it: {closed:?}"
    );
    let empty = element_of(r#"<rect width="0" height="10"/>"#);
    assert!(
        named(&empty, 16).expect("read").is_none(),
        "enclosing nothing"
    );
}

#[test]
fn a_text_path_s_own_data_wins_over_what_it_names_and_empty_data_is_none() {
    let target = element_of(r#"<path d="M0 0 H100" transform="translate(5 6)" pathLength="20"/>"#);
    let own = element_of(r#"<textPath path="M1 2 V50"/>"#);
    let (path, stated) = geometry(&own, Some(&target), (100.0, 100.0), 16)
        .expect("read")
        .expect("a path");
    assert_eq!(
        path.subpaths[0].start,
        Point::new(1.0, 2.0),
        "untransformed"
    );
    assert_eq!(stated, None, "its own length unstated");
    let blank = element_of(r#"<textPath path=" "/>"#);
    assert!(
        geometry(&blank, Some(&target), (100.0, 100.0), 16)
            .expect("read")
            .is_none(),
        "empty data is no path, whatever is named"
    );
    let broken = element_of(r#"<textPath path="M1"/>"#);
    assert!(geometry(&broken, None, (100.0, 100.0), 16).is_err());
}

#[test]
fn a_start_offset_is_a_share_of_the_length_or_a_length_in_the_path_s_units() {
    let at = |offset: &str, stated: Option<f64>| {
        let document = alloc::format!(r#"<textPath startOffset="{offset}"/>"#);
        let element = element_of(&document);
        start(&element, 10.0, 200.0, stated, (100.0, 100.0))
    };
    assert_eq!(at("25%", None), Ok(50.0));
    assert_eq!(
        at("25%", Some(50.0)),
        Ok(50.0),
        "a share of the real length"
    );
    assert_eq!(at("30", None), Ok(30.0));
    assert_eq!(
        at("30", Some(50.0)),
        Ok(120.0),
        "in the path's stated units"
    );
    assert_eq!(at("2em", None), Ok(20.0));
    assert_eq!(at("-15", None), Ok(-15.0));
    assert_eq!(at("far", None), Err(SvgError::InvalidNumber));
    let none = element_of("<textPath/>");
    assert_eq!(start(&none, 10.0, 200.0, None, (100.0, 100.0)), Ok(0.0));
}

#[test]
fn a_path_run_from_its_end_turns_its_text_round() {
    let target = element_of(r#"<path d="M0 0 H100 V100"/>"#);
    let (path, _) = named(&target, 16).expect("read").expect("a path");
    let forwards = Along::new(&path, false).expect("measured");
    let backwards = Along::new(&path, true).expect("measured");
    assert!((forwards.length() - 200.0).abs() < 1e-9);
    let (point, direction) = forwards.at(150.0).expect("on the second leg");
    assert!(near(point, Point::new(100.0, 50.0)) && near(direction, Point::new(0.0, 1.0)));
    let (point, direction) = backwards.at(50.0).expect("on the second leg");
    assert!(near(point, Point::new(100.0, 50.0)) && near(direction, Point::new(0.0, -1.0)));
    assert_eq!(forwards.at(-1.0), None);
    let (past, onward) = forwards.extended(210.0).expect("past the end");
    assert!(near(past, Point::new(100.0, 110.0)) && near(onward, Point::new(0.0, 1.0)));
    let (before, _) = forwards.extended(-5.0).expect("before the start");
    assert!(near(before, Point::new(-5.0, 0.0)));
}
