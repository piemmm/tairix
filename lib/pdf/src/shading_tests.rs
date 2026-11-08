//! Shadings as gradients: exact stops for straight runs and sharp steps,
//! sampled ones for curves.

#![allow(
    clippy::float_cmp,
    reason = "a radius read from the file is compared exactly: it is the number written"
)]

use super::{shading, Shading, MAX_STOPS};
use crate::display::GradientKind;
use crate::document::Document;
use crate::fixture::{reference, with_objects, OPTIONS};
use crate::PdfError;

fn read(objects: &[(u32, &str)]) -> Result<Option<Shading>, PdfError> {
    let file = with_objects(objects, &[]);
    let document = Document::open(&file, &OPTIONS)?;
    shading(&document, &reference(10), None)
}

fn colours(shading: &Shading) -> alloc::vec::Vec<(f64, [u8; 4])> {
    shading
        .gradient
        .stops
        .iter()
        .map(|stop| (stop.offset, stop.colour.to_array()))
        .collect()
}

#[test]
fn a_straight_axial_shading_is_two_stops() {
    let read = read(&[(10, "<</ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] /Extend [true false] /Function <</FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1>>>>")])
        .expect("a shading")
        .expect("an axial shading");
    assert_eq!(
        colours(&read),
        [(0.0, [255, 0, 0, 255]), (1.0, [0, 0, 255, 255])]
    );
    assert_eq!(read.gradient.extend, (true, false));
    assert!(matches!(read.gradient.kind, GradientKind::Axial { .. }));
}

#[test]
fn stitched_straight_runs_keep_their_sharp_steps() {
    let read = read(&[
        (10, "<</ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 1 0] /Function 11 0 R>>"),
        (11, "<</FunctionType 3 /Domain [0 1] /Functions [12 0 R 13 0 R] /Bounds [0.5] /Encode [0 1 0 1]>>"),
        (12, "<</FunctionType 2 /Domain [0 1] /C0 [0] /C1 [0.2] /N 1>>"),
        (13, "<</FunctionType 2 /Domain [0 1] /C0 [0.8] /C1 [1] /N 1>>"),
    ])
    .expect("a shading")
    .expect("an axial shading");
    assert_eq!(
        colours(&read),
        [
            (0.0, [0, 0, 0, 255]),
            (0.5, [51, 51, 51, 255]),
            (0.5, [204, 204, 204, 255]),
            (1.0, [255, 255, 255, 255])
        ]
    );
}

#[test]
fn a_curved_run_is_sampled_within_the_tolerance() {
    let read = read(&[(10, "<</ShadingType 3 /ColorSpace /DeviceGray /Coords [0 0 0 0 0 10] /Function <</FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 2>>>>")])
        .expect("a shading")
        .expect("a radial shading");
    let stops = &read.gradient.stops;
    assert!(
        stops.len() > 4 && stops.len() <= MAX_STOPS,
        "{} stops",
        stops.len()
    );
    for pair in stops.windows(2) {
        let middle = f64::midpoint(pair[0].offset, pair[1].offset);
        let straight = f64::midpoint(f64::from(pair[0].colour.r), f64::from(pair[1].colour.r));
        assert!((straight - middle * middle * 255.0).abs() < 1.5, "{pair:?}");
    }
    assert!(
        matches!(read.gradient.kind, GradientKind::Radial { end_radius, .. } if end_radius == 10.0)
    );
}

#[test]
fn one_function_a_component_is_read_together() {
    let read = read(&[(10, "<</ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 1 0] /Function [<</FunctionType 2 /Domain [0 1] /N 1>> <</FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1>> <</FunctionType 2 /Domain [0 1] /C0 [0] /C1 [0] /N 1>>]>>")])
        .expect("a shading")
        .expect("an axial shading");
    assert_eq!(
        colours(&read),
        [(0.0, [0, 255, 0, 255]), (1.0, [255, 0, 0, 255])]
    );
}

#[test]
fn a_domain_may_run_backwards() {
    let read = read(&[(10, "<</ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 1 0] /Domain [1 0] /Function <</FunctionType 2 /Domain [0 1] /N 1>>>>")])
        .expect("a shading")
        .expect("an axial shading");
    assert_eq!(
        colours(&read),
        [(0.0, [255, 255, 255, 255]), (1.0, [0, 0, 0, 255])]
    );
}

#[test]
fn other_shading_types_are_not_gradients() {
    for kind in [1, 4, 5, 6, 7] {
        let body = alloc::format!("<</ShadingType {kind} /ColorSpace /DeviceGray>>");
        assert!(read(&[(10, &body)]).expect("read").is_none(), "{kind}");
    }
}

#[test]
fn malformed_shadings_are_refused() {
    let function = "/Function <</FunctionType 2 /Domain [0 1] /N 1>>";
    for body in [
        alloc::format!(
            "<</ShadingType 3 /ColorSpace /DeviceGray /Coords [0 0 -1 0 0 1] {function}>>"
        ),
        alloc::format!("<</ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 1] {function}>>"),
        alloc::format!("<</ShadingType 2 /ColorSpace /Pattern /Coords [0 0 1 0] {function}>>"),
        alloc::format!("<</ShadingType 9 /ColorSpace /DeviceGray /Coords [0 0 1 0] {function}>>"),
    ] {
        assert_eq!(
            read(&[(10, &body)]).err(),
            Some(PdfError::Malformed),
            "{body}"
        );
    }
}

#[test]
fn a_shading_whose_pieces_meet_past_the_stops_a_gradient_holds_is_none() {
    let pieces = 200;
    let functions = "<</FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1>> ".repeat(pieces);
    let bounds: alloc::vec::Vec<alloc::string::String> = (1..pieces)
        .map(|piece| alloc::format!("{}", f64::from(u32::try_from(piece).expect("few")) / 200.0))
        .collect();
    let function = alloc::format!(
        "<</FunctionType 3 /Domain [0 1] /Functions [{functions}] /Bounds [{}] /Encode [{}]>>",
        bounds.join(" "),
        "0 1 ".repeat(pieces)
    );
    let shading = alloc::format!(
        "<</ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 1 0] /Function {function}>>"
    );
    let read = read(&[(10, &shading)]).expect("a shading read");
    assert!(read.is_none());
    assert!(2 * (pieces + 1) > MAX_STOPS);
}
