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

#[test]
fn a_path_is_set_along_in_the_text_s_space_with_its_stated_length() {
    let target = element_of(r#"<path d="M0 0 H100" transform="translate(5 6)" pathLength="20"/>"#);
    let (path, stated) = geometry(&target, 16).expect("read").expect("a path");
    assert_eq!(
        path.subpaths[0].start,
        Point::new(5.0, 6.0),
        "its own transform applied"
    );
    assert_eq!(stated, Some(20.0));
    let unstated = element_of(r#"<path d="M0 0 H100" pathLength="0"/>"#);
    assert_eq!(
        geometry(&unstated, 16).expect("read").expect("a path").1,
        None
    );
    let circle = element_of(r#"<circle r="10"/>"#);
    assert!(
        geometry(&circle, 16).expect("read").is_none(),
        "only a path"
    );
    let long = element_of(r#"<path d="M0 0 L1 1 L2 2 L3 3"/>"#);
    assert_eq!(geometry(&long, 3).err(), Some(SvgError::TooComplex));
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
    let (path, _) = geometry(&target, 16).expect("read").expect("a path");
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
