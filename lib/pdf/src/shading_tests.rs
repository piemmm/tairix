//! Shadings as gradients: exact stops for straight runs and sharp steps,
//! sampled ones for curves.

#![allow(
    clippy::float_cmp,
    reason = "a radius read from the file is compared exactly: it is the number written"
)]

use alloc::sync::Arc;

use super::{shading, stops, Colours, Shading, MAX_STOPS};
use crate::colour::Space;
use crate::display::GradientKind;
use crate::document::Document;
use crate::fixture::{reference, with_objects, OPTIONS};
use crate::function::Work;
use crate::limits::MAX_EVALUATION_STEPS;
use crate::PdfError;

fn read(objects: &[(u32, &str)]) -> Result<Option<Shading>, PdfError> {
    let file = with_objects(objects, &[]);
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS)?;
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
fn a_curve_before_many_sharp_steps_leaves_room_for_them() {
    // The curve alone would take every stop; each of the 126 steps after it
    // needs two.
    let functions: alloc::vec::Vec<&str> = core::iter::once("12 0 R")
        .chain((1..127u32).map(|step| if step % 2 == 0 { "13 0 R" } else { "14 0 R" }))
        .collect();
    let bounds: alloc::vec::Vec<alloc::string::String> = core::iter::once(0.5)
        .chain((1..126u32).map(|step| 0.5 + f64::from(step) / 252.0))
        .map(|bound| alloc::format!("{bound}"))
        .collect();
    let (functions, bounds) = (functions.join(" "), bounds.join(" "));
    let stitched = alloc::format!(
        "<</FunctionType 3 /Domain [0 1] /Functions [{functions}] /Bounds [{bounds}] /Encode [{}]>>",
        "0 1 ".repeat(127)
    );
    let file = with_objects(
        &[
            (
                10,
                "<</ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 1 0] /Function 11 0 R>>",
            ),
            (11, stitched.as_str()),
            (13, "<</FunctionType 2 /Domain [0 1] /C0 [0] /C1 [0] /N 1>>"),
            (14, "<</FunctionType 2 /Domain [0 1] /C0 [1] /C1 [1] /N 1>>"),
        ],
        &[(
            12,
            "/FunctionType 4 /Domain [0 1] /Range [0 1]",
            b"{ 10007 mul sin 1 add 2 div }",
        )],
    );
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let read = shading(&document, &reference(10), None)
        .expect("a shading")
        .expect("an axial shading");
    let stops = &read.gradient.stops;
    assert!(stops.len() <= MAX_STOPS, "{} stops", stops.len());
    assert!(
        stops.len() > 2 * 126,
        "every step kept: {} stops",
        stops.len()
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

/// A curve is refined evaluating each place once: t² is halved down to runs
/// of a sixteenth, 31 runs probed at their quarters, each run's middle a
/// quarter of the run it halves — so its 2 ends, 1 middle and 62 quarters.
#[test]
fn a_curve_is_refined_evaluating_each_place_once() {
    let file = with_objects(
        &[(10, "<</FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 2>>")],
        &[],
    );
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let colours = Colours::load(&document, &reference(10)).expect("a function");
    let work = Work::new();
    let placed = stops(&colours, &Arc::new(Space::Gray), (0.0, 1.0), &work)
        .expect("work to spare")
        .expect("stops");
    assert_eq!(
        placed.len(),
        17,
        "an end each side and a stop each run halved"
    );
    assert_eq!(MAX_EVALUATION_STEPS - work.left(), 2 + 1 + 2 * 31);
}
